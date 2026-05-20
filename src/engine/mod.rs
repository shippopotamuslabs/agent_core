//! Session state machine (`AgentEngine`) — AGENT-017, AGENT-018 preflight via idle compaction.

pub mod envelope_ingest;
pub mod leaked_tool_recovery;
mod provider_tool_calls;
mod state;
pub use envelope_ingest::{ingest_model_response_envelopes, EnvelopeIngestOutcome};
pub use leaked_tool_recovery::{resolve_model_turn_text, ModelTurnTextResolution};
pub use state::{SessionHandle, SessionIoParams, SessionTestSnapshot};

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::action::AgentAction;
use crate::artifact_finalize::{record_artifact_progress, synthetic_finalize_calls};
use crate::budget::{effective_tool_result_budget, should_compact_at, TokenLedger};
use crate::catalog::ToolCatalog;
use crate::compact::{
    apply_summary_result,
    artifact_snapshot::persist_artifact_snapshot,
    artifact_squash::extract_artifact_anchor,
    compaction_pipeline_for_plan, default_compaction_pipeline,
    history_snapshot::snapshot_history_range,
    policy::build_compaction_plan,
    tool_spill::{spill_tool_messages_in_session, spill_tool_result_if_needed},
    CompactionContext, CompactionOutcome, CompactionPipeline, CompactionTier as _,
};
use crate::context_store::{insert_terminal_output, ContextReference, ContextStore};
use crate::events::{AgentError, AgentEvent, CompactionEvent, CompactionTier, CompactionTrigger};
use crate::file_context::condense_file_context;
use crate::guardrails::{is_soft_failure_result_json, GuardrailState};
use crate::host::{AgentSummarizer, AgentToolExecutor, AgentTransport};
use crate::loop_metrics::{LoopMetrics, PendingWorkItem};
use crate::manual_compact::ManualCompactResult;
use crate::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use crate::policy::resolve_context_policy;
use crate::repair::{RepairOutcome, ToolRepairPipeline};
use crate::request_pipeline::render_runtime_tool_view;
use crate::session::{AgentConfig, AgentSession, SessionCapabilities};
use crate::state_ledger::StateLedger;
use crate::tool_context::build_tool_context_index;
use crate::truncate::truncate_tool_result_json;
use crate::turn_io::{FinishReason, ModelRequest, ModelResponse, SummaryResult};

use state::{EnginePhase, InnerSession};

/// Generic agent loop engine (desktop-ready traits; Android drives via `submit_*`).
pub struct AgentEngine<T: AgentTransport, E: AgentToolExecutor, S: AgentSummarizer> {
    #[allow(dead_code)]
    transport: Arc<T>,
    #[allow(dead_code)]
    executor: Arc<E>,
    #[allow(dead_code)]
    summarizer: Arc<S>,
    #[allow(dead_code)]
    pipeline: Arc<CompactionPipeline>,
}

impl<T: AgentTransport, E: AgentToolExecutor, S: AgentSummarizer> AgentEngine<T, E, S> {
    pub fn new(transport: Arc<T>, executor: Arc<E>, summarizer: Arc<S>) -> Self {
        Self {
            transport,
            executor,
            summarizer,
            pipeline: Arc::new(default_compaction_pipeline()),
        }
    }

    pub fn with_pipeline(
        transport: Arc<T>,
        executor: Arc<E>,
        summarizer: Arc<S>,
        pipeline: Arc<CompactionPipeline>,
    ) -> Self {
        Self {
            transport,
            executor,
            summarizer,
            pipeline,
        }
    }

