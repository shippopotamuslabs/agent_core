//! TOOL-PARSE-002 engine envelope scenarios E-01 … E-07, MD-02.
//! centralized-leaked-tool-call-recovery engine scenarios CL-E-*, CL-MD-01.

use std::sync::{Arc, Mutex};

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::host::{AgentToolExecutor, AgentTransport, NoOpSummarizer};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse};

const MALFORMED_DSML: &str = r#"<|DSML|tool_calls><|DSML|invoke name="web_search"></|DSML|invoke>"#;

const VALID_DSML_WEB_SEARCH: &str = concat!(
    "<|DSML|tool_calls><|DSML|invoke name=\"web_search\">",
    "<|DSML|parameter name=\"query\" string=\"true\">rust</|DSML|parameter>",
    "</|DSML|invoke></|DSML|tool_calls>"
);

struct PanicTransport;
impl AgentTransport for PanicTransport {
    fn complete_model_turn(
        &self,
        _: ModelRequest,
    ) -> Result<ModelResponse, agent_core::events::AgentError> {
        panic!("transport unused")
    }
}

struct RecordingExecutor {
    pub results: Mutex<Vec<ToolCallRecord>>,
}

impl AgentToolExecutor for RecordingExecutor {
    fn execute_tools(
        &self,
        calls: Vec<ToolCallRecord>,
    ) -> Result<Vec<ToolResultRecord>, agent_core::events::AgentError> {
        self.results.lock().unwrap().extend(calls.clone());
        Ok(calls
            .into_iter()
            .map(|c| ToolResultRecord {
                call_id: c.call_id,
                result_json: r#"{"results":[{"title":"ok"}]}"#.into(),
                truncated: false,
                soft_failure: false,
                activated_tool_names: Vec::new(),
                result_metadata: None,
            })
            .collect())
    }
}

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
        metadata: serde_json::Value::Null,
    }
}

fn new_session(
    eng: &AgentEngine<PanicTransport, RecordingExecutor, NoOpSummarizer>,
) -> agent_core::engine::SessionHandle {
    eng.create_session(
        vec![user_msg()],
        AgentConfig::default(),
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

fn malformed_response() -> ModelResponse {
    ModelResponse {
        assistant_text: Some(MALFORMED_DSML.into()),
        tool_calls: vec![],
        finish_reason: FinishReason::Stop,
        provider_reasoning_content: None,
    }
}

fn tool_role_messages(snapshot: &agent_core::engine::SessionTestSnapshot) -> Vec<&AgentMessage> {
    snapshot
        .messages
        .iter()
        .filter(|m| m.role == AgentRole::Tool)
        .collect()
}

fn non_budget_tool_role_messages(
    snapshot: &agent_core::engine::SessionTestSnapshot,
) -> Vec<&AgentMessage> {
    tool_role_messages(snapshot)
        .into_iter()
        .filter(|m| {
            !m.metadata
                .get("agent_tool_budget")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .collect()
}

fn last_assistant_content(snapshot: &agent_core::engine::SessionTestSnapshot) -> &str {
    snapshot
        .messages
        .iter()
        .rev()
        .find(|m| m.role == AgentRole::Assistant)
        .map(|m| m.content.as_str())
        .unwrap_or("")
}

#[test]
fn e_01_malformed_dsml_retries_model() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng);
    assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
    eng.submit_model_response(&h, malformed_response());
    let next = eng.next_action(&h);
    assert!(
        matches!(next, AgentAction::CallModel(_)),
        "E-01 expected CallModel, got {next:?}"
    );
    assert!(
        !matches!(next, AgentAction::Finished),
        "E-01 must not finish"
    );
    let snapshot = h.integration_test_snapshot();
    assert_eq!(
        non_budget_tool_role_messages(&snapshot).len(),
        1,
        "E-01 tool messages"
    );
    assert!(
        tool_role_messages(&snapshot)
            .iter()
            .any(|m| m.metadata.get("agent_tool_budget").is_some()),
        "E-01 budget sidecar"
    );
}

#[test]
fn e_02_envelope_failure_json_shape() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng);
    let _ = eng.next_action(&h);
    eng.submit_model_response(&h, malformed_response());
    let snapshot = h.integration_test_snapshot();
    let tool_msgs = tool_role_messages(&snapshot);
    let tool_msg = tool_msgs.first().expect("E-02 tool message");
    assert!(tool_msg.content.contains("\"error\""), "E-02 error key");
    assert!(tool_msg.content.contains("Malformed"), "E-02 Malformed");
    assert!(tool_msg.content.contains("DSML"), "E-02 DSML");
    assert!(
        tool_msg.content.contains("web_search"),
        "E-02 available_tools"
    );
}

