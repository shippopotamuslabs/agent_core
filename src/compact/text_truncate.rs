//! Tier 5 — text truncation: shrink the oldest truncatable message bodies when FIFO
//! cannot drop enough history to fit the hard prompt ceiling.

use super::{estimate_session_pressure, CompactionContext, CompactionOutcome, CompactionTier};
use crate::budget::{effective_hard_prompt_ceiling_tokens, should_compact_at_token_threshold};
use crate::compact::policy::threshold_ratio_for_trigger;
use crate::events::{CompactionEvent, CompactionTier as TierEnum};
use crate::message::AgentRole;
use crate::session::{AgentConfig, AgentSession};
use crate::truncate::truncate_message_content_to_tokens;

pub struct TextTruncateCompactionTier;

impl Default for TextTruncateCompactionTier {
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
) -> bool {
    if matches!(msg.role, AgentRole::System | AgentRole::CompactSummary) {
        return true;
    }
    if let Some(u) = last_user {
        if idx >= u {
            return true;
        }
    }
    false
}

fn first_truncatable_index(
    messages: &[crate::message::AgentMessage],
    last_user: Option<usize>,
) -> Option<usize> {
    let mut best: Option<(usize, u32)> = None;
    for (idx, msg) in messages.iter().enumerate() {
        if is_protected_index(idx, msg, last_user) {
            continue;
        }
        let tokens = crate::budget::count_tokens(&msg.content);
        if tokens <= 32 {
            continue;
        }
        match best {
            Some((_, best_tokens)) if tokens <= best_tokens => {}
            _ => best = Some((idx, tokens)),
        }
    }
    best.map(|(idx, _)| idx)
}

fn target_token_budget(config: &AgentConfig, ctx: &CompactionContext) -> u32 {
    let threshold_ratio = threshold_ratio_for_trigger(config, ctx.trigger);
    let ratio_target = crate::budget::effective_threshold_tokens(
        config,
        ctx.context_window_tokens,
        threshold_ratio,
        None,
    );
    let hard_ceiling = effective_hard_prompt_ceiling_tokens(config, ctx.context_window_tokens);
    ratio_target.min(hard_ceiling)
}

/// Next single truncation step for the [`TextTruncateCompactionTier::apply`]
/// driver loop: `Some((idx, new_content))` when truncating
/// `session.messages[idx].content` to `new_content` makes progress, `None`
/// when the loop should stop — either no truncatable message remains, or
/// the best candidate cannot shrink further. The two stop cases share one
/// driver behavior (break), so they share one representation.
///
/// This helper only answers the candidate question; the pressure re-check
/// stays visibly in the driver.
fn next_truncation(session: &AgentSession) -> Option<(usize, String)> {
    let last_user = last_user_index(&session.messages);
    let idx = first_truncatable_index(&session.messages, last_user)?;

    let current_content = session.messages[idx].content.clone();
    let current_tokens = crate::budget::count_tokens(&current_content);
    if current_tokens <= 32 {
        return None;
    }
    let next_tokens = (current_tokens / 2).max(32);
    let (new_content, _) = truncate_message_content_to_tokens(&current_content, next_tokens);
    if new_content == current_content {
        return None;
    }
    let new_message_tokens = crate::budget::count_tokens(&new_content);
    if new_message_tokens >= current_tokens {
        return None;
    }
    Some((idx, new_content))
}

