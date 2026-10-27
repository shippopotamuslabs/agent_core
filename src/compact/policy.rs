//! Compaction plan and per-trigger tier selection.
//!
//! See `plans/PLAN-agent-core-context-management/spec.md` — task 3.

use crate::budget::{
    effective_hard_prompt_ceiling_tokens, effective_threshold_tokens,
    should_compact_at_token_threshold,
};
use crate::events::{AgentError, CompactionTier, CompactionTrigger};
use crate::long_loop_policy::should_compact_for_long_loop;
use crate::message::AgentRole;
use crate::policy::{effective_compaction_threshold_ratio, ContextPolicy, ContextPolicyTier};
use crate::session::{AgentConfig, Pressure, SessionCapabilities};

#[derive(Debug, Clone)]
pub struct CompactionPlan {
    pub trigger: CompactionTrigger,
    pub threshold_ratio: f32,
    pub threshold_tokens: u32,
    pub ordered_tiers: Vec<CompactionTier>,
    pub allow_history_rewrite: bool,
    pub require_snapshot_before_destructive_drop: bool,
    pub max_prompt_tokens_after_plan: u32,
}

/// Returns the configured pressure threshold for a given trigger.
pub fn threshold_ratio_for_trigger(config: &AgentConfig, trigger: CompactionTrigger) -> f32 {
    match trigger {
        CompactionTrigger::Preflight => config.threshold_ratio,
        CompactionTrigger::MidTurn => config.mid_turn_threshold_ratio,
        CompactionTrigger::PostToolResult => config.post_tool_result_threshold_ratio,
        CompactionTrigger::Manual => config.manual_compact_threshold_ratio,
    }
}

pub fn build_compaction_plan(
    config: &AgentConfig,
    _capabilities: &SessionCapabilities,
    policy: &ContextPolicy,
    trigger: CompactionTrigger,
    context_window_tokens: u32,
    suppress_summary_tier: bool,
) -> CompactionPlan {
    let threshold_ratio =
        effective_compaction_threshold_ratio(config, policy, trigger, context_window_tokens);
    let token_threshold = match trigger {
        CompactionTrigger::Preflight => config.preflight_token_threshold,
        CompactionTrigger::MidTurn => config.mid_turn_token_threshold,
        CompactionTrigger::PostToolResult => config.post_tool_result_token_threshold,
        CompactionTrigger::Manual => None,
    };
    let threshold_tokens = crate::budget::effective_threshold_tokens(
        config,
        context_window_tokens,
        threshold_ratio,
        token_threshold,
    );

    let summary_allowed = policy.summarization_allowed
        && policy.history_rewrite_allowed
        && crate::long_loop_policy::long_loop_plan_includes_summary(_capabilities)
        && !suppress_summary_tier;
    let history_rewrite_allowed = policy.history_rewrite_allowed;

    let fifo_gate_for_trigger = match trigger {
        CompactionTrigger::Preflight => config.allow_fifo_on_preflight,
        CompactionTrigger::MidTurn => config.allow_fifo_on_mid_turn,
        CompactionTrigger::PostToolResult => config.allow_fifo_on_post_tool_result,
        CompactionTrigger::Manual => true, // manual handled below
    };
    let policy_allows_fifo = history_rewrite_allowed;
    let fifo_allowed = policy_allows_fifo && fifo_gate_for_trigger;

    let mut ordered: Vec<CompactionTier> = Vec::new();
    match trigger {
        CompactionTrigger::Preflight
        | CompactionTrigger::MidTurn
        | CompactionTrigger::PostToolResult => {
            ordered.push(CompactionTier::ToolSpill);
            ordered.push(CompactionTier::Micro);
            ordered.push(CompactionTier::ArtifactSquash);
            // HistorySnapshot is a non-destructive snapshot step that records a
            // history-ref into the ContextStore so subsequent destructive tiers
            // (Summary, Fifo) can satisfy `require_snapshot_before_fifo`.
            ordered.push(CompactionTier::HistorySnapshot);
            if summary_allowed {
                ordered.push(CompactionTier::Summary);
            }
            if fifo_allowed {
                ordered.push(CompactionTier::Fifo);
            }
            if history_rewrite_allowed && text_truncate_allowed(policy.tier, context_window_tokens)
            {
                ordered.push(CompactionTier::TextTruncate);
            }
        }
        CompactionTrigger::Manual => {
            ordered.push(CompactionTier::HistorySnapshot);
            if summary_allowed {
                ordered.push(CompactionTier::Summary);
            } else if history_rewrite_allowed {
                ordered.push(CompactionTier::Fifo);
            }
            if history_rewrite_allowed && text_truncate_allowed(policy.tier, context_window_tokens)
            {
                ordered.push(CompactionTier::TextTruncate);
            }
        }
    }

    CompactionPlan {
        trigger,
        threshold_ratio,
        threshold_tokens,
        ordered_tiers: ordered,
        allow_history_rewrite: history_rewrite_allowed,
        require_snapshot_before_destructive_drop: config.require_snapshot_before_fifo,
        max_prompt_tokens_after_plan: effective_hard_prompt_ceiling_tokens(
            config,
            context_window_tokens,
        ),
    }
}

