//! TDD Red — context store tests (S8, S9, S10, S23, E4, E5).

use agent_core::action::AgentAction;
use agent_core::compact::fifo::FifoCompactionTier;
use agent_core::compact::history_snapshot::snapshot_history_range;
use agent_core::compact::tool_spill::{spill_tool_result_if_needed, DEFAULT_TOOL_OUTPUT_TTL_TURNS};
use agent_core::compact::{
    CompactionContext, CompactionOutcome, CompactionTier as CompactionTierTrait,
};
use agent_core::context_store::{is_browsable_context_kind, ContextReferenceKind, ContextStore};
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::{AgentError, CompactionTrigger};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta, ToolCallRecord, ToolResultRecord};
use agent_core::session::{AgentConfig, AgentSession, SessionCapabilities};

#[test]
fn s8_oversized_tool_output_spills_to_context_reference() {
    let mut store = ContextStore::default();
    let payload = "x".repeat(200);
    let call = ToolCallRecord {
        call_id: "call-1".into(),
        name: "web_search".into(),
        arguments_json: "{}".into(),
        attempt_metadata: None,
        repair_metadata: None,
    };
    let mut result = ToolResultRecord {
        call_id: "call-1".into(),
        result_json: payload.clone(),
        truncated: false,
        soft_failure: false,
        activated_tool_names: Vec::new(),
        result_metadata: None,
    };
    let reference = spill_tool_result_if_needed(&mut store, &mut result, &call, 64, 0)
        .expect("spill expected for oversized output");
    assert_eq!(reference.kind, ContextReferenceKind::ToolOutput);
    assert_eq!(reference.byte_len, 200);
    assert!(reference.title.contains("@turn:0"));
    assert_eq!(
        store
            .get("tool-output-call-1")
            .expect("spilled entry")
            .expires_after_turn,
        Some(DEFAULT_TOOL_OUTPUT_TTL_TURNS)
    );
    assert!(
        result.result_json.contains("context_ref"),
        "stored tool message content must contain 'context_ref' marker, got {}",
        result.result_json
    );
}

#[test]
fn tool_output_ttl_expires_after_turn_boundary() {
    let mut store = ContextStore::default();
    let call = ToolCallRecord {
        call_id: "call-ttl".into(),
        name: "web_search".into(),
        arguments_json: "{}".into(),
        attempt_metadata: None,
        repair_metadata: None,
    };
    let mut result = ToolResultRecord {
        call_id: "call-ttl".into(),
        result_json: "x".repeat(200),
        truncated: false,
        soft_failure: false,
        activated_tool_names: Vec::new(),
        result_metadata: None,
    };
    spill_tool_result_if_needed(&mut store, &mut result, &call, 64, 1);
    assert!(store.get("tool-output-call-ttl").is_some());
    let removed = store.expire_after_turn(3);
    assert_eq!(removed, vec!["tool-output-call-ttl".to_string()]);
    assert!(store.get("tool-output-call-ttl").is_none());
}

#[test]
fn duplicate_spill_content_dedupes_to_newest_reference() {
    let mut store = ContextStore::default();
    let call_a = ToolCallRecord {
        call_id: "call-a".into(),
        name: "web_search".into(),
        arguments_json: "{}".into(),
        attempt_metadata: None,
        repair_metadata: None,
    };
    let call_b = ToolCallRecord {
        call_id: "call-b".into(),
        name: "web_search".into(),
        arguments_json: "{}".into(),
        attempt_metadata: None,
        repair_metadata: None,
    };
    let payload = "IDENTICAL_SPILL_PAYLOAD".repeat(10);
    let mut result_a = ToolResultRecord {
        call_id: "call-a".into(),
        result_json: payload.clone(),
        truncated: false,
        soft_failure: false,
        activated_tool_names: Vec::new(),
        result_metadata: None,
    };
    let mut result_b = ToolResultRecord {
        call_id: "call-b".into(),
        result_json: payload,
        truncated: false,
        soft_failure: false,
        activated_tool_names: Vec::new(),
        result_metadata: None,
    };
    spill_tool_result_if_needed(&mut store, &mut result_a, &call_a, 64, 0);
    spill_tool_result_if_needed(&mut store, &mut result_b, &call_b, 64, 0);
    assert!(store.get("tool-output-call-a").is_none());
    assert!(store.get("tool-output-call-b").is_some());
    assert_eq!(store.len(), 1);
}

