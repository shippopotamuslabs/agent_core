#![cfg(feature = "lalr-harness")]

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse};
use serde_json::json;

#[test]
fn pending_work_reminder_filters_superseded_from_wire() {
    let eng = engine();
    let handle = eng.create_session(
        vec![
            message("u1", AgentRole::User, "ship"),
            AgentMessage {
                id: "old-tool".into(),
                role: AgentRole::Tool,
                content: "superseded payload".into(),
                metadata: {
                    let mut meta = MessageMeta::new();
                    meta.mark_superseded();
                    meta
                },
            },
        ],
        pending_config(),
        pending_caps(),
        json!({"pending_work": ["verify failing tests"]}),
        io(),
    );

    let request = pending_request(&eng, &handle);

    assert!(request
        .session_messages
        .iter()
        .all(|message| message.id != "old-tool"));
    assert_eq!(
        request
            .session_messages
            .last()
            .map(|message| message.content.as_str()),
        Some("Pending work remains: verify failing tests")
    );
}

#[test]
fn pending_work_reminder_threads_provider_context_snapshot() {
    let eng = engine();
    let handle = eng.create_session(
        vec![message("u1", AgentRole::User, "ship")],
        pending_config(),
        pending_caps(),
        json!({"pending_work": ["verify failing tests"]}),
        io(),
    );

    let request = pending_request(&eng, &handle);

    assert_eq!(request.provider_context.transcript_revision, 0);
    assert_eq!(request.provider_context.sidecar_generation, 1);
    assert!(request.provider_context.sidecar_byte_len > 0);
    assert!(request.state_ledger_delta_json.is_some());
}

fn engine() -> AgentEngine {
    AgentEngine::new()
}

fn pending_config() -> AgentConfig {
    let mut config = AgentConfig::default();
    config.enable_loop_hooks = true;
    config.enable_pending_work_guard = true;
    config.enable_state_ledger = true;
    config.max_model_requests_per_turn = 2;
    config
}

fn pending_caps() -> SessionCapabilities {
    SessionCapabilities {
        lifecycle_hooks_supported: true,
        ..Default::default()
    }
}

fn io() -> SessionIoParams {
    SessionIoParams::new(
        "anthropic".into(),
        200_000,
        "[]".into(),
        "[]".into(),
        "sys".into(),
    )
    .expect("valid test io params")
}

fn pending_request(engine: &AgentEngine, handle: &agent_core::SessionHandle) -> ModelRequest {
    assert!(matches!(
        engine.next_action(handle),
        AgentAction::CallModel(_)
    ));
    engine.submit_model_response(
        handle,
        ModelResponse {
            assistant_text: Some("final text".into()),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            provider_reasoning_content: None,
        },
    );
    match engine.next_action(handle) {
        AgentAction::CallModel(request) => request,
        other => panic!("expected pending-work CallModel, got {other:?}"),
    }
}

fn message(id: &str, role: AgentRole, content: &str) -> AgentMessage {
    AgentMessage {
        id: id.into(),
        role,
        content: content.into(),
        metadata: MessageMeta::new(),
    }
}
