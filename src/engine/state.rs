//! Mutable per-session engine state.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::action::AgentAction;
use crate::compact::CompactionContext;
use crate::context_store::ContextStore;
use crate::events::AgentEvent;
use crate::guardrails::GuardrailState;
use crate::loop_metrics::LoopMetrics;
use crate::message::AgentMessage;
use crate::message::ToolCallRecord;
use crate::session::{AgentConfig, AgentSession, SessionCapabilities};
use crate::state_ledger::StateLedger;
use crate::turn_io::{ModelRequest, SummaryRequest};

/// Provider/runtime parameters needed for pressure + compaction + model requests.
#[derive(Debug, Clone)]
pub struct SessionIoParams {
    pub provider: String,
    pub context_window_tokens: u32,
    pub tools_schema_json: String,
    pub system_prompt: String,
}

#[derive(Clone)]
pub struct SessionHandle {
    pub(super) inner: Arc<Mutex<InnerSession>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EnginePhase {
    Idle,
    AwaitingModel,
    AwaitingTools,
    AwaitingSummary,
}

pub(super) struct InnerSession {
    pub(super) session: AgentSession,
    pub(super) config: AgentConfig,
    pub(super) capabilities: SessionCapabilities,
    pub(super) io: SessionIoParams,
    pub(super) guardrails: GuardrailState,
    pub(super) loop_metrics: LoopMetrics,
    pub(super) phase: EnginePhase,
    pub(super) pending_call_model: Option<ModelRequest>,
    pub(super) pending_execute: Option<Vec<ToolCallRecord>>,
    pub(super) pending_execute_skip_guardrail_count: bool,
    pub(super) pending_auto_finalize_finish: bool,
    pub(super) pending_finalize_after_tool_stop: bool,
    pub(super) pending_summary_req: Option<SummaryRequest>,
    pub(super) pending_summary_ctx: Option<CompactionContext>,
    pub(super) context_store: ContextStore,
    pub(super) state_ledger: StateLedger,
    pub(super) pending_tool_turn_parallel: usize,
    pub(super) pending_reject_count_this_turn: usize,
    pub(super) model_requests_this_turn: u32,
    pub(super) events: Vec<AgentEvent>,
    pub(super) turn_index: u32,
    pub(super) cancelled: bool,
    pub(super) terminal_action: Option<AgentAction>,
    pub(super) last_model_response_had_append_blocks: bool,
    pub(super) pending_artifact_ids: HashSet<String>,
}

/// Integration-test snapshot (TOOL-PARSE-002).
#[derive(Debug, Clone)]
pub struct SessionTestSnapshot {
    pub messages: Vec<AgentMessage>,
    pub consecutive_envelope_parse_failures: u32,
    pub terminal_action: Option<AgentAction>,
}

impl SessionHandle {
    pub(super) fn lock_inner(&self) -> MutexGuard<'_, InnerSession> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Exposes session state for `tests/engine_envelope_retry_test.rs`.
    pub fn integration_test_snapshot(&self) -> SessionTestSnapshot {
        let inner = self.lock_inner();
        SessionTestSnapshot {
            messages: inner.session.messages.clone(),
            consecutive_envelope_parse_failures: inner
                .guardrails
                .consecutive_envelope_parse_failures,
            terminal_action: inner.terminal_action.clone(),
        }
    }
}
