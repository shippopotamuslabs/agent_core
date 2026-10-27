//! Tier 2 — FIFO compaction: drop the **oldest** messages until context pressure falls below the
//! configured threshold.
//!
//! ## Drop units (prefer conversational integrity)
//!
//! Scanning from the front of [`crate::session::AgentSession::messages`], each step removes **one**
//! unit:
//! - **User block** — a [`crate::message::AgentRole::User`] message followed by zero or more
//!   [`crate::message::AgentRole::Tool`] messages, then a matching
//!   [`crate::message::AgentRole::Assistant`] message. The entire contiguous span is removed
//!   together (tool traffic “rides along” with the user→assistant turn it belongs to).
//! - **Single message** — if the first removable message is not the start of a user block
//!   (e.g. leading assistant/tool orphan), only that one message is removed.
//!
//! ## Protected messages (never removed)
//!
//! - [`crate::message::AgentRole::System`] and [`crate::message::AgentRole::CompactSummary`]
//! - The **last** [`crate::message::AgentRole::User`] message in the list, **and every message
//!   after it** (current user turn tail).
//!
//! Ported from `ChatHistoryCompressor.compressHistory` in the Android domain layer, extended for
//! tool interleaving and protected roles as required by the agent-core ADR.

use super::{estimate_session_pressure, CompactionContext, CompactionOutcome, CompactionTier};
use crate::budget::{
    effective_retention_window_tokens, effective_threshold_tokens,
    should_compact_at_token_threshold,
};
use crate::compact::policy::threshold_ratio_for_trigger;
use crate::events::{AgentError, CompactionEvent, CompactionTier as TierEnum};
use crate::message::AgentRole;
use crate::session::{AgentConfig, AgentSession};

pub struct FifoCompactionTier;

impl Default for FifoCompactionTier {
    fn default() -> Self {
        Self
    }
}

fn last_user_index(messages: &[crate::message::AgentMessage]) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, m)| m.role == AgentRole::User)
        .map(|(i, _)| i)
}

fn is_protected_index(
    idx: usize,
    msg: &crate::message::AgentMessage,
    last_user: Option<usize>,
    retention_start: Option<usize>,
) -> bool {
    if matches!(msg.role, AgentRole::System | AgentRole::CompactSummary) {
        return true;
    }
    if let Some(u) = last_user {
        if idx >= u {
            return true;
        }
    }
    if let Some(start) = retention_start {
        if idx >= start {
            return true;
        }
    }
    false
}

fn retention_start_index(
    messages: &[crate::message::AgentMessage],
    retention_window_tokens: u32,
) -> Option<usize> {
    if retention_window_tokens == 0 {
        return None;
    }
    let mut approximate_tokens = 0u32;
    for (idx, msg) in messages.iter().enumerate().rev() {
        approximate_tokens =
            approximate_tokens.saturating_add((msg.content.len() as u32 / 4).max(1));
        if approximate_tokens >= retention_window_tokens {
            return Some(idx);
        }
    }
    Some(0)
}

/// Returns end-exclusive index of a user-block starting at `start`, or None if not a removable user block.
fn user_block_end(
    messages: &[crate::message::AgentMessage],
    start: usize,
    last_user: Option<usize>,
    retention_start: Option<usize>,
) -> Option<usize> {
    if start >= messages.len() || messages[start].role != AgentRole::User {
        return None;
    }
    if is_protected_index(start, &messages[start], last_user, retention_start) {
        return None;
    }
    let mut i = start + 1;
    while i < messages.len() {
        if is_protected_index(i, &messages[i], last_user, retention_start) {
            return None;
        }
        match messages[i].role {
            AgentRole::Tool => {
                i += 1;
            }
            AgentRole::Assistant => {
                return Some(i + 1);
            }
            _ => return None,
        }
    }
    None
}

fn first_removable_unit_range(
    messages: &[crate::message::AgentMessage],
    last_user: Option<usize>,
    retention_start: Option<usize>,
) -> Option<std::ops::Range<usize>> {
    let mut i = 0usize;
    while i < messages.len() {
        let msg = &messages[i];
        if !is_protected_index(i, msg, last_user, retention_start) {
            break;
        }
        i += 1;
    }
    if i >= messages.len() {
        return None;
    }

    if let Some(end) = user_block_end(messages, i, last_user, retention_start) {
        return Some(i..end);
    }

    Some(i..i + 1)
}

