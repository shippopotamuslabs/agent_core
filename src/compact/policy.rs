//! Compaction plan and per-trigger tier selection.
//!
//! See `plans/PLAN-agent-core-context-management/spec.md` — task 3.

use crate::budget::effective_hard_prompt_ceiling_tokens;
use crate::events::{CompactionTier, CompactionTrigger};
use crate::policy::{effective_compaction_threshold_ratio, ContextPolicy, ContextPolicyTier};
use crate::session::{AgentConfig, SessionCapabilities};

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
            if history_rewrite_allowed
                && text_truncate_allowed(policy.tier, context_window_tokens)
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
            if history_rewrite_allowed
                && text_truncate_allowed(policy.tier, context_window_tokens)
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

fn text_truncate_allowed(tier: ContextPolicyTier, context_window_tokens: u32) -> bool {
    matches!(
        tier,
        ContextPolicyTier::FixedKv | ContextPolicyTier::SmallContext | ContextPolicyTier::LocalMutable
    ) || context_window_tokens < 8_000
}
