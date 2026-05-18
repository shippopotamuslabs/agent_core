//! TDD Red — compaction trigger tests (S1–S7, S16, E1, E2, E3, M1, M2, M3).

use std::sync::Arc;

use agent_core::action::AgentAction;
use agent_core::compact::fifo::FifoCompactionTier;
use agent_core::compact::summary::SummaryCompactionTier;
use agent_core::compact::tool_spill::spill_tool_result_if_needed;
use agent_core::compact::{
    CompactionContext, CompactionOutcome, CompactionTier as CompactionTierTrait,
};
use agent_core::context_store::ContextStore;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::{AgentError, AgentEvent, CompactionTier, CompactionTrigger};
use agent_core::host::{AgentToolExecutor, AgentTransport, NoOpSummarizer};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::session::{AgentConfig, AgentSession, SessionCapabilities};
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall, SummaryResult};

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

fn io_large() -> SessionIoParams {
    SessionIoParams {
        provider: "anthropic".into(),
        context_window_tokens: 200_000,
        tools_schema_json: "[]".into(),
        system_prompt: "sys".into(),
    }
}
fn io_small() -> SessionIoParams {
    SessionIoParams {
        provider: "anthropic".into(),
        context_window_tokens: 256,
        tools_schema_json: "[]".into(),
        system_prompt: "sys".into(),
    }
}
fn user(id: &str, content: &str) -> AgentMessage {
    AgentMessage {
        id: id.into(),
        role: AgentRole::User,
        content: content.into(),
        metadata: serde_json::Value::Null,
    }
}
fn asst(id: &str, content: &str) -> AgentMessage {
    AgentMessage {
        id: id.into(),
        role: AgentRole::Assistant,
        content: content.into(),
        metadata: serde_json::Value::Null,
    }
}
fn system(id: &str, content: &str) -> AgentMessage {
    AgentMessage {
        id: id.into(),
        role: AgentRole::System,
        content: content.into(),
        metadata: serde_json::Value::Null,
    }
}

#[test]
fn s1_preflight_below_threshold_skips_compaction() {
    let cfg = AgentConfig {
        threshold_ratio: 0.80,
        mid_turn_threshold_ratio: 0.90,
        post_tool_result_threshold_ratio: 0.85,
        ..AgentConfig::default()
    };
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![user("u1", "hi")],
        cfg,
        caps,
        serde_json::Value::Null,
        io_large(),
    );
    let action = eng.next_action(&handle);
    assert!(matches!(action, AgentAction::CallModel(_)));
    let events = eng.drain_events(&handle);
    let applied = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::CompactionApplied(_)))
        .count();
    let pressure = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ContextPressure { .. }))
        .count();
    assert_eq!(applied, 0);
    assert_eq!(pressure, 0);
}

#[test]
fn s2_preflight_summary_request_carries_trigger_metadata() {
    let mut cfg = AgentConfig::default();
    cfg.threshold_ratio = 0.80;
    cfg.allow_fifo_on_mid_turn = false;
    cfg.allow_fifo_on_post_tool_result = false;
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let big = "hello ".repeat(400);
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![user("u0", &big), asst("a0", &big), user("u1", "tail")],
        cfg,
        caps,
        serde_json::Value::Null,
        io_small(),
    );
    match eng.next_action(&handle) {
        AgentAction::RequestSummary(req) => {
            assert_eq!(req.trigger, CompactionTrigger::Preflight);
            assert_eq!(req.last_message_id, "a0");
            let ids: Vec<&str> = req
                .messages_to_summarize
                .iter()
                .map(|m| m.id.as_str())
                .collect();
            assert_eq!(ids, vec!["u0", "a0"]);
        }
        other => panic!("expected RequestSummary, got {other:?}"),
    }
}

