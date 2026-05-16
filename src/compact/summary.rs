//! Tier 4 — LLM summary compaction (**host callback**).
//!
//! When synchronous tiers are insufficient and `SessionCapabilities.summarization_allowed` is true,
//! this tier returns [`super::CompactionOutcome::NeedsHostCall`] carrying a [`crate::turn_io::SummaryRequest`].
//! The engine forwards that to the host; the host later calls [`apply_summary_result`] (or equivalent)
//! with a [`crate::turn_io::SummaryResult`].

use crate::budget::TokenLedger;
use crate::events::{
    CompactionEvent,
    CompactionTier as CompactionTierKind,
};
use crate::message::{AgentMessage, AgentRole};
use crate::session::{AgentConfig, AgentSession};
use crate::turn_io::{SummaryRequest, SummaryResult};
use super::{CompactionContext, CompactionOutcome, CompactionTier};

/// Instruction string forwarded to the host summarizer. Must remain **verbatim** across versions.
pub const SUMMARY_INSTRUCTION: &str = "Summarize preserving: tool decisions made, artifact state, key facts. Omit raw tool output.";

pub struct SummaryCompactionTier;

impl Default for SummaryCompactionTier {
    fn default() -> Self {
        Self
    }
}

fn last_user_index(messages: &[AgentMessage]) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, m)| m.role == AgentRole::User)
        .map(|(i, _)| i)
}

/// Chooses messages to summarize: everything **before** the last user turn, excluding system and
/// prior compact summaries (they stay in the session as anchors).
fn messages_to_summarize(messages: &[AgentMessage]) -> Option<(Vec<AgentMessage>, String)> {
    let last_u = last_user_index(messages)?;
    if last_u == 0 {
        return None;
    }
    let mut picked = Vec::new();
    for msg in messages.iter().take(last_u) {
        if matches!(msg.role, AgentRole::System | AgentRole::CompactSummary) {
            continue;
        }
        picked.push(msg.clone());
    }
    if picked.is_empty() {
        return None;
    }
    let last_id = picked.last()?.id.clone();
    Some((picked, last_id))
}

impl CompactionTier for SummaryCompactionTier {
    fn apply(
        &self,
        session: AgentSession,
        _config: &AgentConfig,
        ctx: &CompactionContext,
    ) -> CompactionOutcome {
        if !ctx.capabilities.summarization_allowed {
            return CompactionOutcome::NotApplicable;
        }

        // KV-fixed sessions cannot rebuild around a summarization round-trip (ADR AGENT-047).
        if ctx.capabilities.kv_fixed {
            return CompactionOutcome::NotApplicable;
        }

        let Some((msgs, last_message_id)) = messages_to_summarize(&session.messages) else {
            return CompactionOutcome::NotApplicable;
        };

        let req = SummaryRequest {
            messages_to_summarize: msgs,
            instruction: SUMMARY_INSTRUCTION.to_string(),
            last_message_id,
        };

        // If the selected range is empty of content (shouldn't happen), bail
        if req.messages_to_summarize.is_empty() {
            return CompactionOutcome::NotApplicable;
        }

        debug_assert_eq!(req.instruction, SUMMARY_INSTRUCTION);

        CompactionOutcome::NeedsHostCall(req, session)
    }
}

