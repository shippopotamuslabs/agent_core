//! ENC-305 chunk 3 acceptance: the typed `SessionSnapshot` is the only
//! state crossing a process boundary.
//!
//! Round-trip tests are BEHAVIORAL (ENC-305 fix pass, M4): a control
//! session and a to-be-restored session are driven through the SAME
//! script; one is exported, JSON-round-tripped (simulated process
//! death: the source engine/handle are dropped), and imported into a
//! FRESH `AgentEngine` mid-script — and the FOLLOWING model request
//! built after identical post-import inputs must match the control's
//! (tools schema JSON, full state ledger, ledger delta, provider
//! context, advertised set), not merely replay the stored pending
//! request. A mid-turn test does the same at `AwaitingTools`. Scalar
//! asserts pin the round-tripped snapshot itself, an `active_json()`
//! pin holds the raw schema channel text (M2), and tamper tests pin
//! the import-time coherence gates (M1/M3/m5) at core and FFI level.
//! Import validation otherwise reuses construction's: a tampered
//! dangling semantics reference is rejected with
//! `UnknownToolReference`. The FFI tests (ffi-gated) pin the JSON-text
//! surface on `FfiAgentEngine`.

#[path = "support/enclave_test_semantics.rs"]
mod enclave;

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, EnginePhase, SessionIoParams};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta, ToolCallRecord, ToolResultRecord};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::session_snapshot::SNAPSHOT_VERSION;
use agent_core::tool_name::ToolName;
use agent_core::tool_semantics::{ToolArgumentRepairRule, ToolSemanticsError};
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall};
use agent_core::{SessionHandle, SessionSnapshot};
use serde_json::Value;

fn tn(name: &str) -> ToolName {
    ToolName::new(name).expect("test tool name must be valid")
}

/// Schema channel carrying two enclave-catalog tools, in this order
/// (mirrors `tests/enc304_universe_test.rs`).
const FIXTURE_SCHEMA_JSON: &str = r#"[
  {"name":"web_search","description":"Search the web","parameters":{}},
  {"name":"append_blocks","description":"Append blocks","parameters":{}}
]"#;

/// Same channel with deliberately non-canonical formatting: extra
/// whitespace and reversed key order. `entries_json` canonicalization
/// would render different bytes, so an `active_json()` pin over this
/// text actually bites (M4d).
const NONCANONICAL_SCHEMA_JSON: &str = r#"[ { "parameters" : { }, "name" : "web_search", "description" : "Search the web" },
  { "parameters" : { }, "description" : "Append blocks", "name" : "append_blocks" } ]"#;

fn create_fixture_session(eng: &AgentEngine) -> SessionHandle {
    create_fixture_session_with_schema(eng, FIXTURE_SCHEMA_JSON)
}

fn create_fixture_session_with_schema(eng: &AgentEngine, schema_json: &str) -> SessionHandle {
    eng.create_session(
        vec![AgentMessage {
            id: "u1".to_string(),
            role: AgentRole::User,
            content: "hi".to_string(),
            metadata: MessageMeta::new(),
        }],
        AgentConfig {
            tool_semantics: enclave::enclave_test_semantics(),
            ..AgentConfig::default()
        },
        SessionCapabilities::default(),
        Value::Null,
        SessionIoParams::new(
            "anthropic".to_string(),
            200_000,
            schema_json.to_string(),
            "sys".to_string(),
        )
        .expect("valid test io params"),
    )
    .expect("valid config creates session")
}

fn model_request(eng: &AgentEngine, handle: &SessionHandle) -> ModelRequest {
    match eng.next_action(handle) {
        AgentAction::CallModel(request) => request,
        other => panic!("expected CallModel, got {other:?}"),
    }
}

fn advertised_names(request: &ModelRequest) -> Vec<&str> {
    request
        .tool_schemas
        .iter()
        .map(|entry| entry.name.as_str())
        .collect()
}

