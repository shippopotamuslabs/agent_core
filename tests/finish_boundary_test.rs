use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::{AgentError, AgentEvent};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{FinishReason, ModelResponse, RawToolCall};
use agent_core::{
    create_agent_engine, FfiAgentAction, FfiAgentConfig, FfiAgentMessage, FfiAgentRole,
    FfiFinishReason, FfiModelResponse, FfiSessionCapabilities, FfiSessionIoParams,
};

fn user_message(content: &str) -> AgentMessage {
    AgentMessage {
        id: "u1".into(),
        role: AgentRole::User,
        content: content.into(),
        metadata: MessageMeta::new(),
    }
}

fn engine() -> AgentEngine {
    AgentEngine::new()
}

fn io() -> SessionIoParams {
    SessionIoParams {
        provider: "anthropic".into(),
        context_window_tokens: 200_000,
        tool_catalog_json: "[]".into(),
        tool_schema_registry_json: "[]".into(),
        tools_schema_json: "[]".into(),
        system_prompt: "sys".into(),
    }
}

fn submit_no_tool_response(
    engine: &AgentEngine,
    handle: &agent_core::SessionHandle,
    finish_reason: FinishReason,
) -> AgentAction {
    assert!(matches!(
        engine.next_action(handle),
        AgentAction::CallModel(_)
    ));
    engine.submit_model_response(
        handle,
        ModelResponse {
            assistant_text: Some("final text".into()),
            tool_calls: vec![],
            finish_reason,
            provider_reasoning_content: None,
        },
    );
    engine.next_action(handle)
}

#[test]
fn fb_r_001_stop_without_tools_finishes_terminal_turn() {
    let eng = engine();
    let handle = eng.create_session(
        vec![user_message("answer directly")],
        AgentConfig::default(),
        SessionCapabilities::default(),
        serde_json::Value::Null,
        io(),
    );

    let action = submit_no_tool_response(&eng, &handle, FinishReason::Stop);
    let events = eng.drain_events(&handle);

    assert!(matches!(action, AgentAction::Finished));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::TurnCompleted {
            terminal: true,
            tool_call_count: 0,
            ..
        }
    )));
}

#[test]
fn fb_r_002_max_tokens_without_tools_fails_model_stopped() {
    let eng = engine();
    let handle = eng.create_session(
        vec![user_message("answer directly")],
        AgentConfig::default(),
        SessionCapabilities::default(),
        serde_json::Value::Null,
        io(),
    );

    let action = submit_no_tool_response(&eng, &handle, FinishReason::MaxTokens);

    match action {
        AgentAction::Failed(AgentError::ModelStopped { ref reason }) => {
            assert_eq!("max_tokens", reason)
        }
        other => panic!("FB-R-002 expected ModelStopped(max_tokens), got {other:?}"),
    }
    assert_eq!(
        "Model stopped before completion: max_tokens",
        action_error_text(action)
    );
}

#[test]
fn fb_r_003_other_finish_without_tools_fails_model_stopped() {
    let eng = engine();
    let handle = eng.create_session(
        vec![user_message("answer directly")],
        AgentConfig::default(),
        SessionCapabilities::default(),
        serde_json::Value::Null,
        io(),
    );

    let action =
        submit_no_tool_response(&eng, &handle, FinishReason::Other("content_filter".into()));

    match action {
        AgentAction::Failed(AgentError::ModelStopped { ref reason }) => {
            assert_eq!("content_filter", reason)
        }
        other => panic!("FB-R-003 expected ModelStopped(content_filter), got {other:?}"),
    }
    assert_eq!(
        "Model stopped before completion: content_filter",
        action_error_text(action),
    );
}

#[test]
fn fb_r_004_tool_use_executes_exact_web_search_call() {
    let eng = engine();
    let handle = eng.create_session(
        vec![user_message("search")],
        AgentConfig::default(),
        SessionCapabilities::default(),
        serde_json::Value::Null,
        io(),
    );
    assert!(matches!(
        eng.next_action(&handle),
        AgentAction::CallModel(_)
    ));

    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-1".into()),
                name: "web_search".into(),
                arguments_json: r#"{"query":"finish boundary"}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );

    match eng.next_action(&handle) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(1, calls.len());
            assert_eq!("web_search", calls[0].name);
            assert_eq!(r#"{"query":"finish boundary"}"#, calls[0].arguments_json);
        }
        other => panic!("FB-R-004 expected ExecuteTools, got {other:?}"),
    }
}

#[test]
fn fb_r_005_before_finish_pending_work_injects_message_and_calls_model_under_cap() {
    let eng = engine();
    let config = AgentConfig {
        enable_loop_hooks: true,
        enable_pending_work_guard: true,
        max_model_requests_per_turn: 2,
        ..Default::default()
    };
    let handle = eng.create_session(
        vec![user_message("ship")],
        config,
        SessionCapabilities {
            lifecycle_hooks_supported: true,
            ..Default::default()
        },
        serde_json::json!({"pending_work": ["verify failing tests"]}),
        io(),
    );

    let action = submit_no_tool_response(&eng, &handle, FinishReason::Stop);
    let events = eng.drain_events(&handle);

    match action {
        AgentAction::CallModel(request) => {
            assert_eq!(
                "Pending work remains: verify failing tests",
                request.session_messages.last().unwrap().content
            );
            assert!(request.allow_tool_call);
        }
        other => panic!("FB-R-005 expected CallModel, got {other:?}"),
    }
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::HookInvoked { checkpoint } if checkpoint == "before_finish"
    )));
}

