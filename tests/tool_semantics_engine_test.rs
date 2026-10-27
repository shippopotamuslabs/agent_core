use std::sync::Arc;

use agent_core::action::AgentAction;
use agent_core::compact::{
    ArtifactSquashCompactionTier, CompactionContext, CompactionOutcome, CompactionTier,
    MicroCompactionTier,
};
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::{AgentError, CompactionTrigger};
use agent_core::host::{AgentToolExecutor, AgentTransport, NoOpSummarizer};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::policy::ContextPolicyTier;
use agent_core::session::{AgentConfig, AgentSession, SessionCapabilities};
use agent_core::tool_semantics::ToolSemanticsConfig;
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall};
use serde_json::{json, Value};

#[path = "support/tool_semantics_support.rs"]
mod semantics;

struct PanicTransport;

impl AgentTransport for PanicTransport {
    fn complete_model_turn(&self, _: ModelRequest) -> Result<ModelResponse, AgentError> {
        panic!("transport is not used by engine tests");
    }
}

struct PanicExecutor;

impl AgentToolExecutor for PanicExecutor {
    fn execute_tools(&self, _: Vec<ToolCallRecord>) -> Result<Vec<ToolResultRecord>, AgentError> {
        panic!("executor is not used by engine tests");
    }
}

fn engine() -> AgentEngine<PanicTransport, PanicExecutor, NoOpSummarizer> {
    AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExecutor),
        Arc::new(NoOpSummarizer),
    )
}

fn session_with(active_schema: &str, semantics: ToolSemanticsConfig) -> agent_core::SessionHandle {
    let eng = engine();
    eng.create_session(
        vec![AgentMessage {
            id: "u1".to_string(),
            role: AgentRole::User,
            content: "write".to_string(),
            metadata: Value::Null,
        }],
        AgentConfig {
            tool_semantics: semantics,
            tool_result_budget_cloud_bytes: 1024,
            tool_result_budget_relaxed_bytes: 1024,
            tool_result_truncation_pressure_ratio: 0.0,
            ..AgentConfig::default()
        },
        SessionCapabilities {
            tool_schema_deferral_supported: true,
            context_policy_tier: ContextPolicyTier::HostedMutable,
            ..SessionCapabilities::default()
        },
        Value::Null,
        SessionIoParams {
            provider: "anthropic".to_string(),
            context_window_tokens: 200_000,
            tool_catalog_json: r#"[
              {"name":"open_tools","aliases":[],"reproducible":true,"schema_hint":null},
              {"name":"search_web","aliases":[],"reproducible":true,"schema_hint":null},
              {"name":"write_doc","aliases":[],"reproducible":false,"schema_hint":null},
              {"name":"lookup_memory","aliases":[],"reproducible":true,"schema_hint":null}
            ]"#.to_string(),
            tool_schema_registry_json: r#"[{"name":"open_tools"},{"name":"search_web"},{"name":"write_doc"},{"name":"lookup_memory"}]"#.to_string(),
            tools_schema_json: active_schema.to_string(),
            system_prompt: "sys".to_string(),
        },
    )
}

fn compaction_ctx() -> CompactionContext {
    CompactionContext {
        provider: "anthropic".to_string(),
        context_window_tokens: 200_000,
        tools_schema_json: "[]".to_string(),
        system_prompt: String::new(),
        context_sidecars_estimate: String::new(),
        capabilities: SessionCapabilities::default(),
        trigger: CompactionTrigger::MidTurn,
        suppress_summary_tier: false,
    }
}

fn tool_msg(id: &str, name: &str, content: &str) -> AgentMessage {
    AgentMessage {
        id: id.to_string(),
        role: AgentRole::Tool,
        content: content.to_string(),
        metadata: json!({ "tool_name": name }),
    }
}

#[test]
fn ts_017_custom_disclosure_tool_is_always_callable() {
    let eng = engine();
    let handle = session_with(
        r#"[{"name":"open_tools","parameters":{}},{"name":"search_web","parameters":{}}]"#,
        semantics::custom_semantics(),
    );
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-open".to_string()),
                name: "open_tools".to_string(),
                arguments_json: r#"{"names":["write_doc"]}"#.to_string(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );

    match eng.next_action(&handle) {
        AgentAction::ExecuteTools(calls) => assert_eq!(calls[0].name, "open_tools"),
        other => panic!("expected ExecuteTools, got {other:?}"),
    }
}