fn submit_web_search_call(eng: &AgentEngine, handle: &SessionHandle, call_id: &str) {
    eng.submit_model_response(
        handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some(call_id.to_string()),
                name: "web_search".to_string(),
                arguments_json: r#"{"query":"tools"}"#.to_string(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
}

fn execute_batch(eng: &AgentEngine, handle: &SessionHandle) -> Vec<ToolCallRecord> {
    match eng.next_action(handle) {
        AgentAction::ExecuteTools(calls) => calls,
        other => panic!("expected ExecuteTools, got {other:?}"),
    }
}

fn submit_ok_result(
    eng: &AgentEngine,
    handle: &SessionHandle,
    call_id: &str,
    activated: Vec<ToolName>,
) {
    eng.submit_tool_results(
        handle,
        vec![ToolResultRecord {
            call_id: call_id.to_string(),
            result_json: serde_json::json!({ "status": "ok" }).to_string(),
            truncated: false,
            soft_failure: false,
            activated_tool_names: activated,
            result_metadata: None,
        }],
    );
}

fn json_round_trip(snapshot: &SessionSnapshot) -> SessionSnapshot {
    let json = serde_json::to_string(snapshot).expect("snapshot serializes");
    serde_json::from_str(&json).expect("snapshot deserializes")
}

/// The M4 comparison bundle for two model requests that must be
/// behaviorally identical.
fn assert_requests_match(control: &ModelRequest, restored: &ModelRequest) {
    assert_eq!(
        restored.tools_schema_json(),
        control.tools_schema_json(),
        "tools_schema_json must match"
    );
    assert_eq!(
        advertised_names(restored),
        advertised_names(control),
        "advertised tool set must match"
    );
    // Full state-ledger equality (serialized: StateLedger has no
    // PartialEq), not just presence.
    assert_eq!(
        serde_json::to_value(&restored.state_ledger).expect("ledger serializes"),
        serde_json::to_value(&control.state_ledger).expect("ledger serializes"),
        "state_ledger must match in full"
    );
    assert_eq!(
        restored.state_ledger_delta_json, control.state_ledger_delta_json,
        "state_ledger_delta_json must match"
    );
    // ProviderContextSnapshot is PartialEq: this covers its
    // generation-bearing fields (sidecar_generation, transcript_revision).
    assert_eq!(
        restored.provider_context, control.provider_context,
        "provider_context must match"
    );
}

// ---------------------------------------------------------------------------
// (a) Control-vs-restored: restart mid-script, then drive BOTH sessions
// through a further full tool turn and compare the FOLLOWING request.
// ---------------------------------------------------------------------------

#[test]
fn enc305_snapshot_control_vs_restored_following_request_matches() {
    let eng_control = AgentEngine::new();
    let control = create_fixture_session(&eng_control);
    let eng_source = AgentEngine::new();
    let source = create_fixture_session(&eng_source);

    // Identical pre-restart script on both sessions: a web_search tool
    // turn whose result activates a host-only tool, plus a
    // context-store insertion.
    for (eng, handle) in [(&eng_control, &control), (&eng_source, &source)] {
        let first = model_request(eng, handle);
        assert_eq!(
            advertised_names(&first),
            vec!["web_search", "append_blocks"]
        );
        submit_web_search_call(eng, handle, "call-1");
        let batch = execute_batch(eng, handle);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].call_id, "call-1");
        submit_ok_result(eng, handle, "call-1", vec![tn("host_only_tool")]);
        let reference = eng.insert_terminal_output(
            handle,
            "term-1".to_string(),
            "Terminal".to_string(),
            "terminal payload xyz".to_string(),
        );
        assert_eq!(reference.id, "term-1");
        // Build the next pending request on both, so the export happens
        // at AwaitingModel with the host-only tool advertised.
        let pending = model_request(eng, handle);
        assert_eq!(
            advertised_names(&pending),
            vec!["web_search", "append_blocks", "host_only_tool"]
        );
        assert!(pending.state_ledger.is_some());
        assert!(pending
            .context_references
            .iter()
            .any(|reference| reference.id == "term-1"));
    }

    // Export the source session; the snapshot crosses serde JSON only.
    let source_snapshot = eng_source.export_snapshot(&source);
    assert_eq!(source_snapshot.version, SNAPSHOT_VERSION);
    assert_eq!(
        source_snapshot.active_tool_names,
        vec![tn("web_search"), tn("append_blocks"), tn("host_only_tool")]
    );
    let round_tripped = json_round_trip(&source_snapshot);
    // Source engine + handle dropped at scope end: simulated death.
    drop(source);
    drop(eng_source);

    // (c) Scalar asserts on the JSON-round-tripped snapshot itself.
    assert_eq!(round_tripped.version, source_snapshot.version);
    assert_eq!(round_tripped.phase, EnginePhase::AwaitingModel);
    assert_eq!(round_tripped.phase, source_snapshot.phase);
    assert_eq!(round_tripped.turn_index, source_snapshot.turn_index);
    assert_eq!(
        round_tripped.model_requests_this_turn,
        source_snapshot.model_requests_this_turn
    );
    assert_eq!(
        round_tripped.compaction_passes_this_turn,
        source_snapshot.compaction_passes_this_turn
    );
    assert_eq!(
        round_tripped.destructive_tiers_this_turn,
        source_snapshot.destructive_tiers_this_turn
    );
    assert_eq!(
        round_tripped.tools_blocked_nudges_this_turn,
        source_snapshot.tools_blocked_nudges_this_turn
    );
    assert_eq!(round_tripped.cancelled, source_snapshot.cancelled);
    assert!(!round_tripped.cancelled);
    assert_eq!(
        serde_json::to_value(&round_tripped.terminal_action).expect("terminal_action serializes"),
        serde_json::to_value(&source_snapshot.terminal_action).expect("terminal_action serializes"),
        "terminal_action must match"
    );

    // Simulated restart: import into a FRESH engine.
    let eng_restored = AgentEngine::new();
    let restored = eng_restored
        .import_snapshot(round_tripped)
        .expect("snapshot imports into a fresh engine");

    // A pre-export context-store entry resolves on the imported session.
    let content = eng_restored
        .read_context_reference(&restored, "term-1".to_string())
        .expect("pre-export store entry must resolve after import");
    assert_eq!(content, "terminal payload xyz");

    // Drive BOTH sessions through the same next turn: submit the same
    // model response to the pending request, execute the same batch,
    // submit identical results.
    submit_web_search_call(&eng_control, &control, "call-2");
    submit_web_search_call(&eng_restored, &restored, "call-2");
    let control_batch = execute_batch(&eng_control, &control);
    let restored_batch = execute_batch(&eng_restored, &restored);
    assert_eq!(
        serde_json::to_value(&restored_batch).expect("batch serializes"),
        serde_json::to_value(&control_batch).expect("batch serializes"),
        "post-import ExecuteTools batch must match the control's"
    );
    submit_ok_result(&eng_control, &control, "call-2", vec![]);
    submit_ok_result(&eng_restored, &restored, "call-2", vec![]);

    // The FOLLOWING model request — built fresh by build_model_request
    // on both sides, not a replay of the stored pending request — must
    // match in full.
    let control_next = model_request(&eng_control, &control);
    let restored_next = model_request(&eng_restored, &restored);
    assert!(control_next.state_ledger.is_some());
    assert_requests_match(&control_next, &restored_next);
    assert!(restored_next
        .context_references
        .iter()
        .any(|reference| reference.id == "term-1"));
}

