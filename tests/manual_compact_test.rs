//! TDD Red — manual compact tests (S18, S24, E9).

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::{AgentEvent, CompactionTrigger};
use agent_core::manual_compact::ManualCompactRequest;
use agent_core::message::{AgentMessage, AgentRole, MessageMeta};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{ModelResponse, SummaryResult};

fn user_msg(id: &str, body: &str) -> AgentMessage {
    AgentMessage {
        id: id.into(),
        role: AgentRole::User,
        content: body.into(),
        metadata: MessageMeta::new(),
    }
}

#[test]
fn s18_manual_compact_uses_focus_instruction_and_emits_summary_request() {
    let cfg = AgentConfig {
        enable_manual_compact: true,
        ..Default::default()
    };
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let eng = AgentEngine::new();
    let handle = eng.create_session(
        vec![
            user_msg("u0", "first"),
            AgentMessage {
                id: "a0".into(),
                role: AgentRole::Assistant,
                content: "ack".into(),
                metadata: MessageMeta::new(),
            },
            user_msg("u1", "tail"),
        ],
        cfg,
        caps,
        serde_json::Value::Null,
        SessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tool_catalog_json: "[]".into(),
            tool_schema_registry_json: "[]".into(),
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        },
    );

    let action = eng.request_manual_compact(
        &handle,
        ManualCompactRequest {
            focus_instruction: Some("preserve tool decisions".into()),
            force_summary: true,
        },
    );
    match action {
        AgentAction::RequestSummary(req) => {
            assert_eq!(req.trigger, CompactionTrigger::Manual);
            assert_eq!(
                req.focus_instruction.as_deref(),
                Some("preserve tool decisions")
            );
        }
        other => panic!("expected RequestSummary, got {other:?}"),
    }
}

#[test]
fn s24_pre_compact_and_post_compact_lifecycle_events_bracket_compaction() {
    let cfg = AgentConfig {
        threshold_ratio: 0.80,
        ..Default::default()
    };
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let big = "hello ".repeat(400);
    let eng = AgentEngine::new();
    let handle = eng.create_session(
        vec![
            AgentMessage {
                id: "u0".into(),
                role: AgentRole::User,
                content: big.clone(),
                metadata: MessageMeta::new(),
            },
            AgentMessage {
                id: "a0".into(),
                role: AgentRole::Assistant,
                content: big.clone(),
                metadata: MessageMeta::new(),
            },
            AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "tail".into(),
                metadata: MessageMeta::new(),
            },
        ],
        cfg,
        caps,
        serde_json::Value::Null,
        SessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 256,
            tool_catalog_json: "[]".into(),
            tool_schema_registry_json: "[]".into(),
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        },
    );
    // Should fire preflight summary tier.
    let _ = eng.next_action(&handle);
    eng.submit_summary(
        &handle,
        SummaryResult {
            summary_text: "SUMMARY".into(),
            last_message_id: "a0".into(),
        },
    );
    let events = eng.drain_events(&handle);

    let pre_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::PreCompact { .. }))
        .expect("PreCompact event expected");
    let applied_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::CompactionApplied(_)))
        .expect("CompactionApplied event expected");
    let post_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::PostCompact { .. }))
        .expect("PostCompact event expected");
    assert!(
        pre_idx < applied_idx,
        "PreCompact must precede CompactionApplied"
    );
    assert!(
        applied_idx < post_idx,
        "PostCompact must follow CompactionApplied"
    );
    match &events[post_idx] {
        AgentEvent::PostCompact {
            summary_ref,
            history_ref,
            ..
        } => {
            assert_eq!(summary_ref.as_deref(), Some("compact-summary-a0"));
            assert_eq!(history_ref.as_deref(), Some("history-u0-a0"));
        }
        _ => unreachable!(),
    }
}

#[test]
fn e9_manual_compact_disabled_by_policy_returns_no_op() {
    let cfg = AgentConfig {
        enable_manual_compact: false,
        ..Default::default()
    };
    let eng = AgentEngine::new();
    let handle = eng.create_session(
        vec![],
        cfg,
        SessionCapabilities::default(),
        serde_json::Value::Null,
        SessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tool_catalog_json: "[]".into(),
            tool_schema_registry_json: "[]".into(),
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        },
    );
    let action = eng.request_manual_compact(
        &handle,
        ManualCompactRequest {
            focus_instruction: Some("anything".into()),
            force_summary: true,
        },
    );
    match action {
        AgentAction::ManualCompactCompleted(result) => {
            assert!(result.summary_ref.is_none());
            assert!(result.history_ref.is_none());
        }
        other => panic!("expected ManualCompactCompleted, got {other:?}"),
    }
    let pre_count = eng
        .drain_events(&handle)
        .iter()
        .filter(|e| matches!(e, AgentEvent::PreCompact { .. }))
        .count();
    assert_eq!(pre_count, 0);
}

/// Manual compact requested while a model turn is in flight must not clobber
/// the AwaitingModel phase: the pending submit_model_response would otherwise
/// hit InvalidTransition after the summary round-trip rewrote the phase.
#[test]
fn manual_compact_mid_model_turn_is_deferred_as_noop() {
    let cfg = AgentConfig {
        enable_manual_compact: true,
        ..Default::default()
    };
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let eng = AgentEngine::new();
    let handle = eng.create_session(
        vec![
            user_msg("u0", &"long context ".repeat(64)),
            AgentMessage {
                id: "a0".into(),
                role: AgentRole::Assistant,
                content: "ack".repeat(64),
                metadata: MessageMeta::new(),
            },
            user_msg("u1", "tail"),
        ],
        cfg,
        caps,
        serde_json::Value::Null,
        SessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tool_catalog_json: "[]".into(),
            tool_schema_registry_json: "[]".into(),
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        },
    );

    // Engine is now AwaitingModel.
    assert!(matches!(
        eng.next_action(&handle),
        AgentAction::CallModel(_)
    ));

    let action = eng.request_manual_compact(
        &handle,
        ManualCompactRequest {
            focus_instruction: None,
            force_summary: true,
        },
    );
    assert!(
        matches!(action, AgentAction::ManualCompactCompleted(_)),
        "mid-turn manual compact must be a no-op, got {action:?}"
    );

    // The in-flight model turn must still complete normally.
    eng.submit_model_response(&handle, ModelResponse::text_only("done"));
    let next = eng.next_action(&handle);
    assert!(
        matches!(next, AgentAction::Finished),
        "model turn must finish after deferred manual compact, got {next:?}"
    );
}
