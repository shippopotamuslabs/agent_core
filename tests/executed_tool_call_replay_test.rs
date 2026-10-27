//! FEAT-executed-tool-call-replay: provider-facing `raw_tool_calls` metadata scenarios
//! ETR-R-01 … ETR-R-09, ETR-M-01 … ETR-M-03.

#[path = "support/mod.rs"]
mod support;

use support::enclave_test_agent_config::enclave_test_agent_config;

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams, SessionTestSnapshot};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta, ToolResultRecord};
use agent_core::session::SessionCapabilities;
use agent_core::turn_io::{FinishReason, ModelResponse, RawToolCall};

const VALID_DSML_WEB_SEARCH: &str = concat!(
    "<|DSML|tool_calls><|DSML|invoke name=\"web_search\">",
    "<|DSML|parameter name=\"query\" string=\"true\">rust</|DSML|parameter>",
    "</|DSML|invoke></|DSML|tool_calls>"
);

fn io_large() -> SessionIoParams {
    SessionIoParams {
        provider: "openrouter".into(),
        context_window_tokens: 200_000,
        tool_catalog_json: "[]".into(),
        tool_schema_registry_json: "[]".into(),
        tools_schema_json: "[]".into(),
        system_prompt: "sys".into(),
    }
}

fn user_msg() -> AgentMessage {
    AgentMessage {
        id: "u1".into(),
        role: AgentRole::User,
        content: "hi".into(),
        metadata: MessageMeta::new(),
    }
}

fn new_session(eng: &AgentEngine) -> agent_core::engine::SessionHandle {
    eng.create_session(
        vec![user_msg()],
        enclave_test_agent_config(),
        SessionCapabilities {
            summarization_allowed: false,
            kv_fixed: false,
            tool_result_budget_tokens: None,
            ..Default::default()
        },
        serde_json::Value::Null,
        io_large(),
    )
}

fn last_assistant_message(snapshot: &SessionTestSnapshot) -> &AgentMessage {
    snapshot
        .messages
        .iter()
        .rev()
        .find(|m| m.role == AgentRole::Assistant)
        .expect("last assistant message")
}

fn raw_tool_calls_array(snapshot: &SessionTestSnapshot) -> &[RawToolCall] {
    last_assistant_message(snapshot).metadata.raw_tool_calls()
}

fn raw_tool_call_field<'a>(entry: &'a RawToolCall, field: &str) -> &'a str {
    match field {
        "name" => entry.name.as_str(),
        "id" => entry
            .id
            .as_deref()
            .unwrap_or_else(|| panic!("raw_tool_calls entry missing id: {entry:?}")),
        "arguments_json" => entry.arguments_json.as_str(),
        _ => panic!("raw_tool_calls unknown field {field}"),
    }
}

fn non_budget_tool_messages(snapshot: &SessionTestSnapshot) -> Vec<&AgentMessage> {
    snapshot
        .messages
        .iter()
        .filter(|m| {
            m.role == AgentRole::Tool
                && !m
                    .metadata
                    .extra("agent_tool_budget")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        })
        .collect()
}

fn submit_tool_use(
    eng: &AgentEngine,
    h: &agent_core::engine::SessionHandle,
    tool_calls: Vec<RawToolCall>,
) {
    let _ = eng.next_action(h);
    eng.submit_model_response(
        h,
        ModelResponse {
            assistant_text: None,
            tool_calls,
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
}

// ── ETR-R-01 ───────────────────────────────────────────────────────────────────

#[test]
fn etr_r_01_fence_wrapped_arguments_canonicalized_in_raw_tool_calls() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);
    submit_tool_use(
        &eng,
        &h,
        vec![RawToolCall {
            id: Some("call-fence".into()),
            name: "web_search".into(),
            arguments_json: "```json\n{\"query\":\"rust\"}\n```".into(),
        }],
    );
    match eng.next_action(&h) {
        AgentAction::ExecuteTools(calls) => assert_eq!(calls.len(), 1),
        other => panic!("ETR-R-01 expected ExecuteTools, got {other:?}"),
    }
    let snapshot = h.integration_test_snapshot();
    let raw = raw_tool_calls_array(&snapshot);
    assert_eq!(raw.len(), 1, "ETR-R-01 raw_tool_calls length");
    assert_eq!(raw_tool_call_field(&raw[0], "name"), "web_search");
    assert_eq!(raw_tool_call_field(&raw[0], "id"), "call-fence");
    assert_eq!(
        raw_tool_call_field(&raw[0], "arguments_json"),
        "{\"query\":\"rust\"}"
    );
    assert!(
        !raw_tool_call_field(&raw[0], "arguments_json").contains("```"),
        "ETR-R-01 arguments_json must not contain markdown fences"
    );
}

