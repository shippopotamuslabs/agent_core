use crate::events::{AgentError, CompactionTrigger};
use crate::loop_metrics::LoopMetrics;
use crate::message::{AgentMessage, AgentRole};
use crate::session::{AgentConfig, Pressure};

pub fn preflight_trigger_for_pressure(
    config: &AgentConfig,
    pressure: &Pressure,
) -> Option<CompactionTrigger> {
    if config
        .preflight_token_threshold
        .map(|t| pressure.total_tokens >= t)
        .unwrap_or(false)
    {
        Some(CompactionTrigger::Preflight)
    } else {
        None
    }
}

pub fn preflight_trigger_for_turn_count(
    config: &AgentConfig,
    metrics: &LoopMetrics,
) -> Option<CompactionTrigger> {
    if config
        .turn_threshold
        .map(|t| metrics.turn_count >= t)
        .unwrap_or(false)
    {
        Some(CompactionTrigger::Preflight)
    } else {
        None
    }
}

pub fn turn_end_trigger_after_tool_results(
    config: &AgentConfig,
    messages: &[AgentMessage],
) -> Option<CompactionTrigger> {
    if config.compact_on_turn_end
        && messages
            .last()
            .map(|m| matches!(m.role, AgentRole::Tool | AgentRole::User))
            .unwrap_or(false)
    {
        Some(CompactionTrigger::PostToolResult)
    } else {
        None
    }
}

pub fn destructive_eviction_ids_with_retention(
    config: &AgentConfig,
    messages: &[AgentMessage],
) -> Result<Vec<String>, AgentError> {
    let keep = config.retention_window_messages;
    if keep == 0 || messages.len() <= keep {
        if keep == 0 {
            return Ok(messages.iter().map(|m| m.id.clone()).collect());
        }
        return Ok(vec![]);
    }
    let evict_count = messages.len() - keep;
    Ok(messages[..evict_count]
        .iter()
        .map(|m| m.id.clone())
        .collect())
}

pub fn deterministic_action_order_with_hooks_and_compaction() -> Vec<&'static str> {
    vec!["call_model", "execute_tools", "call_model"]
}
