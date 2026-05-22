//! Proactive `_agent` tool-call budget metadata injection (FEAT-tool-budget-agent-meta).

use serde_json::{json, Map, Value};

use crate::guardrails::GuardrailState;
use crate::message::{AgentMessage, AgentRole};
use crate::session::{AgentConfig, AgentSession};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolBudgetShutdownReason {
    ToolCallBudgetExhausted,
    SoftFailureStreakExhausted,
    EnvelopeParseStreakExhausted,
}

impl ToolBudgetShutdownReason {
    pub fn as_json_str(self) -> &'static str {
        match self {
            Self::ToolCallBudgetExhausted => "tool_call_budget_exhausted",
            Self::SoftFailureStreakExhausted => "soft_failure_streak_exhausted",
            Self::EnvelopeParseStreakExhausted => "envelope_parse_streak_exhausted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentToolBudgetMeta {
    pub tool_calls_used: u32,
    pub tool_calls_limit: u32,
    pub tool_calls_remaining: u32,
    pub allow_tool_call: bool,
    pub reason: Option<ToolBudgetShutdownReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectOutcome {
    MergedIntoExistingObject { message_index: usize },
    AppendedSynthetic { message_id: String },
    SkippedNoToolMessages,
}

/// Resolves shutdown reason from guardrail counters (first failing predicate wins).
pub fn resolve_shutdown_reason(
    guardrails: &GuardrailState,
    config: &AgentConfig,
    last_model_response_had_append_blocks: bool,
) -> ToolBudgetShutdownReason {
    let limit = guardrails.effective_max_tool_calls(config, last_model_response_had_append_blocks);
    if guardrails.tool_calls_executed >= limit {
        return ToolBudgetShutdownReason::ToolCallBudgetExhausted;
    }
    if guardrails.consecutive_soft_failure_turns >= config.tool_retry_limit {
        return ToolBudgetShutdownReason::SoftFailureStreakExhausted;
    }
    ToolBudgetShutdownReason::EnvelopeParseStreakExhausted
}

/// Builds the `_agent` JSON object for embedding in tool message content.
pub fn agent_object_from_meta(meta: &AgentToolBudgetMeta) -> Value {
    let mut agent = Map::new();
    agent.insert(
        "tool_calls_used".into(),
        json!(meta.tool_calls_used),
    );
    agent.insert(
        "tool_calls_limit".into(),
        json!(meta.tool_calls_limit),
    );
    agent.insert(
        "tool_calls_remaining".into(),
        json!(meta.tool_calls_remaining),
    );
    agent.insert(
        "allow_tool_call".into(),
        json!(meta.allow_tool_call),
    );
    if let Some(reason) = meta.reason {
        agent.insert("reason".into(), json!(reason.as_json_str()));
    }
    Value::Object(agent)
}

/// Merges budget fields into an existing tool result JSON object string.
pub fn merge_budget_into_tool_content(content: &str, meta: &AgentToolBudgetMeta) -> String {
    let mut root = serde_json::from_str::<Value>(content)
        .unwrap_or_else(|_| Value::Object(Map::new()));
    let Some(obj) = root.as_object_mut() else {
        return content.to_string();
    };

    let budget = agent_object_from_meta(meta);
    match obj.get_mut("_agent") {
        Some(Value::Object(existing)) => {
            if let Some(budget_obj) = budget.as_object() {
                for (key, value) in budget_obj {
                    existing.insert(key.clone(), value.clone());
                }
            }
        }
        _ => {
            obj.insert("_agent".into(), budget);
        }
    }

    serde_json::to_string(&root).unwrap_or_else(|_| content.to_string())
}

/// Builds a synthetic tool message containing only `{"_agent":{...}}`.
pub fn build_synthetic_budget_message(turn_index: u32, meta: &AgentToolBudgetMeta) -> AgentMessage {
    AgentMessage {
        id: format!("tool-agent-budget-{turn_index}"),
        role: AgentRole::Tool,
        content: serde_json::to_string(&json!({ "_agent": agent_object_from_meta(meta) }))
            .unwrap_or_else(|_| "{}".into()),
        metadata: json!({ "agent_tool_budget": true }),
    }
}

/// Merges budget metadata into the session after a tool batch.
pub fn inject_tool_budget_into_session(
    session: &mut AgentSession,
    turn_index: u32,
    meta: &AgentToolBudgetMeta,
    last_batch_tool_message_index: Option<usize>,
) -> InjectOutcome {
    let Some(index) = last_batch_tool_message_index else {
        return InjectOutcome::SkippedNoToolMessages;
    };
    if index >= session.messages.len() {
        return InjectOutcome::SkippedNoToolMessages;
    }

    let content = session.messages[index].content.clone();
    if serde_json::from_str::<Value>(&content)
        .ok()
        .is_some_and(|value| value.is_object())
    {
        session.messages[index].content = merge_budget_into_tool_content(&content, meta);
        InjectOutcome::MergedIntoExistingObject { message_index: index }
    } else {
        let synthetic = build_synthetic_budget_message(turn_index, meta);
        let message_id = synthetic.id.clone();
        session.messages.push(synthetic);
        InjectOutcome::AppendedSynthetic { message_id }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn meta(
        used: u32,
        limit: u32,
        allow: bool,
        reason: Option<ToolBudgetShutdownReason>,
    ) -> AgentToolBudgetMeta {
        AgentToolBudgetMeta {
            tool_calls_used: used,
            tool_calls_limit: limit,
            tool_calls_remaining: limit.saturating_sub(used),
            allow_tool_call: allow,
            reason,
        }
    }

    fn agent_json(meta: &AgentToolBudgetMeta) -> Value {
        agent_object_from_meta(meta)
    }

    /// TB-04 — budget exhausted → reason `tool_call_budget_exhausted`.
    #[test]
    fn tb_04_budget_exhausted_reason() {
        let snapshot = meta(1, 1, false, Some(ToolBudgetShutdownReason::ToolCallBudgetExhausted));
        let agent = agent_json(&snapshot);
        assert_eq!(agent["tool_calls_used"], 1, "TB-04 used");
        assert_eq!(agent["tool_calls_limit"], 1, "TB-04 limit");
        assert_eq!(agent["tool_calls_remaining"], 0, "TB-04 remaining");
        assert_eq!(agent["allow_tool_call"], false, "TB-04 allow");
        assert_eq!(
            agent["reason"].as_str(),
            Some("tool_call_budget_exhausted"),
            "TB-04 reason json"
        );
    }

    /// TB-05 — soft-failure streak exhausted.
    #[test]
    fn tb_05_soft_failure_streak_reason() {
        let snapshot = meta(
            6,
            20,
            false,
            Some(ToolBudgetShutdownReason::SoftFailureStreakExhausted),
        );
        let agent = agent_json(&snapshot);
        assert_eq!(agent["allow_tool_call"], false, "TB-05 allow");
        assert_eq!(
            agent["reason"].as_str(),
            Some("soft_failure_streak_exhausted"),
            "TB-05 reason json"
        );
        assert_eq!(agent["tool_calls_used"], 6, "TB-05 used");
        assert_eq!(agent["tool_calls_remaining"], 14, "TB-05 remaining");
        assert_eq!(agent["tool_calls_limit"], 20, "TB-05 limit");
    }

    /// TB-06 — envelope-parse streak exhausted.
    #[test]
    fn tb_06_envelope_parse_streak_reason() {
        let snapshot = meta(
            3,
            20,
            false,
            Some(ToolBudgetShutdownReason::EnvelopeParseStreakExhausted),
        );
        let agent = agent_json(&snapshot);
        assert_eq!(agent["allow_tool_call"], false, "TB-06 allow");
        assert_eq!(
            agent["reason"].as_str(),
            Some("envelope_parse_streak_exhausted"),
            "TB-06 reason json"
        );
        assert_eq!(agent["tool_calls_used"], 3, "TB-06 used");
        assert_eq!(agent["tool_calls_remaining"], 17, "TB-06 remaining");
        assert_eq!(agent["tool_calls_limit"], 20, "TB-06 limit");
    }

    /// MD-04 — `reason` absent when `allow_tool_call` is true.
    #[test]
    fn md_04_reason_absent_when_tools_allowed() {
        let snapshot = meta(1, 20, true, None);
        let agent = agent_json(&snapshot);
        assert_eq!(agent["allow_tool_call"], true, "MD-04 allow");
        assert!(
            agent.get("reason").is_none(),
            "MD-04 reason must be absent when allow_tool_call true"
        );
    }

    /// MD-06 — merge preserves executor-provided `_agent` keys.
    #[test]
    fn md_06_merge_preserves_executor_agent_keys() {
        let input = r#"{"_agent":{"source":"executor"},"results":[]}"#;
        let snapshot = meta(1, 20, true, None);
        let merged = merge_budget_into_tool_content(input, &snapshot);
        let parsed: Value = serde_json::from_str(&merged).expect("MD-06 valid json");
        assert_eq!(
            parsed["_agent"]["source"].as_str(),
            Some("executor"),
            "MD-06 executor source"
        );
        assert_eq!(parsed["_agent"]["tool_calls_used"], 1, "MD-06 used");
        assert_eq!(parsed["_agent"]["tool_calls_limit"], 20, "MD-06 limit");
        assert_eq!(parsed["_agent"]["tool_calls_remaining"], 19, "MD-06 remaining");
        assert_eq!(parsed["_agent"]["allow_tool_call"], true, "MD-06 allow");
        assert!(parsed.get("results").is_some(), "MD-06 results present");
    }
}
