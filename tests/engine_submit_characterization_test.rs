//! Characterization tests for ENC-286 / FRG-47.
//!
//! Pins the observable event + action sequences of representative turns through
//! `submit_model_response` / `submit_tool_results` / `run_compaction_pass`
//! (via `next_action`) / `submit_summary` BEFORE the engine decomposition.
//! These tests must stay green (unchanged behavior) through every slice.

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionHandle, SessionIoParams};
use agent_core::events::AgentEvent;
use agent_core::message::{AgentMessage, AgentRole, MessageMeta, ToolResultRecord};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{FinishReason, ModelResponse, RawToolCall, SummaryResult};

fn io_large_window() -> SessionIoParams {
    SessionIoParams::new(
        "anthropic".into(),
        200_000,
        "[]".into(),
        "[]".into(),
        "[]".into(),
        "sys".into(),
    )
}

fn caps_no_summary() -> SessionCapabilities {
    SessionCapabilities {
        summarization_allowed: false,
        kv_fixed: false,
        tool_result_budget_tokens: None,
        ..Default::default()
    }
}

fn new_session(eng: &AgentEngine) -> SessionHandle {
    eng.create_session(
        vec![AgentMessage {
            id: "u1".into(),
            role: AgentRole::User,
            content: "hi".into(),
            metadata: MessageMeta::new(),
        }],
        AgentConfig::default(),
        caps_no_summary(),
        serde_json::Value::Null,
        io_large_window(),
    )
}

fn tool_call(id: &str) -> RawToolCall {
    RawToolCall {
        id: Some(id.into()),
        name: "web_search".into(),
        arguments_json: r#"{"query":"a"}"#.into(),
    }
}

fn tool_result(call_id: &str) -> ToolResultRecord {
    ToolResultRecord {
        call_id: call_id.into(),
        result_json: r#"{"results":[]}"#.into(),
        truncated: false,
        soft_failure: false,
        activated_tool_names: Vec::new(),
        result_metadata: None,
    }
}

fn event_names(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            AgentEvent::TurnStarted { .. } => "TurnStarted",
            AgentEvent::TurnCompleted { terminal, .. } => {
                if *terminal {
                    "TurnCompleted(terminal)"
                } else {
                    "TurnCompleted(open)"
                }
            }
            AgentEvent::ToolSoftFailure { .. } => "ToolSoftFailure",
            AgentEvent::HookInvoked { .. } => "HookInvoked",
            AgentEvent::PreCompact { .. } => "PreCompact",
            AgentEvent::ContextPressure { .. } => "ContextPressure",
            AgentEvent::CompactionApplied(_) => "CompactionApplied",
            AgentEvent::PostCompact { .. } => "PostCompact",
            AgentEvent::CompactionBoundary(_) => "CompactionBoundary",
            AgentEvent::KvCacheInvalidated { .. } => "KvCacheInvalidated",
            AgentEvent::VisibleAssistantText { .. } => "VisibleAssistantText",
            AgentEvent::ModelApiRetryScheduled { .. } => "ModelApiRetryScheduled",
            AgentEvent::ToolRepaired { .. } => "ToolRepaired",
            AgentEvent::ToolPolicyDenied { .. } => "ToolPolicyDenied",
            AgentEvent::ToolTimedOut { .. } => "ToolTimedOut",
        })
        .map(str::to_string)
        .collect()
}

#[test]
fn char_tool_turn_full_event_sequence() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);

    // next_action drives step_idle -> Preflight compaction (no-op) + CallModel.
    assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
    assert_eq!(
        event_names(&eng.drain_events(&h)),
        vec!["TurnStarted".to_string()]
    );

    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: Some("looking it up".into()),
            tool_calls: vec![tool_call("c1")],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    assert!(eng.drain_events(&h).is_empty());
    match eng.next_action(&h) {
        AgentAction::ExecuteTools(v) => assert_eq!(v.len(), 1),
        o => panic!("{o:?}"),
    }

    eng.submit_tool_results(&h, vec![tool_result("c1")]);
    assert_eq!(
        event_names(&eng.drain_events(&h)),
        vec!["TurnCompleted(open)".to_string()]
    );

    // Turn continues: next model call.
    assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
    assert_eq!(
        event_names(&eng.drain_events(&h)),
        vec!["TurnStarted".to_string()]
    );

    let snap = h.integration_test_snapshot();
    let roles: Vec<_> = snap.messages.iter().map(|m| m.role.clone()).collect();
    assert_eq!(
        roles,
        vec![AgentRole::User, AgentRole::Assistant, AgentRole::Tool,]
    );
}

#[test]
fn char_text_stop_finishes_terminal() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);

    assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
    eng.drain_events(&h);

    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: Some("done".into()),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            provider_reasoning_content: None,
        },
    );
    assert_eq!(
        event_names(&eng.drain_events(&h)),
        vec!["TurnCompleted(terminal)".to_string()]
    );
    assert!(matches!(eng.next_action(&h), AgentAction::Finished));
    assert!(eng.drain_events(&h).is_empty());
}

#[test]
fn char_reject_only_turn_returns_to_model() {
    let eng = AgentEngine::new();
    // Seed a non-empty active tool set so the discovery guard rejects unknown tools.
    let io = SessionIoParams::new(
        "anthropic".into(),
        200_000,
        "[]".into(),
        "[]".into(),
        r#"[{"name":"web_search"}]"#.into(),
        "sys".into(),
    );
    let h = eng.create_session(
        vec![AgentMessage {
            id: "u1".into(),
            role: AgentRole::User,
            content: "hi".into(),
            metadata: MessageMeta::new(),
        }],
        AgentConfig::default(),
        caps_no_summary(),
        serde_json::Value::Null,
        io,
    );

    assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
    eng.drain_events(&h);

    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("c9".into()),
                name: "no_such_tool".into(),
                arguments_json: "{}".into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    // Reject-only synthetic turn: soft failure recorded, no ExecuteTools.
    assert_eq!(
        event_names(&eng.drain_events(&h)),
        vec!["ToolSoftFailure".to_string()]
    );
    // Engine is Idle again: next action re-enters the model round-trip.
    assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));

    let snap = h.integration_test_snapshot();
    assert!(
        snap.messages
            .iter()
            .any(|m| m.role == AgentRole::Tool && m.content.contains("no_such_tool")),
        "reject error message must be visible for provider replay"
    );
}

#[test]
fn char_submit_summary_wrong_phase_fails_terminal() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);

    assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
    eng.submit_summary(
        &h,
        SummaryResult {
            summary_text: "s".into(),
            last_message_id: "u1".into(),
        },
    );
    match eng.next_action(&h) {
        AgentAction::Failed(_) => {}
        o => panic!("{o:?}"),
    }
}
