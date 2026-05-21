//! UniFFI callback → native [`crate::host`] shims (traits live in `lib.rs` for UniFFI discovery).

use crate::events::AgentError;
use crate::host::{AgentSummarizer, AgentToolExecutor, AgentTransport};
use crate::message::{ToolCallRecord, ToolResultRecord};
use crate::turn_io::{ModelRequest, ModelResponse, SummaryRequest, SummaryResult};

use super::types::{FfiModelRequest, FfiSummaryRequest, FfiToolCallRecord};
use crate::{AgentSummarizerCallback, AgentToolExecutorCallback, AgentTransportCallback};

pub struct UniFfiTransport(pub Box<dyn AgentTransportCallback>);

impl AgentTransport for UniFfiTransport {
    fn complete_model_turn(&self, request: ModelRequest) -> Result<ModelResponse, AgentError> {
        let ffi_req: FfiModelRequest = request.into();
        let ffi_resp = self.0.complete_model_turn(ffi_req);
        Ok(ffi_resp.into())
    }
}

pub struct UniFfiExecutor(pub Box<dyn AgentToolExecutorCallback>);

impl AgentToolExecutor for UniFfiExecutor {
    fn execute_tools(
        &self,
        calls: Vec<ToolCallRecord>,
    ) -> Result<Vec<ToolResultRecord>, AgentError> {
        let ffi_calls: Vec<FfiToolCallRecord> = calls.into_iter().map(Into::into).collect();
        let out = self.0.execute_tools(ffi_calls);
        Ok(out.into_iter().map(Into::into).collect())
    }
}

pub enum UniFfiSummarizer {
    Callback(Box<dyn AgentSummarizerCallback>),
    NoOp,
}

impl AgentSummarizer for UniFfiSummarizer {
    fn summarize(&self, request: SummaryRequest) -> Result<SummaryResult, AgentError> {
        match self {
            UniFfiSummarizer::Callback(c) => {
                let ffi_req: FfiSummaryRequest = request.into();
                let ffi_res = c.summarize(ffi_req);
                Ok(ffi_res.into())
            }
            UniFfiSummarizer::NoOp => crate::host::NoOpSummarizer.summarize(request),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::types::{FfiFinishReason, FfiModelResponse, FfiToolResultRecord};
    use super::*;
    use crate::action::AgentAction;
    use crate::engine::AgentEngine;
    use crate::message::{AgentMessage, AgentRole};
    use crate::session::{AgentConfig, SessionCapabilities};
    use crate::turn_io::{FinishReason, RawToolCall};

    struct MockTransportCb;
    impl AgentTransportCallback for MockTransportCb {
        fn complete_model_turn(&self, _request: FfiModelRequest) -> FfiModelResponse {
            FfiModelResponse {
                assistant_text: None,
                tool_calls: vec![],
                finish_reason: FfiFinishReason::Stop,
                provider_reasoning_content: None,
            }
        }
    }

    struct MockExecCb;
    impl AgentToolExecutorCallback for MockExecCb {
        fn execute_tools(&self, calls: Vec<FfiToolCallRecord>) -> Vec<FfiToolResultRecord> {
            calls
                .into_iter()
                .map(|c| FfiToolResultRecord {
                    call_id: c.call_id,
                    result_json: r#"{"ok":true}"#.into(),
                    truncated: false,
                    soft_failure: false,
                })
                .collect()
        }
    }

    #[test]
    fn summarizer_none_uses_noop_path() {
        let eng = AgentEngine::new(
            Arc::new(UniFfiTransport(Box::new(MockTransportCb))),
            Arc::new(UniFfiExecutor(Box::new(MockExecCb))),
            Arc::new(UniFfiSummarizer::NoOp),
        );
        let h = eng.create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "hi".into(),
                metadata: serde_json::Value::Null,
            }],
            AgentConfig::default(),
            SessionCapabilities {
                summarization_allowed: false,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            serde_json::Value::Null,
            crate::engine::SessionIoParams {
                provider: "openai".into(),
                context_window_tokens: 128_000,
                tools_schema_json: "[]".into(),
                system_prompt: "sys".into(),
            },
        );
        let _ = eng.next_action(&h);
        eng.submit_model_response(
            &h,
            ModelResponse {
                assistant_text: None,
                tool_calls: vec![RawToolCall {
                    id: Some("c1".into()),
                    name: "web_search".into(),
                    arguments_json: "{}".into(),
                }],
                finish_reason: FinishReason::ToolUse,
                provider_reasoning_content: None,
            },
        );
        let _ = eng.next_action(&h);
        eng.submit_tool_results(
            &h,
            vec![ToolResultRecord {
                call_id: "c1".into(),
                result_json: "{}".into(),
                truncated: false,
                soft_failure: false,
                result_metadata: None,
            }],
        );
        assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
    }
}
