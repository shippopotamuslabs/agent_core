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

fn engine() -> AgentEngine {
    AgentEngine::new()
}

fn search_discovery_config() -> AgentConfig {
    let mut config = enclave_test_agent_config();
    // ENC-297 D4: the catalog merge is deleted; the host catalog now travels
    // as typed ToolSpec values at config construction. The legacy
    // SEARCH_CATALOG_JSON channel text is parsed here into specs (the only
    // JSON->typed parse, at this test's construction edge).
    let mut semantics = agent_core::tool_semantics::ToolSemanticsConfig::from_json(
        r#"{
          "catalog": { "tools": [] },
          "discovery": {
            "tool_name": "tool_discovery_tool",
            "request_field_name": "capability",
            "gate_exemptions": [
              {
                "tool_name": "tool_discovery_tool",
                "activates": false,
                "reason": "discovery entry point"
              }
            ]
          },
          "argument_repairs": [],
          "canonical_shapes": [],
          "result_policies": [],
          "compaction": {},
          "greedy_extension_tools": []
        }"#,
    )
    .expect("search discovery semantics json must parse");
    semantics.catalog.tools = serde_json::from_str(SEARCH_CATALOG_JSON)
        .expect("search catalog json must parse as tool specs");
    config.tool_semantics = semantics;
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
            SEARCH_ACTIVE_SCHEMA_JSON.into(),
            "sys".into(),
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

#[test]
fn tsd_26_default_semantics_reference_search_tools() {
    let semantics =
        agent_core::tool_semantics::ToolSemanticsConfig::from_json(ENCLAVE_SEMANTICS_JSON)
            .expect("enclave semantics json must parse");
    let discovery = semantics
        .discovery
        .expect("enclave default semantics must configure discovery");
    assert_eq!(discovery.tool_name.as_str(), "tool_discovery_tool");
    assert_eq!(discovery.request_field_name, "capability");
    let entry_point = discovery
        .gate_exemption_for(
            &agent_core::tool_name::ToolName::new("tool_discovery_tool").expect("valid test name"),
        )
        .expect("discovery tool must carry a gate exemption");
    assert!(!entry_point.activates);
    let activating_exemption = discovery
        .gate_exemption_for(
            &agent_core::tool_name::ToolName::new("read_context_reference")
                .expect("valid test name"),
        )
        .expect("read_context_reference must carry a gate exemption");
    assert!(activating_exemption.activates);
}

#[test]
fn tsd_catalog_includes_search_tools_not_disclose() {
    let catalog = include_str!("../resources/tool_catalog.json");
    assert!(catalog.contains("\"name\": \"tool_discovery_tool\""));
    assert!(!catalog.contains("\"name\": \"disclose_tool_schemas\""));
}

#[test]
fn tsd_camelcase_model_name_gates_active_without_soft_failure() {
    // J1: the model emitting `webSearch` resolves through the same
    // model-output constructor as the repair pipeline, so the gate sees the
    // active `web_search` — no spurious "not active" soft-failure.
    let eng = engine();
    let handle = create_session(&eng);

    let _initial_request = model_request(&eng, &handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-1".into()),
                name: "webSearch".into(),
                arguments_json: r#"{"query":"x"}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );

    match eng.next_action(&handle) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name, "web_search");
        }
        other => panic!("expected ExecuteTools for camelCase active tool, got {other:?}"),
    }
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
            SEARCH_ACTIVE_SCHEMA_JSON.into(),
            "sys".into(),
        )
        .expect("valid test io params"),
    )
    .expect("valid config creates session")
}