fn snapshot_context_covers(session: &AgentSession, message_ids: &[String]) -> bool {
    session
        .session_context
        .get("history_snapshot_ids")
        .and_then(|value| value.as_array())
        .map(|values| {
            message_ids.iter().all(|id| {
                values
                    .iter()
                    .any(|value| value.as_str().is_some_and(|snapshot_id| snapshot_id == id))
            })
        })
        .unwrap_or(false)
}

impl CompactionTier for FifoCompactionTier {
    fn apply(
        &self,
        mut session: AgentSession,
        config: &AgentConfig,
        ctx: &CompactionContext,
    ) -> CompactionOutcome {
        let tokens_before = estimate_session_pressure(&session, config, ctx).total_tokens;
        let threshold_ratio = threshold_ratio_for_trigger(config, ctx.trigger);
        let threshold_tokens =
            effective_threshold_tokens(config, ctx.context_window_tokens, threshold_ratio, None);

        if !should_compact_at_token_threshold(
            &estimate_session_pressure(&session, config, ctx),
            threshold_tokens,
        ) {
            return CompactionOutcome::NotApplicable;
        }

        let retention_start = retention_start_index(
            &session.messages,
            effective_retention_window_tokens(config, ctx.context_window_tokens),
        );
        if config.require_snapshot_before_fifo {
            let Some(range) = first_removable_unit_range(
                &session.messages,
                last_user_index(&session.messages),
                retention_start,
            ) else {
                return CompactionOutcome::NotApplicable;
            };
            let removal_ids: Vec<String> = session.messages[range]
                .iter()
                .map(|message| message.id.clone())
                .collect();
            if !config.enable_context_store || !snapshot_context_covers(&session, &removal_ids) {
                return CompactionOutcome::Failed(AgentError::CompactionFailed {
                    tier: "fifo".into(),
                    reason: "history snapshot required before fifo".into(),
                });
            }
        }

        let mut last_u = last_user_index(&session.messages);
        let mut changed = false;

        while should_compact_at_token_threshold(
            &estimate_session_pressure(&session, config, ctx),
            threshold_tokens,
        ) {
            let retention_start = retention_start_index(
                &session.messages,
                effective_retention_window_tokens(config, ctx.context_window_tokens),
            );
            let Some(range) =
                first_removable_unit_range(&session.messages, last_u, retention_start)
            else {
                break;
            };
            session.messages.drain(range.clone());
            changed = true;
            last_u = last_user_index(&session.messages);
        }

        if !changed {
            return CompactionOutcome::NotApplicable;
        }

        let tokens_after = estimate_session_pressure(&session, config, ctx).total_tokens;

        CompactionOutcome::Applied(
            session,
            CompactionEvent {
                tier: TierEnum::Fifo,
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
    use crate::message::{AgentMessage, MessageMeta};
    use serde_json::Value::Null;

    fn ctx(cw: u32) -> CompactionContext {
        CompactionContext {
            provider: "anthropic".into(),
            context_window_tokens: cw,
            tools_schema_json: "[]".into(),
            system_prompt: String::new(),
            context_sidecars_estimate: String::new(),
            capabilities: crate::session::SessionCapabilities {
                summarization_allowed: true,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            trigger: CompactionTrigger::MidTurn,
            suppress_summary_tier: false,
        }
    }

    fn msg(id: &str, role: AgentRole, content: &str) -> AgentMessage {
        AgentMessage {
            id: id.into(),
            role,
            content: content.into(),
            metadata: MessageMeta::new(),
        }
    }

    /// Long repeated token to grow tiktoken estimate quickly.
    fn long_text() -> String {
        "hello world alpha beta gamma delta epsilon zeta eta theta ".repeat(40)
    }

    #[test]
    fn empty_history_not_applicable() {
        let session = AgentSession::new(vec![], Null);
        let cfg = AgentConfig {
            require_snapshot_before_fifo: false,
            ..AgentConfig::default()
        };
        let tier = FifoCompactionTier;
        let out = tier.apply(session, &cfg, &ctx(50_000));
        assert!(matches!(out, CompactionOutcome::NotApplicable));
    }

    #[test]
    fn single_user_only_oldest_protected_if_latest_user() {
        let t = long_text();
        let session = AgentSession::new(vec![msg("u1", AgentRole::User, &t)], Null);
        let cfg = AgentConfig {
            require_snapshot_before_fifo: false,
            ..AgentConfig::default()
        };
        let tier = FifoCompactionTier;
        let out = tier.apply(session, &cfg, &ctx(500));
        assert!(matches!(out, CompactionOutcome::NotApplicable));
    }

    #[test]
    fn drops_user_assistant_pair_before_latest_user() {
        let t = long_text();
        let session = AgentSession::new(
            vec![
                msg("u1", AgentRole::User, &t),
                msg("a1", AgentRole::Assistant, &t),
                msg("u2", AgentRole::User, &t),
                msg("a2", AgentRole::Assistant, &t),
            ],
            Null,
        );
        let cfg = AgentConfig {
            require_snapshot_before_fifo: false,
            ..AgentConfig::default()
        };
        let tier = FifoCompactionTier;
        let out = tier.apply(session, &cfg, &ctx(800));
        match out {
            CompactionOutcome::Applied(s, ev) => {
                assert_eq!(ev.tier, TierEnum::Fifo);
                assert_eq!(s.messages.len(), 2);
                assert_eq!(s.messages[0].id, "u2");
                assert_eq!(s.messages[1].id, "a2");
            }
            o => panic!("expected Applied, got {o:?}"),
        }
    }

    #[test]
    fn user_tool_assistant_dropped_as_one_unit() {
        let t = long_text();
        let session = AgentSession::new(
            vec![
                msg("u1", AgentRole::User, &t),
                msg("tl", AgentRole::Tool, r#"{"r":1}"#),
                msg("a1", AgentRole::Assistant, &t),
                msg("u2", AgentRole::User, &t),
                msg("a2", AgentRole::Assistant, &t),
            ],
            Null,
        );
        let cfg = AgentConfig {
            require_snapshot_before_fifo: false,
            ..AgentConfig::default()
        };
        let out = FifoCompactionTier.apply(session, &cfg, &ctx(900));
        match out {
            CompactionOutcome::Applied(s, ev) => {
                assert_eq!(ev.tier, TierEnum::Fifo);
                assert_eq!(s.messages.len(), 2);
                assert_eq!(s.messages[0].id, "u2");
            }
            o => panic!("expected Applied {:?}", o),
        }
    }

    #[test]
    fn never_drops_system_or_compact_summary() {
        let t = long_text();
        let session = AgentSession::new(
            vec![
                msg("sys", AgentRole::System, &t),
                msg("u1", AgentRole::User, &t),
                msg("a1", AgentRole::Assistant, &t),
                msg("u2", AgentRole::User, &t),
                msg("a2", AgentRole::Assistant, &t),
            ],
            Null,
        );
        let cfg = AgentConfig {
            require_snapshot_before_fifo: false,
            ..AgentConfig::default()
        };
        let out = FifoCompactionTier.apply(session, &cfg, &ctx(900));
        match out {
            CompactionOutcome::Applied(s, ev) => {
                assert_eq!(ev.tier, TierEnum::Fifo);
                assert_eq!(s.messages[0].role, AgentRole::System);
                assert_eq!(s.messages.len(), 3); // sys + u2 + a2
            }
            o => panic!("expected Applied {:?}", o),
        }
    }

    #[test]
    fn leading_assistant_dropped_singly_then_pair() {
        let t = long_text();
        let session = AgentSession::new(
            vec![
                msg("a0", AgentRole::Assistant, &t),
                msg("u1", AgentRole::User, &t),
                msg("a1", AgentRole::Assistant, &t),
            ],
            Null,
        );
        let cfg = AgentConfig {
            require_snapshot_before_fifo: false,
            ..AgentConfig::default()
        };
        let out = FifoCompactionTier.apply(session, &cfg, &ctx(700));
        match out {
            CompactionOutcome::Applied(s, _) => {
                assert_eq!(s.messages.len(), 2);
                assert_eq!(s.messages[0].role, AgentRole::User);
            }
            o => panic!("expected Applied {:?}", o),
        }
    }
}
