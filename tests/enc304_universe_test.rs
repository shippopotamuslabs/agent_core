//! ENC-304 acceptance: the session tool world as a `ToolUniverse`.
//!
//! The in-module unit tests in `src/tool_universe.rs` cover the model in
//! isolation; these tests prove the wiring: a session built from the
//! enclave fixture semantics + a schema set resolves entry / spec /
//! result-policy by `ToolName` through the universe, admission grows the
//! world without activating, and an agnostic session still merges host
//! activations for names the world does not know (ENC-296:
//! host owns the registry).

#[path = "support/enclave_test_semantics.rs"]
mod enclave;

use agent_core::action::AgentAction;
use agent_core::catalog::ToolSpec;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta, ToolResultRecord};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::tool_name::ToolName;
use agent_core::tool_schema::{SchemaEntry, ToolSchemaSet};
use agent_core::tool_semantics::{ToolSemanticsConfig, ToolSemanticsError};
use agent_core::tool_universe::ToolUniverse;
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall};
use serde_json::Value;

fn tn(name: &str) -> ToolName {
    ToolName::new(name).expect("test tool name must be valid")
}

/// Schema channel carrying two enclave-catalog tools, in this order.
const FIXTURE_SCHEMA_JSON: &str = r#"[
  {"name":"web_search","description":"Search the web","parameters":{}},
  {"name":"append_blocks","description":"Append blocks","parameters":{}}
]"#;

fn fixture_schemas() -> ToolSchemaSet {
    ToolSchemaSet::parse(FIXTURE_SCHEMA_JSON).expect("fixture schema channel must parse")
}

fn fixture_universe() -> ToolUniverse {
    ToolUniverse::build(fixture_schemas(), enclave::enclave_test_semantics())
        .expect("enclave fixture world must build")
}

fn create_session(
    eng: &AgentEngine,
    config: AgentConfig,
    schemas: &str,
) -> agent_core::SessionHandle {
    eng.create_session(
        vec![AgentMessage {
            id: "u1".to_string(),
            role: AgentRole::User,
            content: "hi".to_string(),
            metadata: MessageMeta::new(),
        }],
        config,
        SessionCapabilities::default(),
        Value::Null,
        SessionIoParams::new(
            "anthropic".to_string(),
            200_000,
            schemas.to_string(),
            "sys".to_string(),
        )
        .expect("valid test io params"),
    )
    .expect("valid config creates session")
}

fn model_request(eng: &AgentEngine, handle: &agent_core::SessionHandle) -> ModelRequest {
    match eng.next_action(handle) {
        AgentAction::CallModel(request) => request,
        other => panic!("expected CallModel, got {other:?}"),
    }
}

fn tool_names(schema_json: &str) -> Vec<String> {
    serde_json::from_str::<Value>(schema_json)
        .expect("schema json must parse")
        .as_array()
        .expect("schema json must be an array")
        .iter()
        .map(|entry| {
            entry
                .get("name")
                .and_then(Value::as_str)
                .expect("schema entry must contain name")
                .to_string()
        })
        .collect()
}

fn entry(name: &str, description: &str) -> SchemaEntry {
    SchemaEntry::from(serde_json::json!({
        "name": name,
        "description": description,
        "parameters": {},
    }))
}

fn spec(name: &str, aliases: &[&str]) -> ToolSpec {
    ToolSpec {
        name: tn(name),
        aliases: aliases.iter().map(|alias| tn(alias)).collect(),
        reproducible: true,
        schema_hint: None,
    }
}

// ---------------------------------------------------------------------------
// Scenario 1: enclave fixture semantics + schema set -> session + universe
// whose lookups resolve entry / spec / result-policy by ToolName.
// ---------------------------------------------------------------------------

