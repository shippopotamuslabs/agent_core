//! TDD Red — `FEAT-long-agent-loop-robustness` integration scenarios LALR-01, LALR-11, LALR-20.

use std::sync::Arc;

use agent_core::action::AgentAction;
use agent_core::host::{AgentToolExecutor, AgentTransport};
use agent_core::lalr_harness::{
    config_with_long_loop, default_long_loop_io, inject_semantic_fact_enclave,
    noop_transport_engine, run_twenty_five_tool_turn_loop,
};
use agent_core::long_loop_policy::LongLoopPolicy;
use agent_core::message::{ToolCallRecord, ToolResultRecord};
use agent_core::session::SessionCapabilities;
use agent_core::state_ledger::StateLedger;
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall};

struct NoopTransport;
impl AgentTransport for NoopTransport {
    fn complete_model_turn(
        &self,
        _: ModelRequest,
    ) -> Result<ModelResponse, agent_core::events::AgentError> {
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
    ) -> Result<Vec<ToolResultRecord>, agent_core::events::AgentError> {
        Ok(calls
            .into_iter()
            .map(|c| ToolResultRecord {
                call_id: c.call_id,
                result_json: r#"{"status":"ok"}"#.into(),
                truncated: false,
                soft_failure: false,
                activated_tool_names: Vec::new(),
                result_metadata: None,
            })
            .collect())
    }
}

#[test]
fn lalr_01_twenty_five_tool_turns_within_sidecar_budget() {
    let mut long_loop = LongLoopPolicy::hosted_mutable_defaults();
    long_loop.sidecar_byte_budget_per_call = 8192;
    let config = config_with_long_loop(long_loop);
    let engine = noop_transport_engine(Arc::new(NoopTransport), Arc::new(NoopExecutor));
    let handle = engine.create_session(
        vec![],
        config.clone(),
        SessionCapabilities {
            summarization_allowed: true,
            ..Default::default()
        },
        serde_json::Value::Null,
        default_long_loop_io(),
    );

    let outcome = run_twenty_five_tool_turn_loop(&engine, &handle, &config, "noop");

    for req in &outcome.model_requests {
        assert!(
            req.provider_context.sidecar_byte_len <= 8192,
            "sidecar_byte_len must be <= 8192 per CallModel"
        );
    }
    assert!(matches!(outcome.final_action, AgentAction::Finished));
}

#[test]
fn lalr_11_codename_survives_twenty_five_tool_turns() {
    let config = config_with_long_loop(LongLoopPolicy::hosted_mutable_defaults());
    let mut ledger = StateLedger::default();
    inject_semantic_fact_enclave(&mut ledger);
    let engine = noop_transport_engine(Arc::new(NoopTransport), Arc::new(NoopExecutor));
    let handle = engine.create_session(
        vec![],
        config.clone(),
        SessionCapabilities {
            summarization_allowed: true,
            ..Default::default()
        },
        serde_json::Value::Null,
        default_long_loop_io(),
    );

    let _ledger = ledger;
    let outcome = run_twenty_five_tool_turn_loop(&engine, &handle, &config, "noop");
    assert!(matches!(outcome.final_action, AgentAction::Finished));
    assert!(
        outcome.semantic_fact_preserved,
        "PROJECT_CODENAME=ENCLAVE must survive in ledger or transcript"
    );
}

#[test]
fn lalr_20_message_count_cap_after_twenty_five_turns() {
    let config = config_with_long_loop(LongLoopPolicy::hosted_mutable_defaults());
    let engine = noop_transport_engine(Arc::new(NoopTransport), Arc::new(NoopExecutor));
    let handle = engine.create_session(
        vec![],
        config.clone(),
        SessionCapabilities {
            summarization_allowed: true,
            ..Default::default()
        },
        serde_json::Value::Null,
        default_long_loop_io(),
    );

    let outcome = run_twenty_five_tool_turn_loop(&engine, &handle, &config, "noop");
    assert!(matches!(outcome.final_action, AgentAction::Finished));
    assert!(
        outcome.session_message_count <= 40,
        "session.messages.len() must be <= 40 after 25 tool rounds with compaction"
    );
}