#[test]
fn s3_mid_turn_summary_resumes_pending_tool_execution() {
    let mut cfg = AgentConfig::default();
    cfg.threshold_ratio = 10.0;
    cfg.mid_turn_threshold_ratio = 0.90;
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let big = "hello ".repeat(400);
    let handle = eng.create_session(
        vec![user("u0", &big), asst("a0", &big), user("u1", "tail")],
        cfg,
        caps,
        serde_json::Value::Null,
        io_small(),
    );
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("c1".into()),
                name: "web_search".into(),
                arguments_json: r#"{"query":"x"}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
        },
    );
    // engine should detect mid-turn pressure and request summary
    let next = eng.next_action(&handle);
    assert!(matches!(next, AgentAction::RequestSummary(_)));
    eng.submit_summary(
        &handle,
        SummaryResult {
            summary_text: "Summary: tool choice retained.".into(),
            last_message_id: "a1".into(),
        },
    );
    match eng.next_action(&handle) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(calls.len(), 1);
        }
        other => panic!("expected ExecuteTools, got {other:?}"),
    }
    let events = eng.drain_events(&handle);
    let mid = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::CompactionApplied(ev) if ev.trigger == CompactionTrigger::MidTurn => {
                Some(ev.clone())
            }
            _ => None,
        })
        .expect("mid_turn CompactionApplied expected");
    assert_eq!(mid.trigger, CompactionTrigger::MidTurn);
}

#[test]
fn s4_post_tool_result_compaction_runs_before_next_model_call() {
    let mut cfg = AgentConfig::default();
    cfg.post_tool_result_threshold_ratio = 0.85;
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![user("u0", "hi")],
        cfg,
        caps,
        serde_json::Value::Null,
        io_small(),
    );
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("c1".into()),
                name: "web_search".into(),
                arguments_json: "{}".into(),
            }],
            finish_reason: FinishReason::ToolUse,
        },
    );
    let _ = eng.next_action(&handle);
    let huge = "x".repeat(50_000);
    eng.submit_tool_results(
        &handle,
        vec![ToolResultRecord {
            call_id: "c1".into(),
            result_json: huge,
            truncated: false,
            soft_failure: false,
        }],
    );
    let events = eng.drain_events(&handle);
    let post_count = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::CompactionApplied(ev) if ev.trigger == CompactionTrigger::PostToolResult))
        .count();
    assert_eq!(post_count, 1);
}

#[test]
fn s5_artifact_squash_is_preferred_over_fifo() {
    let mut cfg = AgentConfig::default();
    cfg.post_tool_result_threshold_ratio = 0.85;
    cfg.allow_fifo_on_post_tool_result = false;
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![user("u0", "make artifact")],
        cfg,
        caps,
        serde_json::Value::Null,
        io_small(),
    );
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: Some("gamma".into()),
            tool_calls: vec![RawToolCall {
                id: Some("c1".into()),
                name: "append_blocks".into(),
                arguments_json: r#"{"artifactId":"art-1","blocks":[]}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
        },
    );
    let _ = eng.next_action(&handle);
    let big_payload = format!(
        r#"{{"status":"success","totalBlocks":3,"_bulk":"{}"}}"#,
        "x".repeat(40_000)
    );
    eng.submit_tool_results(
        &handle,
        vec![ToolResultRecord {
            call_id: "c1".into(),
            result_json: big_payload,
            truncated: false,
            soft_failure: false,
        }],
    );
    let events = eng.drain_events(&handle);
    let artifact_compact = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::CompactionApplied(ev) if ev.tier == CompactionTier::ArtifactSquash => {
                Some(ev.clone())
            }
            _ => None,
        })
        .expect("artifact squash CompactionApplied expected");
    assert_eq!(artifact_compact.tier, CompactionTier::ArtifactSquash);
    // Inserted synthetic system message check would require reading session.
    // The spec text is exact; assert via the engine session messages.
    let inner_msgs = {
        // We use a synthetic check via next ModelRequest content.
        match eng.next_action(&handle) {
            AgentAction::CallModel(req) => req.session_messages,
            other => panic!("expected CallModel after squash, got {other:?}"),
        }
    };
    let synthetic = inner_msgs
        .iter()
        .find(|m| {
            matches!(m.role, AgentRole::System)
                && m.content.contains("Artifact generation in progress")
        })
        .expect("synthetic artifact system message expected");
    assert_eq!(
        synthetic.content,
        "Artifact generation in progress. 3 blocks written so far. Last block: `gamma`. Continue appending."
    );
}

