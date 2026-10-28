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
/// a ToolSchemaSet built from NONCANONICAL_CHANNEL.
const GOLDEN_SNAPSHOT: &str = include_str!("fixtures/enc308_a2_snapshot_golden.json");

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