// ---------------------------------------------------------------------------
// (b) Mid-turn restart: export at AwaitingTools, import, and the same
// ExecuteTools batch comes out; identical results complete the turn
// identically.
// ---------------------------------------------------------------------------

#[test]
fn enc305_snapshot_mid_turn_restart_replays_execute_batch_and_completes() {
    let eng_control = AgentEngine::new();
    let control = create_fixture_session(&eng_control);
    let eng_source = AgentEngine::new();
    let source = create_fixture_session(&eng_source);

    // Drive both to AwaitingTools: model response submitted, results
    // NOT submitted.
    for (eng, handle) in [(&eng_control, &control), (&eng_source, &source)] {
        let _first = model_request(eng, handle);
        submit_web_search_call(eng, handle, "call-1");
    }
    let control_batch = execute_batch(&eng_control, &control);
    let source_snapshot = eng_source.export_snapshot(&source);
    assert_eq!(source_snapshot.phase, EnginePhase::AwaitingTools);

    let round_tripped = json_round_trip(&source_snapshot);
    drop(source);
    drop(eng_source);

    let eng_restored = AgentEngine::new();
    let restored = eng_restored
        .import_snapshot(round_tripped)
        .expect("mid-turn snapshot imports into a fresh engine");

    // next_action on the restored session yields the same ExecuteTools
    // batch the control is holding.
    let restored_batch = execute_batch(&eng_restored, &restored);
    assert_eq!(
        serde_json::to_value(&restored_batch).expect("batch serializes"),
        serde_json::to_value(&control_batch).expect("batch serializes"),
        "restored ExecuteTools batch must match the control's"
    );

    // Submit identical results on both; the subsequent requests match.
    submit_ok_result(&eng_control, &control, "call-1", vec![tn("host_only_tool")]);
    submit_ok_result(
        &eng_restored,
        &restored,
        "call-1",
        vec![tn("host_only_tool")],
    );
    let control_next = model_request(&eng_control, &control);
    let restored_next = model_request(&eng_restored, &restored);
    assert_requests_match(&control_next, &restored_next);
    assert_eq!(
        advertised_names(&restored_next),
        vec!["web_search", "append_blocks", "host_only_tool"]
    );
}

