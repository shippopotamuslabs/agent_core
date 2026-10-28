//! ENC-308 A2 goldens (plan decision D3): byte-level pins of the
//! CURRENT serialized form, generated from the pre-change Wire impl
//! (commit before the derive swap) by running that code — never
//! hand-written. The derive swap in src/tool_schema.rs must keep every
//! byte below identical; any drift fails here before it can reach a
//! stored snapshot.
//!
//! The snapshot fixture uses `AgentConfig::default()` on purpose: the
//! enclave test semantics carry HashMap defaults whose iteration order
//! is process-random (observed while generating these fixtures), which
//! would make a full-byte pin flaky for reasons unrelated to serde.
//! The default config's maps are empty, so the export is deterministic.

use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::tool_schema::{SchemaEntry, ToolSchemaSet};
use agent_core::{SessionHandle, SessionSnapshot};
use serde_json::Value;

/// Canonical two-entry channel: raw text is already the canonical
/// rendering of its entries.
const CANONICAL_CHANNEL: &str = r#"[{"name":"web_search","description":"Search the web","parameters":{"type":"object"}},{"name":"append_blocks","description":"Append blocks","parameters":{"type":"object"}}]"#;

/// Same tool, deliberately non-canonical text: extra whitespace,
/// reversed key order, and a `1.50` number lexeme. `raw_active` must
/// keep this text verbatim while the parsed entry renders canonically
/// (`1.5`) — the fidelity the Wire contract exists to protect.
const NONCANONICAL_CHANNEL: &str = r#"[ { "parameters" : { "type" : "object" }, "name" : "web_search", "description" : "Search the web", "weight" : 1.50 } ]"#;

/// Serialized bytes of the set under test, pinned below.
const GOLDEN_EMPTY: &str = r###"{"active":[],"raw_active":""}"###;
const GOLDEN_CANONICAL: &str = r###"{"active":[{"description":"Search the web","name":"web_search","parameters":{"type":"object"}},{"description":"Append blocks","name":"append_blocks","parameters":{"type":"object"}}],"raw_active":"[{\"name\":\"web_search\",\"description\":\"Search the web\",\"parameters\":{\"type\":\"object\"}},{\"name\":\"append_blocks\",\"description\":\"Append blocks\",\"parameters\":{\"type\":\"object\"}}]"}"###;
const GOLDEN_NONCANONICAL: &str = r###"{"active":[{"description":"Search the web","name":"web_search","parameters":{"type":"object"},"weight":1.5}],"raw_active":"[ { \"parameters\" : { \"type\" : \"object\" }, \"name\" : \"web_search\", \"description\" : \"Search the web\", \"weight\" : 1.50 } ]"}"###;
const GOLDEN_RICH: &str = r###"{"active":[{"description":"Run a command","max_result_bytes":4096,"name":"run_command","parameters":{"properties":{"cmd":{"type":"string"},"dry_run":{"type":"boolean"},"flags":{"items":{"type":"string"},"type":"array"},"timeout":{"default":1.5,"type":"number"}},"required":["cmd"],"type":"object"},"reproducible":false},{"description":"Leave a note","name":"note","parameters":{"properties":{"pinned":{"type":["boolean","null"]},"text":{"type":"string"}},"type":"object"}}],"raw_active":"[{\"description\":\"Run a command\",\"max_result_bytes\":4096,\"name\":\"run_command\",\"parameters\":{\"properties\":{\"cmd\":{\"type\":\"string\"},\"dry_run\":{\"type\":\"boolean\"},\"flags\":{\"items\":{\"type\":\"string\"},\"type\":\"array\"},\"timeout\":{\"default\":1.5,\"type\":\"number\"}},\"required\":[\"cmd\"],\"type\":\"object\"},\"reproducible\":false},{\"description\":\"Leave a note\",\"name\":\"note\",\"parameters\":{\"properties\":{\"pinned\":{\"type\":[\"boolean\",\"null\"]},\"text\":{\"type\":\"string\"}},\"type\":\"object\"}}]"}"###;