#[test]
fn s6_kv_fixed_sessions_stay_on_synchronous_compaction() {
    let cfg = AgentConfig::default();
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: true,
        context_policy_tier: agent_core::policy::ContextPolicyTier::FixedKv,
        history_rewrite_supported: true,
        ..Default::default()
    };
    let big = "hello ".repeat(400);
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![user("u0", &big), asst("a0", &big), user("u1", "tail")],
        cfg,
        caps,
        serde_json::Value::Null,
        io_small(),
    );
    let action = eng.next_action(&handle);
    assert!(!matches!(action, AgentAction::RequestSummary(_)));
    let events = eng.drain_events(&handle);
    let fifo_compact = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::CompactionApplied(ev) if ev.tier == CompactionTier::Fifo => {
                Some(ev.clone())
            }
            _ => None,
        })
        .expect("fifo CompactionApplied expected");
    assert_eq!(fifo_compact.tier, CompactionTier::Fifo);
    let invalidated = events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::KvCacheInvalidated { reason } if reason == "Compacted: fifo"
        )
    });
    assert!(
        invalidated,
        "expected KvCacheInvalidated(reason='Compacted: fifo')"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::PostCompact {
                trigger: CompactionTrigger::Preflight,
                ..
            }
        )),
        "synchronous fifo compaction must emit PostCompact"
    );
}

#[test]
fn s7_summary_request_includes_history_reference_and_ledger_snapshot() {
    let mut cfg = AgentConfig::default();
    cfg.enable_context_store = true;
    cfg.enable_state_ledger = true;
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let big = "hello ".repeat(400);
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![
            user("u0", "Objective: Implement context manager"),
            asst("a0", &big),
            user("u1", "tail"),
        ],
        cfg,
        caps,
        serde_json::Value::Null,
        io_small(),
    );
    match eng.next_action(&handle) {
        AgentAction::RequestSummary(req) => {
            let h = req.history_ref.as_deref().expect("history_ref required");
            assert!(
                h.starts_with("history-"),
                "history_ref must start with 'history-', got {h}"
            );
            let ledger = req
                .state_ledger_snapshot
                .as_ref()
                .expect("state_ledger_snapshot required");
            assert_eq!(
                ledger.objective.as_deref(),
                Some("Implement context manager")
            );
            assert_eq!(req.trigger, CompactionTrigger::Preflight);
        }
        other => panic!("expected RequestSummary, got {other:?}"),
    }
}

#[test]
fn s16_fifo_requires_history_snapshot_before_destructive_deletion() {
    let mut cfg = AgentConfig::default();
    cfg.require_snapshot_before_fifo = true;
    cfg.allow_fifo_on_preflight = true;
    let caps = SessionCapabilities {
        summarization_allowed: false,
        kv_fixed: false,
        ..Default::default()
    };
    let big = "hello ".repeat(400);
    let mut session = AgentSession::new(
        vec![user("u0", &big), asst("a0", &big), user("u1", "tail")],
        serde_json::Value::Null,
    );
    let ctx = CompactionContext {
        provider: "anthropic".into(),
        context_window_tokens: 256,
        tools_schema_json: "[]".into(),
        capabilities: caps.clone(),
        trigger: CompactionTrigger::Preflight,
    };
    // The production path that creates a pre-FIFO history snapshot is
    // `snapshot_history_range`. Drive it against the same `store` used for
    // the assertion so both ends share the same instance, then run FIFO.
    // Both calls hit `todo!()` today; the test fails red at the first
    // unimplemented site instead of silently passing on an empty store.
    let mut store = ContextStore::default();
    let _snapshot_ref = agent_core::compact::history_snapshot::snapshot_history_range(
        &mut store,
        &session.messages[..2],
        CompactionTrigger::Preflight,
        0,
    )
    .expect("snapshot expected before FIFO drops history");
    session.session_context = serde_json::json!({
        "history_snapshot_ids": ["u0", "a0"]
    });

    let outcome = FifoCompactionTier::default().apply(session, &cfg, &ctx);
    match outcome {
        CompactionOutcome::Applied(new_session, _ev) => {
            let snaps = store.search("u0", 10);
            assert_eq!(snaps.len(), 1);
            assert_eq!(
                snaps[0].kind,
                agent_core::context_store::ContextReferenceKind::HistorySnapshot
            );
            let _ = new_session;
        }
        other => panic!("expected Applied with snapshot, got {other:?}"),
    }
}

