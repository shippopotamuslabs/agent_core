//! Compaction orchestration (ENC-286 slice 6; FRG-47 staging; ENC-306).
//!
//! Final idiom (ENC-306): compaction's persistent per-turn state lives in
//! [`CompactionState`], owned by `InnerSession` as `self.compaction`;
//! per-pass working state lives in [`CompactionPassState`], owned by
//! `run_compaction_pass` and lent to the operations that need it. The
//! operations are `InnerSession` methods defined in this module (the
//! module that owns the state) and apply their effects directly: the
//! fused sync spill/snapshot pass ([`InnerSession::sync_spill_pass`]),
//! the pipeline-outcome decision
//! ([`InnerSession::decide_pipeline_outcome`]), and the shared summary
//! staging ([`InnerSession::attach_summary_staging`]). The field-bundle
//! idiom is gone from this module: no input/effect bags remain —
//! persistent and per-pass state live in owned state types with private
//! fields and behavior methods ([`CompactionState`],
//! [`CompactionPassState`]), alongside plain call-shape values
//! ([`PipelinePassContext`]).
//!
//! `run_compaction_pass` keeps the loop. The pure entry gate lives in
//! `compact::policy` (`should_enter_compaction`).

use super::is_destructive_compaction_tier;
use super::state::{EnginePhase, InnerSession};
use crate::action::AgentAction;
use crate::budget::{effective_tool_spill_threshold_bytes, exceeds_hard_prompt_ceiling};
use crate::compact::history_snapshot::snapshot_history_range;
use crate::compact::tool_spill::spill_tool_messages_in_session;
use crate::compact::{CompactionContext, CompactionOutcome};
use crate::events::{AgentError, AgentEvent, CompactionEvent, CompactionTier, CompactionTrigger};
use crate::manual_compact::ManualCompactRequest;
use crate::message::AgentMessage;
use crate::session::AgentSession;
use crate::session::Pressure;
use crate::tool_semantics::ArtifactSquashSemantics;
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
/// `&mut` to the pass operations. Like [`CompactionState`], the fields
/// stay private and every reader/writer goes through these methods.
pub(super) struct CompactionPassState {
    sync_applied: bool,
    last_history_ref: Option<String>,
    destructive_tiers: Vec<CompactionTier>,
    message_count_before: u32,
}

impl CompactionPassState {
    pub(super) fn new(message_count_before: u32) -> Self {
        Self {
            sync_applied: false,
            last_history_ref: None,
            destructive_tiers: Vec::new(),
            message_count_before,
        }
    }

    /// Record that a sync (spill/snapshot or pipeline-applied) compaction
    /// took effect during this pass.
    pub(super) fn mark_sync_applied(&mut self) {
        self.sync_applied = true;
    }

    pub(super) fn sync_applied(&self) -> bool {
        self.sync_applied
    }

    /// Remember the history-snapshot ref produced by the sync spill pass.
    pub(super) fn remember_history_ref(&mut self, history_ref: String) {
        self.last_history_ref = Some(history_ref);
    }

    pub(super) fn last_history_ref(&self) -> Option<&String> {
        self.last_history_ref.as_ref()
    }

    /// Take the remembered history ref, leaving `None` behind (the pass
    /// loop's early-exit branch hands it to `emit_post_compact`).
    pub(super) fn take_history_ref(&mut self) -> Option<String> {
        self.last_history_ref.take()
    }

    /// Record one destructive tier applied by the pipeline during this
    /// pass, in application order.
    pub(super) fn record_tier(&mut self, tier: CompactionTier) {
        self.destructive_tiers.push(tier);
    }

    pub(super) fn destructive_tiers(&self) -> &[CompactionTier] {
        &self.destructive_tiers
    }

    pub(super) fn message_count_before(&self) -> u32 {
        self.message_count_before
    }
}

