//! FEAT-tool-budget-agent-meta — integration scenarios TB-01 … TB-12, MD-01 … MD-06.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::AgentError;
use agent_core::host::{AgentToolExecutor, AgentTransport, NoOpSummarizer};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall};
use serde_json::Value;

struct PanicTransport;
impl AgentTransport for PanicTransport {
    fn complete_model_turn(
        &self,
        _: ModelRequest,
    ) -> Result<ModelResponse, agent_core::events::AgentError> {
        panic!("transport unused")
    }
}

struct RecordingTransport {
    captured_requests: Mutex<Vec<ModelRequest>>,
}

impl AgentTransport for RecordingTransport {
    fn complete_model_turn(
        &self,
        request: ModelRequest,
    ) -> Result<ModelResponse, agent_core::events::AgentError> {
        self.captured_requests.lock().unwrap().push(request);
        Ok(ModelResponse::text_only("done"))
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
                result_json: r#"{"results":[]}"#.into(),
                truncated: false,
                soft_failure: false,
                result_metadata: None,
            })
            .collect())
    }
}

fn io_large() -> SessionIoParams {
    SessionIoParams {
        provider: "anthropic".into(),
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

fn default_caps() -> SessionCapabilities {
    SessionCapabilities {
        summarization_allowed: false,
        kv_fixed: false,
        tool_result_budget_tokens: None,
        ..Default::default()
    }
}

fn new_session_with_transport<T: AgentTransport>(
    eng: &AgentEngine<T, RecordingExecutor, NoOpSummarizer>,
    config: AgentConfig,
) -> agent_core::engine::SessionHandle {
    eng.create_session(
        vec![user_msg()],
        config,
        default_caps(),
        serde_json::Value::Null,
        io_large(),
    )
}

fn new_session(
    eng: &AgentEngine<PanicTransport, RecordingExecutor, NoOpSummarizer>,
    config: AgentConfig,
) -> agent_core::engine::SessionHandle {
    new_session_with_transport(eng, config)
}

fn parse_json(content: &str) -> Value {
    serde_json::from_str(content).expect("valid json")
}

fn agent_block(content: &str) -> Value {
    parse_json(content)
        .get("_agent")
        .cloned()
        .expect("_agent key")
}

fn tool_msg_by_call_id<'a>(
    messages: &'a [AgentMessage],
    call_id: &str,
) -> &'a AgentMessage {
    messages
        .iter()
        .filter(|m| m.role == AgentRole::Tool)
        .find(|m| {
            m.metadata
                .get("call_id")
                .and_then(|v| v.as_str())
                == Some(call_id)
        })
        .unwrap_or_else(|| panic!("tool message for call_id {call_id}"))
}

fn count_agent_bearing_tool_messages(messages: &[AgentMessage]) -> usize {
    messages
        .iter()
        .filter(|m| m.role == AgentRole::Tool && m.content.contains("_agent"))
        .count()
}

fn drive_model_tool_batch(
    eng: &AgentEngine<PanicTransport, RecordingExecutor, NoOpSummarizer>,
    handle: &agent_core::engine::SessionHandle,
    call_ids: &[&str],
    result_jsons: &[&str],
) {
    let tool_calls: Vec<RawToolCall> = call_ids
        .iter()
        .map(|id| RawToolCall {
            id: Some((*id).into()),
            name: "web_search".into(),
            arguments_json: r#"{"query":"x"}"#.into(),
        })
        .collect();

    let _ = eng.next_action(handle);
    eng.submit_model_response(
        handle,
        ModelResponse {
            assistant_text: None,
            tool_calls,
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    match eng.next_action(handle) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(calls.len(), call_ids.len());
        }
        other => panic!("expected ExecuteTools, got {other:?}"),
    }
    eng.submit_tool_results(
        handle,
        call_ids
            .iter()
            .zip(result_jsons.iter())
            .map(|(id, json)| ToolResultRecord {
                call_id: (*id).into(),
                result_json: (*json).into(),
                truncated: false,
                soft_failure: false,
                result_metadata: None,
            })
            .collect(),
    );
}