#[test]
fn fb_r_006_hooks_disabled_stop_finishes_without_second_model_call() {
    let eng = engine();
    let config = AgentConfig {
        enable_loop_hooks: false,
        max_model_requests_per_turn: 1,
        ..Default::default()
    };
    let handle = eng.create_session(
        vec![user_message("ship")],
        config,
        SessionCapabilities::default(),
        serde_json::json!({"pending_work": ["verify failing tests"]}),
        io(),
    );

    let action = submit_no_tool_response(&eng, &handle, FinishReason::Stop);

    assert!(matches!(action, AgentAction::Finished));
}

#[test]
fn fb_r_007_before_finish_pending_work_over_cap_fails_guardrail() {
    let eng = engine();
    let config = AgentConfig {
        enable_loop_hooks: true,
        enable_pending_work_guard: true,
        max_model_requests_per_turn: 1,
        ..Default::default()
    };
    let handle = eng.create_session(
        vec![user_message("ship")],
        config,
        SessionCapabilities {
            lifecycle_hooks_supported: true,
            ..Default::default()
        },
        serde_json::json!({"pending_work": ["verify failing tests"]}),
        io(),
    );

    let action = submit_no_tool_response(&eng, &handle, FinishReason::Stop);

    match action {
        AgentAction::Failed(AgentError::GuardrailExceeded {
            ref kind,
            detail: _,
        }) => {
            assert_eq!("max_model_requests_per_turn", kind);
        }
        other => panic!("FB-R-007 expected max_model_requests_per_turn guardrail, got {other:?}"),
    }
    assert_eq!(
        "Guardrail exceeded: max_model_requests_per_turn",
        action_error_text(action),
    );
}

#[test]
fn fb_r_008_finish_blocked_without_injected_message_fails_guardrail() {
    let eng = engine();
    let config = AgentConfig {
        enable_loop_hooks: true,
        enable_pending_work_guard: true,
        max_model_requests_per_turn: 2,
        ..Default::default()
    };
    let handle = eng.create_session(
        vec![user_message("ship")],
        config,
        SessionCapabilities {
            lifecycle_hooks_supported: true,
            ..Default::default()
        },
        serde_json::json!({"pending_work": [], "force_finish_blocked": true}),
        io(),
    );

    let action = submit_no_tool_response(&eng, &handle, FinishReason::Stop);

    match action {
        AgentAction::Failed(AgentError::GuardrailExceeded {
            ref kind,
            detail: _,
        }) => {
            assert_eq!("finish_blocked_without_injected_message", kind);
        }
        other => panic!(
            "FB-R-008 expected finish_blocked_without_injected_message guardrail, got {other:?}"
        ),
    }
    assert_eq!(
        "Guardrail exceeded: finish_blocked_without_injected_message",
        action_error_text(action),
    );
}

#[test]
fn fb_r_009_hook_continuation_builds_pending_request_before_awaiting_model() {
    let eng = engine();
    let config = AgentConfig {
        enable_loop_hooks: true,
        enable_pending_work_guard: true,
        max_model_requests_per_turn: 2,
        ..Default::default()
    };
    let handle = eng.create_session(
        vec![user_message("ship")],
        config,
        SessionCapabilities {
            lifecycle_hooks_supported: true,
            ..Default::default()
        },
        serde_json::json!({"pending_work": ["ship tests"]}),
        io(),
    );

    let action = submit_no_tool_response(&eng, &handle, FinishReason::Stop);

    match action {
        AgentAction::CallModel(request) => {
            assert_eq!("sys", request.system_prompt);
            assert_eq!("[]", request.tools_schema_json);
            assert_eq!(
                "Pending work remains: ship tests",
                request.session_messages.last().unwrap().content
            );
        }
        other => panic!("FB-R-009 expected CallModel, got {other:?}"),
    }
}

#[test]
fn fb_r_010_ffi_config_enables_pending_work_continuation() {
    let eng = create_agent_engine();
    let config = FfiAgentConfig {
        enable_loop_hooks: true,
        enable_pending_work_guard: true,
        max_model_requests_per_turn: 2,
        ..Default::default()
    };
    let capabilities = FfiSessionCapabilities {
        lifecycle_hooks_supported: true,
        ..Default::default()
    };
    let handle = eng.create_session(
        vec![FfiAgentMessage {
            id: "u1".into(),
            role: FfiAgentRole::User,
            content: "ship".into(),
            metadata_json: "null".into(),
        }],
        config,
        capabilities,
        serde_json::json!({"pending_work": ["verify failing tests"]}).to_string(),
        FfiSessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tool_catalog_json: "[]".into(),
            tool_schema_registry_json: "[]".into(),
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        },
    );

    assert!(matches!(
        eng.next_action(&handle),
        FfiAgentAction::CallModel { .. }
    ));
    eng.submit_model_response(
        &handle,
        FfiModelResponse {
            assistant_text: Some("final text".into()),
            tool_calls: vec![],
            finish_reason: FfiFinishReason::Stop,
            provider_reasoning_content: None,
        },
    );

    match eng.next_action(&handle) {
        FfiAgentAction::CallModel { request } => {
            assert_eq!(
                "Pending work remains: verify failing tests",
                request.session_messages.last().unwrap().content,
            );
        }
        other => panic!("FB-R-010 expected FFI CallModel continuation, got {other:?}"),
    }
}

fn action_error_text(action: AgentAction) -> String {
    match action {
        AgentAction::Failed(error) => error.to_string(),
        other => panic!("expected failed action, got {other:?}"),
    }
}