// ── ETR-R-02 ─────────────────────────────────────────────────────────────────

#[test]
fn etr_r_02_fuzzy_tool_name_web_seach_stored_as_web_search() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);
    submit_tool_use(
        &eng,
        &h,
        vec![RawToolCall {
            id: Some("c1".into()),
            name: "web_seach".into(),
            arguments_json: r#"{"query":"x"}"#.into(),
        }],
    );
    let calls = match eng.next_action(&h) {
        AgentAction::ExecuteTools(calls) => calls,
        other => panic!("ETR-R-02 expected ExecuteTools, got {other:?}"),
    };
    let snapshot = h.integration_test_snapshot();
    let raw = raw_tool_calls_array(&snapshot);
    assert_eq!(raw_tool_call_field(&raw[0], "name"), "web_search");
    assert_ne!(raw_tool_call_field(&raw[0], "name"), "web_seach");
    assert_eq!(calls[0].name, "web_search");
}

// ── ETR-R-03 ─────────────────────────────────────────────────────────────────

#[test]
fn etr_r_03_append_blocks_complete_alias_stored_as_is_complete() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);
    submit_tool_use(
        &eng,
        &h,
        vec![RawToolCall {
            id: Some("c-ab".into()),
            name: "append_blocks".into(),
            arguments_json: r#"{"artifactId":"a","blocks":[],"complete":true}"#.into(),
        }],
    );
    let _ = eng.next_action(&h);
    let snapshot = h.integration_test_snapshot();
    let raw = raw_tool_calls_array(&snapshot);
    assert_eq!(raw_tool_call_field(&raw[0], "name"), "append_blocks");
    let args = raw_tool_call_field(&raw[0], "arguments_json");
    assert!(
        args.contains("\"isComplete\":true"),
        "ETR-R-03 expected isComplete:true in {args}"
    );
    assert!(
        !args.contains("\"complete\":true"),
        "ETR-R-03 must not retain complete alias in {args}"
    );
}

// ── ETR-R-04 ─────────────────────────────────────────────────────────────────

#[test]
fn etr_r_04_valid_json_metadata_matches_execute_tools_record() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);
    submit_tool_use(
        &eng,
        &h,
        vec![RawToolCall {
            id: Some("call-1".into()),
            name: "web_search".into(),
            arguments_json: r#"{"query":"finish boundary"}"#.into(),
        }],
    );
    let calls = match eng.next_action(&h) {
        AgentAction::ExecuteTools(calls) => calls,
        other => panic!("ETR-R-04 expected ExecuteTools, got {other:?}"),
    };
    let snapshot = h.integration_test_snapshot();
    let raw = raw_tool_calls_array(&snapshot);
    assert_eq!(raw_tool_call_field(&raw[0], "id"), "call-1");
    assert_eq!(raw_tool_call_field(&raw[0], "name"), "web_search");
    assert_eq!(
        raw_tool_call_field(&raw[0], "arguments_json"),
        r#"{"query":"finish boundary"}"#
    );
    assert_eq!(calls[0].call_id, "call-1");
    assert_eq!(calls[0].name, "web_search");
    assert_eq!(calls[0].arguments_json, r#"{"query":"finish boundary"}"#);
}

// ── ETR-R-05 ─────────────────────────────────────────────────────────────────

#[test]
fn etr_r_05_reject_only_turn_persists_empty_raw_tool_calls() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);
    submit_tool_use(
        &eng,
        &h,
        vec![RawToolCall {
            id: None,
            name: "totally_unknown_tool_xyz".into(),
            arguments_json: "{}".into(),
        }],
    );
    let next = eng.next_action(&h);
    assert!(
        !matches!(next, AgentAction::ExecuteTools(_)),
        "ETR-R-05 must not ExecuteTools, got {next:?}"
    );
    let snapshot = h.integration_test_snapshot();
    assert_eq!(
        raw_tool_calls_array(&snapshot).len(),
        0,
        "ETR-R-05 raw_tool_calls must be empty"
    );
    assert!(
        snapshot
            .messages
            .iter()
            .any(|m| m.role == AgentRole::Tool && m.content.contains("error")),
        "ETR-R-05 tool error message"
    );
}

