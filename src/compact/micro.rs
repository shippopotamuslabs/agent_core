//! Tier 1 — micro-compact: replace older **reproducible** tool **results** with a placeholder.
//!
//! Only messages with [`crate::message::AgentRole::Tool`] are considered. The canonical tool name
//! is read from message metadata (`metadata.tool_name`, string). Host adapters should set this
//! when mapping provider-native tool messages into [`crate::message::AgentMessage`].
//!
//! Non-reproducible tools (per [`crate::catalog::ToolCatalog`]) are never modified. User and
//! assistant turns are never touched. Tools with `preserve_in_micro_compaction` are excluded.

use super::{estimate_session_pressure, CompactionContext, CompactionOutcome, CompactionTier};
use crate::budget::effective_micro_keep_recent;
use crate::catalog::ToolCatalog;
use crate::events::{CompactionEvent, CompactionTier as TierEnum};
use crate::message::AgentRole;
use crate::session::{AgentConfig, AgentSession};
use crate::tool_semantics::result_policy_for;
use crate::transcript_revision::replace_message_content;

const PLACEHOLDER: &str = "[Previous tool output cleared]";

pub struct MicroCompactionTier;

impl Default for MicroCompactionTier {
    fn default() -> Self {
        Self
    }
}

fn tool_name_from_message(meta: &serde_json::Value) -> Option<String> {
    meta.get("tool_name")
        .or_else(|| meta.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

impl CompactionTier for MicroCompactionTier {
    fn apply(
        &self,
        mut session: AgentSession,
        config: &AgentConfig,
        ctx: &CompactionContext,
    ) -> CompactionOutcome {
        let tokens_before = estimate_session_pressure(&session, config, ctx).total_tokens;

        let keep = effective_micro_keep_recent(config, ctx.context_window_tokens) as usize;
        let catalog = match ToolCatalog::from_semantics(&config.tool_semantics) {
            Ok(catalog) => catalog,
            Err(_) => return CompactionOutcome::NotApplicable,
        };

        // Per-canonical-tool-name, indices of Tool messages (chronological).
        let mut per_tool: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();

        for (idx, msg) in session.messages.iter().enumerate() {
            if msg.role != AgentRole::Tool {
                continue;
            }
            if msg
                .metadata
                .get("superseded")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                continue;
            }
            let Some(raw_name) = tool_name_from_message(&msg.metadata) else {
                continue;
            };
            let Some(spec) = catalog.resolve(&raw_name) else {
                continue;
            };
            if !spec.reproducible {
                continue;
            }
            if result_policy_for(&config.tool_semantics, &spec.name)
                .is_some_and(|policy| policy.preserve_in_micro_compaction)
            {
                continue;
            }
            if config.tool_semantics.artifact.as_ref().is_some_and(|artifact| {
                spec.name == artifact.start_tool_name || spec.name == artifact.append_tool_name
            }) {
                continue;
            }
            per_tool.entry(spec.name.clone()).or_default().push(idx);
        }

        let mut replace: Vec<usize> = Vec::new();
        for indices in per_tool.values() {
            if indices.is_empty() {
                continue;
            }
            if keep == 0 {
                replace.extend(indices.iter().copied());
                continue;
            }
            if indices.len() <= keep {
                continue;
            }
            let cutoff = indices.len() - keep;
            replace.extend(indices.iter().take(cutoff).copied());
        }

        replace.sort_unstable();
        replace.dedup();

        if replace.is_empty() {
            return CompactionOutcome::NotApplicable;
        }

        let ids: Vec<String> = replace
            .iter()
            .filter_map(|&idx| session.messages.get(idx).map(|m| m.id.clone()))
            .collect();
        for id in ids {
            if let Err(err) =
                replace_message_content(&mut session, &id, PLACEHOLDER.into(), TierEnum::Micro)
            {
                return CompactionOutcome::Failed(err);
            }
        }

        let tokens_after = estimate_session_pressure(&session, config, ctx).total_tokens;

        CompactionOutcome::Applied(
            session,
            CompactionEvent {
                tier: TierEnum::Micro,
                tokens_before,
                tokens_after,
                trigger: ctx.trigger,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::CompactionTrigger;
    use crate::message::AgentMessage;
    use serde_json::json;

    fn ctx(trigger: CompactionTrigger) -> CompactionContext {
        CompactionContext {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tools_schema_json: "[]".into(),
            system_prompt: String::new(),
            context_sidecars_estimate: String::new(),
            capabilities: crate::session::SessionCapabilities {
                summarization_allowed: true,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            trigger,
        }
    }

    fn tool_msg(id: &str, name: &str, content: &str) -> AgentMessage {
        AgentMessage {
            id: id.into(),
            role: AgentRole::Tool,
            content: content.into(),
            metadata: json!({ "tool_name": name }),
        }
    }

    #[test]
    fn clears_old_web_search_keeps_recent_two() {
        let mut cfg = AgentConfig::default();
        cfg.tool_semantics = crate::tool_semantics::test_fixture_enclave_semantics();
        cfg.micro_keep_recent = 2;

        let session = AgentSession::new(
            vec![
                tool_msg("t1", "web_search", &"x".repeat(500)),
                tool_msg("t2", "web_search", &"y".repeat(500)),
                tool_msg("t3", "web_search", &"z".repeat(500)),
            ],
            serde_json::Value::Null,
        );

        let before =
            super::estimate_session_pressure(&session, &cfg, &ctx(CompactionTrigger::MidTurn))
                .total_tokens;
        let tier = MicroCompactionTier::default();
        let outcome = tier.apply(session, &cfg, &ctx(CompactionTrigger::MidTurn));
        match outcome {
            CompactionOutcome::Applied(s, ev) => {
                assert_eq!(ev.tier, TierEnum::Micro);
                assert!(ev.tokens_after < ev.tokens_before);
                let after =
                    super::estimate_session_pressure(&s, &cfg, &ctx(CompactionTrigger::MidTurn))
                        .total_tokens;
                assert_eq!(after, ev.tokens_after);
                assert!(after < before);
                assert!(s
                    .messages
                    .iter()
                    .any(|m| m.content == PLACEHOLDER && m.metadata.get("rewrite_of").is_some()));
                assert_eq!(s.messages[0].metadata["superseded"], json!(true));
                assert!(
                    s.messages
                        .iter()
                        .filter(|m| {
                            !m.metadata
                                .get("superseded")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false)
                        })
                        .filter(|m| m.content.contains(PLACEHOLDER))
                        .count()
                        == 1
                );
                assert!(s
                    .messages
                    .iter()
                    .filter(|m| m.id == "t2" || m.id == "t3")
                    .all(|m| !m.content.contains(PLACEHOLDER)));
            }
            o => panic!("expected Applied, got {o:?}"),
        }
    }

    #[test]
    fn append_blocks_never_cleared() {
        let mut cfg = AgentConfig::default();
        cfg.tool_semantics = crate::tool_semantics::test_fixture_enclave_semantics();
        cfg.micro_keep_recent = 0;

        let big = "B".repeat(400);
        let session = AgentSession::new(
            vec![tool_msg("a1", "append_blocks", &big)],
            serde_json::Value::Null,
        );

        let tier = MicroCompactionTier::default();
        let outcome = tier.apply(session, &cfg, &ctx(CompactionTrigger::MidTurn));
        assert!(matches!(outcome, CompactionOutcome::NotApplicable));
    }

    #[test]
    fn micro_keep_recent_zero_clears_all_reproducible() {
        let mut cfg = AgentConfig::default();
        cfg.tool_semantics = crate::tool_semantics::test_fixture_enclave_semantics();
        cfg.micro_keep_recent = 0;

        let session = AgentSession::new(
            vec![
                tool_msg("t1", "web_search", "first".into()),
                tool_msg("t2", "web_extract", "second".into()),
            ],
            serde_json::Value::Null,
        );

        let tier = MicroCompactionTier::default();
        let outcome = tier.apply(session, &cfg, &ctx(CompactionTrigger::PostToolResult));
        match outcome {
            CompactionOutcome::Applied(s, ev) => {
                assert_eq!(ev.tier, TierEnum::Micro);
                assert!(
                    s.messages
                        .iter()
                        .filter(|m| m.content == PLACEHOLDER)
                        .count()
                        >= 2
                );
            }
            o => panic!("expected Applied, got {o:?}"),
        }
    }

    #[test]
    fn search_x_recent_respects_keep_window() {
        let mut cfg = AgentConfig::default();
        cfg.tool_semantics = crate::tool_semantics::test_fixture_enclave_semantics();
        cfg.micro_keep_recent = 1;

        let session = AgentSession::new(
            vec![
                tool_msg("a", "search_x_recent", "old".into()),
                tool_msg("b", "search_x_recent", "new".into()),
            ],
            serde_json::Value::Null,
        );

        let tier = MicroCompactionTier::default();
        let outcome = tier.apply(session, &cfg, &ctx(CompactionTrigger::Preflight));
        match outcome {
            CompactionOutcome::Applied(s, _) => {
                assert!(s
                    .messages
                    .iter()
                    .any(|m| m.content == PLACEHOLDER && m.metadata.get("rewrite_of").is_some()));
                assert!(s.messages.iter().any(|m| m.content == "new"));
            }
            o => panic!("expected Applied, got {o:?}"),
        }
    }
}