#[test]
fn enc304_fixture_universe_resolves_entry_spec_and_policy_by_name() {
    let universe = fixture_universe();

    // The initial active set is the schema channel's names, first-seen order.
    assert_eq!(
        universe.active_names(),
        &[tn("web_search"), tn("append_blocks")]
    );

    // Entry lookup by ToolName (and the typed require_entry seam).
    let web_search = universe
        .entry(&tn("web_search"))
        .expect("web_search entry must resolve");
    assert_eq!(web_search.name, tn("web_search"));
    assert_eq!(web_search.description(), "Search the web");
    assert_eq!(
        universe
            .require_entry(&tn("append_blocks"))
            .expect("append_blocks entry must resolve")
            .description(),
        "Append blocks"
    );
    assert!(universe.entry(&tn("generate_image")).is_none());

    // Spec lookup resolves canonical names and declared aliases alike.
    let canonical = universe
        .spec(&tn("web_search"))
        .expect("web_search spec must resolve");
    assert_eq!(canonical.name, tn("web_search"));
    assert!(canonical.reproducible);
    assert_eq!(
        universe
            .spec(&tn("websearch"))
            .expect("alias websearch must resolve")
            .name,
        tn("web_search")
    );
    assert_eq!(
        universe
            .spec(&tn("web_fetch"))
            .expect("alias web_fetch must resolve")
            .name,
        tn("web_extract")
    );
    assert!(universe.spec(&tn("ghost_tool")).is_none());
    // The enclave fixture catalog is the shipped 20-spec catalog.
    assert_eq!(universe.catalog().len(), 20);
    assert_eq!(universe.semantics().catalog.tools.len(), 20);

    // Result policy by ToolName: append_blocks carries the fixture's only
    // policy; web_search has none.
    let policy = universe
        .result_policy(&tn("append_blocks"))
        .expect("append_blocks result policy must resolve");
    assert!(policy.spill_stored_copy_only);
    assert!(policy.skip_result_truncation);
    assert!(policy.preserve_in_micro_compaction);
    assert!(universe.result_policy(&tn("web_search")).is_none());

    // Schema hint by ToolName comes from the resolved spec.
    let hint = universe
        .schema_hint(&tn("web_search"))
        .expect("web_search schema hint must resolve");
    assert!(
        hint.required_fields
            .iter()
            .any(|field| field.name == "query"),
        "web_search hint must require the query field"
    );

    // Render: active order, full entries for carried tools.
    let rendered = universe.render_active_schemas();
    assert_eq!(
        rendered.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
        vec![tn("web_search"), tn("append_blocks")]
    );
    assert_eq!(rendered[0].description(), "Search the web");
    assert!(universe.active_json().contains("web_search"));
}

#[test]
fn enc304_fixture_session_constructs_and_advertises_universe_active_set() {
    // The same fixture inputs, driven through create_session: construction
    // succeeds (the universe build owns the validation) and the model's
    // first request carries exactly the universe's active schemas.
    let eng = AgentEngine::new();
    let handle = create_session(
        &eng,
        AgentConfig {
            tool_semantics: enclave::enclave_test_semantics(),
            ..AgentConfig::default()
        },
        FIXTURE_SCHEMA_JSON,
    );

    let request = model_request(&eng, &handle);
    assert_eq!(
        tool_names(&request.tools_schema_json()),
        vec!["web_search", "append_blocks"]
    );
    let advertised: Vec<&str> = request
        .tool_schemas
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(advertised, vec!["web_search", "append_blocks"]);
}

// ---------------------------------------------------------------------------
// Scenario 2: require_entry on a missing name -> UnknownToolReference.
// ---------------------------------------------------------------------------

