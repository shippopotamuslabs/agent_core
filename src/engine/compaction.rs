//! Compaction orchestration (ENC-286 slice 6; FRG-47 staging; ENC-306).
//!
//! Compaction's persistent per-turn state lives in [`CompactionState`],
//! owned by `InnerSession` as `self.compaction`; per-pass working state
//! lives in [`CompactionPassState`], owned by `run_compaction_pass` and
//! lent to the operations that need it. Operations are `InnerSession`
//! methods defined in this module (the module that owns the state) and
//! apply their effects directly: the sync spill/snapshot block is the
//! fused [`InnerSession::sync_spill_pass`]. What remains on the older
//! bundle idiom for now: the pipeline-outcome decision
//! (`decide_pipeline_outcome`) and the shared `NeedsHostCall` staging
//! (`attach_summary_staging`) still take field bundles and return their
//! decision/effects for the dispatch in `mod.rs` to apply.
//!
//! `run_compaction_pass` keeps the loop and every phase write. The pure
//! entry gate lives in `compact::policy` (`should_enter_compaction`).

use super::is_destructive_compaction_tier;
use super::state::{EnginePhase, InnerSession};
use crate::action::AgentAction;
use crate::budget::{effective_tool_spill_threshold_bytes, exceeds_hard_prompt_ceiling};
use crate::compact::history_snapshot::snapshot_history_range;
use crate::compact::tool_spill::spill_tool_messages_in_session;
use crate::compact::{CompactionContext, CompactionOutcome};
use crate::context_store::ContextStore;
use crate::events::{AgentError, AgentEvent, CompactionEvent, CompactionTier, CompactionTrigger};
use crate::manual_compact::ManualCompactRequest;
use crate::message::AgentMessage;
use crate::session::AgentSession;
use crate::session::{AgentConfig, Pressure, SessionCapabilities};
use crate::state_ledger::StateLedger;
use crate::tool_semantics::ArtifactSquashSemantics;
use crate::transcript_revision::record_destructive_compaction_boundary;
use crate::turn_io::SummaryRequest;

/// Persistent compaction state for a session (ENC-306 D1): the per-turn
/// pass / destructive-tier counters and the summary-tier suppression flag.
/// Owned by `InnerSession` as a single field; the fields stay private and
/// every reader/writer goes through these methods, so counter and flag
/// timing lives in exactly one place.
pub(super) struct CompactionState {
    passes_this_turn: u32,
    destructive_tiers_this_turn: u32,
    suppress_summary_tier: bool,
}

impl CompactionState {
    pub(super) fn new() -> Self {
        Self {
            passes_this_turn: 0,
            destructive_tiers_this_turn: 0,
            suppress_summary_tier: false,
        }
    }

    /// Start a new user turn: zeroes both counters. The suppression flag
    /// is NOT reset here — it is written only by `retry_without_summary`,
    /// which sets it for the duration of one re-run.
    pub(super) fn begin_turn(&mut self) {
        self.passes_this_turn = 0;
        self.destructive_tiers_this_turn = 0;
    }

    /// Record one applied compaction pass; `destructive` additionally
    /// counts a destructive tier (Micro/Fifo/Summary/TextTruncate).
    pub(super) fn record_applied(&mut self, destructive: bool) {
        self.passes_this_turn += 1;
        if destructive {
            self.destructive_tiers_this_turn += 1;
        }
    }

    pub(super) fn passes_this_turn(&self) -> u32 {
        self.passes_this_turn
    }

    pub(super) fn destructive_tiers_this_turn(&self) -> u32 {
        self.destructive_tiers_this_turn
    }

    pub(super) fn suppress_summary_tier(&self) -> bool {
        self.suppress_summary_tier
    }

    pub(super) fn set_suppress_summary_tier(&mut self, suppress: bool) {
        self.suppress_summary_tier = suppress;
    }
}

/// Per-pass working state for one `run_compaction_pass` invocation
/// (ENC-306 D3). Owns its fields outright (no lifetimes, no mirrors of
/// `InnerSession` fields); constructed by `run_compaction_pass` and lent
/// `&mut` to the pass operations.
pub(super) struct CompactionPassState {
    pub sync_applied: bool,
    pub last_history_ref: Option<String>,
    pub destructive_tiers: Vec<CompactionTier>,
    pub message_count_before: u32,
}

