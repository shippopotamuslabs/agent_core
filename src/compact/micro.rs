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
use crate::events::{AgentError, CompactionEvent, CompactionTier as TierEnum};
use crate::message::{AgentMessage, AgentRole, MessageMeta};
use crate::session::{AgentConfig, AgentSession};
use crate::tool_semantics::result_policy_for;
use crate::transcript_revision::replace_message_content;
use std::collections::HashMap;

const PLACEHOLDER: &str = "[Previous tool output cleared]";

pub struct MicroCompactionTier;

impl Default for MicroCompactionTier {
    fn default() -> Self {
        Self
    }
}

fn tool_name_from_message(meta: &MessageMeta) -> Option<String> {
    meta.tool_name().map(|s| s.to_string())
}

/// A micro-compaction candidate: the canonical tool name and the message index.
/// Returns `None` when the message must be left untouched.
fn micro_candidate(
    msg: &AgentMessage,
    idx: usize,
    catalog: &ToolCatalog,
    config: &AgentConfig,
) -> Option<(String, usize)> {
    if msg.role != AgentRole::Tool {
        return None;
    }
    if msg.metadata.is_superseded() {
        return None;
    }
    let raw_name = tool_name_from_message(&msg.metadata)?;
    let spec = catalog.resolve(&crate::tool_name::ToolName::new(&raw_name).ok()?)?;
    if !spec.reproducible {
        return None;
    }
    if result_policy_for(&config.tool_semantics, &spec.name)
        .is_some_and(|policy| policy.preserve_in_micro_compaction)
    {
        return None;
    }
    if config
        .tool_semantics
        .artifact
        .as_ref()
        .is_some_and(|artifact| {
            spec.name == artifact.start_tool_name || spec.name == artifact.append_tool_name
        })
    {
        return None;
    }
    Some((spec.name.as_str().to_string(), idx))
}

/// Indices to replace: per tool, every index older than the `keep` most recent.
/// A `keep` of 0 selects every candidate index.
fn cutoff_indices(per_tool: &HashMap<String, Vec<usize>>, keep: usize) -> Vec<usize> {
    let mut replace: Vec<usize> = per_tool
        .values()
        .filter(|indices| indices.len() > keep)
        .flat_map(|indices| indices.iter().take(indices.len() - keep).copied())
        .collect();
    replace.sort_unstable();
    replace.dedup();
    replace
}

