//! TDD Red — tool search discovery.

#[path = "support/mod.rs"]
mod support;

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta, ToolResultRecord};
use agent_core::policy::ContextPolicyTier;
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall};
use support::enclave_test_agent_config::enclave_test_agent_config;

// FRG-37: vendored snapshot of the Enclave production semantics file
// (`core/domain/src/main/resources/agent_loop_enclave_tool_semantics.json`).
// The old `include_str!("../../../core/...")` only resolved when this crate
// sat at `enclave/third_party/agent_core`; the vendored copy keeps both
// standalone checkouts and Enclave-layout runs green. Refresh the snapshot by
// hand when the Enclave source changes.
const ENCLAVE_SEMANTICS_JSON: &str = include_str!("support/enclave_tool_semantics.json");

const SEARCH_ACTIVE_SCHEMA_JSON: &str = r#"[
  {"name":"tool_discovery_tool","parameters":{}},
  {"name":"web_search","parameters":{}},
  {"name":"web_extract","parameters":{}}
]"#;

const SEARCH_CATALOG_JSON: &str = r#"[
  {"name":"tool_discovery_tool","aliases":[],"reproducible":true,"schema_hint":{"required_fields":[{"name":"capability","ty":"string","description":"capability"}],"defaults":{}}},
  {"name":"web_search","aliases":["websearch"],"reproducible":true,"schema_hint":{"required_fields":[{"name":"query","ty":"string","description":"query"}],"defaults":{}}},
  {"name":"web_extract","aliases":["web_fetch"],"reproducible":true,"schema_hint":{"required_fields":[{"name":"urls","ty":"array","description":"urls"}],"defaults":{}}},
  {"name":"append_blocks","aliases":[],"reproducible":false,"schema_hint":{"required_fields":[{"name":"artifactId","ty":"string","description":"id"},{"name":"blocks","ty":"array","description":"blocks"}],"defaults":{"isComplete":false}}}
]"#;

const SEARCH_REGISTRY_JSON: &str = r#"[
  {"name":"tool_discovery_tool","parameters":{}},
  {"name":"web_search","parameters":{}},
  {"name":"web_extract","parameters":{}},
  {"name":"append_blocks","parameters":{"properties":{"artifactId":{"type":"string"}}}}
]"#;

fn engine() -> AgentEngine {
    AgentEngine::new()
}

fn search_discovery_config() -> AgentConfig {
    let mut config = enclave_test_agent_config();
    config.tool_semantics = agent_core::tool_semantics::ToolSemanticsConfig::from_json(
        r#"{
          "catalog": { "tools": [] },
          "discovery": {
            "tool_name": "tool_discovery_tool",
            "request_field_name": "capability",
            "always_callable": true
          },
          "argument_repairs": [],
          "canonical_shapes": [],
          "result_policies": [],
          "compaction": {},
          "greedy_extension_tools": []
        }"#,
    )
    .expect("search discovery semantics json must parse");
    config
}

fn capabilities() -> SessionCapabilities {
    SessionCapabilities {
        tool_schema_deferral_supported: true,
        context_policy_tier: ContextPolicyTier::HostedMutable,
        ..SessionCapabilities::default()
    }
}

fn create_session(eng: &AgentEngine) -> agent_core::SessionHandle {
    eng.create_session(
        vec![AgentMessage {
            id: "u1".into(),
            role: AgentRole::User,
            content: "write".into(),
            metadata: MessageMeta::new(),
        }],
        search_discovery_config(),
        capabilities(),
        serde_json::Value::Null,
        SessionIoParams::new(
            "anthropic".into(),
            200_000,
            SEARCH_CATALOG_JSON.into(),
            SEARCH_REGISTRY_JSON.into(),
            SEARCH_ACTIVE_SCHEMA_JSON.into(),
            "sys".into(),
        ),
    )
}

fn model_request(eng: &AgentEngine, handle: &agent_core::SessionHandle) -> ModelRequest {
    match eng.next_action(handle) {
        AgentAction::CallModel(request) => request,
        other => panic!("expected CallModel, got {other:?}"),
    }
}

#[test]
fn tsd_26_default_semantics_reference_search_tools() {
    let semantics =
        agent_core::tool_semantics::ToolSemanticsConfig::from_json(ENCLAVE_SEMANTICS_JSON)
            .expect("enclave semantics json must parse");
    let discovery = semantics
        .discovery
        .expect("enclave default semantics must configure discovery");
    assert_eq!(discovery.tool_name, "tool_discovery_tool");
    assert_eq!(discovery.request_field_name, "capability");
    assert!(discovery.always_callable);
    assert!(discovery
        .companion_callable_tools
        .contains(&"read_context_reference".to_string()));
}