#[test]
fn e_03_valid_leaked_dsml_executes_tool() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
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
            assert_eq!(calls.len(), 1, "E-03 tool count");
            assert_eq!(calls[0].name, "web_search", "E-03 tool name");
            assert!(
                calls[0].arguments_json.contains(r#""query":"rust""#),
                "E-03 args {}",
                calls[0].arguments_json
            );
        }
        other => panic!("E-03 expected ExecuteTools, got {other:?}"),
    }
}

#[test]
fn e_04_persisted_assistant_strips_dsml() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng);
    let _ = eng.next_action(&h);
    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: Some(format!("Searching now.{VALID_DSML_WEB_SEARCH}")),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            provider_reasoning_content: None,
        },
    );
    let _ = eng.next_action(&h);
    let snapshot = h.integration_test_snapshot();
    let content = last_assistant_content(&snapshot);
    assert_eq!(content, "Searching now.", "E-04 assistant content");
    assert!(!content.contains("DSML"), "E-04 no DSML in assistant");
}

#[test]
fn e_05_stop_plus_malformed_not_terminal() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng);
    let _ = eng.next_action(&h);
    eng.submit_model_response(&h, malformed_response());
    let next = eng.next_action(&h);
    assert!(matches!(next, AgentAction::CallModel(_)), "E-05 CallModel");
    assert!(
        h.integration_test_snapshot().terminal_action.is_none(),
        "E-05 terminal_action"
    );
}

#[test]
fn e_06_envelope_streak_disables_tools() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng);
    for _ in 0..3 {
        let _ = eng.next_action(&h);
        eng.submit_model_response(&h, malformed_response());
        let _ = eng.next_action(&h);
    }
    let _ = eng.next_action(&h);
    let AgentAction::CallModel(request) = eng.next_action(&h) else {
        panic!("E-06 expected CallModel");
    };
    assert!(!request.allow_tool_call, "E-06 allow_tool_call false");
    assert_eq!(
        h.integration_test_snapshot()
            .consecutive_envelope_parse_failures,
        3,
        "E-06 streak"
    );
}

#[test]
fn e_07_success_resets_envelope_streak() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng);
    for _ in 0..2 {
        let _ = eng.next_action(&h);
        eng.submit_model_response(&h, malformed_response());
        let _ = eng.next_action(&h);
    }
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
    let AgentAction::ExecuteTools(calls) = eng.next_action(&h) else {
        panic!("E-07 expected ExecuteTools");
    };
    eng.submit_tool_results(
        &h,
        calls
            .into_iter()
            .map(|c| ToolResultRecord {
                call_id: c.call_id,
                result_json: r#"{"results":[]}"#.into(),
                truncated: false,
                soft_failure: false,
                activated_tool_names: Vec::new(),
                result_metadata: None,
            })
            .collect(),
    );
    let _ = eng.next_action(&h);
    let AgentAction::CallModel(request) = eng.next_action(&h) else {
        panic!("E-07 expected CallModel after reset");
    };
    assert!(request.allow_tool_call, "E-07 allow_tool_call true");
}

#[test]
fn md_02_recovery_only_in_engine() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng);
    let _ = eng.next_action(&h);
    eng.submit_model_response(&h, malformed_response());
    assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
    assert_eq!(
        h.integration_test_snapshot()
            .consecutive_envelope_parse_failures,
        1,
        "MD-02 envelope streak"
    );
}

// ---------------------------------------------------------------------------
// centralized-leaked-tool-call-recovery — CL-TR-05 (engine ingest), CL-E-*, CL-MD-01
// ---------------------------------------------------------------------------

const CL_TR_05_LEAKED_TOOL_CALL: &str = r#"<tool_call>{"name":"web_search"}</tool_call>"#;

#[test]
fn cl_tr_05_engine_allow_false_nudges_then_fallback() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = eng.create_session(
        vec![user_msg()],
        AgentConfig {
            max_model_requests_per_turn: 10,
            ..AgentConfig::default()
        },
        SessionCapabilities {
            summarization_allowed: false,
            kv_fixed: false,
            tool_result_budget_tokens: None,
            ..Default::default()
        },
        serde_json::Value::Null,
        io_large(),
    );
    for _ in 0..3 {
        let _ = eng.next_action(&h);
        eng.submit_model_response(&h, malformed_response());
        let _ = eng.next_action(&h);
    }
    let _ = eng.next_action(&h);
    let AgentAction::CallModel(pending) = eng.next_action(&h) else {
        panic!("CL-TR-05 expected CallModel before submit");
    };
    assert!(
        !pending.allow_tool_call,
        "CL-TR-05 pending allow_tool_call must be false"
    );
    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: Some(CL_TR_05_LEAKED_TOOL_CALL.into()),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            provider_reasoning_content: None,
        },
    );
    let next = eng.next_action(&h);
    assert!(
        matches!(next, AgentAction::CallModel(_)),
        "CL-TR-05 first blocked response must nudge with another CallModel, got {next:?}"
    );
    let snapshot = h.integration_test_snapshot();
    assert_eq!(
        last_assistant_content(&snapshot),
        "",
        "CL-TR-05 stripped assistant content before nudge"
    );
    assert!(
        snapshot.messages.iter().any(|m| {
            m.role == AgentRole::Tool
                && m.metadata
                    .get("tools_blocked_nudge")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        }),
        "CL-TR-05 must insert tools_blocked nudge"
    );

    let AgentAction::CallModel(retry) = eng.next_action(&h) else {
        panic!("CL-TR-05 expected retry CallModel");
    };
    assert!(
        !retry.allow_tool_call,
        "CL-TR-05 retry must remain allow_tool_call false"
    );
    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: Some(CL_TR_05_LEAKED_TOOL_CALL.into()),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            provider_reasoning_content: None,
        },
    );
    let next = eng.next_action(&h);
    assert!(
        matches!(next, AgentAction::Finished),
        "CL-TR-05 second blocked response must finish, got {next:?}"
    );
    let snapshot = h.integration_test_snapshot();
    assert_eq!(
        last_assistant_content(&snapshot),
        "Sorry, something went wrong on my end. Please try again.",
        "CL-TR-05 user fallback"
    );
}

