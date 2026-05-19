//! TOOL-PARSE-002 engine envelope scenarios E-01 … E-07, MD-02.

use std::sync::{Arc, Mutex};

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::host::{AgentToolExecutor, AgentTransport, NoOpSummarizer};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse};

const MALFORMED_DSML: &str =
    r#"<|DSML|tool_calls><|DSML|invoke name="web_search"></|DSML|invoke>"#;

const VALID_DSML_WEB_SEARCH: &str = concat!(
    "<|DSML|tool_calls><|DSML|invoke name=\"web_search\">",
    "<|DSML|parameter name=\"query\" string=\"true\">rust</|DSML|parameter>",
    "</|DSML|invoke></|DSML|tool_calls>"
);

struct PanicTransport;
impl AgentTransport for PanicTransport {
    fn complete_model_turn(&self, _: ModelRequest) -> Result<ModelResponse, agent_core::events::AgentError> {
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
                result_metadata: None,
            })
            .collect())
    }
}

fn io_large() -> SessionIoParams {
    SessionIoParams {
        provider: "openrouter".into(),
        context_window_tokens: 200_000,
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

fn new_session(eng: &AgentEngine<PanicTransport, RecordingExecutor, NoOpSummarizer>) -> agent_core::engine::SessionHandle {
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
    }
}

fn tool_role_messages(snapshot: &agent_core::engine::SessionTestSnapshot) -> Vec<&AgentMessage> {
    snapshot
        .messages
        .iter()
        .filter(|m| m.role == AgentRole::Tool)
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
    assert!(!matches!(next, AgentAction::Finished), "E-01 must not finish");
    let snapshot = h.integration_test_snapshot();
    assert_eq!(tool_role_messages(&snapshot).len(), 1, "E-01 tool messages");
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
    assert!(tool_msg.content.contains("web_search"), "E-02 available_tools");
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
