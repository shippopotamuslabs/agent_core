use crate::action::AgentAction;
use crate::events::{AgentError, AgentEvent};
use crate::loop_metrics::LoopMetrics;
use crate::message::{ToolAttemptMetadata, ToolCallRecord, ToolResultMetadata, ToolResultRecord};
use crate::session::AgentConfig;

#[derive(Debug, Clone)]
pub struct ToolPolicyOutcome {
    pub result_metadata: ToolResultMetadata,
    pub events: Vec<AgentEvent>,
    pub result_json: String,
}

pub fn prepare_write_or_patch(
    config: &AgentConfig,
    metrics: &LoopMetrics,
    path: &str,
) -> Result<Option<AgentAction>, AgentError> {
    if config.require_read_before_edit && !metrics.can_edit_path(path) {
        return Err(AgentError::PolicyRejected {
            reason: "read_before_edit_required".into(),
        });
    }
    Ok(None)
}

pub fn execute_delegation_call(call_id: &str) -> AgentAction {
    AgentAction::ExecuteTools(vec![make_delegation_call(call_id)])
}

pub fn serialize_attempt_metadata(
    metadata: &ToolAttemptMetadata,
) -> Result<serde_json::Value, AgentError> {
    serde_json::to_value(metadata).map_err(|e| AgentError::Internal(e.to_string()))
}

pub fn process_policy_denial(tool_name: &str) -> Result<ToolPolicyOutcome, AgentError> {
    Ok(ToolPolicyOutcome {
        result_metadata: ToolResultMetadata {
            duration_ms: None,
            timed_out: false,
            timeout_ms: None,
            policy_denied: true,
            prior_read_satisfied: false,
            delegated_agent_name: None,
        },
        events: vec![AgentEvent::ToolPolicyDenied {
            tool_name: tool_name.into(),
        }],
        result_json: r#"{"error":"policy_denied"}"#.into(),
    })
}

pub fn record_timeout_result(timeout_ms: u64) -> Result<ToolPolicyOutcome, AgentError> {
    Ok(ToolPolicyOutcome {
        result_metadata: ToolResultMetadata {
            duration_ms: None,
            timed_out: true,
            timeout_ms: Some(timeout_ms),
            policy_denied: false,
            prior_read_satisfied: false,
            delegated_agent_name: None,
        },
        events: vec![AgentEvent::ToolTimedOut {
            tool_name: "unknown".into(),
            timeout_ms,
        }],
        result_json: r#"{"error":"timed_out"}"#.into(),
    })
}

pub fn assert_structured_metadata_present_for_mutation_defense(
    result: &ToolResultRecord,
) -> Result<(), AgentError> {
    if result.result_metadata.is_none() {
        return Err(AgentError::PolicyRejected {
            reason: "missing structured metadata fields".into(),
        });
    }
    Ok(())
}

pub fn delegation_tool_name(action: &AgentAction) -> Option<&str> {
    if let AgentAction::ExecuteTools(calls) = action {
        if calls.first().map(|c| c.name.as_str()) == Some("task_delegate") {
            return Some("task_delegate");
        }
    }
    None
}

pub fn make_delegation_call(call_id: &str) -> ToolCallRecord {
    ToolCallRecord {
        call_id: call_id.into(),
        name: "task_delegate".into(),
        arguments_json: "{}".into(),
        attempt_metadata: None,
        repair_metadata: None,
    }
}