// ── ETR-R-06 ─────────────────────────────────────────────────────────────────

#[test]
fn etr_r_06_partial_parallel_one_reject_one_repair_metadata_lists_all_call_ids() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);
    submit_tool_use(
        &eng,
        &h,
        vec![
            RawToolCall {
                id: Some("c-ok".into()),
                name: "web_search".into(),
                arguments_json: r#"{"query":"ok"}"#.into(),
            },
            RawToolCall {
                id: Some("c-bad".into()),
                name: "totally_unknown_tool_xyz".into(),
                arguments_json: "{}".into(),
            },
        ],
    );
    let calls = match eng.next_action(&h) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(calls.len(), 1, "ETR-R-06 ExecuteTools count");
            calls
        }
        other => panic!("ETR-R-06 expected ExecuteTools, got {other:?}"),
    };
    let snapshot = h.integration_test_snapshot();
    let raw = raw_tool_calls_array(&snapshot);
    assert_eq!(raw.len(), 2, "ETR-R-06 raw_tool_calls length");
    assert_eq!(raw_tool_call_field(&raw[0], "id"), "c-ok");
    assert_eq!(raw_tool_call_field(&raw[0], "name"), "web_search");
    assert_eq!(raw_tool_call_field(&raw[1], "id"), "c-bad");
    assert_eq!(calls[0].call_id, "c-ok");

    eng.submit_tool_results(
        &h,
        vec![ToolResultRecord {
            call_id: "c-ok".into(),
            result_json: r#"{"results":[{"title":"ok"}]}"#.into(),
            truncated: false,
            soft_failure: false,
            activated_tool_names: Vec::new(),
            result_metadata: None,
        }],
    );

    let snapshot = h.integration_test_snapshot();
    let tool_msgs = non_budget_tool_messages(&snapshot);
    assert_eq!(tool_msgs.len(), 2, "ETR-R-06 two tool messages");
    assert!(
        tool_msgs[0].metadata.call_id() == Some("c-ok"),
        "ETR-R-06 success tool first"
    );
    assert!(
        tool_msgs[1].id.contains("tool-err-c-bad") || tool_msgs[1].metadata.call_id().is_some(),
        "ETR-R-06 reject tool second"
    );
}

// ── ETR-R-07 ─────────────────────────────────────────────────────────────────

#[test]
fn etr_r_07_invalid_json_arguments_reject_empty_raw_tool_calls() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);
    submit_tool_use(
        &eng,
        &h,
        vec![RawToolCall {
            id: None,
            name: "web_search".into(),
            arguments_json: "".into(),
        }],
    );
    let next = eng.next_action(&h);
    assert!(
        !matches!(next, AgentAction::ExecuteTools(_)),
        "ETR-R-07 must not ExecuteTools, got {next:?}"
    );
    let snapshot = h.integration_test_snapshot();
    assert_eq!(
        raw_tool_calls_array(&snapshot).len(),
        0,
        "ETR-R-07 raw_tool_calls must be empty"
    );
}

// ── ETR-R-08 ─────────────────────────────────────────────────────────────────

#[test]
fn etr_r_08_unknown_tool_zzzzz_reject_empty_raw_tool_calls() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);
    submit_tool_use(
        &eng,
        &h,
        vec![RawToolCall {
            id: None,
            name: "zzzzz_no_real_match_yyyyy".into(),
            arguments_json: "{}".into(),
        }],
    );
    let next = eng.next_action(&h);
    assert!(
        !matches!(next, AgentAction::ExecuteTools(_)),
        "ETR-R-08 must not ExecuteTools, got {next:?}"
    );
    assert!(
        matches!(next, AgentAction::CallModel(_)),
        "ETR-R-08 expected CallModel, got {next:?}"
    );
    let snapshot = h.integration_test_snapshot();
    assert_eq!(
        raw_tool_calls_array(&snapshot).len(),
        0,
        "ETR-R-08 raw_tool_calls must be empty"
    );
}

