//! TDD Red — long-loop policy scenarios LALR-12.

use std::sync::Arc;

use agent_core::action::AgentAction;
use agent_core::events::AgentError;
use agent_core::host::{AgentToolExecutor, AgentTransport};
use agent_core::lalr_harness::{
    config_with_long_loop, default_long_loop_io, guardrail_kind, next_action_at_max_tool_turns,
    noop_transport_engine,
};
use agent_core::long_loop_policy::LongLoopPolicy;
use agent_core::message::{ToolCallRecord, ToolResultRecord};
use agent_core::session::SessionCapabilities;
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall};

struct ToolRequestTransport;
impl AgentTransport for ToolRequestTransport {
    fn complete_model_turn(&self, _: ModelRequest) -> Result<ModelResponse, AgentError> {
        Ok(ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-1".into()),
                name: "noop".into(),
                arguments_json: "{}".into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        })
    }
}

struct NoopExecutor;
impl AgentToolExecutor for NoopExecutor {
    fn execute_tools(
        &self,
        calls: Vec<ToolCallRecord>,
    ) -> Result<Vec<ToolResultRecord>, AgentError> {
        Ok(calls
            .into_iter()
            .map(|c| ToolResultRecord {
                call_id: c.call_id,
                result_json: "{}".into(),
                truncated: false,
                soft_failure: false,
                activated_tool_names: Vec::new(),
                result_metadata: None,
            })
            .collect())
    }
}

#[test]
fn lalr_12_stops_at_five_tool_rounds() {
    let mut long_loop = LongLoopPolicy::hosted_mutable_defaults();
    long_loop.max_tool_turns = 5;
    let config = config_with_long_loop(long_loop);
    let engine = noop_transport_engine(Arc::new(ToolRequestTransport), Arc::new(NoopExecutor));
    let handle = engine.create_session(
        vec![],
        config,
        SessionCapabilities::default(),
        serde_json::Value::Null,
        default_long_loop_io(),
    );

    let action = next_action_at_max_tool_turns(&engine, &handle, 5);
    assert!(matches!(action, AgentAction::Failed(_)));
    if let AgentAction::Failed(err) = action {
        assert_eq!(guardrail_kind(&err), Some("max_tool_turns"));
    }
}