#[test]
fn tsd_catalog_includes_search_tools_not_disclose() {
    let catalog = include_str!("../resources/tool_catalog.json");
    assert!(catalog.contains("\"name\": \"tool_discovery_tool\""));
    assert!(!catalog.contains("\"name\": \"disclose_tool_schemas\""));
}

#[test]
fn tsd_26_engine_rejects_append_blocks_before_search_activation() {
    let eng = engine();
    let handle = create_session(&eng);

    let _initial_request = model_request(&eng, &handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-1".into()),
                name: "append_blocks".into(),
                arguments_json:
                    r#"{"artifactId":"artifact-1","blocks":[{"type":"paragraph","text":"Hi"}]}"#
                        .into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );

    let _repair_request = model_request(&eng, &handle);
    let snapshot = handle.integration_test_snapshot();
    let rejection = snapshot
        .messages
        .iter()
        .rev()
        .find(|message| {
            message.role == AgentRole::Tool
                && !message
                    .metadata
                    .extra("agent_tool_budget")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        })
        .expect("deferred tool rejection must be stored as a tool message");
    let payload: serde_json::Value =
        serde_json::from_str(&rejection.content).expect("rejection payload must be JSON");

    assert_eq!(
        payload["error"].as_str(),
        Some("Tool 'append_blocks' is not active. Call tool_discovery_tool first.")
    );
    assert_eq!(
        payload["available_tools"],
        serde_json::json!(["tool_discovery_tool", "web_search", "web_extract"])
    );
}

fn enclave_discovery_session(eng: &AgentEngine, config: AgentConfig) -> agent_core::SessionHandle {
    eng.create_session(
        vec![AgentMessage {
            id: "u1".into(),
            role: AgentRole::User,
            content: "research".into(),
            metadata: MessageMeta::new(),
        }],
        config,
        capabilities(),
        serde_json::Value::Null,
        SessionIoParams::new(
            "anthropic".into(),
            200_000,
            SEARCH_CATALOG_JSON.into(),
            SEARCH_REGISTRY_JSON.into(),
            SEARCH_ACTIVE_SCHEMA_JSON.into(),
            "sys".into(),
        ),
    )
}

#[test]
fn tsd_companion_read_context_reference_is_callable_without_search_tools() {
    let eng = engine();
    let handle = enclave_discovery_session(&eng, enclave_test_agent_config());

    let _initial_request = model_request(&eng, &handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-read".into()),
                name: "read_context_reference".into(),
                arguments_json: r#"{"reference_id":"tool-output-call-1"}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );

    match eng.next_action(&handle) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name, "read_context_reference");
        }
        other => panic!("expected ExecuteTools for companion tool, got {other:?}"),
    }
}

#[test]
fn tsd_spill_retrieval_hint_activates_read_context_reference() {
    let mut config = enclave_test_agent_config();
    config.tool_spill_threshold_bytes = 64;
    let eng = engine();
    let handle = enclave_discovery_session(&eng, config);

    let _initial_request = model_request(&eng, &handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-extract".into()),
                name: "web_extract".into(),
                arguments_json: r#"{"urls":["https://example.com"]}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );

    let action = eng.next_action(&handle);
    let AgentAction::ExecuteTools(calls) = action else {
        panic!("expected ExecuteTools, got {action:?}");
    };
    assert_eq!(calls[0].name, "web_extract");

    let huge = "x".repeat(128);
    eng.submit_tool_results(
        &handle,
        vec![ToolResultRecord {
            call_id: "call-extract".into(),
            result_json: huge,
            truncated: false,
            soft_failure: false,
            activated_tool_names: Vec::new(),
            result_metadata: None,
        }],
    );

    let snapshot = handle.integration_test_snapshot();
    let spilled = snapshot
        .messages
        .iter()
        .find(|message| message.role == AgentRole::Tool && message.content.contains("context_ref"))
        .expect("oversized tool result must spill to context_ref envelope");
    assert!(spilled.content.contains("read_context_reference"));

    let _follow_up = model_request(&eng, &handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-read".into()),
                name: "read_context_reference".into(),
                arguments_json: r#"{"reference_id":"tool-output-call-extract"}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );

    match eng.next_action(&handle) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(calls[0].name, "read_context_reference");
        }
        other => panic!("expected ExecuteTools after spill hint activation, got {other:?}"),
    }
}
