//! Integration-test harness for `FEAT-long-agent-loop-robustness` scenarios.

use crate::action::AgentAction;
use crate::compact::history_snapshot::snapshot_history_range;
use crate::compact::{
    apply_summary_result, CompactionContext, CompactionOutcome,
    CompactionTier as CompactionTierTrait, SummaryCompactionTier,
};
use crate::context_store::ContextStore;
use crate::engine::{AgentEngine, SessionHandle, SessionIoParams};
use crate::events::{AgentError, AgentEvent, CompactionTier, CompactionTrigger};
use crate::long_loop_policy::{effective_max_tool_turns, should_stop_tool_rounds, LongLoopPolicy};
use crate::message::{AgentMessage, AgentRole, MessageMeta, ToolResultRecord};
use crate::session::{AgentConfig, AgentSession, SessionCapabilities};
use crate::sidecar_builder::ProviderContextSnapshot;
use crate::state_ledger::StateLedger;
use crate::transcript_revision::transcript_revision_of;
use crate::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall, SummaryResult};

/// Outcome of a simulated multi-tool user turn (LALR-01, LALR-11, LALR-20).
#[derive(Debug, Clone)]
pub struct SimulatedLongLoopOutcome {
    pub model_requests: Vec<ModelRequest>,
    pub final_action: AgentAction,
    pub events: Vec<AgentEvent>,
    pub session_message_count: usize,
    pub transcript_revision: u64,
    pub semantic_fact_preserved: bool,
}

const SEMANTIC_FACT: &str = "PROJECT_CODENAME=ENCLAVE";

fn compaction_ctx(trigger: CompactionTrigger) -> CompactionContext {
    CompactionContext {
        provider: "anthropic".into(),
        context_window_tokens: 200_000,
        tools_schema_json: "[]".into(),
        system_prompt: String::new(),
        context_sidecars_estimate: String::new(),
        capabilities: SessionCapabilities {
            summarization_allowed: true,
            ..Default::default()
        },
        suppress_summary_tier: false,
        trigger,
    }
}

fn tool_message(id: &str, name: &str, content: &str) -> AgentMessage {
    AgentMessage {
        id: id.into(),
        role: AgentRole::Tool,
        content: content.into(),
        metadata: MessageMeta::new().with_tool_name(name),
    }
}

/// Runs 25 sequential tool round-trips with compaction enabled.
pub fn run_twenty_five_tool_turn_loop(
    engine: &AgentEngine,
    handle: &SessionHandle,
    _config: &AgentConfig,
    tool_name: &str,
) -> SimulatedLongLoopOutcome {
    let resolved_tool = if tool_name == "noop" {
        "web_search"
    } else {
        tool_name
    };
    handle.lalr_prepare_long_loop_run(30);
    handle.lalr_seed_user_message_if_empty("long loop task");

    let tool_response = ModelResponse {
        assistant_text: None,
        tool_calls: vec![RawToolCall {
            id: Some("call-1".into()),
            name: resolved_tool.into(),
            arguments_json: r#"{"query":"long-loop"}"#.into(),
        }],
        finish_reason: FinishReason::ToolUse,
        provider_reasoning_content: None,
    };

    let mut model_requests = Vec::new();
    let mut all_events = Vec::new();
    let mut final_action = AgentAction::Finished;

    while model_requests.len() < 25 {
        match engine.next_action(handle) {
            AgentAction::CallModel(req) => {
                model_requests.push(req);
                engine.submit_model_response(handle, tool_response.clone());
            }
            AgentAction::ExecuteTools(calls) => {
                let results: Vec<ToolResultRecord> = calls
                    .into_iter()
                    .map(|c| ToolResultRecord {
                        call_id: c.call_id,
                        result_json: format!(
                            r#"{{"status":"ok","payload":"{}"}}"#,
                            "x".repeat(1500)
                        ),
                        truncated: false,
                        soft_failure: false,
                        activated_tool_names: Vec::new(),
                        result_metadata: None,
                    })
                    .collect();
                engine.submit_tool_results(handle, results);
            }
            AgentAction::RequestSummary(req) => {
                engine.submit_summary(
                    handle,
                    SummaryResult {
                        summary_text: format!("Summary preserving {SEMANTIC_FACT} and progress."),
                        last_message_id: req.last_message_id,
                    },
                );
            }
            AgentAction::Failed(err) => {
                final_action = AgentAction::Failed(err);
                break;
            }
            AgentAction::Finished => {
                final_action = AgentAction::Finished;
                break;
            }
            _ => {}
        }
        all_events.extend(engine.drain_events(handle));
    }

    if !matches!(final_action, AgentAction::Failed(_)) {
        loop {
            match engine.next_action(handle) {
                AgentAction::ExecuteTools(calls) => {
                    let results: Vec<ToolResultRecord> = calls
                        .into_iter()
                        .map(|c| ToolResultRecord {
                            call_id: c.call_id,
                            result_json: format!(
                                r#"{{"status":"ok","payload":"{}"}}"#,
                                "x".repeat(12_000)
                            ),
                            truncated: false,
                            soft_failure: false,
                            activated_tool_names: Vec::new(),
                            result_metadata: None,
                        })
                        .collect();
                    engine.submit_tool_results(handle, results);
                }
                AgentAction::RequestSummary(req) => {
                    engine.submit_summary(
                        handle,
                        SummaryResult {
                            summary_text: "wrap-up summary".into(),
                            last_message_id: req.last_message_id,
                        },
                    );
                }
                AgentAction::CallModel(_) => {
                    engine.submit_model_response(
                        handle,
                        ModelResponse {
                            assistant_text: Some("done".into()),
                            tool_calls: vec![],
                            finish_reason: FinishReason::Stop,
                            provider_reasoning_content: None,
                        },
                    );
                    final_action = engine.next_action(handle);
                    break;
                }
                other => {
                    final_action = other;
                    break;
                }
            }
            all_events.extend(engine.drain_events(handle));
        }
    }

    let (session_message_count, transcript_revision, semantic_fact_preserved) =
        handle.lalr_long_loop_outcome_snapshot(SEMANTIC_FACT);
    SimulatedLongLoopOutcome {
        model_requests,
        final_action,
        events: all_events,
        session_message_count,
        transcript_revision,
        semantic_fact_preserved,
    }
}

