//! Context policy resolution.
//!
//! Normalises [`crate::session::AgentConfig`] and [`crate::session::SessionCapabilities`]
//! into a single [`ContextPolicy`] used by compaction planning, summary tiers,
//! tool-schema deferral, file-context condensation, and prompt-cache hints.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPolicyTier {
    #[default]
    HostedMutable,
    HostedProxy,
    LocalMutable,
    FixedKv,
    HighContext,
    SmallContext,
}

#[derive(Debug, Clone)]
pub struct ContextPolicy {
    pub tier: ContextPolicyTier,
    pub summarization_allowed: bool,
    pub history_rewrite_allowed: bool,
    pub kv_rebuild_required_on_rewrite: bool,
    pub tool_schema_deferral_allowed: bool,
    pub file_backed_context_allowed: bool,
    pub manual_compact_allowed: bool,
    pub prompt_cache_hinting_allowed: bool,
    pub fifo_allowed_by_default: bool,
}

/// Effective preflight threshold for the high-context tier; raises the default
/// `threshold_ratio` so very large context windows don't compact prematurely.
pub const HIGH_CONTEXT_PREFLIGHT_THRESHOLD: f32 = 0.90;

/// Large hosted windows (e.g. 128k) should not enter destructive compaction at the
/// same ratios as small local sessions.
pub const LARGE_CONTEXT_WINDOW_TOKENS: u32 = 100_000;
pub const LARGE_CONTEXT_POST_TOOL_THRESHOLD: f32 = 0.94;
pub const LARGE_CONTEXT_MID_TURN_THRESHOLD: f32 = 0.94;

/// Pressure ratio at or above which compaction tiers may run for `trigger`.
pub fn effective_compaction_threshold_ratio(
    config: &crate::session::AgentConfig,
    policy: &ContextPolicy,
    trigger: crate::events::CompactionTrigger,
    context_window_tokens: u32,
) -> f32 {
    use crate::compact::policy::threshold_ratio_for_trigger;
    use crate::events::CompactionTrigger;

    let base = match (trigger, policy.tier) {
        (CompactionTrigger::Preflight, ContextPolicyTier::HighContext) => {
            HIGH_CONTEXT_PREFLIGHT_THRESHOLD
        }
        _ => threshold_ratio_for_trigger(config, trigger),
    };

    match (policy.tier, trigger) {
        (ContextPolicyTier::HighContext, CompactionTrigger::PostToolResult) => {
            base.max(LARGE_CONTEXT_POST_TOOL_THRESHOLD)
        }
        (ContextPolicyTier::HighContext, CompactionTrigger::MidTurn) => {
            base.max(LARGE_CONTEXT_MID_TURN_THRESHOLD)
        }
        _ if context_window_tokens >= LARGE_CONTEXT_WINDOW_TOKENS => match trigger {
            CompactionTrigger::PostToolResult => base.max(LARGE_CONTEXT_POST_TOOL_THRESHOLD),
            CompactionTrigger::MidTurn => base.max(LARGE_CONTEXT_MID_TURN_THRESHOLD),
            _ => base,
        },
        _ => base,
    }
}

pub fn resolve_context_policy(
    config: &crate::session::AgentConfig,
    capabilities: &crate::session::SessionCapabilities,
    _provider: &str,
    _context_window_tokens: u32,
) -> ContextPolicy {
    let tier = capabilities.context_policy_tier;

    match tier {
        ContextPolicyTier::FixedKv => ContextPolicy {
            tier,
            summarization_allowed: capabilities.summarization_allowed
                && capabilities.history_rewrite_supported,
            history_rewrite_allowed: capabilities.history_rewrite_supported,
            kv_rebuild_required_on_rewrite: true,
            tool_schema_deferral_allowed: config.enable_tool_schema_deferral
                && capabilities.tool_schema_deferral_supported,
            file_backed_context_allowed: config.enable_file_condensation
                && capabilities.file_backed_context_supported,
            manual_compact_allowed: config.enable_manual_compact,
            prompt_cache_hinting_allowed: capabilities.prompt_cache_supported,
            fifo_allowed_by_default: false,
        },
        ContextPolicyTier::HostedMutable => ContextPolicy {
            tier,
            summarization_allowed: capabilities.summarization_allowed,
            history_rewrite_allowed: true,
            kv_rebuild_required_on_rewrite: false,
            tool_schema_deferral_allowed: config.enable_tool_schema_deferral
                && capabilities.tool_schema_deferral_supported,
            file_backed_context_allowed: config.enable_file_condensation
                && capabilities.file_backed_context_supported,
            manual_compact_allowed: config.enable_manual_compact,
            prompt_cache_hinting_allowed: capabilities.prompt_cache_supported,
            fifo_allowed_by_default: false,
        },
        ContextPolicyTier::HostedProxy => ContextPolicy {
            tier,
            summarization_allowed: capabilities.summarization_allowed,
            history_rewrite_allowed: capabilities.history_rewrite_supported,
            kv_rebuild_required_on_rewrite: !capabilities.history_rewrite_supported,
            tool_schema_deferral_allowed: config.enable_tool_schema_deferral
                && capabilities.tool_schema_deferral_supported,
            file_backed_context_allowed: config.enable_file_condensation
                && capabilities.file_backed_context_supported,
            manual_compact_allowed: config.enable_manual_compact,
            prompt_cache_hinting_allowed: capabilities.prompt_cache_supported,
            fifo_allowed_by_default: false,
        },
        ContextPolicyTier::LocalMutable => ContextPolicy {
            tier,
            summarization_allowed: capabilities.summarization_allowed,
            history_rewrite_allowed: capabilities.history_rewrite_supported,
            kv_rebuild_required_on_rewrite: capabilities.kv_fixed,
            tool_schema_deferral_allowed: config.enable_tool_schema_deferral
                && capabilities.tool_schema_deferral_supported,
            file_backed_context_allowed: config.enable_file_condensation
                && capabilities.file_backed_context_supported,
            manual_compact_allowed: config.enable_manual_compact,
            prompt_cache_hinting_allowed: capabilities.prompt_cache_supported,
            fifo_allowed_by_default: true,
        },
        ContextPolicyTier::HighContext => ContextPolicy {
            tier,
            summarization_allowed: capabilities.summarization_allowed,
            history_rewrite_allowed: capabilities.history_rewrite_supported,
            kv_rebuild_required_on_rewrite: capabilities.kv_fixed,
            tool_schema_deferral_allowed: config.enable_tool_schema_deferral
                && capabilities.tool_schema_deferral_supported,
            file_backed_context_allowed: config.enable_file_condensation
                && capabilities.file_backed_context_supported,
            manual_compact_allowed: config.enable_manual_compact,
            prompt_cache_hinting_allowed: capabilities.prompt_cache_supported,
            fifo_allowed_by_default: false,
        },
        ContextPolicyTier::SmallContext => ContextPolicy {
            tier,
            summarization_allowed: capabilities.summarization_allowed,
            history_rewrite_allowed: capabilities.history_rewrite_supported,
            kv_rebuild_required_on_rewrite: capabilities.kv_fixed,
            tool_schema_deferral_allowed: config.enable_tool_schema_deferral
                && capabilities.tool_schema_deferral_supported,
            file_backed_context_allowed: config.enable_file_condensation
                && capabilities.file_backed_context_supported,
            manual_compact_allowed: config.enable_manual_compact,
            prompt_cache_hinting_allowed: capabilities.prompt_cache_supported,
            fifo_allowed_by_default: true,
        },
    }
}