/// ENC-286 slice 6 / FRG-47: pure entry-gate evaluation for `run_compaction_pass`.
///
/// All fire conditions are computed from plain data — no session access, no
/// phase writes. The dispatch keeps looping, emitting, and phase wiring.
pub struct FireConditionInputs<'a> {
    pub config: &'a AgentConfig,
    pub capabilities: &'a SessionCapabilities,
    pub trigger: CompactionTrigger,
    pub pressure: &'a Pressure,
    pub threshold_tokens: u32,
    pub ratio_threshold_tokens: u32,
    pub turn_count: u32,
    pub tool_calls_executed: u32,
    pub session_has_tool_messages: bool,
    pub last_message_role: Option<AgentRole>,
}

pub struct FireConditions {
    pub should_enter: bool,
    pub entered_for_long_loop_only: bool,
    pub absolute_fires: bool,
    pub turn_fires: bool,
    pub turn_end_fires: bool,
    pub forced_by_non_ratio: bool,
    pub active_threshold_tokens: u32,
}

pub fn should_enter_compaction(inputs: FireConditionInputs<'_>) -> FireConditions {
    let FireConditionInputs {
        config,
        capabilities,
        trigger,
        pressure,
        threshold_tokens,
        ratio_threshold_tokens,
        turn_count,
        tool_calls_executed,
        session_has_tool_messages,
        last_message_role,
    } = inputs;
    let absolute_fires = match trigger {
        CompactionTrigger::Preflight => config
            .preflight_token_threshold
            .map(|t| pressure.total_tokens >= t)
            .unwrap_or(false),
        CompactionTrigger::MidTurn => config
            .mid_turn_token_threshold
            .map(|t| pressure.total_tokens >= t)
            .unwrap_or(false),
        CompactionTrigger::PostToolResult => config
            .post_tool_result_token_threshold
            .map(|t| pressure.total_tokens >= t)
            .unwrap_or(false),
        CompactionTrigger::Manual => false,
    };
    let has_destructive_compaction_candidate = capabilities.summarization_allowed
        || (session_has_tool_messages && config.micro_keep_recent == 0)
        || match trigger {
            CompactionTrigger::Preflight => config.allow_fifo_on_preflight,
            CompactionTrigger::MidTurn => config.allow_fifo_on_mid_turn,
            CompactionTrigger::PostToolResult => config.allow_fifo_on_post_tool_result,
            CompactionTrigger::Manual => true,
        };
    let absolute_fires = absolute_fires && has_destructive_compaction_candidate;
    let active_threshold_tokens = if has_destructive_compaction_candidate {
        threshold_tokens
    } else {
        ratio_threshold_tokens
    };
    let turn_fires = matches!(trigger, CompactionTrigger::Preflight)
        && config
            .turn_threshold
            .map(|t| turn_count >= t)
            .unwrap_or(false);
    let turn_end_fires = trigger == CompactionTrigger::PostToolResult
        && config.compact_on_turn_end
        && last_message_role
            .map(|role| matches!(role, AgentRole::Tool | AgentRole::User))
            .unwrap_or(false);

    let long_loop_fires = session_has_tool_messages
        && should_compact_for_long_loop(config, capabilities, tool_calls_executed, trigger);
    let entered_for_long_loop_only = long_loop_fires
        && !should_compact_at_token_threshold(pressure, active_threshold_tokens)
        && !absolute_fires
        && !turn_fires
        && !turn_end_fires;

    let should_enter = should_compact_at_token_threshold(pressure, active_threshold_tokens)
        || absolute_fires
        || turn_fires
        || turn_end_fires
        || long_loop_fires;

    // When forced by absolute token threshold, turn count, or turn-end, the
    // dispatch overrides the plan ratio and context trigger (see dispatch).
    let forced_by_non_ratio = !should_compact_at_token_threshold(pressure, active_threshold_tokens)
        && (absolute_fires || turn_fires || turn_end_fires);

    FireConditions {
        should_enter,
        entered_for_long_loop_only,
        absolute_fires,
        turn_fires,
        turn_end_fires,
        forced_by_non_ratio,
        active_threshold_tokens,
    }
}

/// The two identical `CompactionFailed` early-error blocks in
/// `run_compaction_pass` (pre-gate with a non-empty-log requirement, post-emit
/// without). Pure constructor; the dispatch decides.
pub fn mid_turn_stall_error(
    trigger: CompactionTrigger,
    allow_fifo_on_mid_turn: bool,
    summarization_allowed: bool,
    compaction_log_is_empty: bool,
    defer_compaction_failure: bool,
    require_nonempty_log: bool,
) -> Option<AgentError> {
    if !matches!(trigger, CompactionTrigger::MidTurn) {
        return None;
    }
    if allow_fifo_on_mid_turn || summarization_allowed {
        return None;
    }
    if defer_compaction_failure {
        return None;
    }
    if require_nonempty_log && compaction_log_is_empty {
        return None;
    }
    Some(AgentError::CompactionFailed {
        tier: "pipeline".into(),
        reason: "No compaction tier applied while context remains over threshold".into(),
    })
}

/// Recomputes `ratio_threshold_tokens` the way the dispatch did inline
/// (kept next to the plan so call sites stay one line).
pub fn ratio_threshold_tokens_for_plan(
    config: &AgentConfig,
    context_window_tokens: u32,
    plan: &CompactionPlan,
) -> u32 {
    effective_threshold_tokens(config, context_window_tokens, plan.threshold_ratio, None)
}

fn text_truncate_allowed(tier: ContextPolicyTier, context_window_tokens: u32) -> bool {
    matches!(
        tier,
        ContextPolicyTier::FixedKv
            | ContextPolicyTier::SmallContext
            | ContextPolicyTier::LocalMutable
    ) || context_window_tokens < 8_000
}