/// Host semantics for the destructive-compaction scenarios: since the
/// global catalog was removed (1195bca; ratified by ENC-296, host owns
/// the tool registry), the micro tier only touches tools the host
/// declares reproducible. A scenario whose subject is a destructive
/// micro pass must therefore declare its tool the way a real host
/// does — without this, the tier has no candidates and only the
/// non-destructive HistorySnapshot tier applies (ENC-309).
fn scenario_tool_semantics() -> crate::tool_semantics::ToolSemanticsConfig {
    let mut semantics = crate::tool_semantics::ToolSemanticsConfig::agnostic();
    semantics.catalog.tools.push(crate::catalog::ToolSpec {
        name: crate::tool_name::ToolName::new("web_search").expect("valid tool name"),
        aliases: Vec::new(),
        reproducible: true,
        schema_hint: None,
    });
    semantics
}

/// Runs one destructive micro compaction pass with Preflight trigger.
pub fn run_destructive_micro_preflight_pass(
    session: &mut AgentSession,
    config: &AgentConfig,
    caps: &SessionCapabilities,
) -> (u64, Vec<AgentEvent>) {
    let mut test_config = config.clone();
    test_config.preflight_token_threshold = Some(1);
    test_config.micro_keep_recent = 0;
    test_config.tool_semantics = scenario_tool_semantics();

    let engine = test_engine();

    let initial = if session.messages.is_empty() {
        vec![tool_message("tool-msg-1", "web_search", &"x".repeat(500))]
    } else {
        for message in &mut session.messages {
            if message.role == AgentRole::Tool && message.metadata.tool_name().is_none() {
                message.metadata.set_tool_name("web_search");
            }
        }
        session.messages.clone()
    };

    let handle = engine
        .create_session(
            initial,
            test_config,
            caps.clone(),
            serde_json::Value::Null,
            default_long_loop_io(),
        )
        .expect("valid config creates session");

    let _action = engine.next_action(&handle);
    let events = engine.drain_events(&handle);

    *session = handle.lalr_get_session();

    (session.transcript_revision, events)
}

