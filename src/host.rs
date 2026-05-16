use crate::events::AgentError;
use crate::message::{ToolCallRecord, ToolResultRecord};
use crate::turn_io::{ModelRequest, ModelResponse, SummaryRequest, SummaryResult};

/// Performs one provider inference pass.
/// Implemented in Kotlin by `AgentTransportAdapter` (AGENT-027) via UniFFI callback shim.
/// Implemented directly in Rust for desktop consumers and test mocks.
pub trait AgentTransport: Send + Sync {
    fn complete_model_turn(&self, request: ModelRequest) -> Result<ModelResponse, AgentError>;
}

/// Executes a batch of repaired tool calls.
/// Implemented in Kotlin by `AgentToolExecutorAdapter` (AGENT-029) via UniFFI callback shim.
pub trait AgentToolExecutor: Send + Sync {
    fn execute_tools(&self, calls: Vec<ToolCallRecord>) -> Result<Vec<ToolResultRecord>, AgentError>;
}

/// Invokes LLM summarization for the summary compaction tier.
/// Implemented in Kotlin by `AgentSummarizerAdapter` (AGENT-028) via UniFFI callback shim.
/// Only called when `SessionCapabilities.summarization_allowed = true`.
pub trait AgentSummarizer: Send + Sync {
    fn summarize(&self, request: SummaryRequest) -> Result<SummaryResult, AgentError>;
}

/// No-op summarizer used when `summarization_allowed = false` (Llama, LiteRT).
/// Returns an `Internal` error if ever invoked — callers must gate on capabilities.
pub struct NoOpSummarizer;

impl AgentSummarizer for NoOpSummarizer {
    fn summarize(&self, _: SummaryRequest) -> Result<SummaryResult, AgentError> {
        Err(AgentError::Internal(
            "Summarizer not available for this session (summarization_allowed = false)".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockTransport {
        pub response: ModelResponse,
    }

    impl AgentTransport for MockTransport {
        fn complete_model_turn(&self, _: ModelRequest) -> Result<ModelResponse, AgentError> {
            Ok(self.response.clone())
        }
    }

    struct MockExecutor;

    impl AgentToolExecutor for MockExecutor {
        fn execute_tools(&self, calls: Vec<ToolCallRecord>) -> Result<Vec<ToolResultRecord>, AgentError> {
            Ok(calls
                .into_iter()
                .map(|c| ToolResultRecord {
                    call_id: c.call_id,
                    result_json: r#"{"result":"ok"}"#.into(),
                    truncated: false,
                    soft_failure: false,
                })
                .collect())
        }
    }

    #[test]
    fn mock_transport_returns_configured_response() {
        let transport = MockTransport {
            response: ModelResponse::text_only("hello"),
        };
        let req = ModelRequest {
            session_messages: vec![],
            system_prompt: "".into(),
            tools_schema_json: "[]".into(),
            allow_tool_call: false,
        };
        let resp = transport.complete_model_turn(req).unwrap();
        assert_eq!(resp.assistant_text.as_deref(), Some("hello"));
    }

    #[test]
    fn mock_executor_maps_calls_to_results() {
        let executor = MockExecutor;
        let calls = vec![
            ToolCallRecord { call_id: "c1".into(), name: "web_search".into(), arguments_json: "{}".into(), repair_metadata: None },
        ];
        let results = executor.execute_tools(calls).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].call_id, "c1");
        assert!(!results[0].soft_failure);
    }

    #[test]
    fn no_op_summarizer_returns_error() {
        let s = NoOpSummarizer;
        let result = s.summarize(SummaryRequest {
            messages_to_summarize: vec![],
            instruction: "".into(),
            last_message_id: "m1".into(),
        });
        assert!(matches!(result, Err(AgentError::Internal(_))));
    }

    #[test]
    fn traits_are_object_safe() {
        // If this compiles, the traits are dyn-compatible.
        let _t: Box<dyn AgentTransport>;
        let _e: Box<dyn AgentToolExecutor>;
        let _s: Box<dyn AgentSummarizer>;
    }
}
