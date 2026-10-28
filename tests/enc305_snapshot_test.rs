//! ENC-305 chunk 3 acceptance: the typed `SessionSnapshot` is the only
//! state crossing a process boundary.
//!
//! Round-trip: an enclave-fixture session is driven through a tool turn
//! that activates a host-only tool plus a context-store insertion, its
//! next `ModelRequest` is captured, the snapshot crosses serde JSON
//! (simulated process death: the original engine/handle are dropped),
//! and a FRESH `AgentEngine` imports it — the restored session's next
//! request must match the pre-export one, and a pre-export context-store
//! entry must still resolve. Import validation reuses construction's:
//! a tampered dangling semantics reference is rejected with
//! `UnknownToolReference`. The FFI tests (ffi-gated) pin the JSON-text
//! surface on `FfiAgentEngine`.

#[path = "support/enclave_test_semantics.rs"]
mod enclave;

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta, ToolResultRecord};
use agent_core::session::{AgentConfig, SessionCapabilities};
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

fn create_fixture_session(eng: &AgentEngine) -> SessionHandle {
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
            FIXTURE_SCHEMA_JSON.to_string(),
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

// ---------------------------------------------------------------------------
// Round-trip behavior across a simulated restart.
// ---------------------------------------------------------------------------

#[test]
fn enc305_snapshot_round_trip_restores_next_request_and_context_store() {
    // Drive the pre-export session: a web_search tool turn whose result
    // activates a host-only tool, plus a context-store insertion.
    let (pre_request, snapshot_json) = {
        let eng = AgentEngine::new();
        let handle = create_fixture_session(&eng);

        let first = model_request(&eng, &handle);
        assert_eq!(
            advertised_names(&first),
            vec!["web_search", "append_blocks"]
        );

        eng.submit_model_response(
            &handle,
            ModelResponse {
                assistant_text: None,
                tool_calls: vec![RawToolCall {
                    id: Some("call-1".to_string()),
                    name: "web_search".to_string(),
                    arguments_json: r#"{"query":"tools"}"#.to_string(),
                }],
                finish_reason: FinishReason::ToolUse,
                provider_reasoning_content: None,
            },
        );
        assert!(matches!(
            eng.next_action(&handle),
            AgentAction::ExecuteTools(_)
        ));
        eng.submit_tool_results(
            &handle,
            vec![ToolResultRecord {
                call_id: "call-1".to_string(),
                result_json: serde_json::json!({ "status": "ok" }).to_string(),
                truncated: false,
                soft_failure: false,
                activated_tool_names: vec![tn("host_only_tool")],
                result_metadata: None,
            }],
        );

        // Context-store insertion on the pre-export session.
        let reference = eng.insert_terminal_output(
            &handle,
            "term-1".to_string(),
            "Terminal".to_string(),
            "terminal payload xyz".to_string(),
        );
        assert_eq!(reference.id, "term-1");

        // The pre-export session's next request: host_only_tool is now
        // advertised, the state ledger is present, and the inserted
        // context reference rides along.
        let pre_request = model_request(&eng, &handle);
        assert_eq!(
            advertised_names(&pre_request),
            vec!["web_search", "append_blocks", "host_only_tool"]
        );
        assert!(pre_request.state_ledger.is_some());
        assert!(pre_request
            .context_references
            .iter()
            .any(|reference| reference.id == "term-1"));

        let snapshot = eng.export_snapshot(&handle);
        assert_eq!(
            snapshot.active_tool_names,
            vec![tn("web_search"), tn("append_blocks"), tn("host_only_tool")]
        );
        let snapshot_json = serde_json::to_string(&snapshot).expect("snapshot serializes");
        (pre_request, snapshot_json)
        // Original engine + handle dropped here: simulated process death.
    };

    // Simulated restart: the snapshot crosses as JSON text only.
    let snapshot: SessionSnapshot =
        serde_json::from_str(&snapshot_json).expect("snapshot deserializes");
    let eng = AgentEngine::new();
    let handle = eng
        .import_snapshot(snapshot)
        .expect("snapshot imports into a fresh engine");

    // Behavioral identity: the restored session's next request matches
    // the pre-export session's next request.
    let post_request = model_request(&eng, &handle);
    assert_eq!(
        post_request.tools_schema_json(),
        pre_request.tools_schema_json()
    );
    assert_eq!(
        advertised_names(&post_request),
        advertised_names(&pre_request)
    );
    assert_eq!(
        post_request.state_ledger.is_some(),
        pre_request.state_ledger.is_some()
    );
    assert!(post_request.state_ledger.is_some());
    assert!(post_request
        .context_references
        .iter()
        .any(|reference| reference.id == "term-1"));

    // A pre-export context-store entry resolves on the imported session.
    let content = eng
        .read_context_reference(&handle, "term-1".to_string())
        .expect("pre-export store entry must resolve after import");
    assert_eq!(content, "terminal payload xyz");
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
}