impl CompactionTier for TextTruncateCompactionTier {
    fn apply(
        &self,
        mut session: AgentSession,
        config: &AgentConfig,
        ctx: &CompactionContext,
    ) -> CompactionOutcome {
        let target_tokens = target_token_budget(config, ctx);
        let tokens_before = estimate_session_pressure(&session, config, ctx).total_tokens;
        if !should_compact_at_token_threshold(
            &estimate_session_pressure(&session, config, ctx),
            target_tokens,
        ) {
            return CompactionOutcome::NotApplicable;
        }

        let mut changed = false;
        // Pressure re-check, visibly in the driver: the loop only runs while
        // the session is over budget, and each iteration applies one
        // truncation step from `next_truncation`.
        while should_compact_at_token_threshold(
            &estimate_session_pressure(&session, config, ctx),
            target_tokens,
        ) {
            let pressure_before = estimate_session_pressure(&session, config, ctx).total_tokens;
            let Some((idx, new_content)) = next_truncation(&session) else {
                break;
            };
            let message = &mut session.messages[idx];
            message.content = new_content;
            message
                .metadata
                .set_compaction_tier(TierEnum::TextTruncate.to_string());
            let pressure_after = estimate_session_pressure(&session, config, ctx).total_tokens;
            // A truncation that fails to relieve pressure stalls the
            // tier without counting as progress (`changed` untouched),
            // preserving the exact NotApplicable-vs-partial-progress
            // distinction.
            if pressure_after >= pressure_before {
                break;
            }
            changed = true;
        }

        if !changed {
            return CompactionOutcome::NotApplicable;
        }

        let tokens_after = estimate_session_pressure(&session, config, ctx).total_tokens;
        if tokens_after >= tokens_before {
            return CompactionOutcome::NotApplicable;
        }
        CompactionOutcome::Applied(
            session,
            CompactionEvent {
                tier: TierEnum::TextTruncate,
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
    use crate::policy::ContextPolicyTier;
    use serde_json::Value::Null;

    fn ctx(cw: u32, system: &str) -> CompactionContext {
        CompactionContext {
            provider: "LLAMA".into(),
            context_window_tokens: cw,
            tools_schema_json: "[]".into(),
            system_prompt: system.into(),
            context_sidecars_estimate: String::new(),
            capabilities: crate::session::SessionCapabilities {
                summarization_allowed: false,
                kv_fixed: true,
                context_policy_tier: ContextPolicyTier::FixedKv,
                ..Default::default()
            },
            trigger: CompactionTrigger::Preflight,
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

    #[test]
    fn truncates_oldest_message_when_fifo_floor_still_overflows() {
        let system = "sys ".repeat(900);
        let old = "history ".repeat(500);
        let truncatable = "block ".repeat(500);
        let session = AgentSession::new(
            vec![
                msg("u1", AgentRole::User, &old),
                msg("a1", AgentRole::Assistant, &old),
                msg("u2", AgentRole::User, &truncatable),
                msg("u3", AgentRole::User, "latest question"),
            ],
            Null,
        );
        let cfg = AgentConfig::for_policy_tier(ContextPolicyTier::FixedKv);
        let tier = TextTruncateCompactionTier;
        let compaction_ctx = ctx(4096, &system);
        let before = estimate_session_pressure(&session, &cfg, &compaction_ctx).total_tokens;
        assert!(before > effective_hard_prompt_ceiling_tokens(&cfg, 4096));

        match tier.apply(session, &cfg, &compaction_ctx) {
            CompactionOutcome::Applied(s, ev) => {
                assert_eq!(ev.tier, TierEnum::TextTruncate);
                assert!(ev.tokens_after < ev.tokens_before);
                assert!(s
                    .messages
                    .iter()
                    .any(|m| { m.content.contains(crate::truncate::MESSAGE_TRUNCATION_NOTE) }));
                assert_eq!(s.messages[3].content, "latest question");
            }
            o => panic!("expected Applied, got {o:?}"),
        }
    }

    #[test]
    fn applied_with_partial_progress_when_candidates_exhausted_while_still_over_budget() {
        // The system prompt alone keeps pressure over budget, so once the
        // single truncatable message is exhausted the loop stalls (breaks)
        // with `changed == true`. That partial progress must stay Applied —
        // never collapse into NotApplicable.
        let system = "sys ".repeat(4000);
        let big = "block ".repeat(500);
        let session = AgentSession::new(
            vec![
                msg("a1", AgentRole::Assistant, &big),
                msg("u1", AgentRole::User, "latest question"),
            ],
            Null,
        );
        let cfg = AgentConfig::for_policy_tier(ContextPolicyTier::FixedKv);
        let tier = TextTruncateCompactionTier;
        let compaction_ctx = ctx(4096, &system);
        let target = target_token_budget(&cfg, &compaction_ctx);
        assert!(crate::budget::should_compact_at_token_threshold(
            &estimate_session_pressure(&session, &cfg, &compaction_ctx),
            target
        ));
        match tier.apply(session, &cfg, &compaction_ctx) {
            CompactionOutcome::Applied(s, ev) => {
                assert!(ev.tokens_after < ev.tokens_before);
                assert!(s
                    .messages
                    .iter()
                    .any(|m| m.content.contains(crate::truncate::MESSAGE_TRUNCATION_NOTE)));
                // Stall: candidates exhausted but still over budget — yet Applied.
                assert!(
                    crate::budget::should_compact_at_token_threshold(
                        &estimate_session_pressure(&s, &cfg, &compaction_ctx),
                        target
                    ),
                    "expected stall: session should still be over budget after exhausting candidates"
                );
            }
            o => panic!("expected Applied (partial progress), got {o:?}"),
        }
    }

    #[test]
    fn not_applicable_when_already_under_budget() {
        let session = AgentSession::new(vec![msg("u1", AgentRole::User, "short")], Null);
        let cfg = AgentConfig::for_policy_tier(ContextPolicyTier::FixedKv);
        let out = TextTruncateCompactionTier.apply(session, &cfg, &ctx(4096, "tiny"));
        assert!(matches!(out, CompactionOutcome::NotApplicable));
    }
}