/// TB-01 — single object result merges `_agent` mid-loop.
#[test]
fn tb_01_single_tool_result_merges_agent_mid_loop() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());
    h.integration_test_seed_guardrails(16, 0, 0, false);

    drive_model_tool_batch(&eng, &h, &["c1"], &[r#"{"results":[]}"#]);

    let snapshot = h.integration_test_snapshot();
    let tool_msg = tool_msg_by_call_id(&snapshot.messages, "c1");
    let parsed = parse_json(&tool_msg.content);
    assert!(parsed.is_object(), "TB-01 content is object");
    let agent = agent_block(&tool_msg.content);
    assert_eq!(agent["tool_calls_used"], 17, "TB-01 used");
    assert_eq!(agent["tool_calls_limit"], 20, "TB-01 limit");
    assert_eq!(agent["tool_calls_remaining"], 3, "TB-01 remaining");
    assert_eq!(agent["allow_tool_call"], true, "TB-01 allow");
    assert!(agent.get("reason").is_none(), "TB-01 reason absent");
    assert!(parsed.get("results").is_some(), "TB-01 results present");
}

/// TB-02 — parallel batch injects `_agent` only on last tool message.
#[test]
fn tb_02_parallel_batch_injects_once_on_last_tool_message() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());

    drive_model_tool_batch(
        &eng,
        &h,
        &["c1", "c2", "c3"],
        &[
            r#"{"results":[{"id":1}]}"#,
            r#"{"results":[{"id":2}]}"#,
            r#"{"results":[{"id":3}]}"#,
        ],
    );

    let snapshot = h.integration_test_snapshot();
    let batch_tools: Vec<_> = snapshot
        .messages
        .iter()
        .filter(|m| {
            m.role == AgentRole::Tool
                && m.metadata
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .is_some_and(|id| ["c1", "c2", "c3"].contains(&id))
        })
        .collect();
    assert_eq!(batch_tools.len(), 3, "TB-02 three batch tool messages");

    let c1 = tool_msg_by_call_id(&snapshot.messages, "c1");
    let c2 = tool_msg_by_call_id(&snapshot.messages, "c2");
    let c3 = tool_msg_by_call_id(&snapshot.messages, "c3");
    assert!(
        !parse_json(&c1.content).as_object().is_some_and(|o| o.contains_key("_agent")),
        "TB-02 c1 no _agent"
    );
    assert!(
        !parse_json(&c2.content).as_object().is_some_and(|o| o.contains_key("_agent")),
        "TB-02 c2 no _agent"
    );
    let agent = agent_block(&c3.content);
    assert_eq!(agent["tool_calls_used"], 3, "TB-02 used");
    assert_eq!(agent["tool_calls_remaining"], 17, "TB-02 remaining");
    assert_eq!(agent["tool_calls_limit"], 20, "TB-02 limit");
    assert_eq!(agent["allow_tool_call"], true, "TB-02 allow");
}

