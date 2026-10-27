//! Finish / lifecycle handling (ENC-286 slice 3, depends on slice 2).
//!
//! `finish_after_model_stop`, `before_finish_pending_work_action`,
//! `set_terminal_or_pending`, and the `cancel` surface all live here now.
//! Like `model_request`, the `InnerSession`-level helpers run on an explicit
//! disjoint-field bundle (`FinishState`): no `&mut InnerSession`, no Mutex
//! locking, no direct phase writes outside the owned transition. `cancel`
//! stays an `AgentEngine` surface method (it must acquire the guard, like
//! `next_action` / `submit_*`).

use super::model_request::{build_model_request, ModelRequestInputs};
use super::state::{EnginePhase, PendingToolTurn, SessionHandle, SessionIoParams};
use super::AgentEngine;
use crate::action::AgentAction;
use crate::artifact_finalize::synthetic_finalize_calls_with_semantics;
use crate::context_store::ContextStore;
use crate::events::{AgentError, AgentEvent};
use crate::guardrails::GuardrailState;
use crate::loop_metrics::LoopMetrics;
use crate::message::{AgentMessage, AgentRole, MessageMeta};
use crate::session::{AgentConfig, AgentSession, SessionCapabilities};
use crate::sidecar_builder::SidecarBuilder;
use crate::state_ledger::StateLedger;
use crate::turn_io::FinishReason;

/// Disjoint-field view of `InnerSession` needed by the finish path.
pub(super) struct FinishState<'a> {
    pub session: &'a mut AgentSession,
    pub config: &'a AgentConfig,
    pub capabilities: &'a SessionCapabilities,
    pub io: &'a SessionIoParams,
    pub guardrails: &'a mut GuardrailState,
    pub loop_metrics: &'a LoopMetrics,
    pub phase: &'a mut EnginePhase,
    pub terminal_action: &'a mut Option<AgentAction>,
    pub pending: &'a mut PendingToolTurn,
    pub last_model_response_greedy_tool_names: &'a [String],
    pub state_ledger: &'a mut StateLedger,
    pub context_store: &'a mut ContextStore,
    pub sidecar_builder: &'a mut SidecarBuilder,
    pub active_tool_names: &'a [String],
    pub turn_index: u32,
    pub model_requests_this_turn: &'a mut u32,
    pub compaction_passes_this_turn: u32,
    pub destructive_tiers_this_turn: u32,
    pub suppress_summary_tier: bool,
    pub events: &'a mut Vec<AgentEvent>,
}

fn set_terminal(
    phase: &mut EnginePhase,
    terminal_action: &mut Option<AgentAction>,
    action: AgentAction,
) {
    *terminal_action = Some(action);
    *phase = EnginePhase::Idle;
}

pub(super) fn finish_after_model_stop(mut state: FinishState<'_>, reason: FinishReason) {
    match reason {
        FinishReason::Stop => {}
        FinishReason::MaxTokens => {
            set_terminal(
                state.phase,
                state.terminal_action,
                AgentAction::Failed(AgentError::ModelStopped {
                    reason: "max_tokens".into(),
                }),
            );
            return;
        }
        FinishReason::Other(reason) => {
            set_terminal(
                state.phase,
                state.terminal_action,
                AgentAction::Failed(AgentError::ModelStopped { reason }),
            );
            return;
        }
        FinishReason::ToolUse => {
            set_terminal(
                state.phase,
                state.terminal_action,
                AgentAction::Failed(AgentError::ModelStopped {
                    reason: "tool_use_without_tool_calls".into(),
                }),
            );
            return;
        }
    }

    let synth = synthetic_finalize_calls_with_semantics(
        &state.pending.artifact_ids,
        &state.config.tool_semantics,
    );
    if !synth.is_empty() {
        state.pending.execute = Some(synth);
        state.pending.execute_skip_guardrail_count = true;
        state.pending.auto_finalize_finish = true;
        *state.phase = EnginePhase::AwaitingTools;
        return;
    }

    if state.config.enable_loop_hooks && state.capabilities.lifecycle_hooks_supported {
        state.events.push(AgentEvent::HookInvoked {
            checkpoint: "before_finish".into(),
        });
    }

    if state.config.enable_loop_hooks
        && state.capabilities.lifecycle_hooks_supported
        && state.config.enable_pending_work_guard
    {
        // The pending-work check borrows the same disjoint fields through the
        // bundle and hands it back, so dispatch keeps owning the outcome.
        let (action, returned) = before_finish_pending_work_action(state);
        state = returned;
        if let Some(action) = action {
            // set_terminal_or_pending: only non-CallModel actions terminate.
            match action {
                AgentAction::CallModel(_) => {}
                other => set_terminal(state.phase, state.terminal_action, other),
            }
            return;
        }
    }

    *state.terminal_action = Some(AgentAction::Finished);
    let tc = state.guardrails.tool_calls_executed;
    state.events.push(AgentEvent::TurnCompleted {
        turn_index: state.turn_index,
        tool_call_count: tc,
        terminal: true,
    });
}