/// Full engine SessionSnapshot JSON (fixture session below) containing
/// a ToolSchemaSet built from NONCANONICAL_CHANNEL. Embedded as a
/// literal, NOT a file under tests/fixtures/: that directory is
/// globbed by tests/fixtures.rs, which parses every *.json there as a
/// scenario fixture.
const GOLDEN_SNAPSHOT: &str = r###"{"version":1,"session":{"messages":[{"id":"u1","role":"user","content":"hi","metadata":null}],"tool_trace":[],"compaction_log":[],"ledger_snapshot":{"total_tokens":2116,"threshold_tokens":150000,"usable_prompt_tokens":190000,"ratio":0.01058},"session_context":{},"transcript_revision":0},"config":{"max_turns":20,"threshold_ratio":0.8,"micro_keep_recent":4,"tool_retry_limit":3,"model_api_retry_limit":3,"model_api_retry_base_delay_ms":1000,"tool_result_budget_cloud_bytes":16384,"tool_result_budget_relaxed_bytes":131072,"tool_result_truncation_pressure_ratio":0.85,"token_correction":{},"mid_turn_threshold_ratio":0.93,"post_tool_result_threshold_ratio":0.92,"manual_compact_threshold_ratio":0.8,"allow_fifo_on_preflight":true,"allow_fifo_on_mid_turn":false,"allow_fifo_on_post_tool_result":false,"require_snapshot_before_fifo":true,"enable_context_store":true,"enable_state_ledger":true,"enable_tool_schema_deferral":true,"enable_file_condensation":true,"enable_manual_compact":true,"tool_spill_threshold_bytes":8192,"terminal_spill_threshold_bytes":8192,"history_snapshot_threshold_tokens":1024,"file_outline_threshold_tokens":2048,"prompt_cache_anchor_threshold_tokens":0,"output_reserve_ratio":0.05,"output_reserve_floor_tokens":4096,"output_reserve_max_tokens":16384,"preflight_token_threshold":150000,"mid_turn_token_threshold":180000,"post_tool_result_token_threshold":null,"turn_threshold":null,"message_threshold":200,"compact_on_turn_end":false,"retention_window_messages":0,"retention_window_tokens":16384,"eviction_window_ratio":0.0,"enable_loop_hooks":false,"enable_doom_loop_guard":false,"enable_pending_work_guard":false,"require_read_before_edit":false,"default_tool_timeout_ms":null,"max_model_requests_per_turn":1,"long_loop":{"max_tool_turns":40,"sidecar_byte_budget_per_call":8192,"sidecar_total_byte_budget":16384,"summarize_after_tool_turns":0,"compact_every_n_tool_turns":0},"tool_semantics":{"catalog":{"tools":[]},"discovery":null,"artifact":null,"argument_repairs":[],"canonical_shapes":[],"result_policies":[],"compaction":{"artifact_squash":null},"greedy_extension_tools":[]},"tool_call_format":"Structured"},"capabilities":{"summarization_allowed":false,"kv_fixed":false,"context_policy_tier":"hosted_mutable","tool_schema_deferral_supported":false,"file_backed_context_supported":false,"prompt_cache_supported":false,"history_rewrite_supported":true,"request_transform_pipeline_supported":false,"lifecycle_hooks_supported":false,"delegation_tools_supported":false,"host_policy_callbacks_supported":false},"provider":"anthropic","context_window_tokens":200000,"system_prompt":"sys","schemas":{"active":[{"description":"Search the web","name":"web_search","parameters":{"type":"object"},"weight":1.5}],"raw_active":"[ { \"parameters\" : { \"type\" : \"object\" }, \"name\" : \"web_search\", \"description\" : \"Search the web\", \"weight\" : 1.50 } ]"},"active_tool_names":["web_search"],"context_store":{"entries":{},"next_seq":0},"state_ledger":{"objective":null,"constraints":[],"decisions":[],"open_questions":[],"touched_files":[],"tool_decisions":[],"artifact_states":[],"summary_refs":[],"history_refs":[],"retired":[],"retirement_artifacts":[]},"guardrails":{"tool_calls_executed":0,"consecutive_soft_failure_turns":0,"consecutive_envelope_parse_failures":0,"consecutive_model_api_failures":0},"loop_metrics":{"turn_count":1,"message_count":0,"files_read":[],"repeated_tool_signatures":[],"tool_attempts":{},"pending_work_items":[]},"phase":"AwaitingModel","pending":{"call_model":{"session_messages":[{"id":"u1","role":"user","content":"hi","metadata":null}],"system_prompt":"sys","tool_schemas":[{"description":"Search the web","name":"web_search","parameters":{"type":"object"},"weight":1.5}],"rendered_runtime_tools":[{"description":"Search the web","name":"web_search","parameters":{"type":"object"},"weight":1.5}],"allow_tool_call":true,"state_ledger":{"objective":null,"constraints":[],"decisions":[],"open_questions":[],"touched_files":[],"tool_decisions":[],"artifact_states":[],"summary_refs":[],"history_refs":[],"retired":[],"retirement_artifacts":[]},"provider_context":{"transcript_revision":0,"sidecar_generation":0,"sidecar_byte_len":205,"compaction_passes_this_turn":0,"destructive_tiers_this_turn":0,"pressure_ratio_at_call_model_millis":11}},"execute":null,"execute_skip_guardrail_count":false,"auto_finalize_finish":false,"finalize_after_tool_stop":false,"summary_req":null,"summary_ctx":null,"tool_turn_parallel":0,"reject_count_this_turn":0,"artifact_ids":[],"tool_turn_request_order":[],"reject_tool_by_call_id":{}},"sidecar_builder":{"generation":1,"rolling_cache":[{"generation":0,"byte_len":205}],"last_ledger_json":"{\"objective\":null,\"constraints\":[],\"decisions\":[],\"open_questions\":[],\"touched_files\":[],\"tool_decisions\":[],\"artifact_states\":[],\"summary_refs\":[],\"history_refs\":[],\"retired\":[],\"retirement_artifacts\":[]}"},"model_requests_this_turn":1,"events":[{"type":"turn_started","turn_index":1,"allow_tool_call":true}],"turn_index":1,"cancelled":false,"terminal_action":null,"last_model_response_greedy_tool_names":[],"tools_blocked_nudges_this_turn":0,"compaction_passes_this_turn":0,"destructive_tiers_this_turn":0,"suppress_summary_tier":false}"###;