/// Swap each selected result for the placeholder; fails fast on rewrite errors.
fn rewrite_placeholders(session: &mut AgentSession, replace: &[usize]) -> Result<(), AgentError> {
    let ids: Vec<String> = replace
        .iter()
        .filter_map(|&idx| session.messages.get(idx).map(|m| m.id.clone()))
        .collect();
    for id in ids {
        replace_message_content(session, &id, PLACEHOLDER.into(), TierEnum::Micro)?;
    }
    Ok(())
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

        // Collect: per-canonical-tool-name indices of Tool messages (chronological).
        let mut per_tool: HashMap<String, Vec<usize>> = HashMap::new();
        for (tool, idx) in session
            .messages
            .iter()
            .enumerate()
            .filter_map(|(idx, msg)| micro_candidate(msg, idx, &catalog, config))
        {
            per_tool.entry(tool).or_default().push(idx);
        }

        // Replace: oldest indices beyond the keep window.
        let replace = cutoff_indices(&per_tool, keep);
        if replace.is_empty() {
            return CompactionOutcome::NotApplicable;
        }

        // Rewrite: swap each selected result for the placeholder.
        if let Err(err) = rewrite_placeholders(&mut session, &replace) {
            return CompactionOutcome::Failed(err);
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
            suppress_summary_tier: false,
        }
    }

    fn tool_msg(id: &str, name: &str, content: &str) -> AgentMessage {
        AgentMessage {
            id: id.into(),
            role: AgentRole::Tool,
            content: content.into(),
            metadata: MessageMeta::new().with_tool_name(name),
        }
    }

    #[test]
    fn clears_old_web_search_keeps_recent_two() {
        let cfg = AgentConfig {
            tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
            micro_keep_recent: 2,
            ..Default::default()
        };

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
        let tier = MicroCompactionTier;
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
                    .any(|m| m.content == PLACEHOLDER && m.metadata.rewrite_of().is_some()));
                assert!(s.messages[0].metadata.is_superseded());
                assert!(
                    s.messages
                        .iter()
                        .filter(|m| { !m.metadata.is_superseded() })
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
        let cfg = AgentConfig {
            tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
            micro_keep_recent: 0,
            ..Default::default()
        };

        let big = "B".repeat(400);
        let session = AgentSession::new(
            vec![tool_msg("a1", "append_blocks", &big)],
            serde_json::Value::Null,
        );

        let tier = MicroCompactionTier;
        let outcome = tier.apply(session, &cfg, &ctx(CompactionTrigger::MidTurn));
        assert!(matches!(outcome, CompactionOutcome::NotApplicable));
    }

    #[test]
    fn micro_keep_recent_zero_clears_all_reproducible() {
        let cfg = AgentConfig {
            tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
            micro_keep_recent: 0,
            ..Default::default()
        };

        let session = AgentSession::new(
            vec![
                tool_msg("t1", "web_search", "first"),
                tool_msg("t2", "web_extract", "second"),
            ],
            serde_json::Value::Null,
        );

        let tier = MicroCompactionTier;
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
    fn micro_keep_zero_clears_every_same_tool_result() {
        // Characterization (FRG-41): keep == 0 replaces ALL reproducible results,
        // including every message of the same tool — no "recent" survivors.
        let cfg = AgentConfig {
            tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
            micro_keep_recent: 0,
            ..Default::default()
        };

        let session = AgentSession::new(
            vec![
                tool_msg("t1", "web_search", "first"),
                tool_msg("t2", "web_search", "second"),
                tool_msg("t3", "web_search", "third"),
            ],
            serde_json::Value::Null,
        );

        let tier = MicroCompactionTier;
        let outcome = tier.apply(session, &cfg, &ctx(CompactionTrigger::PostToolResult));
        match outcome {
            CompactionOutcome::Applied(s, ev) => {
                assert_eq!(ev.tier, TierEnum::Micro);
                // Rewrite appends one placeholder per replaced id and marks the
                // original superseded, so all 3 originals are superseded and
                // exactly 3 live placeholders remain.
                for id in ["t1", "t2", "t3"] {
                    let original = s.messages.iter().find(|m| m.id == id).unwrap();
                    assert!(original.metadata.is_superseded());
                }
                assert_eq!(
                    s.messages
                        .iter()
                        .filter(|m| !m.metadata.is_superseded())
                        .filter(|m| m.content == PLACEHOLDER)
                        .count(),
                    3
                );
            }
            o => panic!("expected Applied, got {o:?}"),
        }
    }

    #[test]
    fn search_x_recent_respects_keep_window() {
        let cfg = AgentConfig {
            tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
            micro_keep_recent: 1,
            ..Default::default()
        };

        let session = AgentSession::new(
            vec![
                tool_msg("a", "search_x_recent", "old"),
                tool_msg("b", "search_x_recent", "new"),
            ],
            serde_json::Value::Null,
        );

        let tier = MicroCompactionTier;
        let outcome = tier.apply(session, &cfg, &ctx(CompactionTrigger::Preflight));
        match outcome {
            CompactionOutcome::Applied(s, _) => {
                assert!(s
                    .messages
                    .iter()
                    .any(|m| m.content == PLACEHOLDER && m.metadata.rewrite_of().is_some()));
                assert!(s.messages.iter().any(|m| m.content == "new"));
            }
            o => panic!("expected Applied, got {o:?}"),
        }
    }
}