#[test]
fn enc304_require_entry_missing_name_is_unknown_tool_reference() {
    let universe = fixture_universe();

    assert_eq!(
        universe
            .require_entry(&tn("ghost_tool"))
            .expect_err("missing entry must error"),
        ToolSemanticsError::UnknownToolReference {
            section: "lookup".to_string(),
            name: "ghost_tool".to_string(),
        }
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: admission at the model level.
// ---------------------------------------------------------------------------

#[test]
fn enc304_admit_joins_world_and_resolves() {
    let mut universe = fixture_universe();
    let catalog_len_before = universe.catalog().len();

    universe
        .admit(
            entry("fetch_page", "Fetch a page"),
            Some(spec("fetch_page", &["fetchpage"])),
        )
        .expect("admission of a new tool must succeed");

    // Entry joins and resolves, by require_entry as well as entry.
    assert_eq!(
        universe
            .require_entry(&tn("fetch_page"))
            .expect("admitted entry must resolve")
            .description(),
        "Fetch a page"
    );
    assert_eq!(universe.schemas().active.len(), 3);
    // The schema set's raw channel resynced with the admitted entry.
    assert!(universe.active_json().contains("fetch_page"));

    // Spec joins and resolves by canonical name and by its new alias.
    assert_eq!(
        universe
            .spec(&tn("fetch_page"))
            .expect("admitted spec must resolve")
            .name,
        tn("fetch_page")
    );
    assert_eq!(
        universe
            .spec(&tn("fetchpage"))
            .expect("admitted alias must resolve")
            .name,
        tn("fetch_page")
    );
    assert_eq!(universe.catalog().len(), catalog_len_before + 1);
    // No result policy was admitted with it.
    assert!(universe.result_policy(&tn("fetch_page")).is_none());
}

#[test]
fn enc304_admit_duplicate_name_rejected_and_world_unchanged() {
    let mut universe = fixture_universe();
    let active_json_before = universe.active_json().to_string();
    let active_before = universe.active_names().to_vec();

    assert_eq!(
        universe
            .admit(
                entry("web_search", "Duplicate search"),
                Some(spec("web_search", &[])),
            )
            .expect_err("duplicate admission must fail"),
        ToolSemanticsError::DuplicateToolName {
            normalized_name: "web_search".to_string(),
        }
    );

    // World unchanged: same entries, same catalog, same active set, and the
    // original entry was not overwritten.
    assert_eq!(universe.schemas().active.len(), 2);
    assert_eq!(universe.catalog().len(), 20);
    assert_eq!(universe.active_names(), active_before.as_slice());
    assert_eq!(universe.active_json(), active_json_before);
    assert_eq!(
        universe
            .entry(&tn("web_search"))
            .expect("original entry must remain")
            .description(),
        "Search the web"
    );
}

#[test]
fn enc304_admit_alias_colliding_with_canonical_name_rejected() {
    let mut universe = fixture_universe();
    let active_json_before = universe.active_json().to_string();

    // The candidate's alias shadows the existing canonical name web_search.
    assert_eq!(
        universe
            .admit(
                entry("summarize_page", "Summarize a page"),
                Some(spec("summarize_page", &["web_search"])),
            )
            .expect_err("alias shadowing a canonical name must fail"),
        ToolSemanticsError::AliasCollision {
            alias: "web_search".to_string(),
            first_tool: "web_search".to_string(),
            second_tool: "summarize_page".to_string(),
        }
    );

    // World unchanged: the candidate joined neither entries nor catalog,
    // and web_search still resolves to its own spec.
    assert!(universe.entry(&tn("summarize_page")).is_none());
    assert!(universe.spec(&tn("summarize_page")).is_none());
    assert_eq!(
        universe
            .spec(&tn("web_search"))
            .expect("web_search spec must remain")
            .name,
        tn("web_search")
    );
    assert_eq!(universe.schemas().active.len(), 2);
    assert_eq!(universe.catalog().len(), 20);
    assert_eq!(universe.active_json(), active_json_before);
}

#[test]
fn enc304_admit_entry_name_colliding_with_existing_alias_rejected() {
    let mut universe = fixture_universe();

    // websearch is web_search's declared alias; a new entry may not take it.
    assert_eq!(
        universe
            .admit(entry("websearch", "Shadowing entry"), None)
            .expect_err("entry name shadowing an alias must fail"),
        ToolSemanticsError::AliasCollision {
            alias: "websearch".to_string(),
            first_tool: "web_search".to_string(),
            second_tool: "websearch".to_string(),
        }
    );

    assert!(universe.entry(&tn("websearch")).is_none());
    assert_eq!(universe.schemas().active.len(), 2);
    assert_eq!(universe.catalog().len(), 20);
}

#[test]
fn enc304_admit_spec_naming_a_different_tool_rejected() {
    let mut universe = fixture_universe();
    let active_json_before = universe.active_json().to_string();

    assert_eq!(
        universe
            .admit(
                entry("fetch_page", "Fetch a page"),
                Some(spec("other_tool", &[])),
            )
            .expect_err("spec/entry name mismatch must fail"),
        ToolSemanticsError::UnknownToolReference {
            section: "admit".to_string(),
            name: "other_tool".to_string(),
        }
    );

    // World unchanged.
    assert!(universe.entry(&tn("fetch_page")).is_none());
    assert!(universe.spec(&tn("other_tool")).is_none());
    assert_eq!(universe.schemas().active.len(), 2);
    assert_eq!(universe.catalog().len(), 20);
    assert_eq!(universe.active_json(), active_json_before);
}

#[test]
fn enc304_admit_does_not_activate_then_activate_dedupes_first_seen() {
    let mut universe = fixture_universe();
    universe
        .admit(
            entry("fetch_page", "Fetch a page"),
            Some(spec("fetch_page", &["fetchpage"])),
        )
        .expect("admission must succeed");

    // Admission != activation: the admitted tool is in the world but not
    // in the active set, and does not render as an active schema.
    assert_eq!(
        universe.active_names(),
        &[tn("web_search"), tn("append_blocks")]
    );
    assert_eq!(universe.render_active_schemas().len(), 2);

    // activate() adds it, deduped, keeping first-seen order: repeats of
    // admitted and pre-existing names do not move or duplicate them.
    universe.activate([tn("fetch_page"), tn("web_search"), tn("fetch_page")]);
    assert_eq!(
        universe.active_names(),
        &[tn("web_search"), tn("append_blocks"), tn("fetch_page")]
    );

    // Once active, the admitted tool renders its full entry, not a stub.
    let rendered = universe.render_active_schemas();
    assert_eq!(rendered.len(), 3);
    assert_eq!(rendered[2].name, tn("fetch_page"));
    assert_eq!(rendered[2].description(), "Fetch a page");
}

// ---------------------------------------------------------------------------
// Scenario 4: agnostic config session constructs, and host activation of a
// name unknown to the world still merges into the active set.
// ---------------------------------------------------------------------------

#[test]
fn enc304_agnostic_session_constructs_and_merges_unknown_host_activation() {
    let eng = AgentEngine::new();
    // AgentConfig::default carries the agnostic semantics (no catalog, no
    // semantics sections); a small schema set is the whole tool world.
    let handle = create_session(
        &eng,
        AgentConfig::default(),
        r#"[{"name":"web_search","parameters":{}}]"#,
    );

    let initial = model_request(&eng, &handle);
    assert_eq!(tool_names(&initial.tools_schema_json()), vec!["web_search"]);

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

    // The host activates names the world has never heard of (no entry, no
    // spec): ENC-296 host-owns-registry, so they merge anyway, deduped in
    // first-seen order (ltd_09 shape).
    eng.submit_tool_results(
        &handle,
        vec![ToolResultRecord {
            call_id: "call-1".to_string(),
            result_json: serde_json::json!({ "status": "ok" }).to_string(),
            truncated: false,
            soft_failure: false,
            activated_tool_names: vec![
                tn("host_only_tool"),
                tn("another_host_tool"),
                tn("host_only_tool"),
            ],
            result_metadata: None,
        }],
    );

    let next = model_request(&eng, &handle);
    assert_eq!(
        tool_names(&next.tools_schema_json()),
        vec!["web_search", "host_only_tool", "another_host_tool"]
    );
    // The unknown names render as name stubs (the host wires their full
    // schemas provider-side), while the initially active tool keeps its
    // full entry.
    let rendered: Value =
        serde_json::from_str(&next.tools_schema_json()).expect("tools_schema_json must parse");
    let entries = rendered.as_array().expect("must be an array");
    assert_eq!(entries[0]["parameters"], serde_json::json!({}));
    assert_eq!(entries[1], serde_json::json!({"name": "host_only_tool"}));
    assert_eq!(entries[2], serde_json::json!({"name": "another_host_tool"}));
}

#[test]
fn enc304_agnostic_universe_builds_with_no_catalog_or_semantics() {
    // Model-level companion to the session test: the agnostic semantics
    // build a valid, empty-catalog world over a small schema set.
    let universe = ToolUniverse::build(
        ToolSchemaSet::parse(r#"[{"name":"web_search","parameters":{}}]"#)
            .expect("schema channel must parse"),
        ToolSemanticsConfig::agnostic(),
    )
    .expect("agnostic world must build");

    assert_eq!(universe.active_names(), &[tn("web_search")]);
    assert!(universe.catalog().is_empty());
    assert!(universe.spec(&tn("web_search")).is_none());
    assert!(universe.result_policy(&tn("web_search")).is_none());
    assert!(universe.entry(&tn("web_search")).is_some());
}
