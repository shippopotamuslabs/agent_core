use std::sync::Arc;

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::{AgentError, AgentEvent, CompactionTrigger};
use agent_core::forgecode_gap_compaction::destructive_eviction_ids_with_retention;
use agent_core::host::{AgentToolExecutor, AgentTransport, NoOpSummarizer};
use agent_core::loop_hooks::{
    AgentLoopHook, DoomLoopReminderHook, HookCheckpoint, HookContext, PendingWorkReminderHook,
};
use agent_core::loop_metrics::{LoopMetrics, PendingWorkItem, ToolSignatureWindow};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::request_pipeline::{RequestPipeline, RequestTransformer};
use agent_core::session::{AgentConfig, AgentSession, SessionCapabilities};
use agent_core::state_ledger::StateLedger;
use agent_core::tool_policy::{
    assert_structured_metadata_present_for_mutation_defense, delegation_tool_name,
    execute_delegation_call, prepare_write_or_patch, process_policy_denial, record_timeout_result,
};
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall};

fn call(call_id: &str, name: &str, arguments_json: &str) -> ToolCallRecord {
    ToolCallRecord {
        call_id: call_id.into(),
        name: name.into(),
        arguments_json: arguments_json.into(),
        attempt_metadata: None,
        repair_metadata: None,
    }
}

fn message(id: &str, role: AgentRole, content: &str) -> AgentMessage {
    AgentMessage {
        id: id.into(),
        role,
        content: content.into(),
        metadata: serde_json::Value::Null,
    }
}

fn hook_context<'a>(
    checkpoint: HookCheckpoint,
    session: &'a AgentSession,
    state_ledger: &'a StateLedger,
    loop_metrics: &'a LoopMetrics,
) -> HookContext<'a> {
    HookContext {
        checkpoint,
        turn_index: 0,
        session,
        state_ledger,
        loop_metrics,
    }
}

fn model_request(system_prompt: &str) -> ModelRequest {
    ModelRequest {
        session_messages: vec![],
        system_prompt: system_prompt.into(),
        tools_schema_json: "[]".into(),
        rendered_tool_catalog_json: "[]".into(),
        allow_tool_call: true,
        context_references: vec![],
        state_ledger: None,
        tool_context_index: None,
        file_context_index: None,
        cache_hints: vec![],
        provider_context: Default::default(),
        state_ledger_delta_json: None,
        tool_context_index_delta_json: None,
        file_context_index_delta_json: None,
    }
}

fn tool_result(call_id: &str) -> ToolResultRecord {
    ToolResultRecord {
        call_id: call_id.into(),
        result_json: r#"{"status":"success"}"#.into(),
        truncated: false,
        soft_failure: false,
        activated_tool_names: Vec::new(),
        result_metadata: None,
    }
}

struct PanicTransport;

impl AgentTransport for PanicTransport {
    fn complete_model_turn(&self, _: ModelRequest) -> Result<ModelResponse, AgentError> {
        panic!("red tests drive submit_model_response manually");
    }
}

struct PanicExecutor;

impl AgentToolExecutor for PanicExecutor {
    fn execute_tools(&self, _: Vec<ToolCallRecord>) -> Result<Vec<ToolResultRecord>, AgentError> {
        panic!("red tests drive submit_tool_results manually");
    }
}

fn red_engine() -> AgentEngine<PanicTransport, PanicExecutor, NoOpSummarizer> {
    AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExecutor),
        Arc::new(NoOpSummarizer),
    )
}

fn session_io(tools_schema_json: &str, context_window_tokens: u32) -> SessionIoParams {
    SessionIoParams {
        provider: "anthropic".into(),
        context_window_tokens,
        tool_catalog_json: "[]".into(),
        tool_schema_registry_json: "[]".into(),
        tools_schema_json: tools_schema_json.into(),
        system_prompt: "sys".into(),
    }
}