#[test]
fn e1_summary_tier_is_not_applicable_for_protected_tail_only() {
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let session = AgentSession::new(
        vec![system("sys0", "you are helpful"), user("u_tail", "tail")],
        serde_json::Value::Null,
    );
    let ctx = CompactionContext {
        provider: "anthropic".into(),
        context_window_tokens: 200_000,
        tools_schema_json: "[]".into(),
        capabilities: caps,
        trigger: CompactionTrigger::Preflight,
    };
    let outcome = SummaryCompactionTier::default().apply(session, &AgentConfig::default(), &ctx);
    assert!(matches!(outcome, CompactionOutcome::NotApplicable));
}

#[test]
fn e2_submit_summary_rejects_a_mismatched_last_message_id() {
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let big = "hello ".repeat(400);
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![user("u0", &big), asst("a0", &big), user("u1", "tail")],
        AgentConfig::default(),
        caps,
        serde_json::Value::Null,
        io_small(),
    );
    let _ = eng.next_action(&handle); // expected RequestSummary
    eng.submit_summary(
        &handle,
        SummaryResult {
            summary_text: "bad".into(),
            last_message_id: "WRONG".into(),
        },
    );
    let terminal = eng.next_action(&handle);
    match terminal {
        AgentAction::Failed(AgentError::CompactionFailed { tier, reason }) => {
            assert_eq!(tier, "summary");
            assert_eq!(reason, "last_message_id mismatch");
        }
        other => panic!("expected Failed(CompactionFailed), got {other:?}"),
    }
}

#[test]
fn submit_model_response_while_awaiting_summary_is_invalid_transition() {
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let big = "hello ".repeat(400);
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![user("u0", &big), asst("a0", &big), user("u1", "tail")],
        AgentConfig::default(),
        caps,
        serde_json::Value::Null,
        io_small(),
    );

    assert!(matches!(
        eng.next_action(&handle),
        AgentAction::RequestSummary(_)
    ));
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: Some("should be rejected".into()),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
        },
    );

    match eng.next_action(&handle) {
        AgentAction::Failed(AgentError::InvalidTransition { from, to }) => {
            assert_eq!(from, "AwaitingSummary");
            assert_eq!(to, "submit_model_response");
        }
        other => panic!("expected InvalidTransition, got {other:?}"),
    }
}

#[test]
fn e3_over_threshold_pressure_with_no_eligible_tier_fails_loudly() {
    let mut cfg = AgentConfig::default();
    cfg.mid_turn_threshold_ratio = 0.90;
    cfg.allow_fifo_on_mid_turn = false;
    let caps = SessionCapabilities {
        summarization_allowed: false,
        kv_fixed: false,
        ..Default::default()
    };
    let big = "hello ".repeat(400);
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![user("u0", &big), asst("a0", &big), user("u1", "tail")],
        cfg,
        caps,
        serde_json::Value::Null,
        io_small(),
    );
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("c1".into()),
                name: "web_search".into(),
                arguments_json: "{}".into(),
            }],
            finish_reason: FinishReason::ToolUse,
        },
    );
    // mid-turn compaction is supposed to run now
    let next = eng.next_action(&handle);
    match next {
        AgentAction::Failed(AgentError::CompactionFailed { tier, reason }) => {
            assert_eq!(tier, "pipeline");
            assert_eq!(
                reason,
                "No compaction tier applied while context remains over threshold"
            );
        }
        other => panic!("expected Failed(CompactionFailed), got {other:?}"),
    }
}