/// TB-03 — first tool call reports full remaining budget.
#[test]
fn tb_03_first_tool_call_reports_full_remaining_budget() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());

    drive_model_tool_batch(&eng, &h, &["c1"], &[r#"{"status":"ok"}"#]);

    let snapshot = h.integration_test_snapshot();
    let tool_msg = tool_msg_by_call_id(&snapshot.messages, "c1");
    let agent = agent_block(&tool_msg.content);
    assert_eq!(agent["tool_calls_used"], 1, "TB-03 used");
    assert_eq!(agent["tool_calls_limit"], 20, "TB-03 limit");
    assert_eq!(agent["tool_calls_remaining"], 19, "TB-03 remaining");
    assert_eq!(agent["allow_tool_call"], true, "TB-03 allow");
}

/// TB-04 — tool-call budget exhausted.
#[test]
fn tb_04_budget_exhausted_disables_tools_with_reason() {
    let mut cfg = AgentConfig::default();
    cfg.max_turns = 1;
    cfg.greedy_append_blocks = false;

    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, cfg);

    drive_model_tool_batch(&eng, &h, &["c1"], &[r#"{"results":[]}"#]);

    let snapshot = h.integration_test_snapshot();
    let tool_msg = tool_msg_by_call_id(&snapshot.messages, "c1");
    let agent = agent_block(&tool_msg.content);
    assert_eq!(agent["tool_calls_used"], 1, "TB-04 used");
    assert_eq!(agent["tool_calls_limit"], 1, "TB-04 limit");
    assert_eq!(agent["tool_calls_remaining"], 0, "TB-04 remaining");
    assert_eq!(agent["allow_tool_call"], false, "TB-04 allow");
    assert_eq!(
        agent["reason"].as_str(),
        Some("tool_call_budget_exhausted"),
        "TB-04 reason"
    );

    let AgentAction::CallModel(req) = eng.next_action(&h) else {
        panic!("TB-04 expected CallModel");
    };
    assert!(!req.allow_tool_call, "TB-04 next ModelRequest allow false");
}

/// TB-05 — soft-failure streak exhausted.
#[test]
fn tb_05_soft_failure_streak_sets_reason() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());
    h.integration_test_seed_guardrails(5, 3, 0, false);

    drive_model_tool_batch(
        &eng,
        &h,
        &["c1"],
        &[r#"{"error":"validation failed"}"#],
    );

    let snapshot = h.integration_test_snapshot();
    let tool_msg = tool_msg_by_call_id(&snapshot.messages, "c1");
    let agent = agent_block(&tool_msg.content);
    assert_eq!(agent["allow_tool_call"], false, "TB-05 allow");
    assert_eq!(
        agent["reason"].as_str(),
        Some("soft_failure_streak_exhausted"),
        "TB-05 reason"
    );
    assert_eq!(agent["tool_calls_used"], 6, "TB-05 used");
    assert_eq!(agent["tool_calls_remaining"], 14, "TB-05 remaining");
    assert_eq!(agent["tool_calls_limit"], 20, "TB-05 limit");
}

/// TB-06 — envelope-parse streak exhausted.
#[test]
fn tb_06_envelope_parse_streak_sets_reason() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());
    h.integration_test_seed_guardrails(2, 0, 3, false);

    let _ = eng.next_action(&h);
    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("c1".into()),
                name: "web_search".into(),
                arguments_json: r#"{"query":"x"}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    // Native tool responses reset envelope streak; re-seed before tool results.
    h.integration_test_seed_guardrails(2, 0, 3, false);
    match eng.next_action(&h) {
        AgentAction::ExecuteTools(_) => {}
        other => panic!("TB-06 expected ExecuteTools, got {other:?}"),
    }
    eng.submit_tool_results(
        &h,
        vec![ToolResultRecord {
            call_id: "c1".into(),
            result_json: r#"{"results":[]}"#.into(),
            truncated: false,
            soft_failure: false,
            result_metadata: None,
        }],
    );

    let snapshot = h.integration_test_snapshot();
    let tool_msg = tool_msg_by_call_id(&snapshot.messages, "c1");
    let agent = agent_block(&tool_msg.content);
    assert_eq!(agent["allow_tool_call"], false, "TB-06 allow");
    assert_eq!(
        agent["reason"].as_str(),
        Some("envelope_parse_streak_exhausted"),
        "TB-06 reason"
    );
    assert_eq!(agent["tool_calls_used"], 3, "TB-06 used");
    assert_eq!(agent["tool_calls_remaining"], 17, "TB-06 remaining");
    assert_eq!(agent["tool_calls_limit"], 20, "TB-06 limit");
}

/// TB-07 — plain-text tool result appends synthetic budget message.
#[test]
fn tb_07_non_json_tool_result_appends_synthetic_budget_message() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());

    drive_model_tool_batch(&eng, &h, &["c1"], &["OK: 3 files"]);

    let snapshot = h.integration_test_snapshot();
    let tool_msg = tool_msg_by_call_id(&snapshot.messages, "c1");
    assert_eq!(tool_msg.content, "OK: 3 files", "TB-07 original content");

    let synthetic = snapshot
        .messages
        .iter()
        .find(|m| {
            m.role == AgentRole::Tool
                && m.metadata
                    .get("agent_tool_budget")
                    .and_then(|v| v.as_bool())
                    == Some(true)
        })
        .expect("TB-07 synthetic budget message");
    assert_eq!(synthetic.id, "tool-agent-budget-1", "TB-07 synthetic id");
    let agent = agent_block(&synthetic.content);
    assert_eq!(agent["tool_calls_used"], 1, "TB-07 used");
    assert_eq!(agent["tool_calls_limit"], 20, "TB-07 limit");
    assert_eq!(agent["tool_calls_remaining"], 19, "TB-07 remaining");
    assert_eq!(agent["allow_tool_call"], true, "TB-07 allow");
    assert!(agent.get("reason").is_none(), "TB-07 reason absent");
}

