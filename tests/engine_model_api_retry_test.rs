//! Model API transport retry (MAR-01 … MAR-07).

#[path = "support/mod.rs"]
mod support;

use support::enclave_test_agent_config::enclave_test_agent_config;

use std::sync::{Arc, Mutex};

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::{AgentError, AgentEvent};
use agent_core::host::{AgentToolExecutor, AgentTransport, NoOpSummarizer};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::model_api_retry::{ModelApiErrorClass, ModelTurnFailureReport};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{ModelRequest, ModelResponse};

struct PanicTransport;
impl AgentTransport for PanicTransport {
    fn complete_model_turn(
        &self,
        _: ModelRequest,
    ) -> Result<ModelResponse, agent_core::events::AgentError> {
        panic!("transport unused in submit-path tests")
    }
}

struct RecordingExecutor {
    results: Mutex<Vec<ToolCallRecord>>,
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
                result_json: r#"{"ok":true}"#.into(),
                truncated: false,
                soft_failure: false,
                activated_tool_names: Vec::new(),
                result_metadata: None,
            })
            .collect())
    }
}

fn io() -> SessionIoParams {
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
        content: "write a long doc".into(),
        metadata: serde_json::Value::Null,
    }
}

fn new_session(
    eng: &AgentEngine<PanicTransport, RecordingExecutor, NoOpSummarizer>,
    config: AgentConfig,
) -> agent_core::engine::SessionHandle {
    eng.create_session(
        vec![user_msg()],
        config,
        SessionCapabilities {
            summarization_allowed: false,
            kv_fixed: false,
            tool_result_budget_tokens: None,
            ..Default::default()
        },
        serde_json::Value::Null,
        io(),
    )
}

fn transient_failure(message: &str) -> ModelTurnFailureReport {
    ModelTurnFailureReport {
        error_class: ModelApiErrorClass::Transient,
        http_status: Some(502),
        provider_code: Some("502".into()),
        message: message.into(),
        had_partial_output: true,
        partial_output_chars: 1200,
        retry_after_ms: None,
    }
}

fn permanent_failure() -> ModelTurnFailureReport {
    ModelTurnFailureReport {
        error_class: ModelApiErrorClass::Permanent,
        http_status: Some(401),
        provider_code: Some("401".into()),
        message: "Invalid API key".into(),
        had_partial_output: false,
        partial_output_chars: 0,
        retry_after_ms: None,
    }
}

fn expect_call_model(
    eng: &AgentEngine<PanicTransport, RecordingExecutor, NoOpSummarizer>,
    handle: &agent_core::engine::SessionHandle,
) -> ModelRequest {
    match eng.next_action(handle) {
        AgentAction::CallModel(request) => request,
        other => panic!("expected CallModel, got {other:?}"),
    }
}

fn drain_retry_events(
    eng: &AgentEngine<PanicTransport, RecordingExecutor, NoOpSummarizer>,
    handle: &agent_core::engine::SessionHandle,
) -> Vec<AgentEvent> {
    eng.drain_events(handle)
        .into_iter()
        .filter(|event| matches!(event, AgentEvent::ModelApiRetryScheduled { .. }))
        .collect()
}

#[test]
fn mar_01_transient_failure_schedules_retry_call_model() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, enclave_test_agent_config());
    let first = expect_call_model(&eng, &h);
    assert_eq!(first.system_prompt, "sys");

    eng.submit_model_turn_failure(&h, transient_failure("Upstream idle timeout exceeded"));
    let retry_events = drain_retry_events(&eng, &h);
    assert_eq!(retry_events.len(), 1);
    let AgentEvent::ModelApiRetryScheduled {
        attempt,
        max_attempts,
        delay_ms,
        reason,
    } = &retry_events[0]
    else {
        panic!("expected ModelApiRetryScheduled");
    };
    assert_eq!(*attempt, 1);
    assert_eq!(*max_attempts, 3);
    assert!(*delay_ms >= 1000);
    assert!(reason.contains("Upstream idle timeout"));

    let next = eng.next_action(&h);
    assert!(
        matches!(next, AgentAction::CallModel(_)),
        "MAR-01 expected retry CallModel, got {next:?}"
    );
    assert!(
        !matches!(next, AgentAction::Finished | AgentAction::Failed(_)),
        "MAR-01 must not terminate"
    );
}