    pub fn create_session(
        &self,
        initial_messages: Vec<AgentMessage>,
        config: AgentConfig,
        capabilities: SessionCapabilities,
        session_context: serde_json::Value,
        io: SessionIoParams,
    ) -> SessionHandle {
        let mut state_ledger = StateLedger::default();
        if config.enable_state_ledger {
            for message in &initial_messages {
                state_ledger.observe_user_message(message);
                state_ledger.observe_assistant_message(message);
            }
        }

        let initial_msg_count = initial_messages.len() as u32;
        let pending_work_items = session_context
            .get("pending_work")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .map(|text| PendingWorkItem {
                        text: text.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        SessionHandle {
            inner: Arc::new(Mutex::new(InnerSession {
                session: AgentSession::new(initial_messages, session_context),
                config,
                capabilities,
                io,
                guardrails: GuardrailState::default(),
                loop_metrics: LoopMetrics {
                    turn_count: initial_msg_count,
                    pending_work_items,
                    ..LoopMetrics::default()
                },
                phase: EnginePhase::Idle,
                pending_call_model: None,
                pending_execute: None,
                pending_execute_skip_guardrail_count: false,
                pending_auto_finalize_finish: false,
                pending_finalize_after_tool_stop: false,
                pending_summary_req: None,
                pending_summary_ctx: None,
                context_store: ContextStore::default(),
                state_ledger,
                pending_tool_turn_parallel: 0,
                pending_reject_count_this_turn: 0,
                model_requests_this_turn: 0,
                events: Vec::new(),
                turn_index: 0,
                cancelled: false,
                terminal_action: None,
                last_model_response_had_append_blocks: false,
                pending_artifact_ids: HashSet::new(),
            })),
        }
    }

    pub fn next_action(&self, handle: &SessionHandle) -> AgentAction {
        let mut g = handle.lock_inner();
        if let Some(ref a) = g.terminal_action {
            return a.clone();
        }
        if g.cancelled {
            return AgentAction::Failed(AgentError::Internal("cancelled".into()));
        }

        match g.phase {
            EnginePhase::AwaitingModel => {
                return AgentAction::CallModel(
                    g.pending_call_model.clone().expect("awaiting model"),
                );
            }
            EnginePhase::AwaitingTools => {
                let calls = g.pending_execute.clone().expect("awaiting tools");
                return AgentAction::ExecuteTools(calls);
            }
            EnginePhase::AwaitingSummary => {
                let req = g.pending_summary_req.clone().expect("awaiting summary");
                return AgentAction::RequestSummary(req);
            }
            EnginePhase::Idle => {}
        }

        match g.step_idle() {
            Ok(a) => a,
            Err(e) => {
                g.set_terminal(AgentAction::Failed(e));
                g.terminal_action.clone().unwrap()
            }
        }
    }

    pub fn submit_model_response(&self, handle: &SessionHandle, response: ModelResponse) {
        let mut g = handle.lock_inner();
        if g.terminal_action.is_some() || g.cancelled {
            return;
        }
        if !matches!(g.phase, EnginePhase::AwaitingModel) {
            let ph = g.phase;
            g.set_terminal(AgentAction::Failed(AgentError::InvalidTransition {
                from: format!("{:?}", ph),
                to: "submit_model_response".into(),
            }));
            return;
        }

        g.phase = EnginePhase::Idle;
        let allow_tool_call = g
            .pending_call_model
            .as_ref()
            .map(|request| request.allow_tool_call)
            .unwrap_or(true);
        g.pending_call_model = None;

        let (ingest_outcome, merged_tool_calls) = ingest_model_response_envelopes(
            response.assistant_text.as_deref(),
            &response.tool_calls,
            allow_tool_call,
        );
        let (assistant_content, ingest_metadata) = match &ingest_outcome {
            EnvelopeIngestOutcome::NoMarkers => {
                g.guardrails.reset_envelope_parse_failures();
                (
                    response.assistant_text.clone().unwrap_or_default(),
                    serde_json::Value::Null,
                )
            }
            EnvelopeIngestOutcome::Parsed {
                visible_assistant_text,
                ..
            } => {
                g.guardrails.reset_envelope_parse_failures();
                (
                    visible_assistant_text.clone(),
                    json!({ "envelope_ingest": "reified" }),
                )
            }
            EnvelopeIngestOutcome::ParseFailed {
                visible_assistant_text,
                error_json_for_model,
            } => {
                g.guardrails.record_envelope_parse_failure();
                let assistant_id = format!(
                    "a{}",
                    g.session
                        .messages
                        .iter()
                        .filter(|message| message.role == AgentRole::Assistant)
                        .count()
                );
                let assistant_id_clone = assistant_id.clone();
                g.session.messages.push(AgentMessage {
                    id: assistant_id,
                    role: AgentRole::Assistant,
                    content: visible_assistant_text.clone(),
                    metadata: json!({ "envelope_ingest": "failed" }),
                });
                if g.config.enable_state_ledger {
                    if let Some(message) = g.session.messages.last().cloned() {
                        g.state_ledger.observe_assistant_message(&message);
                    }
                }
                g.session.messages.push(AgentMessage {
                    id: format!("tool-err-envelope-{assistant_id_clone}"),
                    role: AgentRole::Tool,
                    content: error_json_for_model.clone(),
                    metadata: json!({ "envelope_parse_failure": true }),
                });
                let cfg = g.config.clone();
                g.guardrails.count_tool_executed(1);
                g.guardrails
                    .on_tool_turn_finished(&cfg, 1, true, false, false, "agent_engine");
                return;
            }
        };

        let assistant_id = format!(
            "a{}",
            g.session
                .messages
                .iter()
                .filter(|message| message.role == AgentRole::Assistant)
                .count()
        );
        let mut metadata = json!({ "raw_tool_calls": [] });
        if !ingest_metadata.is_null() {
            if let Some(obj) = metadata.as_object_mut() {
                if let Some(ingest_obj) = ingest_metadata.as_object() {
                    for (k, v) in ingest_obj {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        g.session.messages.push(AgentMessage {
            id: assistant_id,
            role: AgentRole::Assistant,
            content: assistant_content,
            metadata,
        });
        if g.config.enable_state_ledger {
            if let Some(message) = g.session.messages.last().cloned() {
                g.state_ledger.observe_assistant_message(&message);
            }
        }

        // Emit after_model_response only when there are tool calls (model returned with tools)
        if g.config.enable_loop_hooks
            && g.capabilities.lifecycle_hooks_supported
            && !merged_tool_calls.is_empty()
        {
            g.emit(AgentEvent::HookInvoked {
                checkpoint: "after_model_response".into(),
            });
        }

        g.last_model_response_had_append_blocks = merged_tool_calls.iter().any(|c| {
            ToolCatalog::global()
                .resolve(&normalize_tool_name_hint(&c.name))
                .map(|s| s.name.as_str())
                == Some("append_blocks")
        });

        if merged_tool_calls.is_empty() {
            g.finish_after_model_stop(response.finish_reason);
            return;
        }

        let mut repaired: Vec<ToolCallRecord> = Vec::new();
        let mut rejects = 0usize;
        let parallel = merged_tool_calls.len();

        for raw in &merged_tool_calls {
            let (outcome, evs) = ToolRepairPipeline::repair(raw, ToolCatalog::global());
            for ev in evs {
                g.emit(ev);
            }
            match outcome {
                RepairOutcome::Repaired(mut rec) => {
                    if g.config.default_tool_timeout_ms.is_some() {
                        rec.attempt_metadata = Some(crate::message::ToolAttemptMetadata {
                            attempt_index: g.turn_index + 1,
                            attempts_remaining: Some(
                                g.config
                                    .tool_retry_limit
                                    .saturating_sub(g.turn_index)
                                    .saturating_sub(1),
                            ),
                            timeout_ms: g.config.default_tool_timeout_ms,
                            policy_denied: false,
                            requires_prior_read: g.config.require_read_before_edit,
                        });
                    }
                    repaired.push(rec);
                }
                RepairOutcome::Reject {
                    error_json_for_model,
                } => {
                    let ti = g.turn_index;
                    g.emit(AgentEvent::ToolSoftFailure {
                        tool_name: raw.name.clone(),
                        turn_index: ti,
                    });
                    g.session.messages.push(AgentMessage {
                        id: format!(
                            "tool-err-{}",
                            raw.id.clone().unwrap_or_else(|| "noid".into())
                        ),
                        role: AgentRole::Tool,
                        content: error_json_for_model,
                        metadata: json!({"call_id": raw.id}),
                    });
                    rejects += 1;
                }
                RepairOutcome::Suggest {
                    original,
                    closest_name,
                } => {
                    let ti = g.turn_index;
                    g.emit(AgentEvent::ToolSoftFailure {
                        tool_name: raw.name.clone(),
                        turn_index: ti,
                    });
                    let err = serde_json::json!({
                        "error": format!("Ambiguous tool '{original}' (closest '{closest_name}')"),
                        "available_tools": ToolCatalog::global().all().iter().map(|t| &t.name).collect::<Vec<_>>(),
                    })
                    .to_string();
                    g.session.messages.push(AgentMessage {
                        id: format!(
                            "tool-err-{}",
                            raw.id.clone().unwrap_or_else(|| "noid".into())
                        ),
                        role: AgentRole::Tool,
                        content: err,
                        metadata: json!({"call_id": raw.id}),
                    });
                    rejects += 1;
                }
            }
        }

        if !merged_tool_calls.is_empty() {
            let provider_calls = if repaired.is_empty() {
                Vec::new()
            } else {
                provider_tool_calls::provider_facing_raw_tool_calls(&repaired)
            };
            provider_tool_calls::patch_last_assistant_raw_tool_calls(
                &mut g.session,
                provider_calls,
            );
        }

        if !repaired.is_empty() {
            if g.config.enable_state_ledger {
                for call in &repaired {
                    g.state_ledger.observe_tool_call(call);
                }
            }
            g.pending_tool_turn_parallel = parallel;
            g.pending_reject_count_this_turn = rejects;
            g.pending_execute = Some(repaired);
            g.pending_execute_skip_guardrail_count = false;
            g.pending_auto_finalize_finish = false;
            g.pending_finalize_after_tool_stop =
                matches!(response.finish_reason, FinishReason::Stop);

            match g.run_compaction_pass(CompactionTrigger::MidTurn) {
                Ok(Some(_)) => return,
                Ok(None) => {
                    if g.config.enable_loop_hooks && g.capabilities.lifecycle_hooks_supported {
                        g.emit(AgentEvent::HookInvoked {
                            checkpoint: "before_tool_execution".into(),
                        });
                    }
                    g.phase = EnginePhase::AwaitingTools;
                }
                Err(e) => {
                    g.set_terminal(AgentAction::Failed(e));
                }
            }
            return;
        }

        // Reject-only synthetic turn
        let cfg = g.config.clone();
        g.guardrails.count_tool_executed(rejects as u32);
        g.guardrails.on_tool_turn_finished(
            &cfg,
            parallel,
            parallel > 0,
            false,
            false,
            "agent_engine",
        );
    }

    pub fn submit_tool_results(&self, handle: &SessionHandle, results: Vec<ToolResultRecord>) {
        let mut g = handle.lock_inner();
        if g.terminal_action.is_some() || g.cancelled {
            return;
        }
        if !matches!(g.phase, EnginePhase::AwaitingTools) {
            let ph = g.phase;
            g.set_terminal(AgentAction::Failed(AgentError::InvalidTransition {
                from: format!("{:?}", ph),
                to: "submit_tool_results".into(),
            }));
            return;
        }

        let calls = g.pending_execute.take().expect("tools");
        let skip_count = g.pending_execute_skip_guardrail_count;
        let finalize_after_tool_stop = g.pending_finalize_after_tool_stop;
        let pending_parallel = g.pending_tool_turn_parallel;
        let pending_rejects = g.pending_reject_count_this_turn;
        g.phase = EnginePhase::Idle;
        g.pending_finalize_after_tool_stop = false;

        let mut processed_results: Vec<ToolResultRecord> = Vec::with_capacity(results.len());

        for (call, mut res) in calls.iter().zip(results.into_iter()) {
            let spill_tokens_before = TokenLedger::estimate(
                &g.session,
                &g.config,
                &g.io.provider,
                g.io.context_window_tokens,
                &g.io.tools_schema_json,
            )
            .total_tokens;
            let spilled = if call.name == "append_blocks" {
                let mut stored_copy = res.clone();
                let threshold = g.config.tool_spill_threshold_bytes;
                let turn_index = g.turn_index;
                spill_tool_result_if_needed(
                    &mut g.context_store,
                    &mut stored_copy,
                    call,
                    threshold,
                    turn_index,
                )
            } else {
                let threshold = g.config.tool_spill_threshold_bytes;
                let turn_index = g.turn_index;
                spill_tool_result_if_needed(
                    &mut g.context_store,
                    &mut res,
                    call,
                    threshold,
                    turn_index,
                )
            };

            if call.name != "append_blocks" {
                let pressure = TokenLedger::estimate(
                    &g.session,
                    &g.config,
                    &g.io.provider,
                    g.io.context_window_tokens,
                    &g.io.tools_schema_json,
                );
                let budget = effective_tool_result_budget(&pressure, &g.capabilities, &g.config);
                let (payload, truncated) = truncate_tool_result_json(&res.result_json, budget);
                res.result_json = payload;
                res.truncated = res.truncated || truncated;
            }

            record_artifact_progress(
                &mut g.pending_artifact_ids,
                &call.name,
                &call.arguments_json,
                &res.result_json,
            );

            g.session.messages.push(AgentMessage {
                id: format!("tool-{}", res.call_id),
                role: AgentRole::Tool,
                content: res.result_json.clone(),
                metadata: json!({"call_id": res.call_id, "tool_name": call.name}),
            });
            if g.config.enable_state_ledger {
                g.state_ledger.observe_tool_result(&res);
            }
            if spilled.is_some() && call.name != "append_blocks" {
                let spill_tokens_after = TokenLedger::estimate(
                    &g.session,
                    &g.config,
                    &g.io.provider,
                    g.io.context_window_tokens,
                    &g.io.tools_schema_json,
                )
                .total_tokens;
                g.emit(AgentEvent::CompactionApplied(CompactionEvent {
                    tier: CompactionTier::ToolSpill,
                    tokens_before: spill_tokens_before,
                    tokens_after: spill_tokens_after,
                    trigger: CompactionTrigger::PostToolResult,
                }));
            }
            processed_results.push(res);
        }

        if g.config.enable_loop_hooks && g.capabilities.lifecycle_hooks_supported {
            g.emit(AgentEvent::HookInvoked {
                checkpoint: "after_tool_result".into(),
            });
        }

        if !skip_count {
            let repaired_n = processed_results.len();
            g.guardrails
                .count_tool_executed((pending_rejects + repaired_n) as u32);

            let non_soft_outcome = processed_results
                .iter()
                .any(|res| !res.soft_failure && !is_soft_failure_result_json(&res.result_json));

            let cfg = g.config.clone();
            g.guardrails.on_tool_turn_finished(
                &cfg,
                pending_parallel,
                pending_parallel > 0,
                false,
                non_soft_outcome,
                "agent_engine",
            );
            g.pending_tool_turn_parallel = 0;
            g.pending_reject_count_this_turn = 0;
        }

        if g.pending_auto_finalize_finish {
            g.pending_auto_finalize_finish = false;
            g.terminal_action = Some(AgentAction::Finished);
            let ti = g.turn_index;
            let tc = g.guardrails.tool_calls_executed;
            g.emit(AgentEvent::TurnCompleted {
                turn_index: ti,
                tool_call_count: tc,
                terminal: true,
            });
            return;
        }

        if finalize_after_tool_stop {
            let synth = synthetic_finalize_calls(&g.pending_artifact_ids);
            if !synth.is_empty() {
                g.pending_execute = Some(synth);
                g.pending_execute_skip_guardrail_count = true;
                g.pending_auto_finalize_finish = true;
                g.phase = EnginePhase::AwaitingTools;
            }
        }

        match g.run_compaction_pass(CompactionTrigger::PostToolResult) {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(e) => {
                g.set_terminal(AgentAction::Failed(e));
                return;
            }
        }

        let ti = g.turn_index;
        let tc = g.guardrails.tool_calls_executed;
        g.emit(AgentEvent::TurnCompleted {
            turn_index: ti,
            tool_call_count: tc,
            terminal: false,
        });
    }

    pub fn submit_summary(&self, handle: &SessionHandle, summary: SummaryResult) {
        let mut g = handle.lock_inner();
        if g.terminal_action.is_some() || g.cancelled {
            return;
        }
        if !matches!(g.phase, EnginePhase::AwaitingSummary) {
            let ph = g.phase;
            g.set_terminal(AgentAction::Failed(AgentError::InvalidTransition {
                from: format!("{:?}", ph),
                to: "submit_summary".into(),
            }));
            return;
        }

        let req = g.pending_summary_req.take().expect("summary req");
        let ctx = g.pending_summary_ctx.take().expect("summary ctx");
        g.phase = EnginePhase::Idle;

        let cfg = g.config.clone();
        if let Err(reason) = apply_summary_result(&mut g.session, &req, &summary, &cfg, &ctx) {
            g.set_terminal(AgentAction::Failed(AgentError::CompactionFailed {
                tier: "summary".into(),
                reason: reason.into(),
            }));
            return;
        }

        if g.capabilities.kv_fixed {
            g.emit(AgentEvent::KvCacheInvalidated {
                reason: "summary_apply".into(),
            });
        }

        let summary_ref = format!("compact-summary-{}", summary.last_message_id);
        if g.config.enable_state_ledger {
            g.state_ledger
                .merge_summary(&summary.summary_text, Some(&summary_ref));
        }

        let compact_ev = g
            .session
            .compaction_log
            .back()
            .cloned()
            .expect("summary event");
        g.emit(AgentEvent::CompactionApplied(compact_ev));
        g.emit(AgentEvent::PostCompact {
            trigger: ctx.trigger,
            summary_ref: Some(summary_ref),
            history_ref: req.history_ref.clone(),
        });

        if g.pending_execute.is_some() {
            g.phase = EnginePhase::AwaitingTools;
        }
    }

    pub fn cancel(&self, handle: &SessionHandle) {
        let mut g = handle.lock_inner();
        g.cancelled = true;
        g.set_terminal(AgentAction::Failed(AgentError::Internal(
            "cancelled".into(),
        )));
    }

    pub fn drain_events(&self, handle: &SessionHandle) -> Vec<AgentEvent> {
        let mut g = handle.lock_inner();
        std::mem::take(&mut g.events)
    }

    pub fn request_manual_compact(
        &self,
        handle: &SessionHandle,
        request: crate::manual_compact::ManualCompactRequest,
    ) -> AgentAction {
        let mut g = handle.lock_inner();
        if !g.config.enable_manual_compact {
            let pressure = TokenLedger::estimate(
                &g.session,
                &g.config,
                &g.io.provider,
                g.io.context_window_tokens,
                &g.io.tools_schema_json,
            );
            return AgentAction::ManualCompactCompleted(ManualCompactResult {
                summary_ref: None,
                history_ref: None,
                tokens_before: pressure.total_tokens,
                tokens_after: pressure.total_tokens,
            });
        }

        let trigger = CompactionTrigger::Manual;
        g.emit(AgentEvent::PreCompact { trigger });
        let ctx = g.make_ctx(trigger);
        let before = TokenLedger::estimate(
            &g.session,
            &g.config,
            &g.io.provider,
            g.io.context_window_tokens,
            &g.io.tools_schema_json,
        )
        .total_tokens;

        let outcome = crate::compact::SummaryCompactionTier::default().apply(
            g.session.clone(),
            &g.config,
            &ctx,
        );
        match outcome {
            CompactionOutcome::NeedsHostCall(mut req, sess) => {
                g.session = sess;
                req.focus_instruction = request.focus_instruction;
                if g.config.enable_context_store {
                    let turn_index = g.turn_index;
                    if let Some(reference) = snapshot_history_range(
                        &mut g.context_store,
                        &req.messages_to_summarize,
                        trigger,
                        turn_index,
                    ) {
                        req.history_ref = Some(reference.id.clone());
                        if g.config.enable_state_ledger {
                            g.state_ledger.add_history_ref(&reference.id);
                        }
                    }
                }
                if g.config.enable_state_ledger {
                    req.state_ledger_snapshot = Some(g.state_ledger.clone());
                }
                g.pending_summary_req = Some(req.clone());
                g.pending_summary_ctx = Some(ctx);
                g.phase = EnginePhase::AwaitingSummary;
                AgentAction::RequestSummary(req)
            }
            CompactionOutcome::Applied(sess, ev) => {
                let tokens_after = ev.tokens_after;
                g.session = sess;
                g.emit(AgentEvent::CompactionApplied(ev));
                g.emit_post_compact(trigger, None, None);
                AgentAction::ManualCompactCompleted(ManualCompactResult {
                    summary_ref: None,
                    history_ref: None,
                    tokens_before: before,
                    tokens_after,
                })
            }
            CompactionOutcome::NotApplicable => {
                g.emit_post_compact(trigger, None, None);
                AgentAction::ManualCompactCompleted(ManualCompactResult {
                    summary_ref: None,
                    history_ref: None,
                    tokens_before: before,
                    tokens_after: before,
                })
            }
            CompactionOutcome::Failed(err) => AgentAction::Failed(err),
        }
    }

    pub fn read_context_reference(
        &self,
        handle: &SessionHandle,
        reference_id: String,
    ) -> Result<String, AgentError> {
        let g = handle.lock_inner();
        g.context_store
            .get(&reference_id)
            .map(|entry| entry.content.clone())
            .ok_or(AgentError::ContextReferenceNotFound { reference_id })
    }

    pub fn search_context_references(
        &self,
        handle: &SessionHandle,
        query: String,
        limit: usize,
    ) -> Vec<crate::context_store::ContextReference> {
        let g = handle.lock_inner();
        g.context_store.search(&query, limit)
    }

    pub fn insert_terminal_output(
        &self,
        handle: &SessionHandle,
        id: String,
        title: String,
        content: String,
    ) -> ContextReference {
        let mut g = handle.lock_inner();
        let turn_index = g.turn_index;
        insert_terminal_output(&mut g.context_store, id, title, content, turn_index)
    }
}

fn normalize_tool_name_hint(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    loop {
        if let Some(r) = s.strip_prefix("functions.") {
            s = r.to_string();
        } else if let Some(r) = s.strip_prefix("tools.") {
            s = r.to_string();
        } else {
            break;
        }
    }
    s.to_lowercase().replace(['-', ' '], "_")
}

impl InnerSession {
    fn set_terminal(&mut self, a: AgentAction) {
        self.terminal_action = Some(a);
        self.phase = EnginePhase::Idle;
    }

    fn emit(&mut self, ev: AgentEvent) {
        self.events.push(ev);
    }

    fn emit_post_compact(
        &mut self,
        trigger: CompactionTrigger,
        summary_ref: Option<String>,
        history_ref: Option<String>,
    ) {
        self.emit(AgentEvent::PostCompact {
            trigger,
            summary_ref,
            history_ref,
        });
    }

    fn make_ctx(&self, trigger: CompactionTrigger) -> CompactionContext {
        CompactionContext {
            provider: self.io.provider.clone(),
            context_window_tokens: self.io.context_window_tokens,
            tools_schema_json: self.io.tools_schema_json.clone(),
            capabilities: self.capabilities.clone(),
            trigger,
        }
    }

    fn persist_artifact_state_if_present(&mut self) {
        let Some(anchor) = extract_artifact_anchor(&self.session.messages) else {
            return;
        };
        if self.config.enable_context_store {
            persist_artifact_snapshot(&mut self.context_store, &anchor, self.turn_index);
        }
        if self.config.enable_state_ledger {
            self.state_ledger.observe_artifact_anchor(anchor);
        }
    }

    fn remember_history_snapshot_ids(&mut self, message_ids: Vec<String>) {
        if !self.session.session_context.is_object() {
            self.session.session_context = serde_json::json!({});
        }
        self.session.session_context["history_snapshot_ids"] = serde_json::Value::Array(
            message_ids
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        );
    }

    fn run_compaction_pass(
        &mut self,
        trigger: CompactionTrigger,
    ) -> Result<Option<AgentAction>, AgentError> {
        let mut sync_applied = false;
        let mut last_history_ref: Option<String> = None;
        loop {
            let ctx = self.make_ctx(trigger);
            let policy = resolve_context_policy(
                &self.config,
                &self.capabilities,
                &self.io.provider,
                self.io.context_window_tokens,
            );
            let plan = build_compaction_plan(
                &self.config,
                &self.capabilities,
                &policy,
                trigger,
                self.io.context_window_tokens,
            );
            let threshold_ratio = plan.threshold_ratio;
            let pressure = TokenLedger::estimate(
                &self.session,
                &self.config,
                &self.io.provider,
                self.io.context_window_tokens,
                &self.io.tools_schema_json,
            );
            self.session.ledger_snapshot = Some(pressure.clone());

            if matches!(trigger, CompactionTrigger::MidTurn)
                && !self.config.allow_fifo_on_mid_turn
                && !self.capabilities.summarization_allowed
                && !self.session.compaction_log.is_empty()
            {
                return Err(AgentError::CompactionFailed {
                    tier: "pipeline".into(),
                    reason: "No compaction tier applied while context remains over threshold"
                        .into(),
                });
            }

            let absolute_fires = match trigger {
                CompactionTrigger::Preflight => self
                    .config
                    .preflight_token_threshold
                    .map(|t| pressure.total_tokens >= t)
                    .unwrap_or(false),
                CompactionTrigger::MidTurn => self
                    .config
                    .mid_turn_token_threshold
                    .map(|t| pressure.total_tokens >= t)
                    .unwrap_or(false),
                CompactionTrigger::PostToolResult => self
                    .config
                    .post_tool_result_token_threshold
                    .map(|t| pressure.total_tokens >= t)
                    .unwrap_or(false),
                CompactionTrigger::Manual => false,
            };
            let turn_fires = matches!(trigger, CompactionTrigger::Preflight)
                && self
                    .config
                    .turn_threshold
                    .map(|t| self.loop_metrics.turn_count >= t)
                    .unwrap_or(false);
            let turn_end_fires = trigger == CompactionTrigger::PostToolResult
                && self.config.compact_on_turn_end
                && self
                    .session
                    .messages
                    .last()
                    .map(|m| matches!(m.role, AgentRole::Tool | AgentRole::User))
                    .unwrap_or(false);

            if !should_compact_at(&pressure, threshold_ratio)
                && !absolute_fires
                && !turn_fires
                && !turn_end_fires
            {
                if sync_applied {
                    self.emit_post_compact(trigger, None, last_history_ref);
                }
                return Ok(None);
            }

            self.emit(AgentEvent::PreCompact { trigger });
            self.emit(AgentEvent::ContextPressure {
                ratio: pressure.ratio,
            });

            let has_append_context = self.session.messages.iter().any(|message| {
                message.content.contains("append_blocks")
                    || message.content.contains("\"totalBlocks\"")
                    || message
                        .metadata
                        .get("raw_tool_calls")
                        .and_then(|v| v.as_array())
                        .map(|calls| {
                            calls.iter().any(|call| {
                                call.get("name")
                                    .and_then(|v| v.as_str())
                                    .map(|name| name == "append_blocks")
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false)
            });
            if matches!(trigger, CompactionTrigger::MidTurn)
                && !self.config.allow_fifo_on_mid_turn
                && !self.capabilities.summarization_allowed
                && !has_append_context
            {
                return Err(AgentError::CompactionFailed {
                    tier: "pipeline".into(),
                    reason: "No compaction tier applied while context remains over threshold"
                        .into(),
                });
            }

            if self.config.enable_context_store {
                if let Some(event) = spill_tool_messages_in_session(
                    &mut self.context_store,
                    &mut self.session,
                    self.config.tool_spill_threshold_bytes,
                    self.turn_index,
                    trigger,
                ) {
                    self.session.push_compaction_event(event.clone());
                    self.emit(AgentEvent::CompactionApplied(event));
                    sync_applied = true;
                }
                let snapshot_ids: Vec<String> = self
                    .session
                    .messages
                    .iter()
                    .map(|message| message.id.clone())
                    .collect();
                if let Some(reference) = snapshot_history_range(
                    &mut self.context_store,
                    &self.session.messages,
                    trigger,
                    self.turn_index,
                ) {
                    last_history_ref = Some(reference.id.clone());
                    self.remember_history_snapshot_ids(snapshot_ids);
                    if self.config.enable_state_ledger {
                        self.state_ledger.add_history_ref(&reference.id);
                    }
                    let event = CompactionEvent {
                        tier: CompactionTier::HistorySnapshot,
                        tokens_before: pressure.total_tokens,
                        tokens_after: pressure.total_tokens,
                        trigger,
                    };
                    self.session.push_compaction_event(event.clone());
                    self.emit(AgentEvent::CompactionApplied(event));
                }
            }
            if has_append_context {
                self.persist_artifact_state_if_present();
            }

            // When forced by absolute token threshold, turn count, or turn-end,
            // override:
            //  - plan.threshold_ratio = 0.0 so the pipeline's own ratio guard passes
            //  - ctx.trigger = MidTurn so the summary tier summarizes all messages
            //    (not just "before last user") for short sessions
            let forced_by_non_ratio = !should_compact_at(&pressure, threshold_ratio)
                && (absolute_fires || turn_fires || turn_end_fires);
            let (effective_plan, effective_ctx) = if forced_by_non_ratio {
                let mut p = plan.clone();
                p.threshold_ratio = 0.0;
                let mut c = ctx.clone();
                c.trigger = CompactionTrigger::MidTurn;
                (p, c)
            } else {
                (plan.clone(), ctx.clone())
            };

            let pipeline = compaction_pipeline_for_plan(&effective_plan);
            match pipeline.run_plan(
                self.session.clone(),
                &effective_ctx,
                &self.config,
                &effective_plan,
            ) {
                CompactionOutcome::NeedsHostCall(mut req, sess) => {
                    // If we forced MidTurn context for non-ratio compaction,
                    // restore the original trigger in the SummaryRequest.
                    if forced_by_non_ratio {
                        req.trigger = trigger;
                    }
                    self.session = sess;
                    if self.config.enable_context_store {
                        if let Some(reference) = snapshot_history_range(
                            &mut self.context_store,
                            &req.messages_to_summarize,
                            trigger,
                            self.turn_index,
                        ) {
                            if self.config.enable_state_ledger {
                                self.state_ledger.add_history_ref(&reference.id);
                            }
                            req.history_ref = Some(reference.id);
                        }
                    }
                    if self.config.enable_state_ledger {
                        req.state_ledger_snapshot = Some(self.state_ledger.clone());
                    }
                    self.pending_summary_req = Some(req.clone());
                    self.pending_summary_ctx = Some(ctx);
                    self.phase = EnginePhase::AwaitingSummary;
                    return Ok(Some(AgentAction::RequestSummary(req)));
                }
                CompactionOutcome::Applied(sess, ev) => {
                    self.session = sess;
                    self.emit(AgentEvent::CompactionApplied(ev.clone()));
                    sync_applied = true;
                    if self.capabilities.kv_fixed {
                        self.emit(AgentEvent::KvCacheInvalidated {
                            reason: format!("Compacted: {}", ev.tier),
                        });
                    }
                    // loop until under threshold or NeedsHostCall / stuck
                }
                CompactionOutcome::NotApplicable => {
                    let p2 = TokenLedger::estimate(
                        &self.session,
                        &self.config,
                        &self.io.provider,
                        self.io.context_window_tokens,
                        &self.io.tools_schema_json,
                    );
                    if should_compact_at(&p2, threshold_ratio) {
                        return Err(AgentError::CompactionFailed {
                            tier: "pipeline".into(),
                            reason:
                                "No compaction tier applied while context remains over threshold"
                                    .into(),
                        });
                    }
                    if sync_applied {
                        self.emit_post_compact(trigger, None, last_history_ref);
                    }
                    return Ok(None);
                }
                CompactionOutcome::Failed(err) => {
                    return Err(err);
                }
            }
        }
    }

    fn step_idle(&mut self) -> Result<AgentAction, AgentError> {
        if let Some(out) = self.run_compaction_pass(CompactionTrigger::Preflight)? {
            return Ok(out);
        }

        self.turn_index += 1;
        let allow = self
            .guardrails
            .allow_tool_call(&self.config, self.last_model_response_had_append_blocks);

        self.emit(AgentEvent::TurnStarted {
            turn_index: self.turn_index,
            allow_tool_call: allow,
        });

        let cache_hints = if self.capabilities.prompt_cache_supported {
            let stable_chars = self.io.system_prompt.len()
                + self.io.tools_schema_json.len()
                + self
                    .session
                    .messages
                    .iter()
                    .map(|message| message.content.len())
                    .sum::<usize>();
            if (stable_chars / 4) as u32 >= self.config.prompt_cache_anchor_threshold_tokens {
                vec!["stable-prefix".to_string()]
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        let mut session_messages = self.session.messages.clone();
        if self.config.enable_state_ledger
            && !session_messages
                .iter()
                .any(|message| message.role == AgentRole::CompactSummary)
        {
            if let Some(anchor) = self.state_ledger.as_anchor_message() {
                session_messages.insert(0, anchor);
            }
        }

        // Emit before_model_request only when the last message is NOT a tool result
        // (i.e., this is the start of a new user→model cycle, not continuation after tools)
        let last_role = self.session.messages.last().map(|m| m.role.clone());
        if self.config.enable_loop_hooks
            && self.capabilities.lifecycle_hooks_supported
            && !matches!(last_role, Some(AgentRole::Tool))
        {
            self.emit(AgentEvent::HookInvoked {
                checkpoint: "before_model_request".into(),
            });
        }

        let rendered_tool_catalog_json = render_runtime_tool_view(
            &self.io.tools_schema_json,
            &self.config,
            &self.capabilities,
            &self.loop_metrics,
        )
        .unwrap_or_else(|_| self.io.tools_schema_json.clone());

        let tool_context_index = build_tool_context_index(
            &self.io.tools_schema_json,
            "",
            &mut self.context_store,
            &self.config,
            &self.capabilities,
        );
        let tools_schema_json = if tool_context_index.is_some() {
            "[]".to_string()
        } else {
            self.io.tools_schema_json.clone()
        };

        let file_context_index = self
            .session
            .session_context
            .get("included_files")
            .and_then(|value| serde_json::to_string(value).ok())
            .and_then(|included_files_json| {
                condense_file_context(
                    &included_files_json,
                    &mut self.context_store,
                    &self.config,
                    &self.capabilities,
                )
            });

        let req = ModelRequest {
            session_messages,
            system_prompt: self.io.system_prompt.clone(),
            rendered_tool_catalog_json,
            tools_schema_json,
            allow_tool_call: allow,
            context_references: self
                .context_store
                .entries()
                .map(|entry| entry.reference.clone())
                .collect(),
            state_ledger: self
                .config
                .enable_state_ledger
                .then(|| self.state_ledger.clone()),
            tool_context_index,
            file_context_index,
            cache_hints,
        };

        self.pending_call_model = Some(req.clone());
        self.model_requests_this_turn += 1;
        self.phase = EnginePhase::AwaitingModel;
        Ok(AgentAction::CallModel(req))
    }

    fn finish_after_model_stop(&mut self, reason: FinishReason) {
        match reason {
            FinishReason::Stop => {}
            FinishReason::MaxTokens => {
                self.set_terminal(AgentAction::Failed(AgentError::ModelStopped {
                    reason: "max_tokens".into(),
                }));
                return;
            }
            FinishReason::Other(reason) => {
                self.set_terminal(AgentAction::Failed(AgentError::ModelStopped { reason }));
                return;
            }
            FinishReason::ToolUse => {
                self.set_terminal(AgentAction::Failed(AgentError::ModelStopped {
                    reason: "tool_use_without_tool_calls".into(),
                }));
                return;
            }
        }

        let synth = synthetic_finalize_calls(&self.pending_artifact_ids);
        if !synth.is_empty() {
            self.pending_execute = Some(synth);
            self.pending_execute_skip_guardrail_count = true;
            self.pending_auto_finalize_finish = true;
            self.phase = EnginePhase::AwaitingTools;
            return;
        }

        if self.config.enable_loop_hooks && self.capabilities.lifecycle_hooks_supported {
            self.emit(AgentEvent::HookInvoked {
                checkpoint: "before_finish".into(),
            });
        }

        if self.config.enable_loop_hooks
            && self.capabilities.lifecycle_hooks_supported
            && self.config.enable_pending_work_guard
        {
            if let Some(action) = self.before_finish_pending_work_action() {
                self.set_terminal_or_pending(action);
                return;
            }
        }

        self.terminal_action = Some(AgentAction::Finished);
        let ti = self.turn_index;
        let tc = self.guardrails.tool_calls_executed;
        self.emit(AgentEvent::TurnCompleted {
            turn_index: ti,
            tool_call_count: tc,
            terminal: true,
        });
    }

    fn before_finish_pending_work_action(&mut self) -> Option<AgentAction> {
        let pending_items = self
            .loop_metrics
            .pending_work_items
            .iter()
            .map(|item| item.text.clone())
            .collect::<Vec<_>>();
        let force_blocked = self
            .session
            .session_context
            .get("force_finish_blocked")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if pending_items.is_empty() {
            return force_blocked.then(|| {
                AgentAction::Failed(AgentError::GuardrailExceeded {
                    kind: "finish_blocked_without_injected_message".into(),
                })
            });
        }
        if self.model_requests_this_turn >= self.config.max_model_requests_per_turn {
            return Some(AgentAction::Failed(AgentError::GuardrailExceeded {
                kind: "max_model_requests_per_turn".into(),
            }));
        }

        let injected_message = AgentMessage {
            id: format!("pending-work-reminder-{}", self.turn_index),
            role: AgentRole::User,
            content: format!("Pending work remains: {}", pending_items.join(", ")),
            metadata: serde_json::Value::Null,
        };
        self.session.messages.push(injected_message.clone());
        if self.config.enable_state_ledger {
            self.state_ledger.observe_user_message(&injected_message);
        }
        let request = ModelRequest {
            session_messages: self.session.messages.clone(),
            system_prompt: self.io.system_prompt.clone(),
            rendered_tool_catalog_json: render_runtime_tool_view(
                &self.io.tools_schema_json,
                &self.config,
                &self.capabilities,
                &self.loop_metrics,
            )
            .unwrap_or_else(|_| self.io.tools_schema_json.clone()),
            tools_schema_json: self.io.tools_schema_json.clone(),
            allow_tool_call: self
                .guardrails
                .allow_tool_call(&self.config, self.last_model_response_had_append_blocks),
            context_references: self
                .context_store
                .entries()
                .map(|entry| entry.reference.clone())
                .collect(),
            state_ledger: self
                .config
                .enable_state_ledger
                .then(|| self.state_ledger.clone()),
            tool_context_index: None,
            file_context_index: None,
            cache_hints: vec![],
        };
        self.pending_call_model = Some(request.clone());
        self.model_requests_this_turn += 1;
        self.phase = EnginePhase::AwaitingModel;
        Some(AgentAction::CallModel(request))
    }

    fn set_terminal_or_pending(&mut self, action: AgentAction) {
        match action {
            AgentAction::CallModel(_) => {}
            other => self.set_terminal(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::NoOpSummarizer;
    use crate::turn_io::RawToolCall;

    struct PanicTransport;
    impl AgentTransport for PanicTransport {
        fn complete_model_turn(&self, _: ModelRequest) -> Result<ModelResponse, AgentError> {
            panic!("transport not used in submit-path tests");
        }
    }

    struct RecordingExecutor {
        pub results: Mutex<Vec<ToolCallRecord>>,
    }

    impl AgentToolExecutor for RecordingExecutor {
        fn execute_tools(
            &self,
            calls: Vec<ToolCallRecord>,
        ) -> Result<Vec<ToolResultRecord>, AgentError> {
            self.results.lock().unwrap().extend(calls.clone());
            Ok(calls
                .into_iter()
                .map(|c| ToolResultRecord {
                    call_id: c.call_id,
                    result_json: r#"{"status":"success","stub":true}"#.into(),
                    truncated: false,
                    soft_failure: false,
                    result_metadata: None,
                })
                .collect())
        }
    }

    fn io_small_window() -> SessionIoParams {
        SessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 256,
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        }
    }

    fn io_large_window() -> SessionIoParams {
        SessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        }
    }

    #[test]
    fn three_tool_happy_path() {
        let eng = AgentEngine::new(
            Arc::new(PanicTransport),
            Arc::new(RecordingExecutor {
                results: Mutex::new(Vec::new()),
            }),
            Arc::new(NoOpSummarizer),
        );
        let h = eng.create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "hi".into(),
                metadata: serde_json::Value::Null,
            }],
            AgentConfig::default(),
            SessionCapabilities {
                summarization_allowed: false,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            serde_json::Value::Null,
            io_large_window(),
        );

        assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
        eng.submit_model_response(
            &h,
            ModelResponse {
                assistant_text: None,
                tool_calls: vec![
                    RawToolCall {
                        id: Some("c1".into()),
                        name: "web_search".into(),
                        arguments_json: r#"{"query":"a"}"#.into(),
                    },
                    RawToolCall {
                        id: Some("c2".into()),
                        name: "web_search".into(),
                        arguments_json: r#"{"query":"b"}"#.into(),
                    },
                    RawToolCall {
                        id: Some("c3".into()),
                        name: "web_search".into(),
                        arguments_json: r#"{"query":"c"}"#.into(),
                    },
                ],
                finish_reason: FinishReason::ToolUse,
            },
        );
        match eng.next_action(&h) {
            AgentAction::ExecuteTools(v) => assert_eq!(v.len(), 3),
            o => panic!("{o:?}"),
        }
        eng.submit_tool_results(
            &h,
            (1..=3)
                .map(|i| ToolResultRecord {
                    call_id: format!("c{i}"),
                    result_json: r#"{"results":[]}"#.into(),
                    truncated: false,
                    soft_failure: false,
                    result_metadata: None,
                })
                .collect(),
        );
        assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
    }

    #[test]
    fn fuzzy_repair_web_seach() {
        let eng = AgentEngine::new(
            Arc::new(PanicTransport),
            Arc::new(RecordingExecutor {
                results: Mutex::new(Vec::new()),
            }),
            Arc::new(NoOpSummarizer),
        );
        let h = eng.create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "hi".into(),
                metadata: serde_json::Value::Null,
            }],
            AgentConfig::default(),
            SessionCapabilities {
                summarization_allowed: false,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            serde_json::Value::Null,
            io_large_window(),
        );
        let _ = eng.next_action(&h);
        eng.submit_model_response(
            &h,
            ModelResponse {
                assistant_text: None,
                tool_calls: vec![RawToolCall {
                    id: Some("c1".into()),
                    name: "web_seach".into(),
                    arguments_json: r#"{"query":"x"}"#.into(),
                }],
                finish_reason: FinishReason::ToolUse,
            },
        );
        match eng.next_action(&h) {
            AgentAction::ExecuteTools(v) => {
                assert_eq!(v[0].name, "web_search");
                assert!(v[0].repair_metadata.is_some());
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn max_turns_disallows_tools_but_still_calls_model() {
        let mut cfg = AgentConfig::default();
        cfg.max_turns = 1;
        let eng = AgentEngine::new(
            Arc::new(PanicTransport),
            Arc::new(RecordingExecutor {
                results: Mutex::new(Vec::new()),
            }),
            Arc::new(NoOpSummarizer),
        );
        let h = eng.create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "hi".into(),
                metadata: serde_json::Value::Null,
            }],
            cfg,
            SessionCapabilities {
                summarization_allowed: false,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            serde_json::Value::Null,
            io_large_window(),
        );
        let AgentAction::CallModel(r1) = eng.next_action(&h) else {
            panic!("expected model");
        };
        assert!(r1.allow_tool_call);
        eng.submit_model_response(
            &h,
            ModelResponse {
                assistant_text: None,
                tool_calls: vec![RawToolCall {
                    id: Some("c1".into()),
                    name: "web_search".into(),
                    arguments_json: "{}".into(),
                }],
                finish_reason: FinishReason::ToolUse,
            },
        );
        let _ = eng.next_action(&h);
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
        let AgentAction::CallModel(r2) = eng.next_action(&h) else {
            panic!("expected model");
        };
        assert!(!r2.allow_tool_call);
    }

    #[test]
    fn cancel_returns_failed() {
        let eng = AgentEngine::new(
            Arc::new(PanicTransport),
            Arc::new(RecordingExecutor {
                results: Mutex::new(Vec::new()),
            }),
            Arc::new(NoOpSummarizer),
        );
        let h = eng.create_session(
            vec![],
            AgentConfig::default(),
            SessionCapabilities {
                summarization_allowed: false,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            serde_json::Value::Null,
            io_large_window(),
        );
        eng.cancel(&h);
        assert!(matches!(
            eng.next_action(&h),
            AgentAction::Failed(AgentError::Internal(_))
        ));
    }

    #[test]
    fn preflight_compaction_on_oversized_history() {
        let big_pair = "hello ".repeat(400);
        let eng = AgentEngine::new(
            Arc::new(PanicTransport),
            Arc::new(RecordingExecutor {
                results: Mutex::new(Vec::new()),
            }),
            Arc::new(NoOpSummarizer),
        );
        let h = eng.create_session(
            vec![
                AgentMessage {
                    id: "u0".into(),
                    role: AgentRole::User,
                    content: big_pair.clone(),
                    metadata: serde_json::Value::Null,
                },
                AgentMessage {
                    id: "a0".into(),
                    role: AgentRole::Assistant,
                    content: big_pair,
                    metadata: serde_json::Value::Null,
                },
                AgentMessage {
                    id: "u1".into(),
                    role: AgentRole::User,
                    content: "tail".into(),
                    metadata: serde_json::Value::Null,
                },
            ],
            AgentConfig::default(),
            SessionCapabilities {
                summarization_allowed: false,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            serde_json::Value::Null,
            io_small_window(),
        );
        let _ = eng.next_action(&h);
        assert!(!h.lock_inner().session.compaction_log.is_empty());
    }

    #[test]
    fn kv_fixed_emits_invalidated_on_compact() {
        let big_pair = "hello ".repeat(400);
        let eng = AgentEngine::new(
            Arc::new(PanicTransport),
            Arc::new(RecordingExecutor {
                results: Mutex::new(Vec::new()),
            }),
            Arc::new(NoOpSummarizer),
        );
        let h = eng.create_session(
            vec![
                AgentMessage {
                    id: "u0".into(),
                    role: AgentRole::User,
                    content: big_pair.clone(),
                    metadata: serde_json::Value::Null,
                },
                AgentMessage {
                    id: "a0".into(),
                    role: AgentRole::Assistant,
                    content: big_pair,
                    metadata: serde_json::Value::Null,
                },
                AgentMessage {
                    id: "u1".into(),
                    role: AgentRole::User,
                    content: "tail".into(),
                    metadata: serde_json::Value::Null,
                },
            ],
            AgentConfig::default(),
            SessionCapabilities {
                summarization_allowed: false,
                kv_fixed: true,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            serde_json::Value::Null,
            io_small_window(),
        );
        let _ = eng.next_action(&h);
        let evs = eng.drain_events(&h);
        assert!(evs
            .iter()
            .any(|e| matches!(e, AgentEvent::KvCacheInvalidated { .. })));
    }

    #[test]
    fn artifact_auto_finalize_then_finished() {
        let eng = AgentEngine::new(
            Arc::new(PanicTransport),
            Arc::new(RecordingExecutor {
                results: Mutex::new(Vec::new()),
            }),
            Arc::new(NoOpSummarizer),
        );
        let h = eng.create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "hi".into(),
                metadata: serde_json::Value::Null,
            }],
            AgentConfig::default(),
            SessionCapabilities {
                summarization_allowed: false,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            serde_json::Value::Null,
            io_large_window(),
        );

        let turns_before = eng.next_action(&h);
        let AgentAction::CallModel(_) = turns_before else {
            panic!("model");
        };

        eng.submit_model_response(
            &h,
            ModelResponse {
                assistant_text: None,
                tool_calls: vec![RawToolCall {
                    id: Some("c1".into()),
                    name: "start_artifact".into(),
                    arguments_json: r#"{"title":"T","kind":"document"}"#.into(),
                }],
                finish_reason: FinishReason::ToolUse,
            },
        );
        let _ = eng.next_action(&h);
        eng.submit_tool_results(
            &h,
            vec![ToolResultRecord {
                call_id: "c1".into(),
                result_json: r#"{"status":"success","artifactId":"art-1"}"#.into(),
                truncated: false,
                soft_failure: false,
                result_metadata: None,
            }],
        );

        let _ = eng.next_action(&h);

        let executed_before = h.lock_inner().guardrails.tool_calls_executed;

        eng.submit_model_response(&h, ModelResponse::text_only("done"));

        match eng.next_action(&h) {
            AgentAction::ExecuteTools(calls) => {
                assert!(calls
                    .iter()
                    .all(|c| c.call_id.starts_with("auto-finalize-")));
            }
            o => panic!("expected finalize tools {o:?}"),
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

        assert!(matches!(eng.next_action(&h), AgentAction::Finished));
        assert_eq!(
            h.lock_inner().guardrails.tool_calls_executed,
            executed_before
        );
    }

    #[test]
    fn artifact_stop_tool_response_auto_finalizes_after_tool_result() {
        let eng = AgentEngine::new(
            Arc::new(PanicTransport),
            Arc::new(RecordingExecutor {
                results: Mutex::new(Vec::new()),
            }),
            Arc::new(NoOpSummarizer),
        );
        let h = eng.create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "hi".into(),
                metadata: serde_json::Value::Null,
            }],
            AgentConfig::default(),
            SessionCapabilities {
                summarization_allowed: false,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            serde_json::Value::Null,
            io_large_window(),
        );

        assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
        eng.submit_model_response(
            &h,
            ModelResponse {
                assistant_text: None,
                tool_calls: vec![RawToolCall {
                    id: Some("c1".into()),
                    name: "start_artifact".into(),
                    arguments_json: r#"{"title":"T","kind":"document"}"#.into(),
                }],
                finish_reason: FinishReason::ToolUse,
            },
        );
        assert!(matches!(eng.next_action(&h), AgentAction::ExecuteTools(_)));
        eng.submit_tool_results(
            &h,
            vec![ToolResultRecord {
                call_id: "c1".into(),
                result_json: r#"{"status":"success","artifactId":"art-1"}"#.into(),
                truncated: false,
                soft_failure: false,
                result_metadata: None,
            }],
        );

        assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
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
            },
        );
        assert!(matches!(eng.next_action(&h), AgentAction::ExecuteTools(_)));
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
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].call_id, "auto-finalize-art-1");
                assert!(calls[0].arguments_json.contains(r#""isComplete":true"#));
            }
            o => panic!("expected finalize tools {o:?}"),
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

        assert!(matches!(eng.next_action(&h), AgentAction::Finished));
    }

    #[test]
    fn drain_events_clears_buffer() {
        let eng = AgentEngine::new(
            Arc::new(PanicTransport),
            Arc::new(RecordingExecutor {
                results: Mutex::new(Vec::new()),
            }),
            Arc::new(NoOpSummarizer),
        );
        let h = eng.create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "hi".into(),
                metadata: serde_json::Value::Null,
            }],
            AgentConfig::default(),
            SessionCapabilities {
                summarization_allowed: false,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            serde_json::Value::Null,
            io_large_window(),
        );
        let _ = eng.next_action(&h);
        assert!(!eng.drain_events(&h).is_empty());
        assert!(eng.drain_events(&h).is_empty());
    }
}
