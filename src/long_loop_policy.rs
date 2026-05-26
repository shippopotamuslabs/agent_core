//! Long-loop policy for tool-heavy sessions (`FEAT-long-agent-loop-robustness`).

use serde::{Deserialize, Serialize};

use crate::events::CompactionTrigger;
use crate::policy::ContextPolicyTier;
use crate::session::{AgentConfig, SessionCapabilities};

/// Per-session limits for long tool-heavy agent loops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LongLoopPolicy {
    pub max_tool_turns: u32,
    pub sidecar_byte_budget_per_call: usize,
    pub sidecar_total_byte_budget: usize,
    pub summarize_after_tool_turns: u32,
    pub compact_every_n_tool_turns: u32,
}

impl LongLoopPolicy {
    pub fn hosted_mutable_defaults() -> Self {
        Self {
            max_tool_turns: 40,
            sidecar_byte_budget_per_call: 8192,
            sidecar_total_byte_budget: 16384,
            summarize_after_tool_turns: 0,
            compact_every_n_tool_turns: 0,
        }
    }

    pub fn hosted_proxy_defaults() -> Self {
        Self::hosted_mutable_defaults()
    }

    pub fn local_mutable_defaults() -> Self {
        Self {
            max_tool_turns: 30,
            sidecar_byte_budget_per_call: 4096,
            sidecar_total_byte_budget: 8192,
            summarize_after_tool_turns: 0,
            compact_every_n_tool_turns: 0,
        }
    }

    pub fn small_context_defaults() -> Self {
        Self {
            max_tool_turns: 24,
            sidecar_byte_budget_per_call: 2048,
            sidecar_total_byte_budget: 4096,
            summarize_after_tool_turns: 0,
            compact_every_n_tool_turns: 0,
        }
    }

    pub fn fixed_kv_defaults() -> Self {
        Self {
            max_tool_turns: 20,
            sidecar_byte_budget_per_call: 4096,
            sidecar_total_byte_budget: 8192,
            summarize_after_tool_turns: 8,
            compact_every_n_tool_turns: 3,
        }
    }

    pub fn for_policy_tier(tier: ContextPolicyTier) -> Self {
        match tier {
            ContextPolicyTier::HostedMutable | ContextPolicyTier::HighContext => {
                Self::hosted_mutable_defaults()
            }
            ContextPolicyTier::HostedProxy => Self::hosted_proxy_defaults(),
            ContextPolicyTier::LocalMutable => Self::local_mutable_defaults(),
            ContextPolicyTier::SmallContext => Self::small_context_defaults(),
            ContextPolicyTier::FixedKv => Self::fixed_kv_defaults(),
        }
    }

    /// `compact_every_n_tool_turns == 0` disables the periodic schedule.
    pub fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

impl Default for LongLoopPolicy {
    fn default() -> Self {
        Self::hosted_mutable_defaults()
    }
}

/// Effective tool-turn cap: `min(config.max_turns, long_loop.max_tool_turns)` when long_loop is set.
pub fn effective_max_tool_turns(config: &AgentConfig) -> u32 {
    config.max_turns.min(config.long_loop.max_tool_turns)
}

/// Whether the engine should stop issuing `ExecuteTools` for this user turn.
pub fn should_stop_tool_rounds(
    config: &AgentConfig,
    tool_round_index: u32,
    last_model_response_had_append_blocks: bool,
) -> bool {
    let long_loop_cap = effective_max_tool_turns(config);
    if long_loop_cap < config.max_turns {
        return tool_round_index >= long_loop_cap;
    }
    let guard = crate::guardrails::GuardrailState {
        tool_calls_executed: tool_round_index,
        ..Default::default()
    };
    tool_round_index
        >= guard.effective_max_tool_calls(config, last_model_response_had_append_blocks)
}

/// Long-loop compaction trigger evaluation (preflight / periodic / summarize).
/// Long-loop periodic compaction runs on preflight only (not mid-turn / post-tool).
pub fn should_compact_for_long_loop(
    config: &AgentConfig,
    caps: &SessionCapabilities,
    tool_round_index: u32,
    trigger: CompactionTrigger,
) -> bool {
    if tool_round_index == 0 || trigger != CompactionTrigger::Preflight {
        return false;
    }
    if config.long_loop.compact_every_n_tool_turns > 0
        && tool_round_index % config.long_loop.compact_every_n_tool_turns == 0
    {
        return true;
    }
    config.long_loop.summarize_after_tool_turns > 0
        && tool_round_index >= config.long_loop.summarize_after_tool_turns
        && caps.summarization_allowed
}

/// Whether the compaction plan for the current pass should include Summary tier.
pub fn long_loop_plan_includes_summary(caps: &SessionCapabilities) -> bool {
    caps.summarization_allowed && !caps.kv_fixed
}