#[test]
fn tsd_exempt_read_context_reference_is_callable_without_search_tools() {
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
        other => panic!("expected ExecuteTools for exempt tool, got {other:?}"),
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

#[test]
fn tsd_activating_exemption_merges_into_active_set() {
    // ENC-303: `activates == true` merges the activating exemption into the active set,
    // so a later rejection lists it in `available_tools`.
    let eng = engine();
    let handle = enclave_discovery_session(&eng, enclave_test_agent_config());

    let _initial_request = model_request(&eng, &handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![
                RawToolCall {
                    id: Some("call-read".into()),
                    name: "read_context_reference".into(),
                    arguments_json: r#"{"reference_id":"tool-output-call-1"}"#.into(),
                },
                RawToolCall {
                    id: Some("call-append".into()),
                    name: "append_blocks".into(),
                    arguments_json: r#"{"artifactId":"a1","blocks":[]}"#.into(),
                },
            ],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );

    match eng.next_action(&handle) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name, "read_context_reference");
        }
        other => panic!("expected ExecuteTools for exempt tool, got {other:?}"),
    }
    eng.submit_tool_results(
        &handle,
        vec![ToolResultRecord {
            call_id: "call-read".into(),
            result_json: r#"{"ok":true}"#.into(),
            truncated: false,
            soft_failure: false,
            activated_tool_names: Vec::new(),
            result_metadata: None,
        }],
    );

    let snapshot = handle.integration_test_snapshot();
    let rejection = snapshot
        .messages
        .iter()
        .rev()
        .find(|message| {
            message.role == AgentRole::Tool && message.metadata.call_id() == Some("call-append")
        })
        .expect("append_blocks rejection must be stored as a tool message");
    let payload: serde_json::Value =
        serde_json::from_str(&rejection.content).expect("rejection payload must be JSON");
    let available = payload["available_tools"]
        .as_array()
        .expect("rejection must list available_tools");
    assert!(
        available
            .iter()
            .any(|name| name == "read_context_reference"),
        "activating exemption must merge into the active set, got {available:?}"
    );
}

fn non_activating_probe_config() -> AgentConfig {
    let mut semantics = agent_core::tool_semantics::ToolSemanticsConfig::from_json(
        r#"{
          "catalog": { "tools": [] },
          "discovery": {
            "tool_name": "tool_discovery_tool",
            "request_field_name": "capability",
            "gate_exemptions": [
              {
                "tool_name": "probe_a",
                "activates": false,
                "reason": "non-activating probe"
              }
            ]
          },
          "argument_repairs": [],
          "canonical_shapes": [],
          "result_policies": [],
          "compaction": {},
          "greedy_extension_tools": []
        }"#,
    )
    .expect("probe semantics json must parse");
    semantics.catalog.tools = serde_json::from_str(
        r#"[
          {"name":"tool_discovery_tool","aliases":[],"reproducible":true},
          {"name":"probe_a","aliases":[],"reproducible":true},
          {"name":"probe_b","aliases":[],"reproducible":true}
        ]"#,
    )
    .expect("probe catalog json must parse as tool specs");
    let mut config = enclave_test_agent_config();
    config.tool_semantics = semantics;
    config
}

#[test]
fn tsd_non_activating_exemption_is_callable_without_merging() {
    // ENC-303: `activates == false` makes the tool callable now but does NOT
    // merge it into the active set — a later rejection omits it.
    let eng = engine();
    let handle = eng
        .create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "probe".into(),
                metadata: MessageMeta::new(),
            }],
            non_activating_probe_config(),
            capabilities(),
            serde_json::Value::Null,
            SessionIoParams::new(
                "anthropic".into(),
                200_000,
                r#"[{"name":"tool_discovery_tool","parameters":{}}]"#.into(),
                "sys".into(),
            )
            .expect("valid test io params"),
        )
        .expect("valid config creates session");

    let _initial_request = model_request(&eng, &handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![
                RawToolCall {
                    id: Some("call-a".into()),
                    name: "probe_a".into(),
                    arguments_json: r#"{}"#.into(),
                },
                RawToolCall {
                    id: Some("call-b".into()),
                    name: "probe_b".into(),
                    arguments_json: r#"{}"#.into(),
                },
            ],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );

    match eng.next_action(&handle) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name, "probe_a");
        }
        other => panic!("expected ExecuteTools for exempt tool, got {other:?}"),
    }
    eng.submit_tool_results(
        &handle,
        vec![ToolResultRecord {
            call_id: "call-a".into(),
            result_json: r#"{"ok":true}"#.into(),
            truncated: false,
            soft_failure: false,
            activated_tool_names: Vec::new(),
            result_metadata: None,
        }],
    );

    let snapshot = handle.integration_test_snapshot();
    let rejection = snapshot
        .messages
        .iter()
        .rev()
        .find(|message| {
            message.role == AgentRole::Tool && message.metadata.call_id() == Some("call-b")
        })
        .expect("probe_b rejection must be stored as a tool message");
    let payload: serde_json::Value =
        serde_json::from_str(&rejection.content).expect("rejection payload must be JSON");
    assert_eq!(
        payload["available_tools"],
        serde_json::json!(["tool_discovery_tool"]),
        "non-activating exemption must not merge into the active set"
    );
}