impl InnerSession {
    /// Fused sync spill/snapshot pass (ENC-306 D2): runs the spill and the
    /// history snapshot against `self` and applies the effects directly,
    /// in the exact order the old collect/apply pair produced them — the
    /// spill and snapshot first run (spill, snapshot ids, snapshot), then
    /// the spill event is pushed/emitted and `pass.sync_applied` set, then,
    /// under a history ref, the snapshot ids are remembered, the ref goes
    /// to the state ledger, and the snapshot event is pushed/emitted. The
    /// caller keeps the `enable_context_store` guard, as before.
    pub(super) fn sync_spill_pass(
        &mut self,
        pressure_total_tokens: u32,
        pass: &mut CompactionPassState,
        trigger: CompactionTrigger,
    ) {
        let spill_event = spill_tool_messages_in_session(
            &mut self.context_store,
            &mut self.session,
            self.config
                .tool_semantics
                .compaction
                .artifact_squash
                .as_ref(),
            effective_tool_spill_threshold_bytes(&self.config, self.io.context_window_tokens),
            self.turn_index,
            trigger,
        );
        let snapshot_ids: Vec<String> = self
            .session
            .messages
            .iter()
            .map(|message| message.id.clone())
            .collect();
        let snapshot_reference = snapshot_history_range(
            &mut self.context_store,
            &self.session.messages,
            trigger,
            self.turn_index,
        );
        if let Some(event) = spill_event {
            self.session.push_compaction_event(event.clone());
            self.emit(AgentEvent::CompactionApplied(event));
            pass.sync_applied = true;
        }
        if let Some(reference) = snapshot_reference {
            let history_ref = reference.id.clone();
            pass.last_history_ref = Some(history_ref.clone());
            self.remember_history_snapshot_ids(snapshot_ids);
            if self.config.enable_state_ledger {
                self.state_ledger.add_history_ref(&history_ref);
            }
            let event = CompactionEvent {
                tier: CompactionTier::HistorySnapshot,
                tokens_before: pressure_total_tokens,
                tokens_after: pressure_total_tokens,
                trigger,
            };
            self.session.push_compaction_event(event.clone());
            self.emit(AgentEvent::CompactionApplied(event));
        }
    }
}

/// Shared `NeedsHostCall` staging for `run_compaction_pass` and
/// `request_manual_compact`: replaces the session, snapshots the summary
/// messages into the context store, and attaches history/ledger refs.
///
/// Returns the staged request; the dispatch owns the pending fields, the phase
/// write, and the `RequestSummary` action.
pub(super) struct StageSummaryInputs<'a> {
    pub session: &'a mut AgentSession,
    pub context_store: &'a mut ContextStore,
    pub state_ledger: &'a mut StateLedger,
    pub enable_context_store: bool,
    pub enable_state_ledger: bool,
    pub turn_index: u32,
    pub trigger: CompactionTrigger,
}

pub(super) fn attach_summary_staging(
    inputs: StageSummaryInputs<'_>,
    sess: AgentSession,
    mut req: SummaryRequest,
) -> SummaryRequest {
    let StageSummaryInputs {
        session,
        context_store,
        state_ledger,
        enable_context_store,
        enable_state_ledger,
        turn_index,
        trigger,
    } = inputs;
    *session = sess;
    if enable_context_store {
        if let Some(reference) = snapshot_history_range(
            context_store,
            &req.messages_to_summarize,
            trigger,
            turn_index,
        ) {
            if enable_state_ledger {
                state_ledger.add_history_ref(&reference.id);
            }
            req.history_ref = Some(reference.id);
        }
    }
    if enable_state_ledger {
        req.state_ledger_snapshot = Some(state_ledger.clone());
    }
    req
}

/// Manual-compaction request plumbing shared with the pass dispatch: the
/// manual path additionally stamps the host's focus instruction.
pub(super) fn apply_manual_focus_instruction(
    mut req: SummaryRequest,
    request: &ManualCompactRequest,
) -> SummaryRequest {
    req.focus_instruction = request.focus_instruction.clone();
    req
}

/// Pure turn-context probe for `run_compaction_pass`: append-context presence
/// plus the MidTurn/PostToolResult failure-deferral condition.
pub(super) fn probe_turn_flags(
    messages: &[AgentMessage],
    artifact_squash: Option<&ArtifactSquashSemantics>,
    pending_finalize_after_tool_stop: bool,
    pending_execute_is_some: bool,
    compaction_log_is_empty: bool,
    session_has_tool_messages: bool,
) -> (bool, bool) {
    let has_append_context = super::session_has_append_context(messages, artifact_squash);
    let defer_compaction_failure = has_append_context
        || pending_finalize_after_tool_stop
        || (pending_execute_is_some && !compaction_log_is_empty && session_has_tool_messages);
    (has_append_context, defer_compaction_failure)
}

/// Forced-entry override: when entry was forced by an absolute token
/// threshold, turn count, or turn-end (not the ratio gate), the dispatch runs
/// the pipeline with a zeroed ratio and a MidTurn context so the summary tier
/// summarizes all messages (not just "before last user") for short sessions.
/// Pure plan/ctx transform.
pub(super) fn apply_force_override(
    plan: &crate::compact::policy::CompactionPlan,
    loop_ctx: &CompactionContext,
    forced_by_non_ratio: bool,
) -> (crate::compact::policy::CompactionPlan, CompactionContext) {
    if forced_by_non_ratio {
        let mut p = plan.clone();
        p.threshold_ratio = 0.0;
        p.threshold_tokens = 0;
        let mut c = loop_ctx.clone();
        c.trigger = CompactionTrigger::MidTurn;
        (p, c)
    } else {
        (plan.clone(), loop_ctx.clone())
    }
}

