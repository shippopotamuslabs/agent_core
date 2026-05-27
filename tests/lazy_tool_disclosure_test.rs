use std::sync::Arc;

#[path = "support/mod.rs"]
mod support;

use support::enclave_test_agent_config::enclave_test_agent_config;
use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::AgentError;
use agent_core::host::{AgentToolExecutor, AgentTransport, NoOpSummarizer};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::policy::ContextPolicyTier;
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall};

const CATALOG_JSON: &str = r#"[
  {"name":"disclose_tool_schemas","aliases":[],"reproducible":true,"schema_hint":{"required_fields":[{"name":"tool_names","ty":"array","description":"tool names"}],"defaults":{}}},
  {"name":"web_search","aliases":["websearch"],"reproducible":true,"schema_hint":{"required_fields":[{"name":"query","ty":"string","description":"query"}],"defaults":{}}},
  {"name":"web_extract","aliases":["web_fetch"],"reproducible":true,"schema_hint":{"required_fields":[{"name":"urls","ty":"array","description":"urls"}],"defaults":{}}},
  {"name":"search_chat_history","aliases":[],"reproducible":true,"schema_hint":null},
  {"name":"append_blocks","aliases":[],"reproducible":false,"schema_hint":{"required_fields":[{"name":"artifactId","ty":"string","description":"id"},{"name":"blocks","ty":"array","description":"blocks"}],"defaults":{"isComplete":false}}},
  {"name":"search_current_artifact","aliases":[],"reproducible":true,"schema_hint":null}
]"#;

const ACTIVE_SCHEMA_JSON: &str = r#"[
  {"name":"disclose_tool_schemas","parameters":{}},
  {"name":"web_search","parameters":{}},
  {"name":"web_extract","parameters":{}}
]"#;

const REGISTRY_JSON: &str = r#"[
  {"name":"disclose_tool_schemas","parameters":{}},
  {"name":"web_search","parameters":{}},
  {"name":"web_extract","parameters":{}},
  {"name":"search_chat_history","parameters":{}},
  {"name":"append_blocks","parameters":{"properties":{"artifactId":{"type":"string"}}}},
  {"name":"search_current_artifact","parameters":{}}
]"#;

struct PanicTransport;

impl AgentTransport for PanicTransport {
    fn complete_model_turn(&self, _: ModelRequest) -> Result<ModelResponse, AgentError> {
        panic!("transport is not used by engine-driven red tests");
    }
}

struct PanicExecutor;

impl AgentToolExecutor for PanicExecutor {
    fn execute_tools(&self, _: Vec<ToolCallRecord>) -> Result<Vec<ToolResultRecord>, AgentError> {
        panic!("executor is not used by engine-driven red tests");
    }
}

fn engine() -> AgentEngine<PanicTransport, PanicExecutor, NoOpSummarizer> {
    AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExecutor),
        Arc::new(NoOpSummarizer),
    )
}

fn config(enable_deferral: bool) -> AgentConfig {
    AgentConfig {
        enable_tool_schema_deferral: enable_deferral,
        ..enclave_test_agent_config()
    }
}

fn capabilities() -> SessionCapabilities {
    SessionCapabilities {
        tool_schema_deferral_supported: true,
        context_policy_tier: ContextPolicyTier::HostedMutable,
        ..SessionCapabilities::default()
    }
}

fn create_session<E: AgentToolExecutor + 'static>(
    eng: &AgentEngine<PanicTransport, E, NoOpSummarizer>,
    cfg: AgentConfig,
    tools_schema_json: &str,
) -> agent_core::SessionHandle {
    eng.create_session(
        vec![AgentMessage {
            id: "u1".into(),
            role: AgentRole::User,
            content: "write".into(),
            metadata: serde_json::Value::Null,
        }],
        cfg,
        capabilities(),
        serde_json::Value::Null,
        SessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tool_catalog_json: CATALOG_JSON.into(),
            tool_schema_registry_json: REGISTRY_JSON.into(),
            tools_schema_json: tools_schema_json.into(),
            system_prompt: "sys".into(),
        },
    )
}

fn model_request<E: AgentToolExecutor + 'static>(
    eng: &AgentEngine<PanicTransport, E, NoOpSummarizer>,
    handle: &agent_core::SessionHandle,
) -> ModelRequest {
    match eng.next_action(handle) {
        AgentAction::CallModel(request) => request,
        other => panic!("expected CallModel, got {other:?}"),
    }
}