const NO_DISCOVERY_ACTIVE_SCHEMA_JSON: &str = r#"[
  {"name":"web_search","parameters":{}},
  {"name":"web_extract","parameters":{}}
]"#;

fn try_search_session(
    config: AgentConfig,
    schemas: &str,
) -> Result<agent_core::SessionHandle, agent_core::tool_semantics::ToolSemanticsError> {
    engine().create_session(
        vec![AgentMessage {
            id: "u1".into(),
            role: AgentRole::User,
            content: "write".into(),
            metadata: MessageMeta::new(),
        }],
        config,
        capabilities(),
        serde_json::Value::Null,
        SessionIoParams::new("anthropic".into(), 200_000, schemas.into(), "sys".into())
            .expect("valid test io params"),
    )
}

#[test]
fn tsd_lockout_discovery_without_exemption_or_schema_is_rejected() {
    // ENC-303 J1: discovery section present, non-empty initial active set,
    // discovery tool neither gate-exempt nor in the active schemas — the
    // discovery tool could never be called, so construction fails naming
    // the tool and the remediation.
    let mut config = search_discovery_config();
    config
        .tool_semantics
        .discovery
        .as_mut()
        .expect("search config has discovery")
        .gate_exemptions = Vec::new();
    let Err(agent_core::tool_semantics::ToolSemanticsError::InvalidDiscoveryConfig { message }) =
        try_search_session(config, NO_DISCOVERY_ACTIVE_SCHEMA_JSON)
    else {
        panic!("expected InvalidDiscoveryConfig");
    };
    assert!(
        message.contains("tool_discovery_tool"),
        "error must name the discovery tool, got: {message}"
    );
    assert!(
        message.contains("gate_exemptions") && message.contains("initial active schemas"),
        "error must state the remediation, got: {message}"
    );
}

#[test]
fn tsd_exempt_discovery_outside_active_set_constructs() {
    // ENC-303 J1: a self-exemption (activates == false) keeps the discovery
    // tool callable even when the active schemas do not list it.
    let config = search_discovery_config();
    try_search_session(config, NO_DISCOVERY_ACTIVE_SCHEMA_JSON)
        .expect("exempt discovery outside the active set must construct");
}

#[test]
fn tsd_in_schema_discovery_without_exemption_constructs() {
    // ENC-303 J1: a discovery tool listed in the initial active schemas
    // needs no self-exemption.
    let mut config = search_discovery_config();
    config
        .tool_semantics
        .discovery
        .as_mut()
        .expect("search config has discovery")
        .gate_exemptions = Vec::new();
    try_search_session(config, SEARCH_ACTIVE_SCHEMA_JSON)
        .expect("in-schema discovery without exemption must construct");
}

#[test]
fn tsd_empty_active_set_disables_discovery_gate() {
    // ENC-303 J1: an empty initial active set means the gate is disabled
    // (host has not opted in), so a discovery tool with no exemption and no
    // schema entry still constructs.
    let mut config = search_discovery_config();
    config
        .tool_semantics
        .discovery
        .as_mut()
        .expect("search config has discovery")
        .gate_exemptions = Vec::new();
    try_search_session(config, "[]").expect("empty active set must disable the gate");
}