#[test]
fn s9_raw_spilled_tool_output_can_be_read_by_reference_id() {
    // Drive the spill through the engine so the engine-owned ContextStore
    // is the one that holds the entry. Use a tiny spill threshold so any
    // realistic tool result triggers a spill.
    let cfg = AgentConfig {
        tool_spill_threshold_bytes: 10,
        ..Default::default()
    };

    let eng = AgentEngine::new();
    let handle = eng.create_session(
        vec![AgentMessage {
            id: "u0".into(),
            role: AgentRole::User,
            content: "hi".into(),
            metadata: MessageMeta::new(),
        }],
        cfg,
        SessionCapabilities::default(),
        serde_json::Value::Null,
        SessionIoParams::new(
            "anthropic".into(),
            200_000,
            "[]".into(),
            "[]".into(),
            "[]".into(),
            "sys".into(),
        ),
    );

    // Advance to the awaiting-tools phase so we can submit a tool result.
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        agent_core::turn_io::ModelResponse {
            assistant_text: None,
            tool_calls: vec![agent_core::turn_io::RawToolCall {
                id: Some("call-1".into()),
                name: "web_search".into(),
                arguments_json: "{}".into(),
            }],
            finish_reason: agent_core::turn_io::FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    let _ = eng.next_action(&handle);

    let payload = "RAW_PAYLOAD_123_OVER_THRESHOLD".to_string();
    eng.submit_tool_results(
        &handle,
        vec![ToolResultRecord {
            call_id: "call-1".into(),
            result_json: payload.clone(),
            truncated: false,
            soft_failure: false,
            activated_tool_names: Vec::new(),
            result_metadata: None,
        }],
    );

    // After spill, the stored tool message content should contain a
    // `context_ref` marker pointing at a reference id in the engine-owned
    // store. We can't introspect the session directly here; instead the
    // spec says `submit_tool_results` spills oversized tool results and
    // registers them under a reference id keyed on the call_id. We probe
    // by reading the conventional id.
    let raw = eng
        .read_context_reference(&handle, "tool-output-call-1".into())
        .expect("entry should be readable through the engine-owned store");
    assert!(
        raw.contains("RAW_PAYLOAD_123"),
        "expected stored payload to contain original content, got {raw}"
    );
}

#[test]
fn s10_summarized_history_range_remains_searchable() {
    // Setup: session with a key decision in message a0.
    let msgs = [
        AgentMessage {
            id: "u0".into(),
            role: AgentRole::User,
            content: "hi".into(),
            metadata: MessageMeta::new(),
        },
        AgentMessage {
            id: "a0".into(),
            role: AgentRole::Assistant,
            content: "decision: use ToolSpill before Micro".into(),
            metadata: MessageMeta::new(),
        },
        AgentMessage {
            id: "u1".into(),
            role: AgentRole::User,
            content: "tail".into(),
            metadata: MessageMeta::new(),
        },
    ];
    let mut store = ContextStore::default();
    let _ref = snapshot_history_range(&mut store, &msgs[..2], CompactionTrigger::Preflight, 0)
        .expect("snapshot expected");
    let hits = store.search("ToolSpill", 5);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, ContextReferenceKind::HistorySnapshot);
}