/// Message-role probe for the turn-end fire condition (plain data, so the
/// policy gate never touches the session).
pub(super) fn last_message_role(messages: &[AgentMessage]) -> Option<crate::message::AgentRole> {
    messages.last().map(|m| m.role.clone())
}

/// FRG-47 stage: the 4-arm pipeline-outcome match of `run_compaction_pass`.
/// Returns a decision; the dispatch owns the loop and the `Ok`/`Err` return.
pub(super) struct PipelineOutcomeInputs<'a> {
    pub session: &'a mut AgentSession,
    pub config: &'a AgentConfig,
    pub capabilities: &'a SessionCapabilities,
    pub context_store: &'a mut ContextStore,
    pub state_ledger: &'a mut StateLedger,
    pub events: &'a mut Vec<AgentEvent>,
    pub pending_summary_req: &'a mut Option<SummaryRequest>,
    pub pending_summary_ctx: &'a mut Option<CompactionContext>,
    pub phase: &'a mut EnginePhase,
    pub turn_index: u32,
    pub trigger: CompactionTrigger,
    pub loop_ctx: CompactionContext,
    pub forced_by_non_ratio: bool,
    pub entered_for_long_loop_only: bool,
    pub post_pressure: Option<Pressure>,
    pub sync_applied: &'a mut bool,
    pub last_history_ref: &'a mut Option<String>,
    pub destructive_tiers: &'a mut Vec<CompactionTier>,
    pub compaction: &'a mut CompactionState,
    pub defer_compaction_failure: bool,
    pub message_count_before: u32,
}

pub(super) enum PipelineDecision {
    ReturnOk(Option<Box<AgentAction>>),
    ReturnErr(AgentError),
    LoopAgain,
}

pub(super) fn decide_pipeline_outcome(
    inputs: PipelineOutcomeInputs<'_>,
    outcome: CompactionOutcome,
) -> PipelineDecision {
    let PipelineOutcomeInputs {
        session,
        config,
        capabilities,
        context_store,
        state_ledger,
        events,
        pending_summary_req,
        pending_summary_ctx,
        phase,
        turn_index,
        trigger,
        loop_ctx,
        forced_by_non_ratio,
        entered_for_long_loop_only,
        post_pressure,
        sync_applied,
        last_history_ref,
        destructive_tiers,
        compaction,
        defer_compaction_failure,
        message_count_before,
    } = inputs;
    match outcome {
        CompactionOutcome::NeedsHostCall(mut req, sess) => {
            // If we forced MidTurn context for non-ratio compaction,
            // restore the original trigger in the SummaryRequest.
            if forced_by_non_ratio {
                req.trigger = trigger;
            }
            // Shared staging: session replace + history/ledger refs.
            let req = attach_summary_staging(
                StageSummaryInputs {
                    session,
                    context_store,
                    state_ledger,
                    enable_context_store: config.enable_context_store,
                    enable_state_ledger: config.enable_state_ledger,
                    turn_index,
                    trigger,
                },
                sess,
                req,
            );
            *pending_summary_req = Some(req.clone());
            *pending_summary_ctx = Some(loop_ctx);
            *phase = EnginePhase::AwaitingSummary;
            PipelineDecision::ReturnOk(Some(Box::new(AgentAction::RequestSummary(req))))
        }
        CompactionOutcome::Applied(sess, ev) => {
            let destructive = is_destructive_compaction_tier(&ev.tier);
            if destructive {
                destructive_tiers.push(ev.tier.clone());
            }
            compaction.record_applied(destructive);
            *session = sess;
            events.push(AgentEvent::CompactionApplied(ev.clone()));
            *sync_applied = true;
            if capabilities.kv_fixed {
                events.push(AgentEvent::KvCacheInvalidated {
                    reason: format!("Compacted: {}", ev.tier),
                });
            }
            // loop until under threshold or NeedsHostCall / stuck
            PipelineDecision::LoopAgain
        }
        CompactionOutcome::NotApplicable => {
            let post_pressure = post_pressure.expect("post pressure for NotApplicable");
            if exceeds_hard_prompt_ceiling(&post_pressure)
                && !entered_for_long_loop_only
                && !*sync_applied
                && !defer_compaction_failure
            {
                return PipelineDecision::ReturnErr(AgentError::CompactionFailed {
                    tier: "pipeline".into(),
                    reason: "No compaction tier applied while context remains over threshold"
                        .into(),
                });
            }
            if *sync_applied {
                // Inline `emit_post_compact(trigger, None, history_ref, tiers, count)`.
                if !destructive_tiers.is_empty() {
                    let boundary = record_destructive_compaction_boundary(
                        session,
                        trigger,
                        destructive_tiers.clone(),
                        message_count_before,
                    );
                    events.push(AgentEvent::CompactionBoundary(boundary));
                }
                events.push(AgentEvent::PostCompact {
                    trigger,
                    summary_ref: None,
                    history_ref: last_history_ref.clone(),
                    transcript_revision_after: session.transcript_revision,
                });
            }
            PipelineDecision::ReturnOk(None)
        }
        CompactionOutcome::Failed(err) => PipelineDecision::ReturnErr(err),
    }
}