/// TB-08 — JSON array tool result appends synthetic budget message.
#[test]
fn tb_08_json_array_result_appends_synthetic_budget_message() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());

    drive_model_tool_batch(&eng, &h, &["c1"], &["[1,2,3]"]);

    let snapshot = h.integration_test_snapshot();
    let tool_msg = tool_msg_by_call_id(&snapshot.messages, "c1");
    assert_eq!(tool_msg.content, "[1,2,3]", "TB-08 original array content");

    let synthetics: Vec<_> = snapshot
        .messages
        .iter()
        .filter(|m| {
            m.role == AgentRole::Tool
                && m.metadata
                    .get("agent_tool_budget")
                    .and_then(|v| v.as_bool())
                    == Some(true)
        })
        .collect();
    assert_eq!(synthetics.len(), 1, "TB-08 one synthetic message");
    let agent = agent_block(&synthetics[0].content);
    assert_eq!(agent["tool_calls_used"], 1, "TB-08 used");
    assert_eq!(agent["allow_tool_call"], true, "TB-08 allow");
    assert!(agent.get("reason").is_none(), "TB-08 reason absent");
}

/// TB-09 — greedy append_blocks raises limit to 35.
#[test]
fn tb_09_greedy_append_blocks_raises_limit_to_35() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());
    h.integration_test_seed_guardrails(20, 0, 0, true);

    let _ = eng.next_action(&h);
    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("c1".into()),
                name: "web_search".into(),
                arguments_json: r#"{"query":"x"}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    // Non-append tool responses clear greedy flag; re-seed before budget injection.
    h.integration_test_seed_guardrails(20, 0, 0, true);
    match eng.next_action(&h) {
        AgentAction::ExecuteTools(_) => {}
        other => panic!("TB-09 expected ExecuteTools, got {other:?}"),
    }
    eng.submit_tool_results(
        &h,
        vec![ToolResultRecord {
            call_id: "c1".into(),
            result_json: r#"{"results":[]}"#.into(),
            truncated: false,
            soft_failure: false,
            result_metadata: None,
        }],
    );

    let snapshot = h.integration_test_snapshot();
    let tool_msg = tool_msg_by_call_id(&snapshot.messages, "c1");
    let agent = agent_block(&tool_msg.content);
    assert_eq!(agent["tool_calls_limit"], 41, "TB-09 limit");
    assert_eq!(agent["tool_calls_used"], 21, "TB-09 used");
    assert_eq!(agent["tool_calls_remaining"], 20, "TB-09 remaining");
    assert_eq!(agent["allow_tool_call"], true, "TB-09 allow");
}

/// TB-10 — skip_count auto-finalize path still injects budget.
#[test]
fn tb_10_skip_count_auto_finalize_path_still_injects_budget() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());

    // Reach executed=4 via one normal tool batch from seeded 3.
    h.integration_test_seed_guardrails(3, 0, 0, false);
    drive_model_tool_batch(&eng, &h, &["c1"], &[r#"{"results":[]}"#]);

    // Artifact + append_blocks stop path → auto-finalize with skip_count.
    let _ = eng.next_action(&h);
    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("c-art".into()),
                name: "start_artifact".into(),
                arguments_json: r#"{"title":"T","kind":"document"}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    let _ = eng.next_action(&h);
    eng.submit_tool_results(
        &h,
        vec![ToolResultRecord {
            call_id: "c-art".into(),
            result_json: r#"{"status":"success","artifactId":"art-1"}"#.into(),
            truncated: false,
            soft_failure: false,
            result_metadata: None,
        }],
    );

    let _ = eng.next_action(&h);
    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("c2".into()),
                name: "append_blocks".into(),
                arguments_json: r#"{"artifactId":"art-1","blocks":[]}"#.into(),
            }],
            finish_reason: FinishReason::Stop,
            provider_reasoning_content: None,
        },
    );
    let _ = eng.next_action(&h);
    eng.submit_tool_results(
        &h,
        vec![ToolResultRecord {
            call_id: "c2".into(),
            result_json: r#"{"status":"success","isComplete":false}"#.into(),
            truncated: false,
            soft_failure: false,
            result_metadata: None,
        }],
    );

    match eng.next_action(&h) {
        AgentAction::ExecuteTools(calls) => {
            assert_eq!(calls[0].call_id, "auto-finalize-art-1");
        }
        other => panic!("TB-10 expected auto-finalize ExecuteTools, got {other:?}"),
    }
    eng.submit_tool_results(
        &h,
        vec![ToolResultRecord {
            call_id: "auto-finalize-art-1".into(),
            result_json: r#"{"status":"success","isComplete":true}"#.into(),
            truncated: false,
            soft_failure: false,
            result_metadata: None,
        }],
    );

    let snapshot = h.integration_test_snapshot();
    let budget_msgs: Vec<_> = snapshot
        .messages
        .iter()
        .filter(|m| m.role == AgentRole::Tool && m.content.contains("_agent"))
        .collect();
    assert!(!budget_msgs.is_empty(), "TB-10 budget-bearing message exists");
    let agent = agent_block(&budget_msgs.last().unwrap().content);
    assert_eq!(agent["tool_calls_used"], 6, "TB-10 used");
    assert_eq!(agent["tool_calls_limit"], 26, "TB-10 limit");
    assert_eq!(agent["tool_calls_remaining"], 20, "TB-10 remaining");
    assert_eq!(agent["allow_tool_call"], true, "TB-10 allow");
}