fn response_with_tool(call_id: &str, name: &str) -> ModelResponse {
    ModelResponse {
        assistant_text: None,
        tool_calls: vec![RawToolCall {
            id: Some(call_id.into()),
            name: name.into(),
            arguments_json: r#"{"query":"same"}"#.into(),
        }],
        finish_reason: FinishReason::ToolUse,
        provider_reasoning_content: None,
    }
}

fn action_name(action: &AgentAction) -> &'static str {
    match action {
        AgentAction::CallModel(_) => "call_model",
        AgentAction::ExecuteTools(_) => "execute_tools",
        AgentAction::RequestSummary(_) => "request_summary",
        AgentAction::Finished => "finished",
        AgentAction::Failed(_) => "failed",
        AgentAction::ManualCompactCompleted(_) => "manual_compact_completed",
    }
}

fn next_model_request(
    config: AgentConfig,
    capabilities: SessionCapabilities,
    tools_schema_json: &str,
) -> ModelRequest {
    let engine = red_engine();
    let handle = engine.create_session(
        vec![message("u1", AgentRole::User, "next")],
        config,
        capabilities,
        serde_json::Value::Null,
        session_io(tools_schema_json, 200_000),
    );

    match engine.next_action(&handle) {
        AgentAction::CallModel(request) => request,
        other => panic!("expected model request, got {other:?}"),
    }
}

