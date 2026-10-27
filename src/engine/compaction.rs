//! Compaction orchestration helpers (ENC-286 slice 6; FRG-47 staging).
//!
//! `run_compaction_pass` keeps the loop, the outcome match, and every phase
//! write. The pure entry gate lives in `compact::policy` (`should_enter_
//! compaction`); the two helpers here own the sync spill/snapshot block and
//! the shared `NeedsHostCall` staging. Everything takes disjoint field bundles
//! — no `&mut InnerSession`, no Mutex locking, no phase writes (phase effects
//! are returned, never applied).

use crate::compact::history_snapshot::snapshot_history_range;
use crate::compact::tool_spill::spill_tool_messages_in_session;
use super::is_destructive_compaction_tier;
use super::state::EnginePhase;
use crate::action::AgentAction;
use crate::budget::exceeds_hard_prompt_ceiling;
use crate::compact::{CompactionContext, CompactionOutcome};
use crate::context_store::ContextStore;
use crate::events::{
    AgentError, AgentEvent, CompactionEvent, CompactionTier, CompactionTrigger,
};
use crate::session::{AgentConfig, Pressure, SessionCapabilities};
use crate::transcript_revision::record_destructive_compaction_boundary;
use crate::manual_compact::ManualCompactRequest;
use crate::message::AgentMessage;
use crate::session::AgentSession;
use crate::state_ledger::StateLedger;
use crate::tool_semantics::ArtifactSquashSemantics;
use crate::turn_io::SummaryRequest;

/// Inputs for the sync spill/snapshot block.
pub(super) struct SyncSpillInputs<'a> {
    pub session: &'a mut AgentSession,
    pub context_store: &'a mut ContextStore,
    pub artifact_squash: Option<&'a ArtifactSquashSemantics>,
    pub spill_threshold_bytes: usize,
    pub turn_index: u32,
    pub pressure_total_tokens: u32,
    pub trigger: CompactionTrigger,
}

/// Effect of the sync spill/snapshot block. The dispatch applies flags, ledger
/// refs, session-log pushes, and emits in the original order.
pub(super) struct SyncSpillEffect {
    pub spill_applied: bool,
    pub spill_event: Option<CompactionEvent>,
    pub snapshot_ids: Vec<String>,
    pub history_ref: Option<String>,
    pub snapshot_event: Option<CompactionEvent>,
}

pub(super) fn collect_sync_spill_effect(inputs: SyncSpillInputs<'_>) -> SyncSpillEffect {
    let SyncSpillInputs {
        session,
        context_store,
        artifact_squash,
        spill_threshold_bytes,
        turn_index,
        pressure_total_tokens,
        trigger,
    } = inputs;
    let mut effect = SyncSpillEffect {
        spill_applied: false,
        spill_event: None,
        snapshot_ids: Vec::new(),
        history_ref: None,
        snapshot_event: None,
    };
    if let Some(event) = spill_tool_messages_in_session(
        context_store,
        session,
        artifact_squash,
        spill_threshold_bytes,
        turn_index,
        trigger,
    ) {
        effect.spill_applied = true;
        effect.spill_event = Some(event);
    }
    effect.snapshot_ids = session
        .messages
        .iter()
        .map(|message| message.id.clone())
        .collect();
    if let Some(reference) =
        snapshot_history_range(context_store, &session.messages, trigger, turn_index)
    {
        effect.history_ref = Some(reference.id.clone());
        effect.snapshot_event = Some(CompactionEvent {
            tier: CompactionTier::HistorySnapshot,
            tokens_before: pressure_total_tokens,
            tokens_after: pressure_total_tokens,
            trigger,
        });
    }
    effect
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
    pub destructive_tiers_this_turn: &'a mut u32,
    pub compaction_passes_this_turn: &'a mut u32,
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
        destructive_tiers_this_turn,
        compaction_passes_this_turn,
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
            if is_destructive_compaction_tier(&ev.tier) {
                destructive_tiers.push(ev.tier.clone());
                *destructive_tiers_this_turn += 1;
            }
            *compaction_passes_this_turn += 1;
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