// ── ETR-R-09 ─────────────────────────────────────────────────────────────────

#[test]
fn etr_r_09_envelope_ingest_co_metadata_preserved_after_patch() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);
    let _ = eng.next_action(&h);
    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: Some(VALID_DSML_WEB_SEARCH.into()),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            provider_reasoning_content: None,
        },
    );
    match eng.next_action(&h) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name, "web_search");
        }
        other => panic!("ETR-R-09 expected ExecuteTools, got {other:?}"),
    }
    let snapshot = h.integration_test_snapshot();
    let assistant = last_assistant_message(&snapshot);
    assert_eq!(
        assistant
            .metadata
            .extra("envelope_ingest")
            .and_then(|v| v.as_str()),
        Some("reified"),
        "ETR-R-09 envelope_ingest"
    );
    let raw = raw_tool_calls_array(&snapshot);
    assert_eq!(raw.len(), 1);
    assert_eq!(raw_tool_call_field(&raw[0], "name"), "web_search");
    assert!(
        raw_tool_call_field(&raw[0], "arguments_json").contains(r#""query":"rust""#),
        "ETR-R-09 arguments_json must contain query rust"
    );
}

// ── ETR-M-01 ─────────────────────────────────────────────────────────────────

#[test]
fn etr_m_01_metadata_arguments_json_equals_execute_tools_record() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);
    submit_tool_use(
        &eng,
        &h,
        vec![RawToolCall {
            id: Some("call-fence".into()),
            name: "web_search".into(),
            arguments_json: "```json\n{\"query\":\"rust\"}\n```".into(),
        }],
    );
    let calls = match eng.next_action(&h) {
        AgentAction::ExecuteTools(calls) => calls,
        other => panic!("ETR-M-01 expected ExecuteTools, got {other:?}"),
    };
    let snapshot = h.integration_test_snapshot();
    let raw = raw_tool_calls_array(&snapshot);
    assert_eq!(
        calls[0].arguments_json,
        raw_tool_call_field(&raw[0], "arguments_json")
    );
    assert_eq!(calls[0].arguments_json, "{\"query\":\"rust\"}");
    assert_eq!(
        raw_tool_call_field(&raw[0], "arguments_json"),
        "{\"query\":\"rust\"}"
    );
    assert!(
        !raw_tool_call_field(&raw[0], "arguments_json").contains("```"),
        "ETR-M-01 no fences in metadata"
    );
}

// ── ETR-M-02 ─────────────────────────────────────────────────────────────────

#[test]
fn etr_m_02_rust_snapshot_gate_requires_engine_patch_not_kotlin_only() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);
    submit_tool_use(
        &eng,
        &h,
        vec![RawToolCall {
            id: Some("call-fence".into()),
            name: "web_search".into(),
            arguments_json: "```json\n{\"query\":\"rust\"}\n```".into(),
        }],
    );
    let _ = eng.next_action(&h);
    let snapshot = h.integration_test_snapshot();
    let raw = raw_tool_calls_array(&snapshot);
    assert!(
        !raw_tool_call_field(&raw[0], "arguments_json").contains("```"),
        "ETR-M-02 provider metadata must be canonicalized in Rust, not only at Kotlin replay"
    );
}

// ── ETR-M-03 ─────────────────────────────────────────────────────────────────

#[test]
fn etr_m_03_metadata_must_not_contain_web_seach_typo() {
    let eng = AgentEngine::new();
    let h = new_session(&eng);
    submit_tool_use(
        &eng,
        &h,
        vec![RawToolCall {
            id: Some("c1".into()),
            name: "web_seach".into(),
            arguments_json: r#"{"query":"x"}"#.into(),
        }],
    );
    let _ = eng.next_action(&h);
    let snapshot = h.integration_test_snapshot();
    let meta_str =
        serde_json::to_string(&last_assistant_message(&snapshot).metadata).expect("metadata json");
    assert!(
        !meta_str.contains("web_seach"),
        "ETR-M-03 metadata must not contain web_seach typo: {meta_str}"
    );
    let raw = raw_tool_calls_array(&snapshot);
    assert_eq!(raw_tool_call_field(&raw[0], "name"), "web_search");
}