fn tool_names(schema_json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(schema_json)
        .expect("schema json must parse")
        .as_array()
        .expect("schema json must be an array")
        .iter()
        .map(|entry| {
            entry
                .get("name")
                .and_then(serde_json::Value::as_str)
                .expect("schema entry must contain name")
                .to_string()
        })
        .collect()
}

fn tool_result(call_id: &str, activated_tool_names: Vec<&str>) -> ToolResultRecord {
    ToolResultRecord {
        call_id: call_id.into(),
        result_json: serde_json::json!({ "status": "ok" }).to_string(),
        truncated: false,
        soft_failure: false,
        activated_tool_names: activated_tool_names
            .into_iter()
            .map(str::to_string)
            .collect(),
        result_metadata: None,
    }
}

#[test]
// LTD-08
fn ltd_08_rust_model_requests_keep_active_schemas_separate_from_catalog_and_registry() {
    let eng = engine();
    let handle = create_session(&eng, config(true), ACTIVE_SCHEMA_JSON);

    let request = model_request(&eng, &handle);
    let index = request
        .tool_context_index
        .as_ref()
        .expect("CallModel request must carry a tool context index");
    let advertised: Vec<&str> = index
        .advertised_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    let stored_registry = eng
        .read_context_reference(&handle, index.full_schema_ref.clone())
        .expect("full schema registry reference must be readable");

    assert_eq!(
        tool_names(&request.tools_schema_json),
        vec!["disclose_tool_schemas", "web_search", "web_extract"]
    );
    assert_eq!(
        advertised,
        vec![
            "disclose_tool_schemas",
            "web_search",
            "web_extract",
            "search_chat_history",
            "append_blocks",
            "search_current_artifact",
        ]
    );
    assert!(stored_registry.contains("\"append_blocks\""));
}

#[test]
// LTD-09
fn ltd_09_rust_merges_activated_tool_names_in_first_seen_order() {
    const LTD_09_ACTIVE_SCHEMA_JSON: &str = r#"[
      {"name":"disclose_tool_schemas","parameters":{}},
      {"name":"web_search","parameters":{}}
    ]"#;

    let eng = engine();
    let handle = create_session(&eng, config(false), LTD_09_ACTIVE_SCHEMA_JSON);

    let _initial_request = model_request(&eng, &handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-1".into()),
                name: "web_search".into(),
                arguments_json: r#"{"query":"tools"}"#.into(),
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
        vec![tool_result(
            "call-1",
            vec!["append_blocks", "search_current_artifact", "append_blocks"],
        )],
    );

    let next_request = model_request(&eng, &handle);
    assert_eq!(
        tool_names(&next_request.tools_schema_json),
        vec![
            "disclose_tool_schemas",
            "web_search",
            "append_blocks",
            "search_current_artifact",
        ]
    );
}

#[test]
// LTD-10
fn ltd_10_direct_call_to_deferred_tool_fails_closed_before_disclosure() {
    let eng = engine();
    let handle = create_session(&eng, config(false), ACTIVE_SCHEMA_JSON);

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
                    .get("agent_tool_budget")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        })
        .expect("deferred tool rejection must be stored as a tool message");
    let payload: serde_json::Value =
        serde_json::from_str(&rejection.content).expect("rejection payload must be JSON");

    assert_eq!(
        payload["error"].as_str(),
        Some("Tool 'append_blocks' is not active. Call disclose_tool_schemas first.")
    );
    assert_eq!(
        payload["available_tools"],
        serde_json::json!(["disclose_tool_schemas", "web_search", "web_extract"])
    );
}

#[test]
// LTD-11
fn ltd_11_disclose_tool_schemas_passes_repair_after_deferred_rejection() {
    let eng = engine();
    let handle = create_session(&eng, config(true), ACTIVE_SCHEMA_JSON);

    let _initial_request = model_request(&eng, &handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-deferred".into()),
                name: "append_blocks".into(),
                arguments_json:
                    r#"{"artifactId":"artifact-1","blocks":[{"type":"paragraph","text":"Hi"}]}"#
                        .into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    let _rejection_turn = model_request(&eng, &handle);

    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-disclose".into()),
                name: "disclose_tool_schemas".into(),
                arguments_json: r#"{"tool_names":["start_artifact"]}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );

    let action = eng.next_action(&handle);
    let calls = match action {
        AgentAction::ExecuteTools(calls) => calls,
        other => {
            panic!("disclose_tool_schemas must reach host execution after repair, got {other:?}")
        }
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "disclose_tool_schemas");
}