#[test]
fn browsable_context_references_exclude_infrastructure_kinds() {
    let cfg = AgentConfig {
        enable_tool_schema_deferral: true,
        ..Default::default()
    };
    let caps = SessionCapabilities {
        tool_schema_deferral_supported: true,
        ..Default::default()
    };
    let eng = AgentEngine::new();
    let handle = eng.create_session(
        vec![AgentMessage {
            id: "u1".into(),
            role: AgentRole::User,
            content: "hi".into(),
            metadata: MessageMeta::new(),
        }],
        cfg,
        caps,
        serde_json::Value::Null,
        SessionIoParams::new(
            "anthropic".into(),
            200_000,
            "[]".into(),
            "[]".into(),
            r#"[{"name":"web_search"}]"#.into(),
            "sys".into(),
        ),
    );

    let req = match eng.next_action(&handle) {
        AgentAction::CallModel(req) => req,
        other => panic!("expected CallModel, got {other:?}"),
    };
    assert!(
        req.context_references
            .iter()
            .all(|reference| is_browsable_context_kind(&reference.kind)),
        "model-facing refs must be browsable only: {:?}",
        req.context_references
    );
    assert!(
        !req.context_references
            .iter()
            .any(|reference| reference.kind == ContextReferenceKind::ToolSchema),
        "ToolSchema must not appear in browsable sidecar"
    );
    assert!(
        !req.context_references
            .iter()
            .any(|reference| reference.kind == ContextReferenceKind::ToolCatalogIndex),
        "ToolCatalogIndex must not appear in browsable sidecar"
    );
}

#[test]
fn s23_terminal_output_references_share_retrieval_path() {
    let eng = AgentEngine::new();
    let handle = eng.create_session(
        vec![],
        AgentConfig::default(),
        SessionCapabilities::default(),
        serde_json::Value::Null,
        SessionIoParams::new(
            "anthropic".into(),
            200_000,
            "[]".into(),
            "[]".into(),
            "[]".into(),
            "sys".into(),
        ),
    );
    let reference = eng.insert_terminal_output(
        &handle,
        "terminal-1".into(),
        "terminal:gradle".into(),
        "RAW_TERMINAL_OUTPUT_123".into(),
    );

    assert_eq!(reference.kind, ContextReferenceKind::TerminalOutput);
    let raw = eng
        .read_context_reference(&handle, "terminal-1".into())
        .expect("terminal output must be readable through engine");
    assert_eq!(raw, "RAW_TERMINAL_OUTPUT_123");
    assert_eq!(
        eng.search_context_references(&handle, "TERMINAL_OUTPUT".into(), 5)[0].id,
        "terminal-1"
    );
}

#[test]
fn e4_missing_context_reference_read_returns_not_found() {
    let eng = AgentEngine::new();
    let handle = eng.create_session(
        vec![],
        AgentConfig::default(),
        SessionCapabilities::default(),
        serde_json::Value::Null,
        SessionIoParams::new(
            "anthropic".into(),
            200_000,
            "[]".into(),
            "[]".into(),
            "[]".into(),
            "sys".into(),
        ),
    );
    let err = eng
        .read_context_reference(&handle, "missing-ref".into())
        .expect_err("missing reference should error");
    match err {
        AgentError::ContextReferenceNotFound { reference_id } => {
            assert_eq!(reference_id, "missing-ref");
        }
        other => panic!("expected ContextReferenceNotFound, got {other:?}"),
    }
}

