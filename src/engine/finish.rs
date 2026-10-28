//! Finish / lifecycle handling (ENC-286 slice 3, depends on slice 2; ENC-306).
//!
//! Finish owns NO persistent fields. The state the finish operations work
//! on — `phase`, `pending`, `terminal_action` — is turn-coordination state
//! shared with dispatch in `mod.rs`, not finish-owned state, so there is
//! no finish bundle struct at all (ENC-306 D5). The module's model is the
//! transition operation set itself: `finish_after_model_stop` and
//! `before_finish_pending_work_action` are `InnerSession` methods defined
//! here, in the module that owns them, and apply their effects directly on
//! `self`. `cancel` stays an `AgentEngine` surface method (it must acquire
//! the guard, like `next_action` / `submit_*`).

use super::model_request::{build_model_request, ModelRequestInputs};
use super::state::{EnginePhase, InnerSession, SessionHandle};
use super::AgentEngine;
use crate::action::AgentAction;
use crate::artifact_finalize::synthetic_finalize_calls_with_semantics;
use crate::events::{AgentError, AgentEvent};
use crate::message::{AgentMessage, AgentRole, MessageMeta};
use crate::turn_io::FinishReason;

impl InnerSession {
    pub(super) fn finish_after_model_stop(&mut self, reason: FinishReason) {
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

        let synth = synthetic_finalize_calls_with_semantics(
            &self.pending.artifact_ids,
            &self.config.tool_semantics,
        );
        if !synth.is_empty() {
            self.pending.execute = Some(synth);
            self.pending.execute_skip_guardrail_count = true;
            self.pending.auto_finalize_finish = true;
            self.phase = EnginePhase::AwaitingTools;
            return;
        }

        if self.config.enable_loop_hooks && self.capabilities.lifecycle_hooks_supported {
            self.events.push(AgentEvent::HookInvoked {
                checkpoint: "before_finish".into(),
            });
        }

        if self.config.enable_loop_hooks
            && self.capabilities.lifecycle_hooks_supported
            && self.config.enable_pending_work_guard
        {
            if let Some(action) = self.before_finish_pending_work_action() {
                // set_terminal_or_pending: only non-CallModel actions terminate.
                match action {
                    AgentAction::CallModel(_) => {}
                    other => self.set_terminal(other),
                }
                return;
            }
        }

        self.terminal_action = Some(AgentAction::Finished);
        let tc = self.guardrails.tool_calls_executed;
        self.events.push(AgentEvent::TurnCompleted {
            turn_index: self.turn_index,
            tool_call_count: tc,
            terminal: true,
        });
    }

    /// Pending-work gate before a terminal finish.
    ///
    /// Returns the decision action, if any (no phase write happens in here
    /// except the `AwaitingModel` continuation, matching the
    /// pre-extraction behavior).
    pub(super) fn before_finish_pending_work_action(&mut self) -> Option<AgentAction> {
        let pending_items = self
            .loop_metrics
            .pending_work_items
            .iter()
            .map(|item| item.text.clone())
            .collect::<Vec<_>>();
        let force_blocked = self.session.session_context.force_finish_blocked;
        if pending_items.is_empty() {
            return force_blocked.then(|| {
                AgentAction::Failed(AgentError::GuardrailExceeded {
                    kind: "finish_blocked_without_injected_message".into(),
                    detail: None,
                })
            });
        }
        if self.model_requests_this_turn >= self.config.max_model_requests_per_turn {
            return Some(AgentAction::Failed(AgentError::GuardrailExceeded {
                kind: "max_model_requests_per_turn".into(),
                detail: None,
            }));
        }

        let turn_index = self.turn_index;
        let injected_message = AgentMessage {
            id: format!("pending-work-reminder-{turn_index}"),
            role: AgentRole::User,
            content: format!("Pending work remains: {}", pending_items.join(", ")),
            metadata: MessageMeta::new(),
        };
        self.session.messages.push(injected_message.clone());
        if self.config.enable_state_ledger {
            self.state_ledger.observe_user_message(&injected_message);
        }
        let allow = self
            .guardrails
            .allow_tool_call(&self.config, &self.last_model_response_greedy_tool_names);
        let request = build_model_request(ModelRequestInputs {
            session: &self.session,
            config: &self.config,
            capabilities: &self.capabilities,
            io: &self.io,
            active_tool_names: &self.active_tool_names,
            context_store: &mut self.context_store,
            state_ledger: &self.state_ledger,
            sidecar_builder: &mut self.sidecar_builder,
            turn_index,
            compaction_passes_this_turn: self.compaction.passes_this_turn(),
            destructive_tiers_this_turn: self.compaction.destructive_tiers_this_turn(),
            suppress_summary_tier: self.compaction.suppress_summary_tier(),
            allow_tool_call: allow,
            cache_hints: vec![],
            loop_metrics: &self.loop_metrics,
        });
        self.pending.call_model = Some(request.clone());
        self.model_requests_this_turn += 1;
        self.phase = EnginePhase::AwaitingModel;
        Some(AgentAction::CallModel(request))
    }
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