/// The pin, both directions: the value serializes to exactly the
/// golden bytes, and the golden bytes deserialize to a set whose raw
/// channel text and re-serialized bytes are unchanged.
fn assert_golden(set: &ToolSchemaSet, golden: &str) {
    let bytes = serde_json::to_string(set).expect("set serializes");
    assert_eq!(bytes, golden, "serialized bytes drifted from the golden");
    let restored: ToolSchemaSet =
        serde_json::from_str(golden).expect("golden bytes must deserialize");
    assert_eq!(restored.active_json(), set.active_json());
    assert_eq!(restored.active.len(), set.active.len());
    let reserialized = serde_json::to_string(&restored).expect("restored set serializes");
    assert_eq!(reserialized, golden, "round-tripped bytes drifted");
}

#[test]
fn golden_empty_set_bytes() {
    assert_golden(&ToolSchemaSet::default(), GOLDEN_EMPTY);
}

#[test]
fn golden_canonical_channel_bytes() {
    let set = ToolSchemaSet::parse(CANONICAL_CHANNEL).expect("canonical channel parses");
    assert_golden(&set, GOLDEN_CANONICAL);
}

#[test]
fn golden_noncanonical_raw_text_is_verbatim() {
    let set = ToolSchemaSet::parse(NONCANONICAL_CHANNEL).expect("noncanonical channel parses");
    assert_eq!(set.active_json(), NONCANONICAL_CHANNEL);
    assert_golden(&set, GOLDEN_NONCANONICAL);
}

#[test]
fn golden_rich_object_parameters_bytes() {
    // Entries with nested object parameters (arrays, numbers, bools,
    // null unions) built through the typed FFI path (`from_entries`):
    // raw text is the synthesized canonical rendering.
    let rich: Vec<SchemaEntry> = vec![
        serde_json::from_value(serde_json::json!({
            "name": "run_command",
            "description": "Run a command",
            "parameters": {
                "type": "object",
                "properties": {
                    "cmd": {"type": "string"},
                    "timeout": {"type": "number", "default": 1.5},
                    "flags": {"type": "array", "items": {"type": "string"}},
                    "dry_run": {"type": "boolean"}
                },
                "required": ["cmd"]
            },
            "reproducible": false,
            "max_result_bytes": 4096
        }))
        .expect("entry value"),
        serde_json::from_value(serde_json::json!({
            "name": "note",
            "description": "Leave a note",
            "parameters": {"type": "object", "properties": {"text": {"type": "string"}, "pinned": {"type": ["boolean", "null"]}}}
        }))
        .expect("entry value"),
    ];
    assert_golden(&ToolSchemaSet::from_entries(rich), GOLDEN_RICH);
}

fn create_fixture_session(eng: &AgentEngine) -> SessionHandle {
    eng.create_session(
        vec![AgentMessage {
            id: "u1".to_string(),
            role: AgentRole::User,
            content: "hi".to_string(),
            metadata: MessageMeta::new(),
        }],
        AgentConfig::default(),
        SessionCapabilities::default(),
        Value::Null,
        SessionIoParams::new(
            "anthropic".to_string(),
            200_000,
            NONCANONICAL_CHANNEL.to_string(),
            "sys".to_string(),
        )
        .expect("valid test io params"),
    )
    .expect("valid config creates session")
}

#[test]
fn golden_full_snapshot_bytes() {
    let eng = AgentEngine::new();
    let handle = create_fixture_session(&eng);
    let _ = eng.next_action(&handle);
    let snapshot = eng.export_snapshot(&handle);
    let bytes = serde_json::to_string(&snapshot).expect("snapshot serializes");
    assert_eq!(bytes, GOLDEN_SNAPSHOT, "snapshot bytes drifted");

    // The stored bytes load and re-export byte-identically: a restart
    // neither loses nor rewrites any part of the snapshot, schemas
    // included.
    let restored: SessionSnapshot =
        serde_json::from_str(GOLDEN_SNAPSHOT).expect("golden snapshot deserializes");
    assert_eq!(restored.schemas.active_json(), NONCANONICAL_CHANNEL);
    let eng2 = AgentEngine::new();
    let handle2 = eng2
        .import_snapshot(restored)
        .expect("golden snapshot imports");
    let reexported = eng2.export_snapshot(&handle2);
    let bytes2 = serde_json::to_string(&reexported).expect("re-export serializes");
    assert_eq!(bytes2, GOLDEN_SNAPSHOT, "re-exported bytes drifted");
}
