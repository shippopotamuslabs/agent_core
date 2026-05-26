//! Mutable per-session engine state.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::action::AgentAction;
use crate::compact::CompactionContext;
use crate::context_store::ContextStore;
use crate::events::AgentEvent;
use crate::guardrails::GuardrailState;
use crate::loop_metrics::LoopMetrics;
#[cfg(feature = "lalr-harness")]
use crate::message::AgentRole;
use crate::message::{AgentMessage, ToolCallRecord};
use crate::session::{AgentConfig, AgentSession, SessionCapabilities};
use crate::sidecar_builder::SidecarBuilder;
use crate::state_ledger::StateLedger;
use crate::turn_io::{ModelRequest, RawToolCall, SummaryRequest};

/// Provider/runtime parameters needed for pressure + compaction + model requests.
#[derive(Debug, Clone)]
pub struct SessionIoParams {
    pub provider: String,
    pub context_window_tokens: u32,
    pub tool_catalog_json: String,
    pub tool_schema_registry_json: String,
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

pub(crate) struct InnerSession {
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
    pub(super) tools_blocked_nudges_this_turn: u32,
    /// Model-requested tool calls for the in-flight turn, in provider order.
    pub(super) pending_tool_turn_request_order: Vec<RawToolCall>,
    /// Reject/suggest tool payloads keyed by `call_id`, flushed after successful results.
    pub(super) pending_reject_tool_by_call_id: HashMap<String, AgentMessage>,
    /// Active tool names for the current session. Updated on disclosure activations.
    /// Initially populated from the session's initial `tools_schema_json`.
    pub(super) active_tool_names: Vec<String>,
    pub(super) sidecar_builder: SidecarBuilder,
    /// Telemetry: number of compaction passes run this user turn (reset on turn start).
    pub(super) compaction_passes_this_turn: u32,
    /// Telemetry: number of destructive compaction tiers (Micro/Fifo/Summary) applied
    /// this user turn (reset on turn start).
    pub(super) destructive_tiers_this_turn: u32,
}

/// Integration-test snapshot (TOOL-PARSE-002).
#[derive(Debug, Clone)]
pub struct SessionTestSnapshot {
    pub messages: Vec<AgentMessage>,
    pub consecutive_envelope_parse_failures: u32,
    pub terminal_action: Option<AgentAction>,
}

impl SessionHandle {
    pub(crate) fn lock_inner(&self) -> MutexGuard<'_, InnerSession> {
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

#[cfg(feature = "lalr-harness")]
impl SessionHandle {
    /// Seeds guardrail counters for integration tests (`tool_budget_agent_meta_test`).
    pub(crate) fn lalr_seed_user_message_if_empty(&self, content: &str) {
        let mut inner = self.lock_inner();
        if inner.session.messages.is_empty() {
            inner.session.messages.push(AgentMessage {
                id: "u0".into(),
                role: AgentRole::User,
                content: content.into(),
                metadata: serde_json::Value::Null,
            });
        }
    }

    pub(crate) fn lalr_prepare_long_loop_run(&self, min_tool_turns: u32) {
        let mut inner = self.lock_inner();
        inner
            .state_ledger
            .constraints
            .push("PROJECT_CODENAME=ENCLAVE".to_string());
        inner.config.enable_state_ledger = true;
        inner.config.max_turns = inner.config.max_turns.max(min_tool_turns);
        inner.config.long_loop.max_tool_turns =
            inner.config.long_loop.max_tool_turns.max(min_tool_turns);
        inner.config.threshold_ratio = 0.15;
        inner.config.mid_turn_threshold_ratio = 0.20;
        inner.config.post_tool_result_threshold_ratio = 0.20;
        inner.config.allow_fifo_on_preflight = true;
        inner.config.allow_fifo_on_post_tool_result = true;
        inner.io.context_window_tokens = 8_192;
        inner.config.tool_spill_threshold_bytes = 1024;
    }

    pub(crate) fn lalr_long_loop_outcome_snapshot(
        &self,
        semantic_fact: &str,
    ) -> (usize, u64, bool) {
        let inner = self.lock_inner();
        let preserved = inner
            .state_ledger
            .constraints
            .iter()
            .any(|c| c.contains(semantic_fact))
            || inner
                .session
                .messages
                .iter()
                .any(|m| m.content.contains(semantic_fact))
            || serde_json::to_string(&inner.state_ledger)
                .map(|s| s.contains(semantic_fact))
                .unwrap_or(false);
        (
            inner.session.messages.len(),
            inner.session.transcript_revision,
            preserved,
        )
    }

    pub(crate) fn lalr_seed_tool_round_index(&self, tool_round_index: u32) {
        let mut inner = self.lock_inner();
        inner.guardrails.tool_calls_executed = tool_round_index;
    }

    pub(crate) fn lalr_config(&self) -> AgentConfig {
        self.lock_inner().config.clone()
    }

    pub(crate) fn lalr_get_session(&self) -> AgentSession {
        self.lock_inner().session.clone()
    }
}

impl SessionHandle {
    pub fn integration_test_seed_guardrails(
        &self,
        tool_calls_executed: u32,
        consecutive_soft_failure_turns: u32,
        consecutive_envelope_parse_failures: u32,
        last_model_response_had_append_blocks: bool,
    ) {
        let mut inner = self.lock_inner();
        inner.guardrails.tool_calls_executed = tool_calls_executed;
        inner.guardrails.consecutive_soft_failure_turns = consecutive_soft_failure_turns;
        inner.guardrails.consecutive_envelope_parse_failures = consecutive_envelope_parse_failures;
        inner.last_model_response_had_append_blocks = last_model_response_had_append_blocks;
    }
}
