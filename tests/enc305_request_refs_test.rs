//! ENC-305 chunk 1 acceptance: per-turn requests reference the
//! `ToolUniverse` by `ToolName` and the `ContextStore` by ID.
//!
//! `FfiModelRequest` carries no JSON state fields; the inbound conversion
//! is the fallible, session-bound `FfiModelRequest::into_core`, and the
//! outbound `From<ModelRequest>` conversion is an infallible name/ID
//! projection. These tests pin the dangling-reference errors, the happy
//! path resolution shapes, the strict message-metadata path, and the
//! outbound projection.
#![cfg(feature = "ffi")]

#[path = "support/enclave_test_semantics.rs"]
mod enclave;

use agent_core::context_store::{
    ContextReference, ContextReferenceKind, ContextStore, ContextStoreEntry,
};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta};
use agent_core::tool_name::ToolName;
use agent_core::tool_schema::{SchemaEntry, ToolSchemaSet};
use agent_core::tool_universe::ToolUniverse;
use agent_core::turn_io::ModelRequest;
use agent_core::{
    AgentException, FfiAgentMessage, FfiAgentRole, FfiModelRequest, FfiProviderContextSnapshot,
};

fn tn(name: &str) -> ToolName {
    ToolName::new(name).expect("test tool name must be valid")
}

/// Schema channel carrying two enclave-catalog tools, in this order
/// (mirrors `tests/enc304_universe_test.rs`).
const FIXTURE_SCHEMA_JSON: &str = r#"[
  {"name":"web_search","description":"Search the web","parameters":{}},
  {"name":"append_blocks","description":"Append blocks","parameters":{}}
]"#;

fn fixture_universe() -> ToolUniverse {
    let schemas =
        ToolSchemaSet::parse(FIXTURE_SCHEMA_JSON).expect("fixture schema channel must parse");
    ToolUniverse::build(schemas, enclave::enclave_test_semantics())
        .expect("enclave fixture world must build")
}

fn store_entry(id: &str, content: &str) -> ContextStoreEntry {
    ContextStoreEntry {
        reference: ContextReference {
            id: id.to_string(),
            kind: ContextReferenceKind::ToolOutput,
            title: format!("title-{id}"),
            byte_len: content.len() as u64,
            token_estimate: (content.len() / 4) as u32,
            preview: content.to_string(),
            retrieval_hint: format!("read_context_reference(\"{id}\")"),
        },
        content: content.to_string(),
        source_message_ids: vec![],
        created_turn_index: 0,
        expires_after_turn: None,
    }
}

fn fixture_store() -> ContextStore {
    let mut store = ContextStore::default();
    store.insert_entry(store_entry("ctx-1", "first payload"));
    store.insert_entry(store_entry("ctx-2", "second payload"));
    store
}

fn ffi_message(id: &str, metadata_json: &str) -> FfiAgentMessage {
    FfiAgentMessage {
        id: id.to_string(),
        role: FfiAgentRole::User,
        content: format!("content-{id}"),
        metadata_json: metadata_json.to_string(),
    }
}

fn base_request() -> FfiModelRequest {
    FfiModelRequest {
        session_messages: vec![ffi_message("u1", "{}")],
        system_prompt: "sys".to_string(),
        active_tools: vec!["web_search".to_string()],
        allow_tool_call: true,
        context_reference_ids: vec![],
        cache_hints: vec!["hint-1".to_string()],
        provider_context: FfiProviderContextSnapshot {
            transcript_revision: 3,
            sidecar_generation: 2,
            sidecar_byte_len: 128,
            compaction_passes_this_turn: 0,
            destructive_tiers_this_turn: 0,
            pressure_ratio_at_call_model_millis: 0,
        },
        retry_after_ms: Some(250),
        model_retry_attempt: Some(1),
    }
}

// ---------------------------------------------------------------------------
// Dangling references fail with the typed errors.
// ---------------------------------------------------------------------------

#[test]
fn enc305_into_core_dangling_tool_name_is_internal_unknown_tool_reference() {
    let universe = fixture_universe();
    let store = fixture_store();
    let mut request = base_request();
    request.active_tools = vec!["web_search".to_string(), "ghost_tool".to_string()];

    let error = request
        .into_core(&universe, &store)
        .expect_err("dangling tool name must fail");

    match error {
        AgentException::Internal { detail } => {
            assert!(
                detail.contains("UnknownToolReference"),
                "detail must embed the UnknownToolReference debug form: {detail}"
            );
            assert!(
                detail.contains("ghost_tool"),
                "detail must name the dangling tool: {detail}"
            );
            assert!(
                detail.contains("request"),
                "detail must carry the request section: {detail}"
            );
        }
        other => panic!("expected Internal, got {other:?}"),
    }
}