/// Applies host summarization: removes the summarized contiguous ID range and inserts
/// a single [`AgentRole::CompactSummary`] message at that position.
pub fn apply_summary_result(
    session: &mut AgentSession,
    req: &SummaryRequest,
    result: &SummaryResult,
    config: &AgentConfig,
    ctx: &CompactionContext,
) -> Result<(), &'static str> {
    if result.last_message_id != req.last_message_id {
        return Err("last_message_id mismatch");
    }
    if req.messages_to_summarize.is_empty() {
        return Err("empty summarize set");
    }

    let first_id = &req.messages_to_summarize[0].id;
    let Some(start) = session.messages.iter().position(|m| m.id == *first_id) else {
        return Err("start id not in session");
    };

    let n = req.messages_to_summarize.len();
    if start + n > session.messages.len() {
        return Err("range overflow");
    }

    for i in 0..n {
        if session.messages[start + i].id != req.messages_to_summarize[i].id {
            return Err("non-contiguous summarize range");
        }
    }

    let tokens_before = TokenLedger::estimate(
        session,
        config,
        &ctx.provider,
        ctx.context_window_tokens,
        &ctx.tools_schema_json,
    )
    .total_tokens;

    session.messages.drain(start..start + n);

    let summary_msg = AgentMessage {
        id: format!("compact-summary-{}", result.last_message_id),
        role: AgentRole::CompactSummary,
        content: result.summary_text.clone(),
        metadata: serde_json::Value::Null,
    };
    session.messages.insert(start, summary_msg);

    let tokens_after = TokenLedger::estimate(
        session,
        config,
        &ctx.provider,
        ctx.context_window_tokens,
        &ctx.tools_schema_json,
    )
    .total_tokens;

    session.push_compaction_event(CompactionEvent {
        tier: CompactionTierKind::Summary,
        tokens_before,
        tokens_after,
        trigger: ctx.trigger,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{CompactionTier as CompactionTierKind, CompactionTrigger};
    use serde_json::Value::Null;

    fn ctx(allowed: bool) -> CompactionContext {
        ctx_kv(allowed, false)
    }

    fn ctx_kv(allowed: bool, kv_fixed: bool) -> CompactionContext {
        CompactionContext {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tools_schema_json: "[]".into(),
            capabilities: crate::session::SessionCapabilities {
                summarization_allowed: allowed,
                kv_fixed,
                tool_result_budget_tokens: None,
            },
            trigger: CompactionTrigger::Preflight,
        }
    }

    fn um(id: &str, content: &str) -> AgentMessage {
        AgentMessage {
            id: id.into(),
            role: AgentRole::User,
            content: content.into(),
            metadata: Null,
        }
    }

    fn am(id: &str, content: &str) -> AgentMessage {
        AgentMessage {
            id: id.into(),
            role: AgentRole::Assistant,
            content: content.into(),
            metadata: Null,
        }
    }

    #[test]
    fn not_applicable_when_summarization_disallowed() {
        let session = AgentSession::new(
            vec![um("u1", "hi"), am("a1", "yo"), um("u2", "tail")],
            Null,
        );
        let cfg = AgentConfig::default();
        let out = SummaryCompactionTier.apply(session, &cfg, &ctx(false));
        assert!(matches!(out, CompactionOutcome::NotApplicable));
    }

    #[test]
    fn not_applicable_when_kv_fixed_even_if_summarization_allowed() {
        let session = AgentSession::new(
            vec![um("u1", "hi"), am("a1", "yo"), um("u2", "tail")],
            Null,
        );
        let cfg = AgentConfig::default();
        let out = SummaryCompactionTier.apply(session, &cfg, &ctx_kv(true, true));
        assert!(matches!(out, CompactionOutcome::NotApplicable));
    }

    #[test]
    fn needs_host_call_preserves_instruction_verbatim() {
        let session = AgentSession::new(
            vec![um("u1", &"hello world ".repeat(50)), am("a1", &"reply ".repeat(50)), um("u2", "tail")],
            Null,
        );
        let cfg = AgentConfig::default();
        match SummaryCompactionTier.apply(session, &cfg, &ctx(true)) {
            CompactionOutcome::NeedsHostCall(req, _) => {
                assert_eq!(req.instruction, SUMMARY_INSTRUCTION);
                assert!(!req.messages_to_summarize.is_empty());
                assert_eq!(req.last_message_id, req.messages_to_summarize.last().unwrap().id);
            }
            o => panic!("expected NeedsHostCall, got {o:?}"),
        }
    }

    #[test]
    fn apply_summary_inserts_compact_summary_and_removes_range() {
        let mut session = AgentSession::new(
            vec![
                um("u1", "old user"),
                am("a1", "old assistant"),
                um("u2", "new user"),
            ],
            Null,
        );

        let req = SummaryRequest {
            messages_to_summarize: vec![um("u1", "old user"), am("a1", "old assistant")],
            instruction: SUMMARY_INSTRUCTION.into(),
            last_message_id: "a1".into(),
        };

        let res = SummaryResult {
            summary_text: "SYN".into(),
            last_message_id: "a1".into(),
        };

        apply_summary_result(&mut session, &req, &res, &AgentConfig::default(), &ctx(true)).unwrap();

        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].role, AgentRole::CompactSummary);
        assert_eq!(session.messages[0].content, "SYN");
        assert_eq!(session.messages[1].id, "u2");
        let ev = session.compaction_log.iter().next_back().unwrap();
        assert!(matches!(ev.tier, CompactionTierKind::Summary));
    }

    #[test]
    fn apply_summary_rejects_id_mismatch() {
        let mut session = AgentSession::new(vec![um("u1", "a"), um("u2", "b")], Null);
        let req = SummaryRequest {
            messages_to_summarize: vec![um("u1", "a")],
            instruction: "x".into(),
            last_message_id: "u1".into(),
        };
        let bad = SummaryResult {
            summary_text: "y".into(),
            last_message_id: "other".into(),
        };
        assert!(apply_summary_result(&mut session, &req, &bad, &AgentConfig::default(), &ctx(true)).is_err());
    }
}