struct AppendTransformer(&'static str);

impl RequestTransformer for AppendTransformer {
    fn transform(&self, mut request: ModelRequest) -> Result<ModelRequest, AgentError> {
        request.system_prompt.push('|');
        request.system_prompt.push_str(self.0);
        Ok(request)
    }
}

#[test]
fn fcg_001_repeated_tool_attempts_are_counted_exactly() {
    let mut metrics = LoopMetrics::default();
    let call = call("call-1", "web_search", r#"{"query":"rust"}"#);

    metrics.record_tool_call(&call);
    metrics.record_tool_call(&call);
    metrics.record_tool_call(&call);

    let doom_loop = metrics.detect_doom_loop(3).expect("doom loop");
    assert_eq!(doom_loop.repetition_count, 3);
    assert_eq!(metrics.tool_attempts["web_search"], 3);
}

#[test]
fn fcg_002_patch_is_allowed_after_prior_read() {
    let mut metrics = LoopMetrics::default();
    metrics.files_read.insert("/repo/src/lib.rs".into());

    assert!(metrics.can_edit_path("/repo/src/lib.rs"));
}

#[test]
fn fcg_003_overwrite_is_denied_without_prior_read() {
    let config = AgentConfig {
        require_read_before_edit: true,
        ..AgentConfig::default()
    };
    let metrics = LoopMetrics::default();

    let result = prepare_write_or_patch(&config, &metrics, "/repo/src/lib.rs");

    match result {
        Err(AgentError::PolicyRejected { reason }) => {
            assert_eq!(reason, "read_before_edit_required")
        }
        other => panic!("expected read_before_edit_required policy rejection, got {other:?}"),
    }
}

#[test]
fn fcg_004_doom_loop_reminder_appears_once() {
    let mut metrics = LoopMetrics::default();
    metrics.repeated_tool_signatures.push(ToolSignatureWindow {
        signature: r#"web_search:{"query":"same"}"#.into(),
        repetition_count: 3,
    });
    let session = AgentSession::new(vec![], serde_json::Value::Null);
    let ledger = StateLedger::default();
    let context = hook_context(
        HookCheckpoint::BeforeModelRequest,
        &session,
        &ledger,
        &metrics,
    );

    let effect = DoomLoopReminderHook
        .evaluate(&context)
        .expect("hook effect");

    assert_eq!(effect.injected_messages.len(), 1);
    assert_eq!(
        effect.injected_messages[0].content,
        "You are repeating the same tool pattern. Change approach before the next tool call."
    );
    assert!(!effect.finish_blocked);
}

#[test]
fn fcg_005_pending_work_guard_blocks_finish() {
    let mut metrics = LoopMetrics::default();
    metrics.pending_work_items.push(PendingWorkItem {
        text: "verify failing tests".into(),
    });
    let session = AgentSession::new(vec![], serde_json::Value::Null);
    let ledger = StateLedger::default();
    let context = hook_context(HookCheckpoint::BeforeFinish, &session, &ledger, &metrics);

    let effect = PendingWorkReminderHook::default()
        .evaluate(&context)
        .expect("hook effect");

    assert!(effect.finish_blocked);
    assert_eq!(
        effect.injected_messages[0].content,
        "Pending work remains: verify failing tests"
    );
}

#[test]
fn fcg_006_hook_ordering_is_stable() {
    let engine = red_engine();
    let handle = engine.create_session(
        vec![message("u1", AgentRole::User, "start")],
        AgentConfig {
            enable_loop_hooks: true,
            ..AgentConfig::default()
        },
        SessionCapabilities {
            lifecycle_hooks_supported: true,
            ..SessionCapabilities::default()
        },
        serde_json::Value::Null,
        session_io("[]", 200_000),
    );

    assert!(matches!(
        engine.next_action(&handle),
        AgentAction::CallModel(_)
    ));
    engine.submit_model_response(&handle, response_with_tool("c1", "web_search"));
    assert!(matches!(
        engine.next_action(&handle),
        AgentAction::ExecuteTools(_)
    ));
    engine.submit_tool_results(&handle, vec![tool_result("c1")]);
    assert!(matches!(
        engine.next_action(&handle),
        AgentAction::CallModel(_)
    ));
    engine.submit_model_response(&handle, ModelResponse::text_only("done"));
    assert!(matches!(engine.next_action(&handle), AgentAction::Finished));

    let order = engine
        .drain_events(&handle)
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::HookInvoked { checkpoint } => Some(checkpoint),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        order,
        vec![
            "before_model_request".to_string(),
            "after_model_response".to_string(),
            "before_tool_execution".to_string(),
            "after_tool_result".to_string(),
            "before_finish".to_string(),
        ]
    );
}

#[test]
fn fcg_007_absolute_token_threshold_triggers_compaction() {
    let engine = red_engine();
    let config = AgentConfig {
        threshold_ratio: 1.0,
        preflight_token_threshold: Some(1200),
        ..AgentConfig::default()
    };
    let initial = "token ".repeat(1300);
    let handle = engine.create_session(
        vec![message("u1", AgentRole::User, &initial)],
        config,
        SessionCapabilities {
            summarization_allowed: true,
            ..SessionCapabilities::default()
        },
        serde_json::Value::Null,
        session_io("[]", 200_000),
    );

    match engine.next_action(&handle) {
        AgentAction::RequestSummary(request) => {
            assert_eq!(request.trigger, CompactionTrigger::Preflight);
        }
        other => panic!("expected preflight compaction before CallModel, got {other:?}"),
    }
}

#[test]
fn fcg_008_turn_threshold_triggers_compaction() {
    let engine = red_engine();
    let config = AgentConfig {
        turn_threshold: Some(4),
        ..AgentConfig::default()
    };
    let prior_turns = vec![
        message("u1", AgentRole::User, "one"),
        message("a1", AgentRole::Assistant, "two"),
        message("u2", AgentRole::User, "three"),
        message("a2", AgentRole::Assistant, "four"),
    ];
    let handle = engine.create_session(
        prior_turns,
        config,
        SessionCapabilities {
            summarization_allowed: true,
            ..SessionCapabilities::default()
        },
        serde_json::Value::Null,
        session_io("[]", 200_000),
    );

    match engine.next_action(&handle) {
        AgentAction::RequestSummary(request) => {
            assert_eq!(request.trigger, CompactionTrigger::Preflight);
        }
        other => panic!("expected turn-threshold compaction before CallModel, got {other:?}"),
    }
}

#[test]
fn fcg_009_turn_end_compaction_fires_on_user_tail() {
    let engine = red_engine();
    let config = AgentConfig {
        compact_on_turn_end: true,
        ..AgentConfig::default()
    };
    let handle = engine.create_session(
        vec![message("u1", AgentRole::User, "tail")],
        config,
        SessionCapabilities {
            summarization_allowed: true,
            ..SessionCapabilities::default()
        },
        serde_json::Value::Null,
        session_io("[]", 200_000),
    );

    assert!(matches!(
        engine.next_action(&handle),
        AgentAction::CallModel(_)
    ));
    engine.submit_model_response(&handle, response_with_tool("c1", "web_search"));
    assert!(matches!(
        engine.next_action(&handle),
        AgentAction::ExecuteTools(_)
    ));
    engine.submit_tool_results(&handle, vec![tool_result("c1")]);

    match engine.next_action(&handle) {
        AgentAction::RequestSummary(request) => {
            assert_eq!(request.trigger, CompactionTrigger::PostToolResult);
        }
        other => panic!("expected post-tool-result turn-end compaction, got {other:?}"),
    }
}

#[test]
fn fcg_010_request_transformers_run_in_order() {
    let pipeline = RequestPipeline {
        transformers: vec![
            Arc::new(AppendTransformer("alpha")),
            Arc::new(AppendTransformer("beta")),
        ],
    };

    let output = pipeline
        .transform(model_request("seed"))
        .expect("transformed request");

    assert_eq!(output.system_prompt, "seed|alpha|beta");
}

#[test]
fn fcg_011_runtime_tool_view_hides_unsafe_edit_tools() {
    let config = AgentConfig {
        require_read_before_edit: true,
        ..AgentConfig::default()
    };
    let catalog_json = r#"["web_search","patch_file","write_file"]"#;

    let request = next_model_request(config, SessionCapabilities::default(), catalog_json);
    let rendered: serde_json::Value = serde_json::from_str(&request.rendered_tool_catalog_json)
        .expect("rendered_tool_catalog_json must be valid JSON");

    assert!(rendered
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool == "web_search"));
    assert!(!rendered
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool == "patch_file"));
    assert!(!rendered
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool == "write_file"));
}

