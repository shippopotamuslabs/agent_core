//! Extra kv_fixed assertions (AGENT-047). Run: `cargo test --test kv_fixed_test`

use std::sync::Arc;

use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::AgentEvent;
use agent_core::host::{AgentToolExecutor, AgentTransport};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{ModelRequest, ModelResponse};
use agent_core::events::AgentError;

struct PanicTransport;
impl AgentTransport for PanicTransport {
    fn complete_model_turn(&self, _: ModelRequest) -> Result<ModelResponse, AgentError> {
        panic!("unused");
    }
}

struct PanicExec;
impl AgentToolExecutor for PanicExec {
    fn execute_tools(&self, _: Vec<ToolCallRecord>) -> Result<Vec<ToolResultRecord>, AgentError> {
        panic!("unused");
    }
}

#[test]
fn kv_fixed_emits_invalidated_non_kv_fixed_does_not_on_same_shape() {
    let big = "hello ".repeat(400);
    let mk = || {
        AgentEngine::new(
            Arc::new(PanicTransport),
            Arc::new(PanicExec),
            Arc::new(agent_core::host::NoOpSummarizer),
        )
    };
    let io = || SessionIoParams {
        provider: "anthropic".into(),
        context_window_tokens: 256,
        tools_schema_json: "[]".into(),
        system_prompt: "sys".into(),
    };
    let msgs = vec![
        AgentMessage {
            id: "u0".into(),
            role: AgentRole::User,
            content: big.clone(),
            metadata: serde_json::Value::Null,
        },
        AgentMessage {
            id: "a0".into(),
            role: AgentRole::Assistant,
            content: big,
            metadata: serde_json::Value::Null,
        },
        AgentMessage {
            id: "u1".into(),
            role: AgentRole::User,
            content: "tail".into(),
            metadata: serde_json::Value::Null,
        },
    ];

    let eng_kv = mk();
    let h_kv = eng_kv.create_session(
        msgs.clone(),
        AgentConfig::default(),
        SessionCapabilities {
            summarization_allowed: false,
            kv_fixed: true,
            tool_result_budget_tokens: None,
        },
        serde_json::Value::Null,
        io(),
    );
    let _ = eng_kv.next_action(&h_kv);
    assert!(eng_kv
        .drain_events(&h_kv)
        .iter()
        .any(|e| matches!(e, AgentEvent::KvCacheInvalidated { .. })));

    let eng_plain = mk();
    let h_plain = eng_plain.create_session(
        msgs,
        AgentConfig::default(),
        SessionCapabilities {
            summarization_allowed: false,
            kv_fixed: false,
            tool_result_budget_tokens: None,
        },
        serde_json::Value::Null,
        io(),
    );
    let _ = eng_plain.next_action(&h_plain);
    assert!(!eng_plain
        .drain_events(&h_plain)
        .iter()
        .any(|e| matches!(e, AgentEvent::KvCacheInvalidated { .. })));
}