const APPEND_BLOCKS_LEAK: &str = r#"<tool_call>{"name":"append_blocks","arguments":{"artifactId":"art-1","blocks":[],"isComplete":false}}</tool_call>"#;

#[test]
fn cl_e_01_engine_executes_recovered_append_blocks() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng);
    let _ = eng.next_action(&h);
    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: Some(APPEND_BLOCKS_LEAK.into()),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            provider_reasoning_content: None,
        },
    );
    match eng.next_action(&h) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(calls.len(), 1, "CL-E-01 tool count");
            assert_eq!(calls[0].name, "append_blocks", "CL-E-01 tool name");
        }
        other => panic!("CL-E-01 expected ExecuteTools, got {other:?}"),
    }
    let snapshot = h.integration_test_snapshot();
    assert_eq!(
        last_assistant_content(&snapshot),
        "",
        "CL-E-01 assistant content"
    );
}

#[test]
fn cl_e_02_native_only_when_native_and_leak_present() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng);
    let _ = eng.next_action(&h);
    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: Some(r#"x<|DSML|invoke name="append_blocks"></|DSML|invoke>"#.into()),
            tool_calls: vec![agent_core::turn_io::RawToolCall {
                id: Some("call_native_1".into()),
                name: "web_search".into(),
                arguments_json: r#"{"query":"kotlin"}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    match eng.next_action(&h) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(calls.len(), 1, "CL-E-02 tool count");
            assert_eq!(calls[0].name, "web_search", "CL-E-02 tool name");
        }
        other => panic!("CL-E-02 expected ExecuteTools, got {other:?}"),
    }
}

#[test]
fn cl_e_03_malformed_dsml_retries_model() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng);
    let _ = eng.next_action(&h);
    eng.submit_model_response(&h, malformed_response());
    let next = eng.next_action(&h);
    assert!(
        matches!(next, AgentAction::CallModel(_)),
        "CL-E-03 expected CallModel, got {next:?}"
    );
    assert!(
        !matches!(next, AgentAction::Finished),
        "CL-E-03 must not finish"
    );
    let snapshot = h.integration_test_snapshot();
    let tool_msgs = tool_role_messages(&snapshot);
    let tool_msg = tool_msgs.first().expect("CL-E-03 tool message");
    assert!(
        tool_msg.content.contains("Malformed"),
        "CL-E-03 Malformed in tool message"
    );
}

#[test]
fn cl_e_04_preamble_preserved_marker_stripped() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng);
    let _ = eng.next_action(&h);
    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: Some(
                r#"Done.<tool_call>{"name":"web_search","arguments":{"query":"x"}}</tool_call>"#
                    .into(),
            ),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            provider_reasoning_content: None,
        },
    );
    let _ = eng.next_action(&h);
    let snapshot = h.integration_test_snapshot();
    assert_eq!(
        last_assistant_content(&snapshot),
        "Done.",
        "CL-E-04 assistant content"
    );
}

#[test]
fn cl_md_01_native_only_execution() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng);
    let _ = eng.next_action(&h);
    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: Some(
                r#"prefix<|DSML|invoke name="append_blocks"></|DSML|invoke>"#.into(),
            ),
            tool_calls: vec![agent_core::turn_io::RawToolCall {
                id: Some("call_native_1".into()),
                name: "web_search".into(),
                arguments_json: r#"{"query":"kotlin"}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    match eng.next_action(&h) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(calls.len(), 1, "CL-MD-01 tool count");
            assert_eq!(calls[0].name, "web_search", "CL-MD-01 tool name");
            assert_eq!(calls[0].call_id, "call_native_1", "CL-MD-01 call_id");
        }
        other => panic!("CL-MD-01 expected ExecuteTools, got {other:?}"),
    }
}
