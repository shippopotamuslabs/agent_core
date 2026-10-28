//! Session state machine (`AgentEngine`) — AGENT-017, AGENT-018 preflight via idle compaction.

pub(super) mod compaction;
mod context_refs;
pub mod envelope_ingest;
pub(super) mod finish;
pub mod leaked_tool_recovery;
pub(super) mod model_intake;
pub(super) mod model_request;
mod provider_tool_calls;
mod state;
pub(super) mod tool_results;
pub use envelope_ingest::{ingest_model_response_envelopes, EnvelopeIngestOutcome};
pub use leaked_tool_recovery::{resolve_model_turn_text, ModelTurnTextResolution};
pub use state::{SessionHandle, SessionIoParams, SessionTestSnapshot};

use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::action::AgentAction;
use crate::agent_tool_budget::inject_tool_budget_into_session;
use crate::artifact_finalize::synthetic_finalize_calls_with_semantics;
use crate::budget::{
    effective_tool_result_budget_for_window, effective_tool_spill_threshold_bytes,
    exceeds_hard_prompt_ceiling,
};
use crate::catalog::ToolCatalog;
use crate::compact::{
    apply_summary_result,
    artifact_snapshot::persist_artifact_snapshot,
    artifact_squash::extract_artifact_anchor,
    compaction_pipeline_for_plan, estimate_session_pressure,
    policy::{
        apply_confirmation_gate, build_compaction_plan, mid_turn_stall_error,
        ratio_threshold_tokens_for_plan, should_enter_compaction, FireConditionInputs,
        MidTurnStallGate,
    },
    tool_spill::activation_tools_from_retrieval_hint,
    CompactionContext, CompactionOutcome, CompactionTier as _,
};
use crate::context_store::{ContextReference, ContextStore};
use crate::events::{AgentError, AgentEvent, CompactionTier, CompactionTrigger};
use crate::guardrails::GuardrailState;
use crate::loop_metrics::{LoopMetrics, PendingWorkItem};
use crate::manual_compact::ManualCompactResult;
use crate::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use crate::policy::resolve_context_policy;

use crate::session::{AgentConfig, AgentSession, SessionCapabilities};
use crate::sidecar_builder::SidecarBuilder;
use crate::state_ledger::StateLedger;
use crate::tool_name::ToolName;
use crate::tool_semantics::{result_policy_for, ArtifactSquashSemantics, ToolSemanticsError};
use crate::transcript_revision::record_destructive_compaction_boundary;
use crate::turn_io::{FinishReason, ModelResponse, RawToolCall, SummaryRequest, SummaryResult};

use state::{EnginePhase, InnerSession, PendingToolTurn};

pub(super) fn is_destructive_compaction_tier(tier: &CompactionTier) -> bool {
    matches!(
        tier,
        CompactionTier::Micro
            | CompactionTier::Fifo
            | CompactionTier::Summary
            | CompactionTier::TextTruncate
    )
}

/// Merge canonical tool identities into the active set in first-seen order,
/// deduped (m3: the generic String/ToolName bridge is deleted now that every
/// side is `ToolName`).
pub(super) fn merge_active_tool_names(
    active_tool_names: &mut Vec<ToolName>,
    names: impl IntoIterator<Item = ToolName>,
) {
    for name in names {
        if !active_tool_names.iter().any(|existing| existing == &name) {
            active_tool_names.push(name);
        }
    }
}

/// Single decider for the discovery gate (ENC-303): whether `tool_name`
/// carries a gate exemption, and if so whether it activates (merges into
/// the active set). `None` means no exemption — plain active-set gating.
pub(super) fn gate_exemption_for(
    discovery: &Option<crate::tool_semantics::ToolDiscoverySemantics>,
    tool_name: &ToolName,
) -> Option<bool> {
    discovery
        .as_ref()
        .and_then(|d| d.gate_exemption_for(tool_name))
        .map(|exemption| exemption.activates)
}

pub(super) fn activate_tools_for_spill_reference(
    active_tool_names: &mut Vec<ToolName>,
    result: &mut ToolResultRecord,
    reference: &ContextReference,
) {
    // Host-declared boundary names (retrieval-hint text): ToolName::new,
    // no snake-splitting. Both merges now take ToolName directly (m3).
    let hinted: Vec<ToolName> = activation_tools_from_retrieval_hint(&reference.retrieval_hint)
        .iter()
        .filter_map(|name| ToolName::new(name).ok())
        .collect();
    merge_active_tool_names(active_tool_names, hinted.clone());
    merge_active_tool_names(&mut result.activated_tool_names, hinted);
}

/// Session state machine: hosts drive it exclusively through the polling surface
/// (`next_action` / `submit_model_response` / `submit_tool_results` / `submit_summary`).
///
/// ENC-285 decision (documented once, here): the `AgentEngine<T, E, S>` generic port
/// injection (`AgentTransport` / `AgentToolExecutor` / `AgentSummarizer` Arcs) was
/// decorative DI — the fields were `#[allow(dead_code)]` and never invoked; the only
/// in-repo callers of `complete_model_turn` / `execute_tools` / `summarize` were the
/// UniFFI callback shims themselves. Hosts (Kotlin via UniFFI, native Rust) already
/// drove the polling contract, so construction no longer takes undriven trait objects.
/// The `host.rs` port traits, the `ffi::callbacks` shims, and the UniFFI callback
/// interfaces were removed with the generics: no concrete future direct-driver is
/// ticketed, and retaining them would preserve the same misleading execution model.
/// The never-read stored `CompactionPipeline` (and its zero-caller `with_pipeline`
/// constructor) went with the same change; per-turn compaction builds its pipeline
/// from the plan via `compaction_pipeline_for_plan`.
#[derive(Default)]
pub struct AgentEngine {
    _private: (),
}

