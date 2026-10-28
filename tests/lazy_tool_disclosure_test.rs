#[path = "support/mod.rs"]
mod support;

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta, ToolResultRecord};
use agent_core::policy::ContextPolicyTier;
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall};
use support::enclave_test_agent_config::enclave_test_agent_config;

const ACTIVE_SCHEMA_JSON: &str = r#"[
  {"name":"tool_discovery_tool","parameters":{}},
  {"name":"web_search","parameters":{}},
  {"name":"web_extract","parameters":{}}
]"#;

fn engine() -> AgentEngine {
    AgentEngine::new()
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

fn create_session(
    eng: &AgentEngine,
    cfg: AgentConfig,
    tools_schema_json: &str,
) -> agent_core::SessionHandle {
    eng.create_session(
        vec![AgentMessage {
            id: "u1".into(),
            role: AgentRole::User,
            content: "write".into(),
            metadata: MessageMeta::new(),
        }],
        cfg,
        capabilities(),
        serde_json::Value::Null,
        SessionIoParams::new(
            "anthropic".into(),
            200_000,
            tools_schema_json.into(),
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
    assert_eq!(
        tool_names(&request.tools_schema_json()),
        vec!["tool_discovery_tool", "web_search", "web_extract"]
    );
    // Discovery is real: advertised_tools lists only the active/callable subset, never the
    // deferred catalog. Deferred tools (search_chat_history, append_blocks, ...) must be
    // surfaced via tool_discovery_tool, not listed up front.
    assert_eq!(
        advertised,
        vec!["tool_discovery_tool", "web_search", "web_extract"]
    );
    // ENC-296: no schema text is stashed anymore (host owns the registry;
    // full schemas are wired provider-side), so there is nothing to read
    // back — the store holds no schema text at all.
    assert!(
        eng.search_context_references(&handle, "append_blocks".into(), 5)
            .is_empty(),
        "no schema text may be stashed in the ContextStore"
    );
}

#[test]
// LTD-09
fn ltd_09_rust_merges_activated_tool_names_in_first_seen_order() {
    const LTD_09_ACTIVE_SCHEMA_JSON: &str = r#"[
      {"name":"tool_discovery_tool","parameters":{}},
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
        tool_names(&next_request.tools_schema_json()),
        vec![
            "tool_discovery_tool",
            "web_search",
            "append_blocks",
            "search_current_artifact",
        ]
    );
    // ENC-296: the activated tools are not in the active channel (the
    // registry is deleted), so they render as name stubs for the host to
    // fill provider-side — while initially active tools keep full entries.
    let rendered: serde_json::Value = serde_json::from_str(&next_request.tools_schema_json())
        .expect("tools_schema_json must parse");
    let entries = rendered.as_array().expect("must be an array");
    assert_eq!(entries[0]["parameters"], serde_json::json!({}));
    assert_eq!(entries[1]["parameters"], serde_json::json!({}));
    assert_eq!(entries[2], serde_json::json!({"name": "append_blocks"}));
    assert_eq!(
        entries[3],
        serde_json::json!({"name": "search_current_artifact"})
    );
}

// ENC-296 focused regression: a tool activated mid-session that is NOT in
// the active channel renders as a name stub (never from a registry — the
// channel is deleted); a tool that IS in the active channel renders its
// full active entry.
#[test]
fn enc_296_activated_tool_renders_active_entry_or_name_stub() {
    use agent_core::tool_schema::ToolSchemaSet;

    let set =
        ToolSchemaSet::parse(r#"[{"name":"web_search","description":"Search","parameters":{}}]"#)
            .expect("valid channel");
    let rendered = set.render_active_schemas(&[
        agent_core::tool_name::ToolName::new("web_search").expect("valid test name"),
        agent_core::tool_name::ToolName::new("brand_new_tool").expect("valid test name"),
    ]);
    let json = serde_json::to_value(&rendered).expect("entries serialize");
    assert_eq!(
        json,
        serde_json::json!([
            {"name": "web_search", "description": "Search", "parameters": {}},
            {"name": "brand_new_tool"},
        ])
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

#[test]
// LTD-11
fn ltd_11_search_tools_passes_repair_after_deferred_rejection() {
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
                name: "tool_discovery_tool".into(),
                arguments_json: r#"{"capability":"start artifact"}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );

    let action = eng.next_action(&handle);
    let calls = match action {
        AgentAction::ExecuteTools(calls) => calls,
        other => {
            panic!("search_tools must reach host execution after repair, got {other:?}")
        }
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "tool_discovery_tool");
}