/// TB-11 — invalid phase submit_tool_results does not inject.
#[test]
fn tb_11_invalid_phase_submit_tool_results_does_not_inject() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());

    eng.submit_tool_results(
        &h,
        vec![ToolResultRecord {
            call_id: "c1".into(),
            result_json: r#"{"results":[]}"#.into(),
            truncated: false,
            soft_failure: false,
            result_metadata: None,
        }],
    );

    let snapshot = h.integration_test_snapshot();
    assert!(
        matches!(
            snapshot.terminal_action,
            Some(AgentAction::Failed(AgentError::InvalidTransition { .. }))
        ),
        "TB-11 InvalidTransition"
    );
    assert_eq!(
        count_agent_bearing_tool_messages(&snapshot.messages),
        0,
        "TB-11 zero _agent messages"
    );
}

/// TB-12 — repair reject-only turn merges `_agent` before CallModel.
#[test]
fn tb_12_reject_only_synthetic_turn_merges_agent_before_call_model() {
    use agent_core::repair::{RepairOutcome, ToolRepairPipeline};
    use agent_core::catalog::ToolCatalog;

    let reject_raw = RawToolCall {
        id: Some("c1".into()),
        name: "totally_unknown_tool_xyz".into(),
        arguments_json: "{}".into(),
    };
    let (RepairOutcome::Reject { error_json_for_model }, _) =
        ToolRepairPipeline::repair(&reject_raw, ToolCatalog::global())
    else {
        panic!("TB-12 harness must use a repair Reject path");
    };
    let expected_error: Value = serde_json::from_str(&error_json_for_model).expect("TB-12 reject json");

    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());

    let _ = eng.next_action(&h);
    eng.submit_model_response(
        &h,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![reject_raw],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );

    let AgentAction::CallModel(_) = eng.next_action(&h) else {
        panic!("TB-12 expected CallModel after reject-only turn");
    };

    let snapshot = h.integration_test_snapshot();
    let last_tool = snapshot
        .messages
        .iter()
        .rev()
        .find(|m| m.role == AgentRole::Tool)
        .expect("TB-12 last tool message");
    let parsed = parse_json(&last_tool.content);
    assert_eq!(
        parsed["error"].as_str(),
        expected_error["error"].as_str(),
        "TB-12 error"
    );
    let agent = agent_block(&last_tool.content);
    assert_eq!(agent["tool_calls_used"], 1, "TB-12 used");
    assert_eq!(agent["tool_calls_limit"], 20, "TB-12 limit");
    assert_eq!(agent["tool_calls_remaining"], 19, "TB-12 remaining");
    assert_eq!(agent["allow_tool_call"], true, "TB-12 allow");
    assert!(
        parsed.get("available_tools").is_some(),
        "TB-12 available_tools"
    );
}

/// MD-01 — at most one tool message contains `_agent` per batch.
#[test]
fn md_01_only_one_tool_message_contains_agent_per_batch() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());

    drive_model_tool_batch(
        &eng,
        &h,
        &["c1", "c2", "c3", "c4"],
        &[
            r#"{"results":[]}"#,
            r#"{"results":[]}"#,
            r#"{"results":[]}"#,
            r#"{"results":[]}"#,
        ],
    );

    let snapshot = h.integration_test_snapshot();
    assert_eq!(
        count_agent_bearing_tool_messages(&snapshot.messages),
        1,
        "MD-01 exactly one _agent message"
    );
}

fn count_agent_literal_matches(paths: &[PathBuf]) -> usize {
    if Command::new("rg")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        let output = Command::new("rg")
            .arg(r#""_agent""#)
            .args(paths.iter().map(|p| p.as_os_str()))
            .output()
            .expect("MD-02 rg execution");
        if !output.status.success() && output.status.code() != Some(1) {
            panic!(
                "MD-02 rg failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .count();
    }

    fn walk(dir: &Path, needle: &str, matches: &mut usize) {
        let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("MD-02 read {}: {e}", dir.display()));
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, needle, matches);
            } else if path.extension().is_some_and(|ext| ext == "kt") {
                let content = fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("MD-02 read file {}: {e}", path.display()));
                if content.contains(needle) {
                    *matches += 1;
                }
            }
        }
    }

    let mut matches = 0;
    for path in paths {
        walk(path, "\"_agent\"", &mut matches);
    }
    matches
}