impl AgentEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fallible (ENC-297 J1): re-syncs the policy catalog from the
    /// construction-time semantics and fails on invalid struct-literal /
    /// serde configs (duplicate tool names, alias collisions) instead of
    /// running the session with a stale or empty catalog. ENC-299 D4 also
    /// fails session creation when a semantics section references a tool
    /// that is neither a catalog tool nor an active schema
    /// ([`ToolSemanticsError::UnknownToolReference`]). Returns
    /// [`ToolSemanticsError`] so every caller decides how to surface it.
    pub fn create_session(
        &self,
        initial_messages: Vec<AgentMessage>,
        mut config: AgentConfig,
        capabilities: SessionCapabilities,
        session_context: impl Into<crate::session::SessionContext>,
        io: SessionIoParams,
    ) -> Result<SessionHandle, ToolSemanticsError> {
        config
            .apply_policy_tier_defaults(capabilities.context_policy_tier, io.context_window_tokens);
        // ENC-297 J1: propagate — no `let _` discard. An invalid literal
        // config fails session creation; no live session exists on error.
        config.prepare_tool_semantics()?;
        // ENC-299 D4: referential validation runs here, where the policy
        // catalog and the parsed schema set meet for the first time. Known
        // names are the catalog (canonical + aliases) union the active
        // schemas; sections that are absent/empty are skipped, so the
        // agnostic config passes.
        // Single session-construction parse: names come from the parsed set
        // (m4: one binding shared by referential validation and the active
        // set below).
        let initial_active_tool_names = io.schemas.initial_active_names();
        config
            .tool_semantics
            .validate_references(&config.tool_catalog, &initial_active_tool_names)?;
        let mut state_ledger = StateLedger::default();
        if config.enable_state_ledger {
            for message in &initial_messages {
                state_ledger.observe_user_message(message);
                state_ledger.observe_assistant_message(message);
            }
        }

        let initial_msg_count = initial_messages.len() as u32;
        let session_context = session_context.into();
        let pending_work_items: Vec<PendingWorkItem> = session_context
            .pending_work
            .clone()
            .into_iter()
            .map(|text| PendingWorkItem { text })
            .collect();
        Ok(SessionHandle {
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
                pending: PendingToolTurn {
                    call_model: None,
                    execute: None,
                    execute_skip_guardrail_count: false,
                    auto_finalize_finish: false,
                    finalize_after_tool_stop: false,
                    summary_req: None,
                    summary_ctx: None,
                    ..PendingToolTurn::default()
                },
                context_store: ContextStore::default(),
                state_ledger,
                model_requests_this_turn: 0,
                events: Vec::new(),
                turn_index: 0,
                cancelled: false,
                terminal_action: None,
                last_model_response_greedy_tool_names: Vec::new(),
                tools_blocked_nudges_this_turn: 0,
                active_tool_names: initial_active_tool_names,
                sidecar_builder: SidecarBuilder::new(),
                compaction: compaction::CompactionState::new(),
            })),
        })
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
                    g.pending.call_model.clone().expect("awaiting model"),
                );
            }
            EnginePhase::AwaitingTools => {
                let calls = g.pending.execute.clone().expect("awaiting tools");
                return AgentAction::ExecuteTools(calls);
            }
            EnginePhase::AwaitingSummary => {
                let req = g.pending.summary_req.clone().expect("awaiting summary");
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
        g.guardrails.reset_model_api_failures();
        let allow_tool_call = g
            .pending
            .call_model
            .as_ref()
            .map(|request| request.allow_tool_call)
            .unwrap_or(true);
        g.pending.call_model = None;

        // ENC-286 slice 5: the catalog is built once here and passed into the
        // repair stage (previously cloned + rebuilt per tool call).
        // ENC-297: the catalog is built once at config construction
        // (AgentConfig::with_tool_semantics, re-synced by
        // prepare_tool_semantics at session creation), so the per-turn path
        // borrows the stored value instead of rebuilding + silently
        // falling back to empty on invalid semantics.
        let semantics = g.config.tool_semantics.clone();
        let catalog = g.config.tool_catalog.clone();
        let (ingest_outcome, merged_tool_calls) = ingest_model_response_envelopes(
            g.config.tool_call_format,
            response.assistant_text.as_deref(),
            &response.tool_calls,
            allow_tool_call,
            &catalog,
            &semantics,
        );
        // FRG-47 staging: each ingest arm is a stage fn; this match only
        // dispatches on the returned decision and owns phase/emit behavior.
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
                Self::intake_parse_failed(
                    &mut g,
                    visible_assistant_text.clone(),
                    error_json_for_model.clone(),
                );
                return;
            }
            EnvelopeIngestOutcome::ToolsBlocked {
                nudge_json_for_model,
                visible_assistant_text,
                user_fallback_text,
            } => {
                match Self::intake_tools_blocked(
                    &mut g,
                    visible_assistant_text.clone(),
                    user_fallback_text.clone(),
                    nudge_json_for_model.clone(),
                ) {
                    model_intake::ToolsBlockedEffect::Nudged => return,
                    model_intake::ToolsBlockedEffect::FallbackFinished => {
                        // The fallback text is the final user-facing answer for this turn.
                        // The provider finish reason may be ToolUse/Other for the blocked
                        // attempt; finishing with it would fail the loop after a graceful
                        // fallback, so force Stop semantics.
                        finish::finish_after_model_stop(g.finish_state(), FinishReason::Stop);
                        return;
                    }
                }
            }
        };

        Self::push_assistant_message(
            &mut g,
            &merged_tool_calls,
            assistant_content,
            ingest_metadata,
            response.provider_reasoning_content.as_deref(),
        );

        if merged_tool_calls.is_empty() {
            finish::finish_after_model_stop(g.finish_state(), response.finish_reason);
            return;
        }

        g.pending.tool_turn_request_order = merged_tool_calls.clone();
        g.pending.reject_tool_by_call_id.clear();

        // FRG-47 stage: the repair/discovery loop returns a decision payload;
        // the dispatch owns emit ordering and every phase write below.
        let outcome = Self::intake_repair(&mut g, &merged_tool_calls, &catalog);
        for ev in outcome.events {
            g.emit(ev);
        }
        g.pending.reject_tool_by_call_id = outcome.reject_by_call_id;
        let repaired = outcome.repaired;
        let parallel = outcome.parallel;
        let rejects = outcome.rejects;

        if !merged_tool_calls.is_empty() {
            provider_tool_calls::patch_last_assistant_raw_tool_calls(
                &mut g.session,
                provider_tool_calls::provider_facing_raw_tool_calls_for_turn(
                    &merged_tool_calls,
                    &repaired,
                ),
            );
        }

        if !repaired.is_empty() {
            if g.config.long_loop.max_tool_turns < g.config.max_turns
                && crate::long_loop_policy::should_stop_tool_rounds(
                    &g.config,
                    g.guardrails.tool_calls_executed,
                    &g.last_model_response_greedy_tool_names,
                )
            {
                g.set_terminal(AgentAction::Failed(AgentError::GuardrailExceeded {
                    kind: "max_tool_turns".into(),
                    detail: None,
                }));
                return;
            }
            if g.config.enable_state_ledger {
                for call in &repaired {
                    g.state_ledger.observe_tool_call(call);
                }
            }
            g.pending.tool_turn_parallel = parallel;
            g.pending.reject_count_this_turn = rejects;
            g.pending.execute = Some(repaired);
            g.pending.execute_skip_guardrail_count = false;
            g.pending.auto_finalize_finish = false;
            g.pending.finalize_after_tool_stop =
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

        // Reject-only synthetic turn: the stage applies counts, flush, and
        // budget; the phase is already Idle so the dispatch just returns.
        Self::intake_flush_rejects(&mut g, parallel, rejects);
    }

    pub fn submit_model_turn_failure(
        &self,
        handle: &SessionHandle,
        report: crate::model_api_retry::ModelTurnFailureReport,
    ) {
        let mut g = handle.lock_inner();
        if g.terminal_action.is_some() || g.cancelled {
            return;
        }
        if !matches!(g.phase, EnginePhase::AwaitingModel) {
            let ph = g.phase;
            g.set_terminal(AgentAction::Failed(AgentError::InvalidTransition {
                from: format!("{:?}", ph),
                to: "submit_model_turn_failure".into(),
            }));
            return;
        }

        if report.error_class == crate::model_api_retry::ModelApiErrorClass::Permanent {
            let attempts = g
                .guardrails
                .consecutive_model_api_failures
                .saturating_add(1);
            g.set_terminal(AgentAction::Failed(AgentError::ModelApiUnavailable {
                message: report.message.clone(),
                attempts,
            }));
            return;
        }

        g.guardrails.record_model_api_failure();
        let attempt = g.guardrails.consecutive_model_api_failures;
        if attempt > g.config.model_api_retry_limit {
            g.set_terminal(AgentAction::Failed(AgentError::GuardrailExceeded {
                kind: "model_api_retries_exhausted".into(),
                detail: Some(truncate_model_api_failure_reason(&report.message)),
            }));
            return;
        }

        let max_attempts = g.config.model_api_retry_limit;
        let delay_ms = crate::model_api_retry::model_api_retry_delay_ms(
            attempt,
            g.config.model_api_retry_base_delay_ms,
            report.retry_after_ms,
        );
        let reason = if report.had_partial_output {
            format!(
                "{} (partial_output_chars={})",
                truncate_model_api_failure_reason(&report.message),
                report.partial_output_chars,
            )
        } else {
            truncate_model_api_failure_reason(&report.message)
        };
        g.emit(AgentEvent::ModelApiRetryScheduled {
            attempt,
            max_attempts,
            delay_ms,
            reason,
        });

        if let Some(request) = g.pending.call_model.as_mut() {
            request.retry_after_ms = Some(delay_ms);
            request.model_retry_attempt = Some(attempt);
        }
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

        let calls = g.pending.execute.take().expect("tools");
        let skip_count = g.pending.execute_skip_guardrail_count;
        let finalize_after_tool_stop = g.pending.finalize_after_tool_stop;
        let pending_parallel = g.pending.tool_turn_parallel;
        let pending_rejects = g.pending.reject_count_this_turn;
        g.phase = EnginePhase::Idle;
        g.pending.finalize_after_tool_stop = false;

        let turn_order = std::mem::take(&mut g.pending.tool_turn_request_order);
        let mut reject_by_call_id = std::mem::take(&mut g.pending.reject_tool_by_call_id);
        // Merge any host-reported activations into the active set in first-seen order, deduped.
        // Both sides are ToolName now: no re-folding (m3).
        for res in &results {
            merge_active_tool_names(
                &mut g.active_tool_names,
                res.activated_tool_names.iter().cloned(),
            );
        }

        // FRG-47 stage 1 (pure): pair host results to calls.
        let mut executed_by_call_id = tool_results::pair_results_to_calls(&calls, results);

        let mut processed_results: Vec<ToolResultRecord> =
            Vec::with_capacity(executed_by_call_id.len());
        let mut last_batch_tool_message_index: Option<usize> = None;

        let flush_order: Vec<RawToolCall> = if turn_order.is_empty() {
            calls
                .iter()
                .map(|call| RawToolCall {
                    id: Some(call.call_id.clone()),
                    name: call.name.clone(),
                    arguments_json: call.arguments_json.clone(),
                })
                .collect()
        } else {
            turn_order
        };

        for raw in &flush_order {
            let call_id = raw.id.clone().unwrap_or_else(|| "noid".into());
            if let Some(reject_msg) = reject_by_call_id.remove(&call_id) {
                g.session.messages.push(reject_msg);
                last_batch_tool_message_index = Some(g.session.messages.len() - 1);
                continue;
            }
            let Some((call, res)) = executed_by_call_id.remove(&call_id) else {
                continue;
            };
            processed_results.push(Self::process_and_apply_tool_result(
                &mut g,
                &mut last_batch_tool_message_index,
                &call,
                res,
            ));
        }

        for (_, reject_msg) in reject_by_call_id {
            g.session.messages.push(reject_msg);
            last_batch_tool_message_index = Some(g.session.messages.len() - 1);
        }
        for (_, (call, res)) in executed_by_call_id {
            processed_results.push(Self::process_and_apply_tool_result(
                &mut g,
                &mut last_batch_tool_message_index,
                &call,
                res,
            ));
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

            let non_soft_outcome = tool_results::batch_has_non_soft_outcome(&processed_results);

            let cfg = g.config.clone();
            g.guardrails.on_tool_turn_finished(
                &cfg,
                pending_parallel,
                pending_parallel > 0,
                false,
                non_soft_outcome,
                "agent_engine",
            );
            g.pending.tool_turn_parallel = 0;
            g.pending.reject_count_this_turn = 0;
        }

        let budget_meta = g
            .guardrails
            .tool_budget_snapshot(&g.config, &g.last_model_response_greedy_tool_names);
        let turn_index = g.turn_index;
        inject_tool_budget_into_session(
            &mut g.session,
            turn_index,
            &budget_meta,
            last_batch_tool_message_index,
        );

        if g.pending.auto_finalize_finish {
            g.pending.auto_finalize_finish = false;
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
            let synth = synthetic_finalize_calls_with_semantics(
                &g.pending.artifact_ids,
                &g.config.tool_semantics,
            );
            if !synth.is_empty() {
                g.pending.execute = Some(synth);
                g.pending.execute_skip_guardrail_count = true;
                g.pending.auto_finalize_finish = true;
                g.phase = EnginePhase::AwaitingTools;
            }
        }

        let auto_finalize_queued =
            g.pending.execute_skip_guardrail_count && g.pending.execute.is_some();
        if !auto_finalize_queued {
            match g.run_compaction_pass(CompactionTrigger::PostToolResult) {
                Ok(Some(_)) => return,
                Ok(None) => {}
                Err(e) => {
                    g.set_terminal(AgentAction::Failed(e));
                    return;
                }
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

    /// Dispatch glue for slice 5: bundle construction must go through `&mut
    /// InnerSession` (not the `MutexGuard`), because simultaneous field
    /// splits through the guard's autoderef do not borrow-check.
    fn intake_parse_failed(
        g: &mut InnerSession,
        visible_assistant_text: String,
        error_json_for_model: String,
    ) {
        model_intake::apply_ingest_parse_failed(
            model_intake::IngestArmInputs {
                session: &mut g.session,
                guardrails: &mut g.guardrails,
                config: &g.config,
                state_ledger: &mut g.state_ledger,
                events: &mut g.events,
                turn_index: g.turn_index,
                last_model_response_greedy_tool_names: &g.last_model_response_greedy_tool_names,
            },
            visible_assistant_text,
            error_json_for_model,
        );
    }

    /// Dispatch glue for slice 5 (see `intake_parse_failed`).
    fn intake_tools_blocked(
        g: &mut InnerSession,
        visible_assistant_text: String,
        user_fallback_text: String,
        nudge_json_for_model: String,
    ) -> model_intake::ToolsBlockedEffect {
        model_intake::apply_ingest_tools_blocked(
            model_intake::IngestArmInputs {
                session: &mut g.session,
                guardrails: &mut g.guardrails,
                config: &g.config,
                state_ledger: &mut g.state_ledger,
                events: &mut g.events,
                turn_index: g.turn_index,
                last_model_response_greedy_tool_names: &g.last_model_response_greedy_tool_names,
            },
            visible_assistant_text,
            user_fallback_text,
            nudge_json_for_model,
            &mut g.tools_blocked_nudges_this_turn,
            g.model_requests_this_turn,
            g.config.max_model_requests_per_turn,
        )
    }

    /// Dispatch glue for the assistant-append stage.
    fn push_assistant_message(
        g: &mut InnerSession,
        merged_tool_calls: &[RawToolCall],
        assistant_content: String,
        ingest_metadata: serde_json::Value,
        provider_reasoning_content: Option<&str>,
    ) {
        model_intake::append_assistant_message(
            model_intake::AppendAssistantInputs {
                session: &mut g.session,
                state_ledger: &mut g.state_ledger,
                enable_state_ledger: g.config.enable_state_ledger,
                events: &mut g.events,
                enable_loop_hooks: g.config.enable_loop_hooks,
                lifecycle_hooks_supported: g.capabilities.lifecycle_hooks_supported,
                greedy_tool_names: &mut g.last_model_response_greedy_tool_names,
            },
            merged_tool_calls,
            assistant_content,
            ingest_metadata,
            provider_reasoning_content,
        );
    }

    /// Dispatch glue for slice 5 (see `intake_parse_failed`).
    fn intake_repair(
        g: &mut InnerSession,
        merged_tool_calls: &[RawToolCall],
        catalog: &ToolCatalog,
    ) -> model_intake::RepairTurnOutcome {
        model_intake::repair_tool_turn(
            merged_tool_calls,
            model_intake::RepairTurnInputs {
                semantics: &g.config.tool_semantics,
                catalog,
                active_tool_names: &mut g.active_tool_names,
                turn_index: g.turn_index,
                default_tool_timeout_ms: g.config.default_tool_timeout_ms,
                tool_retry_limit: g.config.tool_retry_limit,
                require_read_before_edit: g.config.require_read_before_edit,
            },
        )
    }

    /// Dispatch glue for slice 5 (see `intake_parse_failed`).
    fn intake_flush_rejects(g: &mut InnerSession, parallel: usize, rejects: usize) {
        model_intake::flush_reject_only_turn(model_intake::RejectFlushInputs {
            session: &mut g.session,
            guardrails: &mut g.guardrails,
            config: &g.config,
            pending_tool_turn_request_order: &mut g.pending.tool_turn_request_order,
            pending_reject_tool_by_call_id: &mut g.pending.reject_tool_by_call_id,
            turn_index: g.turn_index,
            parallel,
            rejects,
            last_model_response_greedy_tool_names: &g.last_model_response_greedy_tool_names,
        });
    }

    /// Dispatch glue for slice 4: owns every pressure read and the apply
    /// ordering; the session-free `process_one_result` stage runs in between.
    fn process_and_apply_tool_result(
        g: &mut InnerSession,
        last_batch_tool_message_index: &mut Option<usize>,
        call: &ToolCallRecord,
        res: ToolResultRecord,
    ) -> ToolResultRecord {
        let spill_tokens_before = g
            .estimate_pressure(CompactionTrigger::PostToolResult)
            .total_tokens;
        // J1: policy lookup over the call-record name goes through the
        // model-output constructor (punctuated input resolves instead of
        // silently missing policy); a blank name matches no policy.
        let call_tool = ToolName::from_model_output(&call.name).ok();
        let policy = call_tool
            .as_ref()
            .and_then(|name| result_policy_for(&g.config.tool_semantics, name));
        let skip_result_truncation = policy
            .map(|policy| policy.skip_result_truncation)
            .unwrap_or(false);
        let truncation_budget = if skip_result_truncation {
            None
        } else {
            let pressure = g.estimate_pressure(CompactionTrigger::PostToolResult);
            Some(effective_tool_result_budget_for_window(
                &pressure,
                &g.capabilities,
                &g.config,
                g.io.context_window_tokens,
            ))
        };
        let processed = tool_results::process_one_result(
            call,
            res,
            tool_results::ProcessOneResultInputs {
                semantics: &g.config.tool_semantics,
                spill_threshold_bytes: effective_tool_spill_threshold_bytes(
                    &g.config,
                    g.io.context_window_tokens,
                ),
                truncation_budget,
                turn_index: g.turn_index,
                context_store: &mut g.context_store,
                pending_artifact_ids: &mut g.pending.artifact_ids,
            },
        );
        let spill_tokens_after = if processed.spilled_inline {
            g.estimate_pressure(CompactionTrigger::PostToolResult)
                .total_tokens
        } else {
            spill_tokens_before
        };
        tool_results::apply_processed_tool_result(
            tool_results::ApplyProcessedInputs {
                session: &mut g.session,
                enable_state_ledger: g.config.enable_state_ledger,
                state_ledger: &mut g.state_ledger,
                active_tool_names: &mut g.active_tool_names,
                events: &mut g.events,
                last_batch_tool_message_index,
                spill_tokens_before,
                spill_tokens_after,
            },
            processed,
        )
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

        let req = g.pending.summary_req.take().expect("summary req");
        let ctx = g.pending.summary_ctx.take().expect("summary ctx");
        g.phase = EnginePhase::Idle;

        if summary.summary_text.trim().is_empty() {
            g.retry_without_summary(ctx.trigger);
            return;
        }

        let cfg = g.config.clone();
        let message_count_before = g.session.messages.len() as u32;
        if let Err(reason) = apply_summary_result(&mut g.session, &req, &summary, &cfg, &ctx) {
            if reason == "empty summary_text" {
                g.retry_without_summary(ctx.trigger);
                return;
            }
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
        g.emit_post_compact(
            ctx.trigger,
            Some(summary_ref),
            req.history_ref.clone(),
            &[CompactionTier::Summary],
            message_count_before,
        );

        if g.pending.execute.is_some() {
            g.phase = EnginePhase::AwaitingTools;
        }
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
        // A pending model/tool/summary round-trip owns the phase; rewriting it
        // to AwaitingSummary here would turn the in-flight submit_* call into
        // an InvalidTransition. Defer to a no-op until the engine is Idle.
        if !g.config.enable_manual_compact || !matches!(g.phase, EnginePhase::Idle) {
            let pressure = g.estimate_pressure(CompactionTrigger::Manual);
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
        let before = g.estimate_pressure(CompactionTrigger::Manual).total_tokens;

        let outcome =
            crate::compact::SummaryCompactionTier.apply(g.session.clone(), &g.config, &ctx);
        match outcome {
            CompactionOutcome::NeedsHostCall(req, sess) => {
                let req = compaction::apply_manual_focus_instruction(req, &request);
                // Shared staging: session replace + history/ledger refs.
                let req = InnerSession::stage_summary_request(&mut g, sess, req, trigger);
                g.pending.summary_req = Some(req.clone());
                g.pending.summary_ctx = Some(ctx);
                g.phase = EnginePhase::AwaitingSummary;
                AgentAction::RequestSummary(req)
            }
            CompactionOutcome::Applied(sess, ev) => {
                let tokens_after = ev.tokens_after;
                g.session = sess;
                g.emit(AgentEvent::CompactionApplied(ev));
                g.emit_post_compact(trigger, None, None, &[], 0);
                AgentAction::ManualCompactCompleted(ManualCompactResult {
                    summary_ref: None,
                    history_ref: None,
                    tokens_before: before,
                    tokens_after,
                })
            }
            CompactionOutcome::NotApplicable => {
                g.emit_post_compact(trigger, None, None, &[], 0);
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
}

/// ENC-293: append context is detected from typed `raw_tool_calls` metadata
/// only. Content-substring matching was a compatibility crutch: it misfires
/// on prose mentions and misses structured calls, so the content arms were
/// deleted. Hosts must populate `raw_tool_calls` (the engine does so via
/// `patch_last_assistant_raw_tool_calls` after every model intake).
pub(super) fn session_has_append_context(
    messages: &[AgentMessage],
    artifact_squash: Option<&ArtifactSquashSemantics>,
) -> bool {
    let Some(squash) = artifact_squash else {
        return false;
    };
    if squash.append_tool_name.is_empty() {
        return false;
    }
    messages.iter().any(|message| {
        crate::compact::artifact_squash::metadata_has_tool(
            message,
            squash.append_tool_name.as_str(),
        )
    })
}

impl InnerSession {
    /// ENC-286 slice 3: dispatch glue — bundles the disjoint finish fields for
    /// `finish::finish_after_model_stop`. No logic lives here.
    fn finish_state(&mut self) -> finish::FinishState<'_> {
        finish::FinishState {
            session: &mut self.session,
            config: &self.config,
            capabilities: &self.capabilities,
            io: &self.io,
            guardrails: &mut self.guardrails,
            loop_metrics: &self.loop_metrics,
            phase: &mut self.phase,
            terminal_action: &mut self.terminal_action,
            pending: &mut self.pending,
            last_model_response_greedy_tool_names: &self.last_model_response_greedy_tool_names,
            state_ledger: &mut self.state_ledger,
            context_store: &mut self.context_store,
            sidecar_builder: &mut self.sidecar_builder,
            active_tool_names: &self.active_tool_names,
            turn_index: self.turn_index,
            model_requests_this_turn: &mut self.model_requests_this_turn,
            compaction_passes_this_turn: self.compaction.passes_this_turn(),
            destructive_tiers_this_turn: self.compaction.destructive_tiers_this_turn(),
            suppress_summary_tier: self.compaction.suppress_summary_tier(),
            events: &mut self.events,
        }
    }

    /// Manual-compact entry to the shared `NeedsHostCall` staging, which is
    /// an `InnerSession` method in the compaction module (ENC-306 D2).
    fn stage_summary_request(
        g: &mut InnerSession,
        sess: AgentSession,
        req: SummaryRequest,
        trigger: CompactionTrigger,
    ) -> SummaryRequest {
        g.attach_summary_staging(sess, req, trigger)
    }

    /// FRG-47: shared empty-summary fallback — re-runs compaction without the
    /// summary tier so the next tier can make progress. Owns no phase
    /// transition itself; `run_compaction_pass` does.
    fn retry_without_summary(&mut self, trigger: CompactionTrigger) {
        self.compaction.set_suppress_summary_tier(true);
        match self.run_compaction_pass(trigger) {
            Ok(Some(_)) => {}
            Ok(None) => {}
            Err(e) => {
                self.set_terminal(AgentAction::Failed(e));
            }
        }
        self.compaction.set_suppress_summary_tier(false);
    }

    pub(super) fn set_terminal(&mut self, a: AgentAction) {
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
        destructive_tiers: &[CompactionTier],
        message_count_before: u32,
    ) {
        if !destructive_tiers.is_empty() {
            let boundary = record_destructive_compaction_boundary(
                &mut self.session,
                trigger,
                destructive_tiers.to_vec(),
                message_count_before,
            );
            self.emit(AgentEvent::CompactionBoundary(boundary));
        }
        self.emit(AgentEvent::PostCompact {
            trigger,
            summary_ref,
            history_ref,
            transcript_revision_after: self.session.transcript_revision,
        });
    }

    fn make_ctx(&self, trigger: CompactionTrigger) -> CompactionContext {
        CompactionContext {
            provider: self.io.provider.clone(),
            context_window_tokens: self.io.context_window_tokens,
            tools_schema_json: self.io.schemas.active_json().to_string(),
            system_prompt: self.io.system_prompt.clone(),
            context_sidecars_estimate: String::new(),
            capabilities: self.capabilities.clone(),
            trigger,
            suppress_summary_tier: self.compaction.suppress_summary_tier(),
        }
    }

    fn estimate_pressure(&self, trigger: CompactionTrigger) -> crate::session::Pressure {
        let ctx = self.make_ctx(trigger);
        estimate_session_pressure(&self.session, &self.config, &ctx)
    }

    fn persist_artifact_state_if_present(&mut self) {
        let Some(squash) = self
            .config
            .tool_semantics
            .compaction
            .artifact_squash
            .as_ref()
        else {
            return;
        };
        let Some(anchor) = extract_artifact_anchor(&self.session.messages, squash) else {
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
        self.session.session_context.history_snapshot_ids = message_ids;
    }

    fn run_compaction_pass(
        &mut self,
        trigger: CompactionTrigger,
    ) -> Result<Option<AgentAction>, AgentError> {
        let mut pass = compaction::CompactionPassState {
            sync_applied: false,
            last_history_ref: None,
            destructive_tiers: Vec::new(),
            message_count_before: self.session.messages.len() as u32,
        };
        loop {
            let ctx = self.make_ctx(trigger);
            let policy = resolve_context_policy(
                &self.config,
                &self.capabilities,
                &self.io.provider,
                self.io.context_window_tokens,
            );
            let pressure = self.estimate_pressure(trigger);
            let mut plan = build_compaction_plan(
                &self.config,
                &self.capabilities,
                &policy,
                trigger,
                self.io.context_window_tokens,
                self.compaction.suppress_summary_tier(),
            );
            // ENC-303 Q2: ConfirmationGated tiers (Fifo, TextTruncate) stay
            // in a ratio-triggered plan only on hard-ceiling pressure, a
            // manual trigger, or (Fifo) an already-satisfied snapshot-first
            // requirement. Pressure is plan-independent, so computing it
            // first is a pure reorder.
            apply_confirmation_gate(&mut plan, &pressure);
            let threshold_tokens = plan.threshold_tokens;
            let ratio_threshold_tokens =
                ratio_threshold_tokens_for_plan(&self.config, self.io.context_window_tokens, &plan);
            self.session.ledger_snapshot = Some(pressure.clone());

            let session_has_tool_messages = self
                .session
                .messages
                .iter()
                .any(|m| m.role == AgentRole::Tool);
            let (has_append_context, defer_compaction_failure) = compaction::probe_turn_flags(
                &self.session.messages,
                self.config
                    .tool_semantics
                    .compaction
                    .artifact_squash
                    .as_ref(),
                self.pending.finalize_after_tool_stop,
                self.pending.execute.is_some(),
                self.session.compaction_log.is_empty(),
                session_has_tool_messages,
            );

            if let Some(err) = mid_turn_stall_error(
                trigger,
                MidTurnStallGate {
                    allow_fifo_on_mid_turn: self.config.allow_fifo_on_mid_turn,
                    summarization_allowed: self.capabilities.summarization_allowed,
                    compaction_log_is_empty: self.session.compaction_log.is_empty(),
                    defer_compaction_failure,
                    require_nonempty_log: true,
                },
            ) {
                return Err(err);
            }

            // ENC-286 slice 6 / FRG-47: pure entry gate; the dispatch keeps
            // looping, emitting, and phase wiring.
            let fire = should_enter_compaction(FireConditionInputs {
                config: &self.config,
                capabilities: &self.capabilities,
                trigger,
                pressure: &pressure,
                threshold_tokens,
                ratio_threshold_tokens,
                turn_count: self.loop_metrics.turn_count,
                tool_calls_executed: self.guardrails.tool_calls_executed,
                session_has_tool_messages,
                last_message_role: compaction::last_message_role(&self.session.messages),
            });
            let entered_for_long_loop_only = fire.entered_for_long_loop_only;
            let forced_by_non_ratio = fire.forced_by_non_ratio;

            if !fire.should_enter {
                if pass.sync_applied {
                    self.emit_post_compact(
                        trigger,
                        None,
                        pass.last_history_ref.take(),
                        &pass.destructive_tiers,
                        pass.message_count_before,
                    );
                }
                return Ok(None);
            }

            self.emit(AgentEvent::PreCompact { trigger });
            self.emit(AgentEvent::ContextPressure {
                ratio: pressure.ratio,
            });

            if let Some(err) = mid_turn_stall_error(
                trigger,
                MidTurnStallGate {
                    allow_fifo_on_mid_turn: self.config.allow_fifo_on_mid_turn,
                    summarization_allowed: self.capabilities.summarization_allowed,
                    compaction_log_is_empty: self.session.compaction_log.is_empty(),
                    defer_compaction_failure,
                    require_nonempty_log: false,
                },
            ) {
                return Err(err);
            }

            if self.config.enable_context_store {
                // ENC-306 D2: the sync spill/snapshot block is a fused
                // InnerSession method in the compaction module; it runs
                // against self and applies its effects in order.
                self.sync_spill_pass(pressure.total_tokens, &mut pass, trigger);
            }
            if has_append_context {
                self.persist_artifact_state_if_present();
            }

            let (effective_plan, effective_ctx) =
                compaction::apply_force_override(&plan, &ctx, forced_by_non_ratio);

            let pipeline = compaction_pipeline_for_plan(&effective_plan);
            let outcome = pipeline.run_plan(
                self.session.clone(),
                &effective_ctx,
                &self.config,
                &effective_plan,
            );
            // FRG-47 / ENC-306 D2+D4: the 4-arm outcome match lives exactly
            // once, inside `decide_pipeline_outcome`, which applies its
            // effects directly; the dispatch owns the loop and the return.
            // Post pressure is read only for the NotApplicable arm, exactly
            // as before.
            let post_pressure = matches!(outcome, CompactionOutcome::NotApplicable)
                .then(|| self.estimate_pressure(trigger));
            match self.decide_pipeline_outcome(
                outcome,
                &mut pass,
                compaction::PipelinePassContext {
                    trigger,
                    loop_ctx: ctx,
                    forced_by_non_ratio,
                    entered_for_long_loop_only,
                    post_pressure,
                    defer_compaction_failure,
                },
            ) {
                Ok(compaction::Flow::Loop) => continue,
                Ok(compaction::Flow::Done(action)) => return Ok(action),
                Err(e) => return Err(e),
            }
        }
    }

    fn step_idle(&mut self) -> Result<AgentAction, AgentError> {
        if let Some(out) = self.run_compaction_pass(CompactionTrigger::Preflight)? {
            return Ok(out);
        }

        self.turn_index += 1;
        self.context_store.expire_after_turn(self.turn_index);
        // Reset per-turn telemetry counters for the new user turn.
        self.compaction.begin_turn();
        let allow = self
            .guardrails
            .allow_tool_call(&self.config, &self.last_model_response_greedy_tool_names);

        self.emit(AgentEvent::TurnStarted {
            turn_index: self.turn_index,
            allow_tool_call: allow,
        });

        let cache_hints = if self.capabilities.prompt_cache_supported {
            if self.config.prompt_cache_anchor_threshold_tokens == u32::MAX {
                vec![]
            } else {
                vec!["stable-prefix".to_string()]
            }
        } else {
            vec![]
        };

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

        let preflight_ctx = self.make_ctx(CompactionTrigger::Preflight);
        let preflight_pressure =
            estimate_session_pressure(&self.session, &self.config, &preflight_ctx);
        if exceeds_hard_prompt_ceiling(&preflight_pressure) {
            return Err(AgentError::CompactionFailed {
                tier: "pipeline".into(),
                reason: format!(
                    "Projected prompt {} tokens exceeds hard ceiling {} tokens after compaction",
                    preflight_pressure.total_tokens, preflight_pressure.usable_prompt_tokens
                ),
            });
        }

        // ENC-286 slice 2: disjoint-field borrow split — each field is passed
        // explicitly; this never hands the whole guard to the callee.
        let req = model_request::build_model_request(model_request::ModelRequestInputs {
            session: &self.session,
            config: &self.config,
            capabilities: &self.capabilities,
            io: &self.io,
            active_tool_names: &self.active_tool_names,
            context_store: &mut self.context_store,
            state_ledger: &self.state_ledger,
            sidecar_builder: &mut self.sidecar_builder,
            turn_index: self.turn_index,
            compaction_passes_this_turn: self.compaction.passes_this_turn(),
            destructive_tiers_this_turn: self.compaction.destructive_tiers_this_turn(),
            suppress_summary_tier: self.compaction.suppress_summary_tier(),
            allow_tool_call: allow,
            cache_hints,
            loop_metrics: &self.loop_metrics,
        });

        self.pending.call_model = Some(req.clone());
        self.model_requests_this_turn += 1;
        self.phase = EnginePhase::AwaitingModel;
        Ok(AgentAction::CallModel(req))
    }
}

fn truncate_model_api_failure_reason(message: &str) -> String {
    const MAX_CHARS: usize = 240;
    if message.chars().count() <= MAX_CHARS {
        return message.to_string();
    }
    message.chars().take(MAX_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::MessageMeta;
    use crate::turn_io::RawToolCall;

    fn io_small_window() -> SessionIoParams {
        SessionIoParams::new("anthropic".into(), 256, "[]".into(), "sys".into())
            .expect("valid test io params")
    }

    fn io_large_window() -> SessionIoParams {
        SessionIoParams::new("anthropic".into(), 200_000, "[]".into(), "sys".into())
            .expect("valid test io params")
    }

    #[test]
    #[cfg(feature = "lalr-harness")]
    fn create_session_wires_long_loop_policy_from_context_tier() {
        use crate::long_loop_policy::LongLoopPolicy;
        use crate::policy::ContextPolicyTier;

        let eng = AgentEngine::new();
        let hosted = eng
            .create_session(
                vec![],
                AgentConfig {
                    tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
                    ..AgentConfig::default()
                },
                SessionCapabilities {
                    context_policy_tier: ContextPolicyTier::HostedMutable,
                    ..Default::default()
                },
                serde_json::Value::Null,
                io_large_window(),
            )
            .expect("valid config creates session");
        assert_eq!(
            hosted.lalr_config().long_loop,
            LongLoopPolicy::hosted_mutable_defaults()
        );

        let fixed_kv = eng
            .create_session(
                vec![],
                AgentConfig {
                    tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
                    ..AgentConfig::default()
                },
                SessionCapabilities {
                    context_policy_tier: ContextPolicyTier::FixedKv,
                    kv_fixed: true,
                    ..Default::default()
                },
                serde_json::Value::Null,
                io_large_window(),
            )
            .expect("valid config creates session");
        assert_eq!(
            fixed_kv.lalr_config().long_loop,
            LongLoopPolicy::fixed_kv_defaults()
        );
    }

    #[test]
    fn submit_model_response_preserves_provider_reasoning_metadata() {
        let eng = AgentEngine::new();
        let h = eng
            .create_session(
                vec![AgentMessage {
                    id: "u1".into(),
                    role: AgentRole::User,
                    content: "hi".into(),
                    metadata: MessageMeta::new(),
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
            )
            .expect("valid config creates session");

        assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
        eng.submit_model_response(
            &h,
            ModelResponse {
                assistant_text: None,
                tool_calls: vec![RawToolCall {
                    id: Some("c1".into()),
                    name: "web_search".into(),
                    arguments_json: r#"{"query":"a"}"#.into(),
                }],
                finish_reason: FinishReason::ToolUse,
                provider_reasoning_content: Some("need tool output first".into()),
            },
        );

        let guard = h.lock_inner();
        let assistant = guard
            .session
            .messages
            .iter()
            .find(|message| message.role == AgentRole::Assistant)
            .expect("assistant message");
        assert_eq!(
            assistant.metadata.provider_reasoning_content(),
            Some("need tool output first"),
        );
    }

    #[test]
    fn three_tool_happy_path() {
        let eng = AgentEngine::new();
        let h = eng
            .create_session(
                vec![AgentMessage {
                    id: "u1".into(),
                    role: AgentRole::User,
                    content: "hi".into(),
                    metadata: MessageMeta::new(),
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
            )
            .expect("valid config creates session");

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
                provider_reasoning_content: None,
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
                    activated_tool_names: Vec::new(),
                    result_metadata: None,
                })
                .collect(),
        );
        assert!(matches!(eng.next_action(&h), AgentAction::CallModel(_)));
    }

    #[test]
    fn tool_results_matched_by_call_id_not_position() {
        let eng = AgentEngine::new();
        let h = eng
            .create_session(
                vec![AgentMessage {
                    id: "u1".into(),
                    role: AgentRole::User,
                    content: "hi".into(),
                    metadata: MessageMeta::new(),
                }],
                AgentConfig {
                    tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
                    ..AgentConfig::default()
                },
                SessionCapabilities {
                    summarization_allowed: false,
                    kv_fixed: false,
                    tool_result_budget_tokens: None,
                    ..Default::default()
                },
                serde_json::Value::Null,
                io_large_window(),
            )
            .expect("valid config creates session");
        let _ = eng.next_action(&h);
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
                        name: "web_extract".into(),
                        arguments_json: r#"{"urls":["https://b"]}"#.into(),
                    },
                ],
                finish_reason: FinishReason::ToolUse,
                provider_reasoning_content: None,
            },
        );
        let _ = eng.next_action(&h);
        // Host returns results out of order: c2 first, then c1.
        eng.submit_tool_results(
            &h,
            vec![
                ToolResultRecord {
                    call_id: "c2".into(),
                    result_json: r#"{"results":["B"]}"#.into(),
                    truncated: false,
                    soft_failure: false,
                    activated_tool_names: Vec::new(),
                    result_metadata: None,
                },
                ToolResultRecord {
                    call_id: "c1".into(),
                    result_json: r#"{"results":["A"]}"#.into(),
                    truncated: false,
                    soft_failure: false,
                    activated_tool_names: Vec::new(),
                    result_metadata: None,
                },
            ],
        );
        let inner = h.lock_inner();
        let c1_msg = inner
            .session
            .messages
            .iter()
            .find(|m| m.id == "tool-c1")
            .expect("tool-c1 message");
        assert!(
            c1_msg.content.contains("\"A\""),
            "c1 result misattributed: {}",
            c1_msg.content
        );
        assert_eq!(
            c1_msg.metadata.tool_name(),
            Some("web_search"),
            "c1 tool_name metadata paired with wrong call"
        );
        let c2_msg = inner
            .session
            .messages
            .iter()
            .find(|m| m.id == "tool-c2")
            .expect("tool-c2 message");
        assert!(
            c2_msg.content.contains("\"B\""),
            "c2 result misattributed: {}",
            c2_msg.content
        );
        assert_eq!(
            c2_msg.metadata.tool_name(),
            Some("web_extract"),
            "c2 tool_name metadata paired with wrong call"
        );
    }

    #[test]
    fn missing_tool_result_synthesizes_error_for_provider_replay() {
        let eng = AgentEngine::new();
        let h = eng
            .create_session(
                vec![AgentMessage {
                    id: "u1".into(),
                    role: AgentRole::User,
                    content: "hi".into(),
                    metadata: MessageMeta::new(),
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
            )
            .expect("valid config creates session");
        let _ = eng.next_action(&h);
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
                ],
                finish_reason: FinishReason::ToolUse,
                provider_reasoning_content: None,
            },
        );
        let _ = eng.next_action(&h);
        // Host drops c2's result (e.g. executor bug). Engine must still push a
        // tool message for c2 so provider replay has one per tool_call_id.
        eng.submit_tool_results(
            &h,
            vec![ToolResultRecord {
                call_id: "c1".into(),
                result_json: r#"{"results":["A"]}"#.into(),
                truncated: false,
                soft_failure: false,
                activated_tool_names: Vec::new(),
                result_metadata: None,
            }],
        );
        let inner = h.lock_inner();
        let c2_msg = inner
            .session
            .messages
            .iter()
            .find(|m| m.id == "tool-c2")
            .expect("tool-c2 synthesized message");
        assert!(
            c2_msg.content.contains("tool_result_missing"),
            "expected synthesized error, got: {}",
            c2_msg.content
        );
    }

    #[test]
    fn fuzzy_repair_web_seach() {
        let eng = AgentEngine::new();
        let h = eng
            .create_session(
                vec![AgentMessage {
                    id: "u1".into(),
                    role: AgentRole::User,
                    content: "hi".into(),
                    metadata: MessageMeta::new(),
                }],
                AgentConfig {
                    tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
                    ..AgentConfig::default()
                },
                SessionCapabilities {
                    summarization_allowed: false,
                    kv_fixed: false,
                    tool_result_budget_tokens: None,
                    ..Default::default()
                },
                serde_json::Value::Null,
                io_large_window(),
            )
            .expect("valid config creates session");
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
                provider_reasoning_content: None,
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
        let cfg = AgentConfig {
            max_turns: 1,
            ..Default::default()
        };
        let eng = AgentEngine::new();
        let h = eng
            .create_session(
                vec![AgentMessage {
                    id: "u1".into(),
                    role: AgentRole::User,
                    content: "hi".into(),
                    metadata: MessageMeta::new(),
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
            )
            .expect("valid config creates session");
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
                provider_reasoning_content: None,
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
                activated_tool_names: Vec::new(),
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
        let eng = AgentEngine::new();
        let h = eng
            .create_session(
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
            )
            .expect("valid config creates session");
        eng.cancel(&h);
        assert!(matches!(
            eng.next_action(&h),
            AgentAction::Failed(AgentError::Internal(_))
        ));
    }

    #[test]
    fn preflight_compaction_on_oversized_history() {
        let big_pair = "hello ".repeat(400);
        let eng = AgentEngine::new();
        let h = eng
            .create_session(
                vec![
                    AgentMessage {
                        id: "u0".into(),
                        role: AgentRole::User,
                        content: big_pair.clone(),
                        metadata: MessageMeta::new(),
                    },
                    AgentMessage {
                        id: "a0".into(),
                        role: AgentRole::Assistant,
                        content: big_pair,
                        metadata: MessageMeta::new(),
                    },
                    AgentMessage {
                        id: "u1".into(),
                        role: AgentRole::User,
                        content: "tail".into(),
                        metadata: MessageMeta::new(),
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
            )
            .expect("valid config creates session");
        let _ = eng.next_action(&h);
        assert!(!h.lock_inner().session.compaction_log.is_empty());
    }

    #[test]
    fn kv_fixed_emits_invalidated_on_compact() {
        let big_pair = "hello ".repeat(400);
        let eng = AgentEngine::new();
        let h = eng
            .create_session(
                vec![
                    AgentMessage {
                        id: "u0".into(),
                        role: AgentRole::User,
                        content: big_pair.clone(),
                        metadata: MessageMeta::new(),
                    },
                    AgentMessage {
                        id: "a0".into(),
                        role: AgentRole::Assistant,
                        content: big_pair,
                        metadata: MessageMeta::new(),
                    },
                    AgentMessage {
                        id: "u1".into(),
                        role: AgentRole::User,
                        content: "tail".into(),
                        metadata: MessageMeta::new(),
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
            )
            .expect("valid config creates session");
        let _ = eng.next_action(&h);
        let evs = eng.drain_events(&h);
        assert!(evs
            .iter()
            .any(|e| matches!(e, AgentEvent::KvCacheInvalidated { .. })));
    }

    #[test]
    fn artifact_auto_finalize_then_finished() {
        let eng = AgentEngine::new();
        let h = eng
            .create_session(
                vec![AgentMessage {
                    id: "u1".into(),
                    role: AgentRole::User,
                    content: "hi".into(),
                    metadata: MessageMeta::new(),
                }],
                AgentConfig {
                    tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
                    ..AgentConfig::default()
                },
                SessionCapabilities {
                    summarization_allowed: false,
                    kv_fixed: false,
                    tool_result_budget_tokens: None,
                    ..Default::default()
                },
                serde_json::Value::Null,
                io_large_window(),
            )
            .expect("valid config creates session");

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
                provider_reasoning_content: None,
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
                activated_tool_names: Vec::new(),
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
                activated_tool_names: Vec::new(),
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
        let eng = AgentEngine::new();
        let h = eng
            .create_session(
                vec![AgentMessage {
                    id: "u1".into(),
                    role: AgentRole::User,
                    content: "hi".into(),
                    metadata: MessageMeta::new(),
                }],
                AgentConfig {
                    tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
                    ..AgentConfig::default()
                },
                SessionCapabilities {
                    summarization_allowed: false,
                    kv_fixed: false,
                    tool_result_budget_tokens: None,
                    ..Default::default()
                },
                serde_json::Value::Null,
                io_large_window(),
            )
            .expect("valid config creates session");

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
                provider_reasoning_content: None,
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
                activated_tool_names: Vec::new(),
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
                provider_reasoning_content: None,
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
                activated_tool_names: Vec::new(),
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
                activated_tool_names: Vec::new(),
                result_metadata: None,
            }],
        );

        assert!(matches!(eng.next_action(&h), AgentAction::Finished));
    }

    #[test]
    fn tb_10_seed_guardrails_auto_finalize_repro() {
        let eng = AgentEngine::new();
        let h = eng
            .create_session(
                vec![AgentMessage {
                    id: "u1".into(),
                    role: AgentRole::User,
                    content: "hi".into(),
                    metadata: MessageMeta::new(),
                }],
                AgentConfig {
                    tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
                    ..AgentConfig::default()
                },
                SessionCapabilities {
                    summarization_allowed: false,
                    kv_fixed: false,
                    tool_result_budget_tokens: None,
                    ..Default::default()
                },
                serde_json::Value::Null,
                io_large_window(),
            )
            .expect("valid config creates session");
        h.integration_test_seed_guardrails(3, 0, 0, vec![]);
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
        let _ = eng.next_action(&h);
        eng.submit_tool_results(
            &h,
            vec![ToolResultRecord {
                call_id: "c1".into(),
                result_json: r#"{"results":[]}"#.into(),
                truncated: false,
                soft_failure: false,
                activated_tool_names: Vec::new(),
                result_metadata: None,
            }],
        );
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
        let after_art = eng.next_action(&h);
        assert!(
            matches!(after_art, AgentAction::ExecuteTools(_)),
            "after start_artifact: {after_art:?}"
        );
        eng.submit_tool_results(
            &h,
            vec![ToolResultRecord {
                call_id: "c-art".into(),
                result_json: r#"{"status":"success","artifactId":"art-1"}"#.into(),
                truncated: false,
                soft_failure: false,
                activated_tool_names: Vec::new(),
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
        let inner = h.lock_inner();
        assert!(
            inner.terminal_action.is_none(),
            "terminal after append submit: {:?}",
            inner.terminal_action
        );
        drop(inner);
        let after_append_submit = eng.next_action(&h);
        assert!(
            matches!(after_append_submit, AgentAction::ExecuteTools(_)),
            "after append submit: {after_append_submit:?}"
        );
        eng.submit_tool_results(
            &h,
            vec![ToolResultRecord {
                call_id: "c2".into(),
                result_json: r#"{"status":"success","isComplete":false}"#.into(),
                truncated: false,
                soft_failure: false,
                activated_tool_names: Vec::new(),
                result_metadata: None,
            }],
        );
        match eng.next_action(&h) {
            AgentAction::ExecuteTools(calls) => {
                assert_eq!(calls[0].call_id, "auto-finalize-art-1");
            }
            other => panic!("expected auto-finalize, got {other:?}"),
        }
    }

    #[test]
    fn drain_events_clears_buffer() {
        let eng = AgentEngine::new();
        let h = eng
            .create_session(
                vec![AgentMessage {
                    id: "u1".into(),
                    role: AgentRole::User,
                    content: "hi".into(),
                    metadata: MessageMeta::new(),
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
            )
            .expect("valid config creates session");
        let _ = eng.next_action(&h);
        assert!(!eng.drain_events(&h).is_empty());
        assert!(eng.drain_events(&h).is_empty());
    }

    #[test]
    fn create_session_rejects_duplicate_name_struct_literal() {
        // ENC-297 J1: an invalid struct-literal config (normalized-duplicate
        // tool names) fails session creation — no live session runs with a
        // stale or empty catalog.
        use crate::catalog::ToolSpec;
        use crate::tool_semantics::{ToolCatalogConfig, ToolSemanticsConfig};

        let eng = AgentEngine::new();
        let semantics = ToolSemanticsConfig {
            catalog: ToolCatalogConfig {
                tools: vec![
                    ToolSpec {
                        name: crate::tool_name::ToolName::new("web_search").expect("valid name"),
                        aliases: Vec::new(),
                        reproducible: true,
                        schema_hint: None,
                    },
                    ToolSpec {
                        name: crate::tool_name::ToolName::new("web-search").expect("valid name"),
                        aliases: Vec::new(),
                        reproducible: true,
                        schema_hint: None,
                    },
                ],
            },
            ..ToolSemanticsConfig::agnostic()
        };
        let config = AgentConfig {
            tool_semantics: semantics,
            ..AgentConfig::default()
        };
        let error = match eng.create_session(
            vec![],
            config,
            SessionCapabilities::default(),
            serde_json::Value::Null,
            io_large_window(),
        ) {
            Err(error) => error,
            Ok(_) => panic!("duplicate tool names must fail session creation"),
        };
        assert!(
            matches!(
                error,
                crate::tool_semantics::ToolSemanticsError::DuplicateToolName { .. }
            ),
            "unexpected error: {error:?}"
        );
    }

    fn frg14_squash(
        append_tool_name: &str,
        total_blocks_result_field: &str,
    ) -> ArtifactSquashSemantics {
        ArtifactSquashSemantics {
            // The empty-name test exercises the never-engage guard via the
            // lenient sentinel; every other caller passes a valid name.
            append_tool_name: crate::tool_name::ToolName::new(append_tool_name)
                .unwrap_or_else(|_| crate::tool_name::ToolName::from_raw_lenient(append_tool_name)),
            id_field: "docId".into(),
            blocks_field: "chunks".into(),
            total_blocks_result_field: total_blocks_result_field.into(),
            status_field: "status".into(),
            success_status_value: "success".into(),
            summary_template: "t".into(),
            continuation_instruction: "c".into(),
            snippet_max_chars: 200,
        }
    }

    fn frg14_msg(content: &str, metadata: MessageMeta) -> AgentMessage {
        AgentMessage {
            id: "m".into(),
            role: AgentRole::Assistant,
            content: content.into(),
            metadata,
        }
    }

    // ENC-293: content alone is inert — detection reads `raw_tool_calls`
    // metadata only, so tool-name mentions and total-field payloads without
    // metadata no longer engage the gate.
    #[test]
    fn frg14_append_context_custom_content_alone_inert() {
        let squash = frg14_squash("write_doc", "chunkCount");
        let messages = vec![frg14_msg(
            r#"call:write_doc{"docId":"1","chunks":[]}"#,
            MessageMeta::new(),
        )];
        assert!(!session_has_append_context(&messages, Some(&squash)));
    }

    #[test]
    fn frg14_append_context_custom_raw_tool_call_match() {
        let squash = frg14_squash("write_doc", "chunkCount");
        // Decoy content names a different tool: the metadata is authoritative.
        let messages = vec![frg14_msg(
            "call:other_tool{}",
            MessageMeta::new().with_raw_tool_calls(vec![RawToolCall {
                id: None,
                name: "write_doc".into(),
                arguments_json: "{}".into(),
            }]),
        )];
        assert!(session_has_append_context(&messages, Some(&squash)));
    }

    #[test]
    fn frg14_append_context_custom_total_field_alone_inert() {
        let squash = frg14_squash("write_doc", "chunkCount");
        let messages = vec![frg14_msg(
            r#"{"status":"success","chunkCount":2}"#,
            MessageMeta::new(),
        )];
        assert!(!session_has_append_context(&messages, Some(&squash)));
    }

    // ENC-293: prose that merely mentions the configured tool must not engage
    // the gate (the old content arm misfired on exactly this shape).
    #[test]
    fn frg14_append_context_custom_prose_mention_inert() {
        let squash = frg14_squash("write_doc", "chunkCount");
        let messages = vec![frg14_msg(
            "I will call write_doc next to append the chunkCount results",
            MessageMeta::new(),
        )];
        assert!(!session_has_append_context(&messages, Some(&squash)));
    }

    #[test]
    fn frg14_append_context_custom_ignores_legacy_literals() {
        let squash = frg14_squash("write_doc", "chunkCount");
        let messages = vec![
            frg14_msg("call:append_blocks{}", MessageMeta::new()),
            frg14_msg(r#"{"totalBlocks":1}"#, MessageMeta::new()),
        ];
        assert!(!session_has_append_context(&messages, Some(&squash)));
    }

    #[test]
    fn frg14_append_context_none_never_engages() {
        let messages = vec![
            frg14_msg("call:append_blocks{}", MessageMeta::new()),
            frg14_msg(r#"{"totalBlocks":1}"#, MessageMeta::new()),
            frg14_msg(
                "plain text",
                MessageMeta::new().with_raw_tool_calls(vec![RawToolCall {
                    id: None,
                    name: "append_blocks".into(),
                    arguments_json: "{}".into(),
                }]),
            ),
        ];
        assert!(!session_has_append_context(&messages, None));
    }

    #[test]
    fn frg14_append_context_empty_fields_never_engage() {
        let squash = frg14_squash("", "");
        let messages = vec![
            frg14_msg("call:append_blocks{}", MessageMeta::new()),
            frg14_msg(r#"{"totalBlocks":1}"#, MessageMeta::new()),
            frg14_msg(
                "plain text",
                MessageMeta::new().with_raw_tool_calls(vec![RawToolCall {
                    id: None,
                    name: "append_blocks".into(),
                    arguments_json: "{}".into(),
                }]),
            ),
        ];
        assert!(!session_has_append_context(&messages, Some(&squash)));
    }

    // ENC-293: legacy literals engage only via the metadata path now;
    // content-only legacy matches are inert.
    #[test]
    fn frg14_append_context_enclave_metadata_match() {
        let squash = frg14_squash("append_blocks", "totalBlocks");
        let via_metadata = vec![frg14_msg(
            "plain assistant text",
            MessageMeta::new().with_raw_tool_calls(vec![RawToolCall {
                id: None,
                name: "append_blocks".into(),
                arguments_json: "{}".into(),
            }]),
        )];
        assert!(session_has_append_context(&via_metadata, Some(&squash)));
        let via_tool_content = vec![frg14_msg("call:append_blocks{}", MessageMeta::new())];
        assert!(!session_has_append_context(
            &via_tool_content,
            Some(&squash)
        ));
        let via_total_content = vec![frg14_msg(r#"{"totalBlocks":1}"#, MessageMeta::new())];
        assert!(!session_has_append_context(
            &via_total_content,
            Some(&squash)
        ));
    }
}
