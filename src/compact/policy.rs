//! Compaction plan and per-trigger tier selection.
//!
//! See `plans/PLAN-agent-core-context-management/spec.md` — task 3.
//!
//! ## Pressure estimation and error budget (ENC-303)
//!
//! [`estimate_session_pressure`](super::estimate_session_pressure) projects the
//! full prompt through the [`TokenCounter`](crate::token_counter::TokenCounter)
//! port (default: `tiktoken` `cl100k_base`, so provider tokenizers disagree by
//! a tokenizer-dependent margin) plus four modeled overheads: per-message
//! chat-template tokens (`CHAT_TEMPLATE_OVERHEAD_PER_MESSAGE` = 8/message,
//! assistant primer = 4, on-device Llama ChatML extra = 6), the tools-schema
//! JSON, the system prompt, and the Kotlin context-sidecar estimate (or the
//! live sidecar payload when present). The raw sum is scaled by the host's
//! per-provider `token_correction` factor
//! ([`AgentConfig::token_correction`](crate::session::AgentConfig::token_correction),
//! default 1.0 — hosts calibrate it against their provider's tokenizer).
//!
//! ## Safety margin (ENC-303)
//!
//! The output reserve ([`effective_output_reserve_tokens`](crate::budget::effective_output_reserve_tokens):
//! 5% of the window, clamped to `output_reserve_floor_tokens` = 4096 ..
//! `output_reserve_max_tokens` = 16384, and capped at a quarter of windows
//! under 8192) is subtracted from the window to form the hard ceiling
//! ([`effective_hard_prompt_ceiling_tokens`](crate::budget::effective_hard_prompt_ceiling_tokens)).
//! Ratio thresholds (preflight 0.80, mid-turn 0.93, post-tool-result 0.92,
//! manual 0.80) fire routine compaction well before the ceiling; pressure at
//! or above the ceiling ([`exceeds_hard_prompt_ceiling`](crate::budget::exceeds_hard_prompt_ceiling))
//! means the reserve itself is threatened.
//!
//! ## Pressure-class rule table (ENC-303)
//!
//! | Class | Tiers | Ratio-triggered plan includes them when |
//! |---|---|---|
//! | `Reversible` | ToolSpill, ArtifactSquash, HistorySnapshot | always (non-destructive snapshot steps) |
//! | `EstimateAllowed` | Micro, Summary | always (destructive but routine: reproducible drops / host round-trip) |
//! | `ConfirmationGated` | Fifo, TextTruncate | hard ceiling exceeded, trigger is Manual, or (Fifo only) the snapshot-first requirement is already satisfied |
//!
//! The gate is [`apply_confirmation_gate`], applied by the dispatch after
//! plan construction; the FIFO tier additionally enforces its snapshot
//! requirement at apply time.

use crate::budget::{
    effective_hard_prompt_ceiling_tokens, effective_threshold_tokens, exceeds_hard_prompt_ceiling,
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

/// Pressure class per compaction tier (ENC-303 Q1): how much pressure
/// evidence a tier needs before a ratio-triggered plan may include it.
/// See the module-level rule table. Only `ConfirmationGated` gates plan inclusion; the other classes are documentary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureClass {
    /// Non-destructive snapshot steps: safe to run once the plan fires.
    Reversible,
    /// Destructive but routine: a ratio trigger alone suffices.
    EstimateAllowed,
    /// Destructive and lossy: needs the hard ceiling, a manual trigger,
    /// or (Fifo only) an already-satisfied snapshot-first requirement.
    ConfirmationGated,
}

/// Maps each [`CompactionTier`] to its [`PressureClass`] (ENC-303 Q1).
pub fn tier_pressure_class(tier: &CompactionTier) -> PressureClass {
    match tier {
        CompactionTier::ToolSpill
        | CompactionTier::ArtifactSquash
        | CompactionTier::HistorySnapshot => PressureClass::Reversible,
        CompactionTier::Micro | CompactionTier::Summary => PressureClass::EstimateAllowed,
        CompactionTier::Fifo | CompactionTier::TextTruncate => PressureClass::ConfirmationGated,
    }
}