#[test]
fn enc305_into_core_blank_tool_name_is_internal_naming_active_tools() {
    let universe = fixture_universe();
    let store = fixture_store();
    let mut request = base_request();
    request.active_tools = vec!["  ".to_string()];

    let error = request
        .into_core(&universe, &store)
        .expect_err("blank tool name must fail");

    match error {
        AgentException::Internal { detail } => {
            assert!(
                detail.contains("active_tools"),
                "detail must name the active_tools field: {detail}"
            );
        }
        other => panic!("expected Internal, got {other:?}"),
    }
}

#[test]
fn enc305_into_core_dangling_context_id_is_context_reference_not_found() {
    let universe = fixture_universe();
    let store = fixture_store();
    let mut request = base_request();
    request.context_reference_ids = vec!["ctx-1".to_string(), "ctx-missing".to_string()];

    let error = request
        .into_core(&universe, &store)
        .expect_err("dangling context id must fail");

    match error {
        AgentException::ContextReferenceNotFound { reference_id } => {
            assert_eq!(reference_id, "ctx-missing");
        }
        other => panic!("expected ContextReferenceNotFound, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Happy path: entry-bearing, catalog-only (canonical + alias), and
// host-activated entry-less names all resolve; context IDs resolve to
// the store's references; engine-derived state is None/empty.
// ---------------------------------------------------------------------------

#[test]
fn enc305_into_core_happy_path_resolves_tools_and_context_ids() {
    let mut universe = fixture_universe();
    // Host-activated entry-less name: no entry, no spec, but tracked in
    // the universe's active set.
    universe.activate([tn("host_only_tool")]);
    let store = fixture_store();

    let mut request = base_request();
    request.session_messages = vec![
        ffi_message("u1", r#"{"call_id":"c1"}"#),
        ffi_message("u2", "null"),
    ];
    request.active_tools = vec![
        // Entry-bearing.
        "web_search".to_string(),
        // Catalog-only (spec, no entry).
        "generate_image".to_string(),
        // Catalog alias: web_fetch is web_extract's alias (spec
        // resolves, no entry) — m2 canonicalizes it to web_extract
        // before lookup/stub, so the stub carries the canonical name.
        "web_fetch".to_string(),
        // websearch is web_search's alias: m2 canonicalizes it to
        // web_search, and m3 dedupes it against the first entry.
        "websearch".to_string(),
        // Host-activated entry-less.
        "host_only_tool".to_string(),
    ];
    // Request order, not store (BTree) order; the repeated ctx-2 is
    // deduped first-seen (m3).
    request.context_reference_ids = vec![
        "ctx-2".to_string(),
        "ctx-1".to_string(),
        "ctx-2".to_string(),
    ];

    let core = request
        .into_core(&universe, &store)
        .expect("all references must resolve");

    // Tool schemas: full entry for the entry-bearing name, stub shape
    // ({"name": ...} only, canonical name) for every name without an
    // entry; the alias-to-web_search duplicate is gone.
    let names: Vec<&str> = core
        .tool_schemas
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "web_search",
            "generate_image",
            "web_extract",
            "host_only_tool"
        ]
    );
    assert_eq!(core.tool_schemas[0].description(), "Search the web");
    assert_eq!(
        core.tool_schemas[0].value,
        serde_json::json!({
            "name": "web_search",
            "description": "Search the web",
            "parameters": {},
        })
    );
    for stub in &core.tool_schemas[1..] {
        assert_eq!(
            stub.value,
            serde_json::json!({ "name": stub.name.as_str() }),
            "entry-less names must resolve to the stub shape"
        );
    }

    // Context references: the store's references, in request order.
    let ids: Vec<&str> = core
        .context_references
        .iter()
        .map(|reference| reference.id.as_str())
        .collect();
    assert_eq!(ids, vec!["ctx-2", "ctx-1"]);
    assert_eq!(core.context_references[0].title, "title-ctx-2");
    assert_eq!(
        core.context_references[0].kind,
        ContextReferenceKind::ToolOutput
    );

    // Engine-derived state is None/empty by construction.
    assert!(core.state_ledger.is_none());
    assert!(core.tool_context_index.is_none());
    assert!(core.file_context_index.is_none());
    assert!(core.state_ledger_delta_json.is_none());
    assert!(core.tool_context_index_delta_json.is_none());
    assert!(core.file_context_index_delta_json.is_none());
    assert!(core.rendered_runtime_tools.is_empty());

    // Remaining fields map directly.
    assert_eq!(core.session_messages.len(), 2);
    assert_eq!(core.session_messages[0].metadata.call_id(), Some("c1"));
    assert!(core.session_messages[1].metadata.is_empty());
    assert_eq!(core.session_messages[0].role, AgentRole::User);
    assert_eq!(core.system_prompt, "sys");
    assert!(core.allow_tool_call);
    assert_eq!(core.cache_hints, vec!["hint-1".to_string()]);
    assert_eq!(core.provider_context.transcript_revision, 3);
    assert_eq!(core.provider_context.sidecar_generation, 2);
    assert_eq!(core.retry_after_ms, Some(250));
    assert_eq!(core.model_retry_attempt, Some(1));
}

// ---------------------------------------------------------------------------
// Malformed message metadata fails; it never defaults silently.
// ---------------------------------------------------------------------------

#[test]
fn enc305_into_core_malformed_metadata_json_is_agent_exception() {
    let universe = fixture_universe();
    let store = fixture_store();
    let mut request = base_request();
    request.session_messages = vec![ffi_message("u1", "{}"), ffi_message("u2", "{not json")];

    let error = request
        .into_core(&universe, &store)
        .expect_err("malformed metadata_json must fail");

    match error {
        AgentException::Internal { detail } => {
            assert!(
                detail.contains("session_messages[1]"),
                "detail must name the message index: {detail}"
            );
            assert!(
                detail.contains("metadata_json"),
                "detail must name the metadata_json field: {detail}"
            );
        }
        other => panic!("expected Internal, got {other:?}"),
    }
}

#[test]
fn enc305_into_core_non_object_metadata_json_is_agent_exception() {
    // m1: valid JSON that is neither an object nor null ("42", "[1,2]")
    // must fail the strict metadata path, not silently empty the meta.
    let universe = fixture_universe();
    let store = fixture_store();
    for metadata_json in ["42", "[1,2]"] {
        let mut request = base_request();
        request.session_messages = vec![ffi_message("u1", metadata_json)];
        let error = request
            .into_core(&universe, &store)
            .expect_err("non-object metadata_json must fail");
        match error {
            AgentException::Internal { detail } => {
                assert!(
                    detail.contains("session_messages[0]"),
                    "detail must name the message index: {detail}"
                );
                assert!(
                    detail.contains("metadata_json"),
                    "detail must name the metadata_json field: {detail}"
                );
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Outbound projection: names ++ runtime names deduped first-seen,
// reference IDs in order, kept fields direct.
// ---------------------------------------------------------------------------

#[test]
fn enc305_outbound_from_projects_names_and_ids_deduped_and_ordered() {
    let core = ModelRequest {
        session_messages: vec![AgentMessage {
            id: "u1".to_string(),
            role: AgentRole::User,
            content: "hi".to_string(),
            metadata: MessageMeta::new().with_call_id("c1"),
        }],
        system_prompt: "sys".to_string(),
        tool_schemas: vec![
            SchemaEntry::from(serde_json::json!({
                "name": "web_search",
                "description": "Search the web",
                "parameters": {},
            })),
            SchemaEntry::stub(&tn("host_only_tool")),
        ],
        rendered_runtime_tools: vec![
            // Duplicate of a tool_schemas name: deduped, first-seen wins.
            SchemaEntry::from(serde_json::json!({
                "name": "web_search",
                "description": "Search the web",
                "parameters": {},
            })),
            SchemaEntry::stub(&tn("runtime_tool")),
            // Duplicate within rendered_runtime_tools itself.
            SchemaEntry::stub(&tn("host_only_tool")),
        ],
        allow_tool_call: false,
        context_references: vec![
            ContextReference {
                id: "ctx-2".to_string(),
                kind: ContextReferenceKind::FileChunk,
                title: "t2".to_string(),
                byte_len: 4,
                token_estimate: 1,
                preview: "p2".to_string(),
                retrieval_hint: "read(ctx-2)".to_string(),
            },
            ContextReference {
                id: "ctx-1".to_string(),
                kind: ContextReferenceKind::ToolOutput,
                title: "t1".to_string(),
                byte_len: 4,
                token_estimate: 1,
                preview: "p1".to_string(),
                retrieval_hint: "read(ctx-1)".to_string(),
            },
        ],
        // Engine-derived state exists on the core request but must NOT
        // project onto the FFI request (its fields are deleted).
        state_ledger: None,
        tool_context_index: None,
        file_context_index: None,
        cache_hints: vec!["hint-1".to_string()],
        provider_context: Default::default(),
        state_ledger_delta_json: Some("delta".to_string()),
        tool_context_index_delta_json: None,
        file_context_index_delta_json: None,
        retry_after_ms: Some(500),
        model_retry_attempt: Some(2),
    };

    let ffi = FfiModelRequest::from(core);

    assert_eq!(
        ffi.active_tools,
        vec![
            "web_search".to_string(),
            "host_only_tool".to_string(),
            "runtime_tool".to_string(),
        ]
    );
    assert_eq!(
        ffi.context_reference_ids,
        vec!["ctx-2".to_string(), "ctx-1".to_string()]
    );
    assert_eq!(ffi.system_prompt, "sys");
    assert!(!ffi.allow_tool_call);
    assert_eq!(ffi.cache_hints, vec!["hint-1".to_string()]);
    assert_eq!(ffi.retry_after_ms, Some(500));
    assert_eq!(ffi.model_retry_attempt, Some(2));
    assert_eq!(ffi.session_messages.len(), 1);
    assert_eq!(ffi.session_messages[0].id, "u1");
    // Message metadata still crosses as its JSON string on the message.
    assert!(ffi.session_messages[0].metadata_json.contains("c1"));
}
