//! Tier 1 — micro-compact: replace older **reproducible** tool **results** with a placeholder.
//!
//! Only messages with [`crate::message::AgentRole::Tool`] are considered. The canonical tool name
//! is read from message metadata (`metadata.tool_name`, string). Host adapters should set this
//! when mapping provider-native tool messages into [`crate::message::AgentMessage`].
//!
//! Non-reproducible tools (per [`crate::catalog::ToolCatalog`]) are never modified. User and
//! assistant turns are never touched. `append_blocks` is explicitly excluded here (tier 3 squashes it).

use super::{CompactionContext, CompactionOutcome, CompactionTier};
use crate::budget::TokenLedger;
use crate::catalog::ToolCatalog;
use crate::events::{CompactionEvent, CompactionTier as TierEnum};
use crate::message::AgentRole;
use crate::session::{AgentConfig, AgentSession};

const PLACEHOLDER: &str = "[Previous tool output cleared]";

pub struct MicroCompactionTier {
    catalog: &'static ToolCatalog,
}

impl MicroCompactionTier {
    pub fn new(catalog: &'static ToolCatalog) -> Self {
        Self { catalog }
    }

    pub fn global() -> Self {
        Self::new(ToolCatalog::global())
    }
}

impl Default for MicroCompactionTier {
    fn default() -> Self {
        Self::global()
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
        let tokens_before = TokenLedger::estimate(
            &session,
            config,
            &ctx.provider,
            ctx.context_window_tokens,
            &ctx.tools_schema_json,
        )
        .total_tokens;

        let keep = config.micro_keep_recent as usize;

        // Per-canonical-tool-name, indices of Tool messages (chronological).
        let mut per_tool: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();

        for (idx, msg) in session.messages.iter().enumerate() {
            if msg.role != AgentRole::Tool {
                continue;
            }
            let Some(raw_name) = tool_name_from_message(&msg.metadata) else {
                continue;
            };
            let Some(spec) = self.catalog.resolve(&raw_name) else {
                continue;
            };
            if !spec.reproducible {
                continue;
            }
            // Explicit guard: append_blocks / artifact tools must not be micro-cleared.
            if spec.name == "append_blocks" || spec.name == "start_artifact" {
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

        for idx in replace {
            if let Some(msg) = session.messages.get_mut(idx) {
                msg.content = PLACEHOLDER.into();
            }
        }

        let tokens_after = TokenLedger::estimate(
            &session,
            config,
            &ctx.provider,
            ctx.context_window_tokens,
            &ctx.tools_schema_json,
        )
        .total_tokens;

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
        cfg.micro_keep_recent = 2;

        let session = AgentSession::new(
            vec![
                tool_msg("t1", "web_search", &"x".repeat(500)),
                tool_msg("t2", "web_search", &"y".repeat(500)),
                tool_msg("t3", "web_search", &"z".repeat(500)),
            ],
            serde_json::Value::Null,
        );

        let before = TokenLedger::estimate(&session, &cfg, "anthropic", 200_000, "[]").total_tokens;
        let tier = MicroCompactionTier::global();
        let outcome = tier.apply(session, &cfg, &ctx(CompactionTrigger::MidTurn));
        match outcome {
            CompactionOutcome::Applied(s, ev) => {
                assert_eq!(ev.tier, TierEnum::Micro);
                assert!(ev.tokens_after < ev.tokens_before);
                let after =
                    TokenLedger::estimate(&s, &cfg, "anthropic", 200_000, "[]").total_tokens;
                assert_eq!(after, ev.tokens_after);
                assert!(after < before);
                assert_eq!(s.messages[0].content, PLACEHOLDER);
                assert!(!s.messages[1].content.contains(PLACEHOLDER));
                assert!(!s.messages[2].content.contains(PLACEHOLDER));
            }
            o => panic!("expected Applied, got {o:?}"),
        }
    }

    #[test]
    fn append_blocks_never_cleared() {
        let mut cfg = AgentConfig::default();
        cfg.micro_keep_recent = 0;

        let big = "B".repeat(400);
        let session = AgentSession::new(
            vec![tool_msg("a1", "append_blocks", &big)],
            serde_json::Value::Null,
        );

        let tier = MicroCompactionTier::global();
        let outcome = tier.apply(session, &cfg, &ctx(CompactionTrigger::MidTurn));
        assert!(matches!(outcome, CompactionOutcome::NotApplicable));
    }

    #[test]
    fn micro_keep_recent_zero_clears_all_reproducible() {
        let mut cfg = AgentConfig::default();
        cfg.micro_keep_recent = 0;

        let session = AgentSession::new(
            vec![
                tool_msg("t1", "web_search", "first".into()),
                tool_msg("t2", "web_extract", "second".into()),
            ],
            serde_json::Value::Null,
        );

        let tier = MicroCompactionTier::global();
        let outcome = tier.apply(session, &cfg, &ctx(CompactionTrigger::PostToolResult));
        match outcome {
            CompactionOutcome::Applied(s, ev) => {
                assert_eq!(ev.tier, TierEnum::Micro);
                assert_eq!(s.messages[0].content, PLACEHOLDER);
                assert_eq!(s.messages[1].content, PLACEHOLDER);
            }
            o => panic!("expected Applied, got {o:?}"),
        }
    }

    #[test]
    fn search_x_recent_respects_keep_window() {
        let mut cfg = AgentConfig::default();
        cfg.micro_keep_recent = 1;

        let session = AgentSession::new(
            vec![
                tool_msg("a", "search_x_recent", "old".into()),
                tool_msg("b", "search_x_recent", "new".into()),
            ],
            serde_json::Value::Null,
        );

        let tier = MicroCompactionTier::global();
        let outcome = tier.apply(session, &cfg, &ctx(CompactionTrigger::Preflight));
        match outcome {
            CompactionOutcome::Applied(s, _) => {
                assert_eq!(s.messages[0].content, PLACEHOLDER);
                assert_eq!(s.messages[1].content, "new");
            }
            o => panic!("expected Applied, got {o:?}"),
        }
    }
}