#[test]
fn mar_02_exhausted_transient_failures_terminal_guardrail() {
    let mut config = enclave_test_agent_config();
    config.model_api_retry_limit = 2;
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, config);
    expect_call_model(&eng, &h);

    for attempt in 1..=2 {
        eng.submit_model_turn_failure(&h, transient_failure("timeout"));
        let next = eng.next_action(&h);
        assert!(
            matches!(next, AgentAction::CallModel(_)),
            "MAR-02 attempt {attempt} expected retry, got {next:?}"
        );
    }

    eng.submit_model_turn_failure(&h, transient_failure("timeout"));
    let final_action = eng.next_action(&h);
    match final_action {
        AgentAction::Failed(AgentError::GuardrailExceeded { kind }) => {
            assert_eq!(kind, "model_api_retries_exhausted");
        }
        other => panic!("MAR-02 expected guardrail failure, got {other:?}"),
    }
}

#[test]
fn mar_03_permanent_failure_fails_immediately() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, enclave_test_agent_config());
    expect_call_model(&eng, &h);
    eng.submit_model_turn_failure(&h, permanent_failure());

    let next = eng.next_action(&h);
    match next {
        AgentAction::Failed(AgentError::ModelApiUnavailable { message, .. }) => {
            assert!(message.contains("Invalid API key"));
        }
        other => panic!("MAR-03 expected ModelApiUnavailable, got {other:?}"),
    }
}

#[test]
fn mar_04_success_resets_consecutive_failure_counter() {
    let mut config = enclave_test_agent_config();
    config.model_api_retry_limit = 1;
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, config);
    expect_call_model(&eng, &h);
    eng.submit_model_turn_failure(&h, transient_failure("timeout"));
    expect_call_model(&eng, &h);

    eng.submit_model_response(&h, ModelResponse::text_only("done"));
    let next = eng.next_action(&h);
    assert!(
        matches!(next, AgentAction::Finished),
        "MAR-04 expected Finished after recovery, got {next:?}"
    );

    // New user turn should allow another full retry budget.
    let mut config2 = enclave_test_agent_config();
    config2.model_api_retry_limit = 1;
    let h2 = new_session(&eng, config2);
    expect_call_model(&eng, &h2);
    eng.submit_model_turn_failure(&h2, transient_failure("timeout"));
    let retry = eng.next_action(&h2);
    assert!(
        matches!(retry, AgentAction::CallModel(_)),
        "MAR-04 expected fresh retry budget on new session"
    );
}

#[test]
fn mar_05_retry_preserves_pending_request_shape() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, enclave_test_agent_config());
    let first = expect_call_model(&eng, &h);
    eng.submit_model_turn_failure(&h, transient_failure("timeout"));

    let AgentAction::CallModel(retry) = eng.next_action(&h) else {
        panic!("MAR-05 expected CallModel");
    };
    assert_eq!(retry.system_prompt, first.system_prompt);
    assert_eq!(retry.allow_tool_call, first.allow_tool_call);
    assert_eq!(retry.session_messages.len(), first.session_messages.len());
    assert!(retry.retry_after_ms.is_some());
    assert_eq!(retry.model_retry_attempt, Some(1));
}

#[test]
fn mar_06_honors_retry_after_ms_from_report() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, enclave_test_agent_config());
    expect_call_model(&eng, &h);

    let mut failure = transient_failure("rate limited");
    failure.retry_after_ms = Some(5_000);
    eng.submit_model_turn_failure(&h, failure);

    let AgentAction::CallModel(retry) = eng.next_action(&h) else {
        panic!("MAR-06 expected CallModel");
    };
    assert_eq!(retry.retry_after_ms, Some(5_000));
}

#[test]
fn mar_07_invalid_transition_when_not_awaiting_model() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, enclave_test_agent_config());
    eng.submit_model_turn_failure(&h, transient_failure("timeout"));
    match eng.next_action(&h) {
        AgentAction::Failed(AgentError::InvalidTransition { to, .. }) => {
            assert_eq!(to, "submit_model_turn_failure");
        }
        other => panic!("MAR-07 expected invalid transition, got {other:?}"),
    }
}