// ---------------------------------------------------------------------------
// (d) M2 pin: active_json() — the raw schema channel text — survives
// the round trip byte-identically for a string-path session whose
// channel formatting is non-canonical.
// ---------------------------------------------------------------------------

#[test]
fn enc305_snapshot_active_json_is_byte_identical_across_round_trip() {
    let eng = AgentEngine::new();
    let handle = create_fixture_session_with_schema(&eng, NONCANONICAL_SCHEMA_JSON);
    let source_snapshot = eng.export_snapshot(&handle);

    // The pin must actually bite: the raw text differs from the
    // canonical entries_json rendering of the same entries.
    assert_eq!(
        source_snapshot.schemas.active_json(),
        NONCANONICAL_SCHEMA_JSON
    );
    assert_ne!(
        source_snapshot.schemas.active_json(),
        agent_core::tool_schema::entries_json(&source_snapshot.schemas.active),
        "fixture formatting must differ from the canonical rendering"
    );

    let round_tripped = json_round_trip(&source_snapshot);
    assert_eq!(
        round_tripped.schemas.active_json(),
        NONCANONICAL_SCHEMA_JSON,
        "raw_active must cross the snapshot wire verbatim (M2)"
    );

    let eng_restored = AgentEngine::new();
    let restored = eng_restored
        .import_snapshot(round_tripped)
        .expect("snapshot imports");
    let restored_snapshot = eng_restored.export_snapshot(&restored);
    assert_eq!(
        restored_snapshot.schemas.active_json(),
        NONCANONICAL_SCHEMA_JSON,
        "the restored session's schema set must keep the raw channel text"
    );
}

// ---------------------------------------------------------------------------
// (e) Tamper tests: incoherent snapshots are rejected with the typed
// InvalidSnapshot error — never coerced, never panicking on first poll.
// ---------------------------------------------------------------------------

/// Export a snapshot at AwaitingModel (pending.call_model present) as a
/// mutable JSON value, for field-level tampering the typed API cannot
/// express (PendingToolTurn's fields are engine-private).
fn snapshot_json_at_awaiting_model() -> Value {
    let eng = AgentEngine::new();
    let handle = create_fixture_session(&eng);
    let _request = model_request(&eng, &handle);
    let snapshot = eng.export_snapshot(&handle);
    assert_eq!(snapshot.phase, EnginePhase::AwaitingModel);
    serde_json::to_value(&snapshot).expect("snapshot serializes to a value")
}

fn import_json(eng: &AgentEngine, value: Value) -> ToolSemanticsError {
    let snapshot: SessionSnapshot =
        serde_json::from_value(value).expect("tampered snapshot still deserializes");
    match eng.import_snapshot(snapshot) {
        Ok(_) => panic!("tampered snapshot must fail import"),
        Err(error) => error,
    }
}

fn assert_invalid_snapshot(error: &ToolSemanticsError, needle: &str) {
    match error {
        ToolSemanticsError::InvalidSnapshot { message } => {
            assert!(
                message.contains(needle),
                "InvalidSnapshot message must name '{needle}': {message}"
            );
        }
        other => panic!("expected InvalidSnapshot, got {other:?}"),
    }
}

#[test]
fn enc305_snapshot_import_rejects_awaiting_model_without_pending_request() {
    let eng = AgentEngine::new();
    let mut value = snapshot_json_at_awaiting_model();
    value["pending"]["call_model"] = Value::Null;
    let error = import_json(&eng, value);
    assert_invalid_snapshot(&error, "call_model");
}