/// Runs two destructive compaction passes; returns drained events and final revision.
pub fn run_two_destructive_compaction_passes(session: &mut AgentSession) -> (u64, Vec<AgentEvent>) {
    let config = AgentConfig {
        preflight_token_threshold: Some(1),
        post_tool_result_token_threshold: Some(1),
        micro_keep_recent: 0,
        tool_semantics: scenario_tool_semantics(),
        ..AgentConfig::default()
    };
    let caps = SessionCapabilities::default();

    let engine = test_engine();

    let messages = vec![tool_message("tool-pass-0", "web_search", &"x".repeat(500))];

    let handle = engine
        .create_session(
            messages,
            config,
            caps,
            serde_json::Value::Null,
            default_long_loop_io(),
        )
        .expect("valid config creates session");

    // Turn 1 preflight compaction
    let _action1 = engine.next_action(&handle);
    let mut events = engine.drain_events(&handle);

    // Call Model -> ToolUse
    let tool_call_response = ModelResponse {
        assistant_text: None,
        tool_calls: vec![RawToolCall {
            id: Some("call-pass-1".into()),
            name: "web_search".into(),
            arguments_json: "{}".into(),
        }],
        finish_reason: FinishReason::ToolUse,
        provider_reasoning_content: None,
    };
    engine.submit_model_response(&handle, tool_call_response);

    // ExecuteTools action
    let _action2 = engine.next_action(&handle);
    events.extend(engine.drain_events(&handle));

    // Submit tool result -> triggers PostToolResult compaction pass
    let results = vec![ToolResultRecord {
        call_id: "call-pass-1".into(),
        result_json: "{}".into(),
        truncated: false,
        soft_failure: false,
        activated_tool_names: Vec::new(),
        result_metadata: None,
    }];
    engine.submit_tool_results(&handle, results);
    events.extend(engine.drain_events(&handle));

    *session = handle.lalr_get_session();

    (session.transcript_revision, events)
}

/// Emits three CallModel actions in one user turn; collects provider snapshots.
pub fn collect_three_call_model_snapshots(
    engine: &AgentEngine,
    handle: &SessionHandle,
) -> Vec<ProviderContextSnapshot> {
    handle.lalr_seed_user_message_if_empty("seed");

    let tool_response = ModelResponse {
        assistant_text: None,
        tool_calls: vec![RawToolCall {
            id: Some("call-loop".into()),
            name: "web_search".into(),
            arguments_json: r#"{"query":"sidecar"}"#.into(),
        }],
        finish_reason: FinishReason::ToolUse,
        provider_reasoning_content: None,
    };
    let mut snapshots = Vec::new();
    for round in 0..3 {
        match engine.next_action(handle) {
            AgentAction::CallModel(req) => {
                snapshots.push(req.provider_context.clone());
                engine.submit_model_response(handle, tool_response.clone());
            }
            other => panic!("round {round}: expected CallModel, got {other:?}"),
        }
        match engine.next_action(handle) {
            AgentAction::ExecuteTools(calls) => {
                let results: Vec<ToolResultRecord> = calls
                    .into_iter()
                    .map(|c| ToolResultRecord {
                        call_id: c.call_id,
                        result_json: r#"{"status":"ok"}"#.into(),
                        truncated: false,
                        soft_failure: false,
                        activated_tool_names: Vec::new(),
                        result_metadata: None,
                    })
                    .collect();
                engine.submit_tool_results(handle, results);
            }
            other => panic!("round {round}: expected ExecuteTools, got {other:?}"),
        }
    }
    snapshots
}

/// KV-fixed session destructive preflight without Summary tier.
pub fn run_kv_fixed_destructive_preflight(
    session: &mut AgentSession,
    config: &AgentConfig,
) -> Vec<AgentEvent> {
    let caps = SessionCapabilities {
        kv_fixed: true,
        summarization_allowed: false,
        ..Default::default()
    };
    *session = AgentSession::new(
        vec![tool_message("tool-kv", "web_search", &"k".repeat(600))],
        serde_json::Value::Null,
    );
    run_destructive_micro_preflight_pass(session, config, &caps).1
}