/// MD-02 — production Kotlin sources do not reference `_agent`.
#[test]
fn md_02_production_kotlin_sources_do_not_reference_agent() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.join("../..");
    let paths = [
        repo_root.join("feature/inference/src/main"),
        repo_root.join("core/agent/src/main"),
    ];

    for path in &paths {
        assert!(path.is_dir(), "MD-02 path exists: {}", path.display());
    }

    let match_count = count_agent_literal_matches(&paths);
    assert_eq!(match_count, 0, "MD-02 match count");
}

/// MD-03 — merge keeps original tool payload keys.
#[test]
fn md_03_merge_keeps_original_tool_keys() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());

    drive_model_tool_batch(
        &eng,
        &h,
        &["c1"],
        &[r#"{"results":[{"title":"A"}],"status":"success"}"#],
    );

    let snapshot = h.integration_test_snapshot();
    let tool_msg = tool_msg_by_call_id(&snapshot.messages, "c1");
    let parsed = parse_json(&tool_msg.content);
    assert_eq!(parsed["results"][0]["title"], "A", "MD-03 title");
    assert_eq!(parsed["status"], "success", "MD-03 status");
    assert_eq!(parsed["_agent"]["tool_calls_used"], 1, "MD-03 used");
}

/// MD-04 — reason key absent when allow_tool_call true.
#[test]
fn md_04_reason_key_absent_when_allow_tool_call_true() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());

    drive_model_tool_batch(&eng, &h, &["c1"], &[r#"{"results":[]}"#]);

    let snapshot = h.integration_test_snapshot();
    let tool_msg = tool_msg_by_call_id(&snapshot.messages, "c1");
    let parsed = parse_json(&tool_msg.content);
    assert_eq!(parsed["_agent"]["allow_tool_call"], true, "MD-04 allow");
    assert!(
        parsed["_agent"].get("reason").is_none(),
        "MD-04 reason absent"
    );
}

/// MD-05 — budget injection only on submit paths, not from next_action alone.
#[test]
fn md_05_budget_injection_not_invoked_from_next_action_transport() {
    let transport = Arc::new(RecordingTransport {
        captured_requests: Mutex::new(Vec::new()),
    });
    let eng = AgentEngine::new(
        transport.clone(),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session_with_transport(&eng, AgentConfig::default());

    let AgentAction::CallModel(req) = eng.next_action(&h) else {
        panic!("MD-05 expected CallModel");
    };

    for message in &req.session_messages {
        assert!(
            !message.content.contains("_agent"),
            "MD-05 no _agent before submit_tool_results"
        );
    }

    assert!(
        transport.captured_requests.lock().unwrap().is_empty(),
        "MD-05 transport not invoked on next_action alone"
    );
}

/// MD-06 — merge preserves executor-provided `_agent` fields.
#[test]
fn md_06_merge_preserves_executor_provided_agent_fields() {
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(RecordingExecutor {
            results: Mutex::new(Vec::new()),
        }),
        Arc::new(NoOpSummarizer),
    );
    let h = new_session(&eng, AgentConfig::default());

    drive_model_tool_batch(
        &eng,
        &h,
        &["c1"],
        &[r#"{"_agent":{"source":"executor"},"results":[]}"#],
    );

    let snapshot = h.integration_test_snapshot();
    let tool_msg = tool_msg_by_call_id(&snapshot.messages, "c1");
    let parsed = parse_json(&tool_msg.content);
    assert_eq!(
        parsed["_agent"]["source"].as_str(),
        Some("executor"),
        "MD-06 source"
    );
    assert_eq!(parsed["_agent"]["tool_calls_used"], 1, "MD-06 used");
    assert_eq!(parsed["_agent"]["tool_calls_limit"], 20, "MD-06 limit");
    assert_eq!(parsed["_agent"]["tool_calls_remaining"], 19, "MD-06 remaining");
    assert_eq!(parsed["_agent"]["allow_tool_call"], true, "MD-06 allow");
    assert!(parsed.get("results").is_some(), "MD-06 results present");
}