impl InnerSession {
    /// Fused sync spill/snapshot pass (ENC-306 D2): runs the spill and the
    /// history snapshot against `self` and applies the effects directly,
    /// in the exact order the old collect/apply pair produced them — the
    /// spill and snapshot first run (spill, snapshot ids, snapshot), then
    /// the spill event is pushed/emitted and the pass marked sync-applied, then,
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
            pass.mark_sync_applied();
        }
        if let Some(reference) = snapshot_reference {
            let history_ref = reference.id.clone();
            pass.remember_history_ref(history_ref.clone());
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

    /// Shared `NeedsHostCall` staging for `run_compaction_pass` and
    /// `request_manual_compact` (ENC-306 D2): replaces the session with
    /// the staged one, snapshots the summary messages into the context
    /// store (attaching the history ref to the request and the state
    /// ledger), and attaches a state-ledger snapshot to the request.
    ///
    /// Returns the staged request; the caller owns the pending fields,
    /// the phase write, and the `RequestSummary` action.
    pub(super) fn attach_summary_staging(
        &mut self,
        sess: AgentSession,
        mut req: SummaryRequest,
        trigger: CompactionTrigger,
    ) -> SummaryRequest {
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
        req
    }

    /// FRG-47 stage (ENC-306 D2/D4): the 4-arm pipeline-outcome match of
    /// `run_compaction_pass`, applying every effect directly — session
    /// replace, event push + emit, counters via
    /// [`CompactionState::record_applied`], pending summary writes, the
    /// `AwaitingSummary` phase write, and the `NotApplicable` arm's
    /// boundary recording. Returns [`Flow::Loop`] to run another pipeline
    /// iteration (the caller carries `pass` across iterations) or
    /// [`Flow::Done`] with the pass's final action.
    pub(super) fn decide_pipeline_outcome(
        &mut self,
        outcome: CompactionOutcome,
        pass: &mut CompactionPassState,
        pass_ctx: PipelinePassContext,
    ) -> Result<Flow, AgentError> {
        match outcome {
            CompactionOutcome::NeedsHostCall(mut req, sess) => {
                // If we forced MidTurn context for non-ratio compaction,
                // restore the original trigger in the SummaryRequest.
                if pass_ctx.forced_by_non_ratio {
                    req.trigger = pass_ctx.loop_ctx.trigger;
                }
                // Shared staging: session replace + history/ledger refs.
                let req = self.attach_summary_staging(sess, req, pass_ctx.loop_ctx.trigger);
                self.pending.summary_req = Some(req.clone());
                self.pending.summary_ctx = Some(pass_ctx.loop_ctx);
                self.phase = EnginePhase::AwaitingSummary;
                Ok(Flow::Done(Some(Box::new(AgentAction::RequestSummary(req)))))
            }
            CompactionOutcome::Applied(sess, ev) => {
                let destructive = is_destructive_compaction_tier(&ev.tier);
                if destructive {
                    pass.record_tier(ev.tier.clone());
                }
                self.compaction.record_applied(destructive);
                self.session = sess;
                self.emit(AgentEvent::CompactionApplied(ev.clone()));
                pass.mark_sync_applied();
                if self.capabilities.kv_fixed {
                    self.emit(AgentEvent::KvCacheInvalidated {
                        reason: format!("Compacted: {}", ev.tier),
                    });
                }
                // loop until under threshold or NeedsHostCall / stuck
                Ok(Flow::Loop)
            }
            CompactionOutcome::NotApplicable => {
                let post_pressure = pass_ctx
                    .post_pressure
                    .expect("post pressure for NotApplicable");
                if exceeds_hard_prompt_ceiling(&post_pressure)
                    && !pass_ctx.entered_for_long_loop_only
                    && !pass.sync_applied()
                    && !pass_ctx.defer_compaction_failure
                {
                    return Err(AgentError::CompactionFailed {
                        tier: "pipeline".into(),
                        reason: "No compaction tier applied while context remains over threshold"
                            .into(),
                    });
                }
                if pass.sync_applied() {
                    self.emit_post_compact(
                        pass_ctx.loop_ctx.trigger,
                        None,
                        pass.last_history_ref().cloned(),
                        pass.destructive_tiers(),
                        pass.message_count_before(),
                    );
                }
                Ok(Flow::Done(None))
            }
            CompactionOutcome::Failed(err) => Err(err),
        }
    }
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

/// Loop-control signal returned by
/// [`InnerSession::decide_pipeline_outcome`] (ENC-306 D4): the 4-arm
/// `CompactionOutcome` match exists exactly once, inside the method; this
/// 2-variant signal is all the pass loop in `run_compaction_pass` needs
/// to continue or finish. It carries no re-encoded outcome data — every
/// effect has already been applied by the method. The finished action is
/// boxed: `AgentAction` is large, and the pre-ENC-306 `PipelineDecision`
/// boxed it for the same reason (clippy::large_enum_variant).
pub(super) enum Flow {
    Loop,
    Done(Option<Box<AgentAction>>),
}

/// Call-shape data for one pipeline iteration (ENC-306 D4): plain owned
/// values computed by `run_compaction_pass`'s loop for this iteration —
/// the loop's `CompactionContext` (which carries the trigger), the
/// entry-gate flags, the failure-deferral flag, and the post pressure
/// read only for the `NotApplicable` arm. No borrows and no
/// `InnerSession` mirrors: like `FireConditionInputs`, this is per-call
/// data, not state.
pub(super) struct PipelinePassContext {
    pub loop_ctx: CompactionContext,
    pub forced_by_non_ratio: bool,
    pub entered_for_long_loop_only: bool,
    pub post_pressure: Option<Pressure>,
    pub defer_compaction_failure: bool,
}