#[test]
fn enc305_snapshot_import_rejects_awaiting_summary_without_pending_context() {
    let eng = AgentEngine::new();
    let mut value = snapshot_json_at_awaiting_model();
    // Phase claims AwaitingSummary but neither summary_req nor
    // summary_ctx is present (both are null in this snapshot).
    value["phase"] = Value::String("AwaitingSummary".to_string());
    assert_eq!(value["pending"]["summary_req"], Value::Null);
    assert_eq!(value["pending"]["summary_ctx"], Value::Null);
    let error = import_json(&eng, value);
    assert_invalid_snapshot(&error, "summary_req");
}

#[test]
fn enc305_snapshot_import_rejects_store_key_id_mismatch() {
    let eng = AgentEngine::new();
    let handle = create_fixture_session(&eng);
    eng.insert_terminal_output(
        &handle,
        "term-1".to_string(),
        "Terminal".to_string(),
        "terminal payload xyz".to_string(),
    );
    let snapshot = eng.export_snapshot(&handle);
    let mut value = serde_json::to_value(&snapshot).expect("snapshot serializes to a value");
    // The entry's reference id no longer matches its map key.
    value["context_store"]["entries"]["term-1"]["reference"]["id"] =
        Value::String("term-other".to_string());
    let error = import_json(&eng, value);
    assert_invalid_snapshot(&error, "term-1");
    assert_invalid_snapshot(&error, "term-other");
}

#[test]
fn enc305_snapshot_import_rejects_store_next_seq_behind_minted_suffix() {
    let eng = AgentEngine::new();
    let handle = create_fixture_session(&eng);
    eng.insert_terminal_output(
        &handle,
        "term-1".to_string(),
        "Terminal".to_string(),
        "terminal payload xyz".to_string(),
    );
    let snapshot = eng.export_snapshot(&handle);
    let mut value = serde_json::to_value(&snapshot).expect("snapshot serializes to a value");
    // Rename the entry (key AND reference id, consistently) to a minted
    // ctxref-*-7 shape while next_seq stays at 0: a future minted ID
    // would collide with it.
    let mut entry = value["context_store"]["entries"]["term-1"].take();
    entry["reference"]["id"] = Value::String("ctxref-sidecar-7".to_string());
    value["context_store"]["entries"] = serde_json::json!({ "ctxref-sidecar-7": entry });
    value["context_store"]["next_seq"] = Value::from(0);
    let error = import_json(&eng, value);
    assert_invalid_snapshot(&error, "next_seq");
}

#[test]
fn enc305_snapshot_import_rejects_wrong_version() {
    let eng = AgentEngine::new();
    let handle = create_fixture_session(&eng);
    let mut snapshot = eng.export_snapshot(&handle);
    snapshot.version = SNAPSHOT_VERSION + 1;
    let error = match eng.import_snapshot(snapshot) {
        Ok(_) => panic!("wrong snapshot version must fail import"),
        Err(error) => error,
    };
    assert_invalid_snapshot(&error, "version");
}

// ---------------------------------------------------------------------------
// Import validation is construction's validation.
// ---------------------------------------------------------------------------