#[test]
fn fcg_012_delegation_metadata_stays_inside_tool_metadata() {
    let action = execute_delegation_call("c-delegate");

    assert!(matches!(action, AgentAction::ExecuteTools(_)));
    assert_eq!(delegation_tool_name(&action), Some("task_delegate"));
}

#[test]
fn fcg_013_tool_result_metadata_is_structured() {
    let engine = red_engine();
    let handle = engine.create_session(
        vec![message("u1", AgentRole::User, "run tool")],
        AgentConfig {
            default_tool_timeout_ms: Some(300000),
            ..AgentConfig::default()
        },
        SessionCapabilities::default(),
        serde_json::Value::Null,
        session_io("[]", 200_000),
    );

    assert!(matches!(
        engine.next_action(&handle),
        AgentAction::CallModel(_)
    ));
    engine.submit_model_response(&handle, response_with_tool("c1", "web_search"));
    let calls = match engine.next_action(&handle) {
        AgentAction::ExecuteTools(calls) => calls,
        other => panic!("expected ExecuteTools with structured attempt metadata, got {other:?}"),
    };
    let metadata = calls[0]
        .attempt_metadata
        .as_ref()
        .expect("ToolCallRecord must carry structured ToolAttemptMetadata");

    assert_eq!(metadata.attempt_index, 2);
    assert_eq!(metadata.attempts_remaining, Some(1));
    assert_eq!(metadata.timeout_ms, Some(300000));
}

#[test]
fn fcg_014_pending_work_reminder_deduplicates() {
    let mut metrics = LoopMetrics::default();
    metrics.pending_work_items.push(PendingWorkItem {
        text: "verify failing tests".into(),
    });
    let session = AgentSession::new(vec![], serde_json::Value::Null);
    let ledger = StateLedger::default();
    let context = hook_context(HookCheckpoint::BeforeFinish, &session, &ledger, &metrics);
    let hook = PendingWorkReminderHook {
        last_injected_content: Some("Pending work remains: verify failing tests".into()),
    };

    let effect = hook.evaluate(&context).expect("hook effect");

    assert_eq!(effect.injected_messages.len(), 0);
    assert!(effect.finish_blocked);
}