/// Confirmation gate for ratio-triggered plans (ENC-303 Q2): strips
/// [`PressureClass::ConfirmationGated`] tiers from `plan.ordered_tiers`
/// unless the plan may include them — hard ceiling exceeded (the output
/// reserve is threatened), a manual trigger, or (Fifo only) the
/// snapshot-first requirement already satisfied. Snapshot satisfaction is a
/// plan-level pre-filter on `plan.require_snapshot_before_destructive_drop`:
/// the requirement is off, or the plan already runs `HistorySnapshot` ahead
/// of the destructive tiers (as [`build_compaction_plan`] always does on the
/// ratio path). Enforcement stays at apply time — the FIFO tier keeps its
/// own session-state snapshot gate (see `snapshot_gate_outcome` in
/// [`super::fifo`]) regardless of what the pre-filter kept. A forced
/// non-ratio entry (`forced_by_non_ratio`: absolute threshold, turn count,
/// or turn-end) zeroes the plan ratio but still passes through this gate,
/// so forced entry does not imply the destructive tiers survive it.
///
/// Manual plans are untouched: the host explicitly asked for full compaction.
pub fn apply_confirmation_gate(plan: &mut CompactionPlan, pressure: &Pressure) {
    if plan.trigger == CompactionTrigger::Manual {
        return;
    }
    let hard_ceiling_exceeded = exceeds_hard_prompt_ceiling(pressure);
    let snapshot_first_satisfied = !plan.require_snapshot_before_destructive_drop
        || plan
            .ordered_tiers
            .contains(&CompactionTier::HistorySnapshot);
    plan.ordered_tiers.retain(|tier| {
        confirmation_gate_allows(tier, hard_ceiling_exceeded, snapshot_first_satisfied)
    });
}

/// The confirmation gate as ONE predicate over (tier, conditions) — the
/// gate does not re-discriminate tiers by class and then by identity.
/// [`tier_pressure_class`] remains the documentary classification; this
/// predicate is the behavior.
fn confirmation_gate_allows(
    tier: &CompactionTier,
    hard_ceiling_exceeded: bool,
    snapshot_first_satisfied: bool,
) -> bool {
    match tier {
        // Fifo may also ride an already-satisfied snapshot-first
        // requirement; TextTruncate has no such proxy.
        CompactionTier::Fifo => hard_ceiling_exceeded || snapshot_first_satisfied,
        CompactionTier::TextTruncate => hard_ceiling_exceeded,
        _ => true,
    }
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

/// The dispatch-facing outcome of the entry gate. Only the fields the
/// dispatch actually reads are returned; the individual fire signals
/// stay local to [`should_enter_compaction`], where they are combined.
pub struct FireConditions {
    pub should_enter: bool,
    pub entered_for_long_loop_only: bool,
    pub forced_by_non_ratio: bool,
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
        forced_by_non_ratio,
    }
}

/// The gate inputs for [`mid_turn_stall_error`], as one named-field
/// struct: five loose bools at a call site are trivially swapped, named
/// fields are not.
pub struct MidTurnStallGate {
    pub allow_fifo_on_mid_turn: bool,
    pub summarization_allowed: bool,
    pub compaction_log_is_empty: bool,
    pub defer_compaction_failure: bool,
    /// Pre-gate call sites require a non-empty compaction log before the
    /// stall error may fire; the post-emit call site does not.
    pub require_nonempty_log: bool,
}

/// The two identical `CompactionFailed` early-error blocks in
/// `run_compaction_pass` (pre-gate with a non-empty-log requirement, post-emit
/// without). Pure constructor; the dispatch decides.
pub fn mid_turn_stall_error(
    trigger: CompactionTrigger,
    gate: MidTurnStallGate,
) -> Option<AgentError> {
    if !matches!(trigger, CompactionTrigger::MidTurn) {
        return None;
    }
    if gate.allow_fifo_on_mid_turn || gate.summarization_allowed {
        return None;
    }
    if gate.defer_compaction_failure {
        return None;
    }
    if gate.require_nonempty_log && gate.compaction_log_is_empty {
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