#[test]
fn enc305_snapshot_import_rejects_dangling_argument_repairs_reference() {
    let eng = AgentEngine::new();
    let handle = create_fixture_session(&eng);
    let mut snapshot = eng.export_snapshot(&handle);

    // Tamper post-export: a semantics section now references a tool that
    // is neither a catalog tool nor an active schema.
    snapshot
        .config
        .tool_semantics
        .argument_repairs
        .push(ToolArgumentRepairRule {
            tool_name: tn("ghost_tool"),
            aliases: vec![],
            special_values: vec![],
        });

    let error = match eng.import_snapshot(snapshot) {
        Ok(_) => panic!("dangling semantics reference must fail import"),
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            ToolSemanticsError::UnknownToolReference { ref name, .. } if name == "ghost_tool"
        ),
        "expected UnknownToolReference for ghost_tool, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// FFI surface: snapshot JSON round-trip + fallible import.
// ---------------------------------------------------------------------------

#[cfg(feature = "ffi")]
mod ffi_tests {
    use agent_core::{
        create_agent_engine, AgentException, FfiAgentAction, FfiAgentConfig, FfiAgentMessage,
        FfiAgentRole, FfiSessionCapabilities, FfiSessionIoParams, FfiToolSchemaEntry,
    };

    #[test]
    fn enc305_ffi_snapshot_json_round_trip_restores_working_session() {
        let eng = create_agent_engine();
        let session = eng
            .create_session(
                vec![FfiAgentMessage {
                    id: "u1".to_string(),
                    role: FfiAgentRole::User,
                    content: "hi".to_string(),
                    metadata_json: "null".to_string(),
                }],
                FfiAgentConfig::default(),
                FfiSessionCapabilities::default(),
                "{}".to_string(),
                FfiSessionIoParams {
                    provider: "anthropic".to_string(),
                    context_window_tokens: 200_000,
                    tool_schemas: vec![FfiToolSchemaEntry {
                        name: "web_search".to_string(),
                        description: Some("Search the web".to_string()),
                        reproducible: None,
                        deferrable: None,
                        max_result_bytes: None,
                    }],
                    system_prompt: "sys".to_string(),
                },
            )
            .expect("ffi create_session");

        eng.insert_terminal_output(
            &session,
            "term-ffi".to_string(),
            "Terminal".to_string(),
            "ffi terminal payload".to_string(),
        );

        let snapshot_json = eng
            .export_snapshot(&session)
            .expect("ffi export_snapshot returns JSON");
        assert!(snapshot_json.contains("web_search"));

        let restored = eng
            .import_snapshot(snapshot_json)
            .expect("ffi import_snapshot restores the session");

        // The restored session is live: it produces a model request
        // advertising the exported tool, and the pre-export context-store
        // entry resolves through it.
        match eng.next_action(&restored) {
            FfiAgentAction::CallModel { request } => {
                assert_eq!(request.active_tools, vec!["web_search".to_string()]);
            }
            other => panic!("expected FFI CallModel, got {other:?}"),
        }
        let content = eng
            .read_context_reference(&restored, "term-ffi".to_string())
            .expect("pre-export store entry must resolve after ffi import");
        assert_eq!(content, "ffi terminal payload");
    }

    #[test]
    fn enc305_ffi_import_snapshot_rejects_non_json() {
        let eng = create_agent_engine();
        let error = match eng.import_snapshot("not json".to_string()) {
            Ok(_) => panic!("non-JSON snapshot text must fail"),
            Err(error) => error,
        };
        match error {
            AgentException::Internal { detail } => {
                assert!(
                    detail.contains("invalid session snapshot JSON"),
                    "detail must name the snapshot parse failure: {detail}"
                );
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    #[test]
    fn enc305_ffi_import_snapshot_rejects_incoherent_phase_pending() {
        // M1 at the FFI surface: a tampered (phase, pending) pair maps
        // the core InvalidSnapshot error to AgentException::Internal
        // (the create_session ToolSemanticsError mapping).
        let eng = create_agent_engine();
        let session = eng
            .create_session(
                vec![FfiAgentMessage {
                    id: "u1".to_string(),
                    role: FfiAgentRole::User,
                    content: "hi".to_string(),
                    metadata_json: "null".to_string(),
                }],
                FfiAgentConfig::default(),
                FfiSessionCapabilities::default(),
                "{}".to_string(),
                FfiSessionIoParams {
                    provider: "anthropic".to_string(),
                    context_window_tokens: 200_000,
                    tool_schemas: vec![],
                    system_prompt: "sys".to_string(),
                },
            )
            .expect("ffi create_session");
        let snapshot_json = eng
            .export_snapshot(&session)
            .expect("ffi export_snapshot returns JSON");
        // The idle export has pending.call_model = null; claiming
        // AwaitingModel makes the pair incoherent.
        let mut value: serde_json::Value =
            serde_json::from_str(&snapshot_json).expect("snapshot JSON parses");
        assert_eq!(value["pending"]["call_model"], serde_json::Value::Null);
        value["phase"] = serde_json::Value::String("AwaitingModel".to_string());
        let tampered = serde_json::to_string(&value).expect("tampered snapshot serializes");

        let error = match eng.import_snapshot(tampered) {
            Ok(_) => panic!("incoherent snapshot must fail ffi import"),
            Err(error) => error,
        };
        match error {
            AgentException::Internal { detail } => {
                assert!(
                    detail.contains("InvalidSnapshot"),
                    "detail must embed the InvalidSnapshot debug form: {detail}"
                );
                assert!(
                    detail.contains("call_model"),
                    "detail must name the incoherence: {detail}"
                );
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }
}