#[test]
fn fcg_015_retention_window_preserves_the_last_two_messages() {
    let config = AgentConfig {
        retention_window_messages: 2,
        ..AgentConfig::default()
    };
    let messages = vec![
        message("m1", AgentRole::User, "one"),
        message("m2", AgentRole::Assistant, "two"),
        message("m3", AgentRole::User, "three"),
        message("m4", AgentRole::Assistant, "four"),
    ];

    let selected =
        destructive_eviction_ids_with_retention(&config, &messages).expect("eviction ids");

    assert!(!selected.contains(&"m3".to_string()));
    assert!(!selected.contains(&"m4".to_string()));
}

#[test]
fn fcg_016_delegation_tools_are_removed_when_unsupported() {
    let capabilities = SessionCapabilities {
        delegation_tools_supported: false,
        ..SessionCapabilities::default()
    };

    let request = next_model_request(AgentConfig::default(), capabilities, r#"["task_delegate"]"#);
    let rendered: serde_json::Value = serde_json::from_str(&request.rendered_tool_catalog_json)
        .expect("rendered_tool_catalog_json must be valid JSON");

    assert!(!rendered
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool == "task_delegate"));
}

#[test]
fn fcg_017_policy_denial_is_typed() {
    let outcome = process_policy_denial("shell_exec").expect("policy denial outcome");

    assert!(outcome.result_metadata.policy_denied);
    assert!(outcome
        .events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolPolicyDenied { .. })));
    assert!(outcome.result_json.contains("policy_denied"));
}

#[test]
fn fcg_018_timeout_is_typed() {
    let outcome = record_timeout_result(100).expect("timeout outcome");

    assert!(outcome.result_metadata.timed_out);
    assert_eq!(outcome.result_metadata.timeout_ms, Some(100));
    assert!(outcome
        .events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolTimedOut { .. })));
}

#[test]
fn fcg_019_end_to_end_order_remains_deterministic() {
    let engine = red_engine();
    let handle = engine.create_session(
        vec![message("u1", AgentRole::User, "start")],
        AgentConfig {
            enable_loop_hooks: true,
            post_tool_result_token_threshold: Some(1),
            ..AgentConfig::default()
        },
        SessionCapabilities {
            lifecycle_hooks_supported: true,
            request_transform_pipeline_supported: true,
            ..SessionCapabilities::default()
        },
        serde_json::Value::Null,
        session_io("[]", 200_000),
    );

    let mut order = Vec::new();
    let first = engine.next_action(&handle);
    order.push(action_name(&first));
    assert!(matches!(first, AgentAction::CallModel(_)));

    engine.submit_model_response(&handle, response_with_tool("c1", "web_search"));
    let second = engine.next_action(&handle);
    order.push(action_name(&second));
    assert!(matches!(second, AgentAction::ExecuteTools(_)));

    engine.submit_tool_results(&handle, vec![tool_result("c1")]);
    let third = engine.next_action(&handle);
    order.push(action_name(&third));

    assert_eq!(order, vec!["call_model", "execute_tools", "call_model"]);
    assert!(
        engine
            .drain_events(&handle)
            .iter()
            .any(|event| matches!(event, AgentEvent::HookInvoked { .. })),
        "enabled hooks must run during the deterministic loop"
    );
}

#[test]
fn fcg_020_string_only_reminder_is_insufficient() {
    let string_only_result = ToolResultRecord {
        call_id: "call-1".into(),
        result_json: r#"{"message":"policy_denied"}"#.into(),
        truncated: false,
        soft_failure: false,
        activated_tool_names: Vec::new(),
        result_metadata: None,
    };

    match assert_structured_metadata_present_for_mutation_defense(&string_only_result) {
        Err(AgentError::PolicyRejected { reason }) => {
            assert!(
                reason.contains("missing structured metadata fields"),
                "expected missing structured metadata fields reason, got {reason}"
            );
        }
        other => panic!("expected missing structured metadata fields error, got {other:?}"),
    }
}
