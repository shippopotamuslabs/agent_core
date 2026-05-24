//! Desktop-ready smoke: native traits only (AGENT-041). Run: `cargo run --example mock_desktop_loop`

use std::sync::Arc;

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::AgentError;
use agent_core::host::{AgentToolExecutor, AgentTransport};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall};

struct PanicTransport;

impl AgentTransport for PanicTransport {
    fn complete_model_turn(&self, _: ModelRequest) -> Result<ModelResponse, AgentError> {
        panic!("example drives submit_model_response manually");
    }
}

struct MockExec;

impl AgentToolExecutor for MockExec {
    fn execute_tools(
        &self,
        calls: Vec<ToolCallRecord>,
    ) -> Result<Vec<ToolResultRecord>, AgentError> {
        Ok(calls
            .into_iter()
            .map(|c| ToolResultRecord {
                call_id: c.call_id,
                result_json: r#"{"results":[]}"#.into(),
                truncated: false,
                soft_failure: false,
                activated_tool_names: Vec::new(),
                result_metadata: None,
            })
            .collect())
    }
}

fn main() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(MockExec),
        Arc::new(agent_core::host::NoOpSummarizer),
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
        SessionIoParams {
            provider: "openai".into(),
            context_window_tokens: 128_000,
            tool_catalog_json: "[]".into(),
            tool_schema_registry_json: "[]".into(),
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        },
    );
    assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
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
    assert!(matches!(eng.next_action(&h), AgentAction::ExecuteTools(_)));
    eng.submit_tool_results(
        &h,
        vec![ToolResultRecord {
            call_id: "c1".into(),
            result_json: r#"{"results":[]}"#.into(),
            truncated: false,
            soft_failure: false,
            activated_tool_names: Vec::new(),
            result_metadata: None,
        }],
    );
    assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
    eng.submit_model_response(&h, ModelResponse::text_only("done"));
    assert!(matches!(eng.next_action(&h), AgentAction::Finished));
    println!("mock_desktop_loop ok");
}
