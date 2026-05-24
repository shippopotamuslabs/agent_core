//! TDD Red — context store tests (S8, S9, S10, S23, E4, E5).

use std::sync::Arc;

use agent_core::compact::fifo::FifoCompactionTier;
use agent_core::compact::history_snapshot::snapshot_history_range;
use agent_core::compact::tool_spill::spill_tool_result_if_needed;
use agent_core::compact::{
    CompactionContext, CompactionOutcome, CompactionTier as CompactionTierTrait,
};
use agent_core::context_store::{ContextReferenceKind, ContextStore};
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::{AgentError, CompactionTrigger};
use agent_core::host::{AgentToolExecutor, AgentTransport, NoOpSummarizer};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::session::{AgentConfig, AgentSession, SessionCapabilities};
use agent_core::turn_io::{ModelRequest, ModelResponse};

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
    assert!(
        result.result_json.contains("context_ref"),
        "stored tool message content must contain 'context_ref' marker, got {}",
        result.result_json
    );
}

#[test]
fn s9_raw_spilled_tool_output_can_be_read_by_reference_id() {
    // Drive the spill through the engine so the engine-owned ContextStore
    // is the one that holds the entry. Use a tiny spill threshold so any
    // realistic tool result triggers a spill.
    let mut cfg = AgentConfig::default();
    cfg.tool_spill_threshold_bytes = 10;

    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![AgentMessage {
            id: "u0".into(),
            role: AgentRole::User,
            content: "hi".into(),
            metadata: serde_json::Value::Null,
        }],
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
    let msgs = vec![
        AgentMessage {
            id: "u0".into(),
            role: AgentRole::User,
            content: "hi".into(),
            metadata: serde_json::Value::Null,
        },
        AgentMessage {
            id: "a0".into(),
            role: AgentRole::Assistant,
            content: "decision: use ToolSpill before Micro".into(),
            metadata: serde_json::Value::Null,
        },
        AgentMessage {
            id: "u1".into(),
            role: AgentRole::User,
            content: "tail".into(),
            metadata: serde_json::Value::Null,
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
fn s23_terminal_output_references_share_retrieval_path() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![],
        AgentConfig::default(),
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
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![],
        AgentConfig::default(),
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
    let mut cfg = AgentConfig::default();
    cfg.require_snapshot_before_fifo = true;
    cfg.enable_context_store = false; // disables snapshot creation
    cfg.allow_fifo_on_preflight = true;

    let session = AgentSession::new(
        vec![
            AgentMessage {
                id: "u0".into(),
                role: AgentRole::User,
                content: "hello world ".repeat(200),
                metadata: serde_json::Value::Null,
            },
            AgentMessage {
                id: "a0".into(),
                role: AgentRole::Assistant,
                content: "ack ".repeat(200),
                metadata: serde_json::Value::Null,
            },
            AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "tail".into(),
                metadata: serde_json::Value::Null,
            },
        ],
        serde_json::Value::Null,
    );

    let ctx = CompactionContext {
        provider: "anthropic".into(),
        context_window_tokens: 256,
        tools_schema_json: "[]".into(),
        capabilities: SessionCapabilities {
            summarization_allowed: true,
            kv_fixed: false,
            ..Default::default()
        },
        trigger: CompactionTrigger::Preflight,
    };

    let outcome = FifoCompactionTier::default().apply(session, &cfg, &ctx);
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