#[test]
fn ts_018_deferred_custom_tool_rejection_names_configured_disclosure_tool() {
    let eng = engine();
    let handle = session_with(
        r#"[{"name":"open_tools","parameters":{}},{"name":"search_web","parameters":{}}]"#,
        semantics::custom_semantics(),
    );
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-write".to_string()),
                name: "write_doc".to_string(),
                arguments_json: r#"{"docId":"doc-1","chunks":[]}"#.to_string(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    let _ = eng.next_action(&handle);
    let snapshot = handle.integration_test_snapshot();
    let rejection = snapshot
        .messages
        .iter()
        .rev()
        .find(|m| m.role == AgentRole::Tool && m.metadata["call_id"] == "call-write")
        .unwrap();
    let payload: Value = serde_json::from_str(&rejection.content).unwrap();

    assert_eq!(
        payload["error"],
        "Tool 'write_doc' is not active. Call open_tools first."
    );
    assert_eq!(
        payload["available_tools"],
        json!(["open_tools", "search_web"])
    );
}

#[test]
fn ts_019_missing_disclosure_semantics_fails_closed() {
    let eng = engine();
    let handle = session_with(
        r#"[{"name":"search_web","parameters":{}}]"#,
        ToolSemanticsConfig::agnostic(),
    );
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-write".to_string()),
                name: "write_doc".to_string(),
                arguments_json: r#"{"docId":"doc-1","chunks":[]}"#.to_string(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    let _ = eng.next_action(&handle);
    let snapshot = handle.integration_test_snapshot();
    let rejection = snapshot
        .messages
        .iter()
        .rev()
        .find(|m| m.role == AgentRole::Tool && m.metadata["call_id"] == "call-write")
        .unwrap();
    let payload: Value = serde_json::from_str(&rejection.content).unwrap();

    assert_eq!(payload["error"], "Tool 'write_doc' is not active.");
    assert_eq!(payload["available_tools"], json!(["search_web"]));
}

#[test]
fn ts_024_custom_append_result_skips_truncation() {
    let eng = engine();
    let handle = session_with(
        r#"[{"name":"write_doc","parameters":{}}]"#,
        semantics::custom_semantics(),
    );
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-write".to_string()),
                name: "write_doc".to_string(),
                arguments_json: r#"{"docId":"doc-1","chunks":[]}"#.to_string(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    assert!(matches!(
        eng.next_action(&handle),
        AgentAction::ExecuteTools(_)
    ));
    let result_json = json!({"data":"x".repeat(2048)}).to_string();
    eng.submit_tool_results(
        &handle,
        vec![ToolResultRecord {
            call_id: "call-write".to_string(),
            result_json: result_json.clone(),
            truncated: false,
            soft_failure: false,
            activated_tool_names: Vec::new(),
            result_metadata: None,
        }],
    );
    let snapshot = handle.integration_test_snapshot();
    let tool = snapshot
        .messages
        .iter()
        .rev()
        .find(|m| m.role == AgentRole::Tool && m.metadata["call_id"] == "call-write")
        .unwrap();

    assert_eq!(tool.content, result_json);
    let result: Value = serde_json::from_str(&tool.content).unwrap();
    assert_eq!(result["data"].as_str().unwrap().len(), 2048);
    assert_eq!(tool.metadata["truncated"].as_bool(), Some(false));
}

#[test]
fn ts_025_non_exempt_large_result_is_truncated() {
    let eng = engine();
    let handle = session_with(
        r#"[{"name":"lookup_memory","parameters":{}}]"#,
        ToolSemanticsConfig::agnostic(),
    );
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-lookup".to_string()),
                name: "lookup_memory".to_string(),
                arguments_json: r#"{"query":"alpha"}"#.to_string(),
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
        vec![ToolResultRecord {
            call_id: "call-lookup".to_string(),
            result_json: json!({"data":"x".repeat(2048)}).to_string(),
            truncated: false,
            soft_failure: false,
            activated_tool_names: Vec::new(),
            result_metadata: None,
        }],
    );
    let snapshot = handle.integration_test_snapshot();
    let tool = snapshot
        .messages
        .iter()
        .rev()
        .find(|m| m.role == AgentRole::Tool && m.metadata["call_id"] == "call-lookup")
        .unwrap();

    assert!(tool.content.contains("truncated"));
    assert_eq!(tool.metadata["truncated"].as_bool(), Some(true));
}