/// HistorySnapshot-only prelude; must not bump revision or emit boundary.
pub fn run_history_snapshot_only_prelude(session: &mut AgentSession) -> (u64, Vec<AgentEvent>) {
    let mut events = Vec::new();
    *session = AgentSession::new(
        vec![
            AgentMessage {
                id: "u0".into(),
                role: AgentRole::User,
                content: "hello".into(),
                metadata: MessageMeta::new(),
            },
            AgentMessage {
                id: "a0".into(),
                role: AgentRole::Assistant,
                content: "world".into(),
                metadata: MessageMeta::new(),
            },
        ],
        serde_json::Value::Null,
    );
    let mut store = ContextStore::default();
    if let Some(reference) = snapshot_history_range(
        &mut store,
        &session.messages,
        CompactionTrigger::Preflight,
        0,
    ) {
        events.push(AgentEvent::CompactionApplied(
            crate::events::CompactionEvent {
                tier: CompactionTier::HistorySnapshot,
                tokens_before: 10,
                tokens_after: 10,
                trigger: CompactionTrigger::Preflight,
            },
        ));
        let _ = reference;
    }
    (transcript_revision_of(session), events)
}

/// Summary tier compaction with objective anchor preservation.
pub fn run_summary_compaction_with_objective(ledger: &mut StateLedger, session: &mut AgentSession) {
    ledger.objective = Some("ship long-loop v1".into());
    *session = AgentSession::new(
        vec![
            AgentMessage {
                id: "u0".into(),
                role: AgentRole::User,
                content: "start".into(),
                metadata: MessageMeta::new(),
            },
            AgentMessage {
                id: "a0".into(),
                role: AgentRole::Assistant,
                content: "working".into(),
                metadata: MessageMeta::new(),
            },
            AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "tail".into(),
                metadata: MessageMeta::new(),
            },
        ],
        serde_json::Value::Null,
    );
    let config = AgentConfig::default();
    let ctx = compaction_ctx(CompactionTrigger::Preflight);
    let tier = SummaryCompactionTier;
    let CompactionOutcome::NeedsHostCall(req, mut working) =
        tier.apply(session.clone(), &config, &ctx)
    else {
        panic!("expected summary NeedsHostCall");
    };
    apply_summary_result(
        &mut working,
        &req,
        &SummaryResult {
            summary_text: "Summary of prior work.".into(),
            last_message_id: req.last_message_id.clone(),
        },
        &config,
        &ctx,
    )
    .expect("summary apply");
    *session = working;
    ledger.objective = Some("ship long-loop v1".into());
}

/// Injects `PROJECT_CODENAME=ENCLAVE` into ledger constraints before a long loop.
pub fn inject_semantic_fact_enclave(ledger: &mut StateLedger) {
    ledger.constraints.push(SEMANTIC_FACT.to_string());
}

/// Stops tool execution when `max_tool_turns` is reached while model still requests tools.
pub fn next_action_at_max_tool_turns(
    engine: &AgentEngine,
    handle: &SessionHandle,
    tool_round_index: u32,
) -> AgentAction {
    handle.lalr_seed_tool_round_index(tool_round_index);
    let config = handle.lalr_config();
    let cap = effective_max_tool_turns(&config);
    assert!(
        should_stop_tool_rounds(&config, tool_round_index, &[]),
        "harness expects tool_round_index {tool_round_index} >= cap {cap}"
    );

    let _ = engine.next_action(handle);
    engine.submit_model_response(
        handle,
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![RawToolCall {
                id: Some("call-1".into()),
                name: "web_search".into(),
                arguments_json: r#"{"query":"cap"}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    engine.next_action(handle)
}

pub fn default_long_loop_io() -> SessionIoParams {
    SessionIoParams::new("anthropic".into(), 200_000, "[]".into(), "sys".into())
        .expect("valid long-loop io params")
}

pub fn config_with_long_loop(long_loop: LongLoopPolicy) -> AgentConfig {
    let _ = long_loop.validate();
    AgentConfig {
        long_loop,
        ..AgentConfig::default()
    }
}

pub fn test_engine() -> AgentEngine {
    AgentEngine::new()
}

pub fn guardrail_kind(err: &AgentError) -> Option<&str> {
    match err {
        AgentError::GuardrailExceeded { kind, detail: _ } => Some(kind.as_str()),
        _ => None,
    }
}

pub fn post_compact_revision(events: &[AgentEvent], trigger: CompactionTrigger) -> Option<u64> {
    events.iter().find_map(|e| match e {
        AgentEvent::PostCompact {
            trigger: t,
            transcript_revision_after,
            ..
        } if *t == trigger => Some(*transcript_revision_after),
        _ => None,
    })
}

pub fn compaction_boundary_events(
    events: &[AgentEvent],
) -> Vec<&crate::transcript_revision::CompactionBoundary> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::CompactionBoundary(b) => Some(b),
            _ => None,
        })
        .collect()
}