#[test]
fn e5_fifo_fails_when_required_snapshot_cannot_be_created() {
    let cfg = AgentConfig {
        require_snapshot_before_fifo: true,
        enable_context_store: false, // disables snapshot creation
        allow_fifo_on_preflight: true,
        ..Default::default()
    };

    let session = AgentSession::new(
        vec![
            AgentMessage {
                id: "u0".into(),
                role: AgentRole::User,
                content: "hello world ".repeat(200),
                metadata: MessageMeta::new(),
            },
            AgentMessage {
                id: "a0".into(),
                role: AgentRole::Assistant,
                content: "ack ".repeat(200),
                metadata: MessageMeta::new(),
            },
            AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "tail".into(),
                metadata: MessageMeta::new(),
            },
        ],
        serde_json::Value::Null,
    );

    let ctx = CompactionContext {
        provider: "anthropic".into(),
        context_window_tokens: 256,
        tools_schema_json: "[]".into(),
        system_prompt: String::new(),
        context_sidecars_estimate: String::new(),
        capabilities: SessionCapabilities {
            summarization_allowed: true,
            kv_fixed: false,
            ..Default::default()
        },
        trigger: CompactionTrigger::Preflight,
        suppress_summary_tier: false,
    };

    let outcome = FifoCompactionTier.apply(session, &cfg, &ctx);
    // Spec: outcome is CompactionFailed with tier="fifo",
    // reason="history snapshot required before fifo".
    assert!(
        matches!(
            outcome,
            CompactionOutcome::Failed(AgentError::CompactionFailed { ref tier, ref reason })
            if tier == "fifo" && reason == "history snapshot required before fifo"
        ),
        "expected CompactionFailed(fifo / history snapshot required), got {:?}",
        outcome
    );
}

/// Probe: a spilled tool-output reference must remain readable and searchable
/// across subsequent turn compaction passes (snapshot/spill) — compaction must
/// not garbage-collect store entries that retrieval hints still point at.
#[test]
fn spilled_reference_remains_readable_across_turn_compaction_passes() {
    let cfg = AgentConfig {
        tool_spill_threshold_bytes: 10,
        allow_fifo_on_preflight: true,
        ..Default::default()
    };

    let filler = "history ".repeat(220);
    let mut initial: Vec<AgentMessage> = Vec::new();
    for i in 0..6 {
        initial.push(AgentMessage {
            id: format!("u{i}"),
            role: AgentRole::User,
            content: filler.clone(),
            metadata: MessageMeta::new(),
        });
        initial.push(AgentMessage {
            id: format!("a{i}"),
            role: AgentRole::Assistant,
            content: filler.clone(),
            metadata: MessageMeta::new(),
        });
    }
    initial.push(AgentMessage {
        id: "u-tail".into(),
        role: AgentRole::User,
        content: "tail".into(),
        metadata: MessageMeta::new(),
    });

    let eng = AgentEngine::new();
    let handle = eng.create_session(
        initial,
        cfg,
        SessionCapabilities {
            summarization_allowed: false,
            ..Default::default()
        },
        serde_json::Value::Null,
        SessionIoParams::new(
            "anthropic".into(),
            200_000,
            "[]".into(),
            "[]".into(),
            "[]".into(),
            "sys".into(),
        ),
    );

    // First turn: spill a tool result into the store.
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        agent_core::turn_io::ModelResponse {
            assistant_text: None,
            tool_calls: vec![agent_core::turn_io::RawToolCall {
                id: Some("call-1".into()),
                name: "web_search".into(),
                arguments_json: "{}".into(),
            }],
            finish_reason: agent_core::turn_io::FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    let _ = eng.next_action(&handle);
    eng.submit_tool_results(
        &handle,
        vec![ToolResultRecord {
            call_id: "call-1".into(),
            result_json: "SPILLED_PAYLOAD_FOR_FIFO_PROBE".into(),
            truncated: false,
            soft_failure: false,
            activated_tool_names: Vec::new(),
            result_metadata: None,
        }],
    );

    // Advance to the next turn so preflight compaction passes (history
    // snapshot, tool spill) run over the seeded history. The store contract is
    // what we assert: the reference stays readable regardless of how the
    // marker message was rewritten.
    let _ = eng.next_action(&handle);
    let raw = eng
        .read_context_reference(&handle, "tool-output-call-1".into())
        .expect("spilled reference must remain readable after compaction passes");
    assert!(raw.contains("SPILLED_PAYLOAD_FOR_FIFO_PROBE"));
    assert!(
        !eng.search_context_references(&handle, "web_search".into(), 5)
            .is_empty(),
        "spilled reference must remain searchable"
    );
}