#[test]
fn ts_026_micro_compaction_preserves_configured_tool() {
    let session = AgentSession::new(
        vec![tool_msg("t1", "write_doc", "DOCUMENT_BODY")],
        Value::Null,
    );
    let config = AgentConfig {
        micro_keep_recent: 0,
        tool_semantics: semantics::custom_semantics(),
        ..AgentConfig::default()
    };

    let outcome = MicroCompactionTier.apply(session, &config, &compaction_ctx());

    assert!(matches!(outcome, CompactionOutcome::NotApplicable));
}

#[test]
fn ts_029_custom_artifact_squash_collapses_configured_append_loop() {
    let session = AgentSession::new(
        vec![
            AgentMessage {
                id: "a1".to_string(),
                role: AgentRole::Assistant,
                content: r#"call:write_doc {"docId":"doc-1","chunks":[{"text":"Part 1"}]}"#
                    .to_string(),
                metadata: json!({"raw_tool_calls":[{"name":"write_doc"}]}),
            },
            tool_msg("t1", "write_doc", r#"{"status":"success","chunkCount":1}"#),
            AgentMessage {
                id: "a2".to_string(),
                role: AgentRole::Assistant,
                content: r#"call:write_doc {"docId":"doc-1","chunks":[{"text":"Part 2"}]}"#
                    .to_string(),
                metadata: json!({"raw_tool_calls":[{"name":"write_doc"}]}),
            },
            tool_msg("t2", "write_doc", r#"{"status":"success","chunkCount":2}"#),
        ],
        Value::Null,
    );
    let config = AgentConfig {
        tool_semantics: semantics::custom_semantics(),
        ..AgentConfig::default()
    };

    match ArtifactSquashCompactionTier.apply(session, &config, &compaction_ctx()) {
        CompactionOutcome::Applied(applied, _) => {
            assert!(applied.messages.iter().any(|m| {
                m.role == AgentRole::System
                    && m.content
                        == "Artifact generation in progress. 2 blocks written so far. Last block: `Part 2`. Continue appending."
            }));
            assert!(!applied
                .messages
                .iter()
                .any(|m| m.content.contains("call:write_doc")));
        }
        other => panic!("expected Applied, got {other:?}"),
    }
}

#[test]
fn ts_033_hard_coded_disclosure_name_fails_custom_only_config() {
    let eng = engine();
    let handle = session_with(
        r#"[{"name":"open_tools","parameters":{}}]"#,
        semantics::custom_semantics(),
    );
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-open".to_string()),
                name: "open_tools".to_string(),
                arguments_json: r#"{"names":["write_doc"]}"#.to_string(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );

    match eng.next_action(&handle) {
        AgentAction::ExecuteTools(calls) => assert_eq!(calls[0].name, "open_tools"),
        other => panic!("expected ExecuteTools, got {other:?}"),
    }
    let snapshot = handle.integration_test_snapshot();
    assert_eq!(
        snapshot
            .messages
            .iter()
            .filter(|m| m.role == AgentRole::Tool)
            .count(),
        0,
    );
}

#[test]
fn ts_034_hard_coded_artifact_squash_misses_custom_append_tool() {
    let session = AgentSession::new(
        vec![
            AgentMessage {
                id: "a1".to_string(),
                role: AgentRole::Assistant,
                content: r#"call:write_doc {"docId":"doc-1","chunks":[{"text":"Only block"}]}"#
                    .to_string(),
                metadata: json!({"raw_tool_calls":[{"name":"write_doc"}]}),
            },
            tool_msg("t1", "write_doc", r#"{"status":"success","chunkCount":1}"#),
        ],
        Value::Null,
    );
    let config = AgentConfig {
        tool_semantics: semantics::custom_semantics(),
        ..AgentConfig::default()
    };

    match ArtifactSquashCompactionTier.apply(session, &config, &compaction_ctx()) {
        CompactionOutcome::Applied(applied, _) => {
            assert!(applied.messages.iter().any(|m| {
                m.role == AgentRole::System
                    && m.content
                        == "Artifact generation in progress. 1 blocks written so far. Last block: `Only block`. Continue appending."
            }));
            assert!(!applied
                .messages
                .iter()
                .any(|m| m.content.contains("write_doc")));
        }
        other => panic!("expected Applied, got {other:?}"),
    }
}
