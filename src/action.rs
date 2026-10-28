use crate::events::AgentError;
use crate::message::ToolCallRecord;
use crate::turn_io::SummaryRequest;
use serde::{Deserialize, Serialize};

/// Actions yielded by `AgentEngine::next_action()`.
/// The host fulfills each action and calls the appropriate `submit_*` method.
///
/// Note: there is no `EmitEvent` variant. Events accumulate internally and are
/// drained via `AgentEngine::drain_events(handle)` (AGENT-049).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
// large_enum_variant: boxing a variant would change this public serde-tagged API
// consumed by both hosts; the size cost is accepted deliberately.
#[allow(clippy::large_enum_variant)]
pub enum AgentAction {
    /// Host must call the transport (provider inference) and then
    /// call `submit_model_response`.
    CallModel(crate::turn_io::ModelRequest),

    /// Host must execute the listed tool calls and then call
    /// `submit_tool_results`.
    ExecuteTools(Vec<ToolCallRecord>),

    /// Host must invoke the summarizer and then call `submit_summary`.
    /// Only emitted when `SessionCapabilities.summarization_allowed = true`.
    RequestSummary(SummaryRequest),

    /// Session completed successfully. No further `submit_*` calls needed.
    Finished,

    /// Session terminated with an error.
    Failed(AgentError),

    /// Manual compaction (S18/E9) finished without requiring a host round-trip
    /// (no-op when policy disables manual compact, or after a forced summary
    /// completes synchronously).
    ManualCompactCompleted(crate::manual_compact::ManualCompactResult),
}

impl AgentAction {
    pub fn is_terminal(&self) -> bool {
        matches!(self, AgentAction::Finished | AgentAction::Failed(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn_io::ModelRequest;

    fn make_request() -> ModelRequest {
        ModelRequest {
            session_messages: vec![],
            system_prompt: "test".into(),
            tool_schemas: Vec::new(),
            rendered_runtime_tools: Vec::new(),
            allow_tool_call: true,
            context_references: vec![],
            state_ledger: None,
            tool_context_index: None,
            file_context_index: None,
            cache_hints: vec![],
            provider_context: Default::default(),
            state_ledger_delta_json: None,
            tool_context_index_delta_json: None,
            file_context_index_delta_json: None,
            retry_after_ms: None,
            model_retry_attempt: None,
        }
    }

    #[test]
    fn call_model_roundtrip() {
        let action = AgentAction::CallModel(make_request());
        let json = serde_json::to_string(&action).unwrap();
        let back: AgentAction = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, AgentAction::CallModel(_)));
        assert!(!back.is_terminal());
    }

    #[test]
    fn finished_is_terminal() {
        assert!(AgentAction::Finished.is_terminal());
    }

    #[test]
    fn failed_is_terminal() {
        let action = AgentAction::Failed(crate::events::AgentError::Internal("boom".into()));
        assert!(action.is_terminal());
    }

    #[test]
    fn execute_tools_not_terminal() {
        let action = AgentAction::ExecuteTools(vec![]);
        assert!(!action.is_terminal());
    }

    #[test]
    fn request_summary_roundtrip() {
        let action = AgentAction::RequestSummary(crate::turn_io::SummaryRequest {
            messages_to_summarize: vec![],
            instruction: "Summarize.".into(),
            last_message_id: "m1".into(),
            trigger: crate::events::CompactionTrigger::Preflight,
            focus_instruction: None,
            history_ref: None,
            state_ledger_snapshot: None,
        });
        let json = serde_json::to_string(&action).unwrap();
        let back: AgentAction = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, AgentAction::RequestSummary(_)));
    }
}