/// Pending-work gate before a terminal finish.
///
/// Takes the finish bundle and hands it back with the decision, so the caller
/// retains ownership of every field (no phase write happens in here except the
/// `AwaitingModel` continuation, matching the pre-extraction behavior).
pub(super) fn before_finish_pending_work_action(
    state: FinishState<'_>,
) -> (Option<AgentAction>, FinishState<'_>) {
    let pending_items = state
        .loop_metrics
        .pending_work_items
        .iter()
        .map(|item| item.text.clone())
        .collect::<Vec<_>>();
    let force_blocked = state.session.session_context.force_finish_blocked();
    if pending_items.is_empty() {
        let action = force_blocked.then(|| {
            AgentAction::Failed(AgentError::GuardrailExceeded {
                kind: "finish_blocked_without_injected_message".into(),
                detail: None,
            })
        });
        return (action, state);
    }
    if *state.model_requests_this_turn >= state.config.max_model_requests_per_turn {
        return (
            Some(AgentAction::Failed(AgentError::GuardrailExceeded {
                kind: "max_model_requests_per_turn".into(),
                detail: None,
            })),
            state,
        );
    }

    let turn_index = state.turn_index;
    let injected_message = AgentMessage {
        id: format!("pending-work-reminder-{turn_index}"),
        role: AgentRole::User,
        content: format!("Pending work remains: {}", pending_items.join(", ")),
        metadata: MessageMeta::new(),
    };
    state.session.messages.push(injected_message.clone());
    if state.config.enable_state_ledger {
        state.state_ledger.observe_user_message(&injected_message);
    }
    let allow = state
        .guardrails
        .allow_tool_call(state.config, state.last_model_response_greedy_tool_names);
    let request = build_model_request(ModelRequestInputs {
        session: state.session,
        config: state.config,
        capabilities: state.capabilities,
        io: state.io,
        active_tool_names: state.active_tool_names,
        context_store: state.context_store,
        state_ledger: state.state_ledger,
        sidecar_builder: state.sidecar_builder,
        turn_index,
        compaction_passes_this_turn: state.compaction_passes_this_turn,
        destructive_tiers_this_turn: state.destructive_tiers_this_turn,
        suppress_summary_tier: state.suppress_summary_tier,
        allow_tool_call: allow,
        cache_hints: vec![],
        loop_metrics: state.loop_metrics,
    });
    state.pending.call_model = Some(request.clone());
    *state.model_requests_this_turn += 1;
    *state.phase = EnginePhase::AwaitingModel;
    (Some(AgentAction::CallModel(request)), state)
}

impl AgentEngine {
    pub fn cancel(&self, handle: &SessionHandle) {
        let mut g = handle.lock_inner();
        g.cancelled = true;
        g.set_terminal(AgentAction::Failed(AgentError::Internal(
            "cancelled".into(),
        )));
    }
}