#[test]
fn fixed_kv_without_history_rewrite_support_does_not_fifo_through_engine() {
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: true,
        context_policy_tier: agent_core::policy::ContextPolicyTier::FixedKv,
        history_rewrite_supported: false,
        ..Default::default()
    };
    let big = "hello ".repeat(400);
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![user("u0", &big), asst("a0", &big), user("u1", "tail")],
        AgentConfig::default(),
        caps,
        serde_json::Value::Null,
        io_small(),
    );

    match eng.next_action(&handle) {
        AgentAction::Failed(AgentError::CompactionFailed { tier, reason }) => {
            assert_eq!(tier, "pipeline");
            assert_eq!(
                reason,
                "No compaction tier applied while context remains over threshold"
            );
        }
        other => panic!("expected fixed-kv compaction failure, got {other:?}"),
    }
    let events = eng.drain_events(&handle);
    assert!(
        !events.iter().any(
            |e| matches!(e, AgentEvent::CompactionApplied(ev) if ev.tier == CompactionTier::Fifo)
        ),
        "fifo must not run when fixed-kv cannot rebuild history"
    );
}

#[test]
fn m1_post_tool_result_trigger_must_fire_without_a_later_preflight() {
    let mut cfg = AgentConfig::default();
    cfg.threshold_ratio = 0.80;
    cfg.post_tool_result_threshold_ratio = 0.85;
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![user("u0", "small")],
        cfg,
        caps,
        serde_json::Value::Null,
        io_small(),
    );
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("c1".into()),
                name: "web_search".into(),
                arguments_json: "{}".into(),
            }],
            finish_reason: FinishReason::ToolUse,
        },
    );
    let _ = eng.next_action(&handle);
    let huge = "x".repeat(50_000);
    eng.submit_tool_results(
        &handle,
        vec![ToolResultRecord {
            call_id: "c1".into(),
            result_json: huge,
            truncated: false,
            soft_failure: false,
        }],
    );
    // Do NOT call next_action; drain immediately.
    let events = eng.drain_events(&handle);
    let post = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::CompactionApplied(ev) if ev.trigger == CompactionTrigger::PostToolResult))
        .count();
    let preflight = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::CompactionApplied(ev) if ev.trigger == CompactionTrigger::Preflight))
        .count();
    assert_eq!(post, 1);
    assert_eq!(preflight, 0);
}

#[test]
fn m2_fifo_before_summary_ordering_is_rejected() {
    // Constructed so summary alone reduces pressure below 0.80; FIFO would erase
    // the "decision: keep fact" message. Expect summary tier first.
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        ..Default::default()
    };
    let big = "hello ".repeat(400);
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![
            user("u0", &big),
            asst("a0", "decision: keep fact"),
            user("u1", "tail"),
        ],
        AgentConfig::default(),
        caps,
        serde_json::Value::Null,
        io_small(),
    );
    let _ = eng.next_action(&handle); // RequestSummary
    eng.submit_summary(
        &handle,
        SummaryResult {
            summary_text: "decision: keep fact preserved".into(),
            last_message_id: "a0".into(),
        },
    );
    let events = eng.drain_events(&handle);
    let destructive_tiers: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::CompactionApplied(ev)
                if matches!(ev.tier, CompactionTier::Summary | CompactionTier::Fifo) =>
            {
                Some(ev.tier.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(destructive_tiers.first(), Some(&CompactionTier::Summary));

    // History snapshot must still preserve the verbatim decision content.
    let refs = eng.search_context_references(&handle, "decision: keep fact".into(), 5);
    assert!(
        refs.iter()
            .any(|r| r.kind == agent_core::context_store::ContextReferenceKind::HistorySnapshot),
        "expected history_snapshot containing 'decision: keep fact'"
    );
}

#[test]
fn m3_tool_spillover_must_preserve_full_raw_output() {
    let mut store = ContextStore::default();
    let call = ToolCallRecord {
        call_id: "call-9".into(),
        name: "web_search".into(),
        arguments_json: "{}".into(),
        repair_metadata: None,
    };
    let full = "BEGIN_RAW_abcdefghijklmnopqrstuvwxyz_END_RAW".to_string();
    let mut result = ToolResultRecord {
        call_id: "call-9".into(),
        result_json: full.clone(),
        truncated: false,
        soft_failure: false,
    };
    let reference =
        spill_tool_result_if_needed(&mut store, &mut result, &call, 10, 0).expect("spill expected");
    let entry = store
        .get(&reference.id)
        .expect("stored entry must be retrievable");
    assert_eq!(entry.content, full);
}
