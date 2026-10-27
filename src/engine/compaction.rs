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
use crate::context_store::ContextStore;
use crate::events::{CompactionEvent, CompactionTier, CompactionTrigger};
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
