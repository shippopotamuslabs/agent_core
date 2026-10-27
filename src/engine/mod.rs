//! Session state machine (`AgentEngine`) — AGENT-017, AGENT-018 preflight via idle compaction.

pub mod envelope_ingest;
pub mod leaked_tool_recovery;
mod provider_tool_calls;
mod state;
pub use envelope_ingest::{ingest_model_response_envelopes, EnvelopeIngestOutcome};
pub use leaked_tool_recovery::{resolve_model_turn_text, ModelTurnTextResolution};
pub use state::{SessionHandle, SessionIoParams, SessionTestSnapshot};

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::action::AgentAction;
use crate::agent_tool_budget::inject_tool_budget_into_session;
use crate::artifact_finalize::{
    record_artifact_progress_with_semantics, synthetic_finalize_calls_with_semantics,
};
use crate::budget::{
    effective_threshold_tokens, effective_tool_result_budget_for_window,
    effective_tool_spill_threshold_bytes, exceeds_hard_prompt_ceiling,
    should_compact_at_token_threshold,
};
use crate::catalog::ToolCatalog;
use crate::compact::{
    apply_summary_result,
    artifact_snapshot::persist_artifact_snapshot,
    artifact_squash::extract_artifact_anchor,
    compaction_pipeline_for_plan, estimate_session_pressure,
    history_snapshot::snapshot_history_range,
    policy::build_compaction_plan,
    tool_spill::{
        activation_tools_from_retrieval_hint, spill_tool_messages_in_session,
        spill_tool_result_if_needed,
    },
    CompactionContext, CompactionOutcome, CompactionTier as _,
};
use crate::context_store::{
    insert_terminal_output, is_browsable_context_kind, ContextReference, ContextStore,
};
use crate::events::{AgentError, AgentEvent, CompactionEvent, CompactionTier, CompactionTrigger};
use crate::file_context::condense_file_context;
use crate::guardrails::{is_soft_failure_result_json, GuardrailState};
use crate::long_loop_policy::should_compact_for_long_loop;
use crate::loop_metrics::{LoopMetrics, PendingWorkItem};
use crate::manual_compact::ManualCompactResult;
use crate::message::{AgentMessage, AgentRole, MessageMeta, ToolCallRecord, ToolResultRecord};
use crate::policy::resolve_context_policy;
use crate::repair::{RepairOutcome, ToolRepairPipeline};
use crate::request_pipeline::render_runtime_tool_view;
use crate::session::{AgentConfig, AgentSession, SessionCapabilities};
use crate::sidecar_builder::{ProviderContextSnapshot, SidecarBuilder};
use crate::state_ledger::StateLedger;
#[allow(unused_imports)]
use crate::tool_context::build_tool_context_index;
use crate::tool_context::build_tool_context_index_with_registry;
use crate::tool_semantics::{result_policy_for, ArtifactSquashSemantics};
use crate::transcript_revision::record_destructive_compaction_boundary;
use crate::truncate::truncate_tool_result_json;
use crate::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall, SummaryResult};

use state::{EnginePhase, InnerSession};

fn is_destructive_compaction_tier(tier: &CompactionTier) -> bool {
    matches!(
        tier,
        CompactionTier::Micro
            | CompactionTier::Fifo
            | CompactionTier::Summary
            | CompactionTier::TextTruncate
    )
}

fn merge_active_tool_names(
    active_tool_names: &mut Vec<String>,
    names: impl IntoIterator<Item = String>,
) {
    for name in names {
        if !active_tool_names.iter().any(|existing| existing == &name) {
            active_tool_names.push(name);
        }
    }
}

fn is_companion_callable_tool(
    discovery: &Option<crate::tool_semantics::ToolDiscoverySemantics>,
    tool_name: &str,
) -> bool {
    discovery
        .as_ref()
        .map(|d| d.companion_callable_tools.iter().any(|n| n == tool_name))
        .unwrap_or(false)
}

fn activate_tools_for_spill_reference(
    active_tool_names: &mut Vec<String>,
    result: &mut ToolResultRecord,
    reference: &ContextReference,
) {
    let hinted = activation_tools_from_retrieval_hint(&reference.retrieval_hint);
    merge_active_tool_names(active_tool_names, hinted.iter().cloned());
    merge_active_tool_names(&mut result.activated_tool_names, hinted);
}

fn parse_tool_names_from_schema(schema_json: &str) -> Vec<String> {
    let parsed: serde_json::Value = match serde_json::from_str(schema_json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(array) = parsed.as_array() else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|entry| entry.get("name").and_then(|v| v.as_str()).map(String::from))
        .collect()
}

/// Render an active-only schemas-JSON payload by filtering the registry by name.
/// Preserves the order in `active_names`.
fn render_active_schema_json(registry_json: &str, active_names: &[String]) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(registry_json) {
        Ok(v) => v,
        Err(_) => return "[]".to_string(),
    };
    let Some(array) = parsed.as_array() else {
        return "[]".to_string();
    };
    let by_name: std::collections::HashMap<&str, &serde_json::Value> = array
        .iter()
        .filter_map(|entry| {
            entry
                .get("name")
                .and_then(|v| v.as_str())
                .map(|name| (name, entry))
        })
        .collect();
    let mut out = Vec::new();
    for name in active_names {
        if let Some(entry) = by_name.get(name.as_str()) {
            out.push((*entry).clone());
        } else {
            // If the registry doesn't carry this tool, advertise a stub so the active set
            // remains structurally consistent.
            out.push(serde_json::json!({"name": name}));
        }
    }
    serde_json::Value::Array(out).to_string()
}

fn inject_tool_budget_after_batch(
    session: &mut InnerSession,
    last_batch_tool_message_index: Option<usize>,
) {
    let meta = session.guardrails.tool_budget_snapshot(
        &session.config,
        &session.last_model_response_greedy_tool_names,
    );
    inject_tool_budget_into_session(
        &mut session.session,
        session.turn_index,
        &meta,
        last_batch_tool_message_index,
    );
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

    pub fn create_session(
        &self,
        initial_messages: Vec<AgentMessage>,
        mut config: AgentConfig,
        capabilities: SessionCapabilities,
        session_context: serde_json::Value,
        io: SessionIoParams,
    ) -> SessionHandle {
        config
            .apply_policy_tier_defaults(capabilities.context_policy_tier, io.context_window_tokens);
        config.prepare_tool_semantics(&io);
        let mut state_ledger = StateLedger::default();
        if config.enable_state_ledger {
            for message in &initial_messages {
                state_ledger.observe_user_message(message);
                state_ledger.observe_assistant_message(message);
            }
        }

        let initial_msg_count = initial_messages.len() as u32;
        let initial_active_tool_names_source = io.tools_schema_json.clone();
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
                last_model_response_greedy_tool_names: Vec::new(),
                pending_artifact_ids: HashSet::new(),
                tools_blocked_nudges_this_turn: 0,
                pending_tool_turn_request_order: Vec::new(),
                pending_reject_tool_by_call_id: HashMap::new(),
                active_tool_names: parse_tool_names_from_schema(&initial_active_tool_names_source),
                sidecar_builder: SidecarBuilder::new(),
                compaction_passes_this_turn: 0,
                destructive_tiers_this_turn: 0,
                suppress_summary_tier: false,
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
        g.guardrails.reset_model_api_failures();
        let allow_tool_call = g
            .pending_call_model
            .as_ref()
            .map(|request| request.allow_tool_call)
            .unwrap_or(true);
        g.pending_call_model = None;

        let semantics = g.config.tool_semantics.clone();
        let catalog = ToolCatalog::from_semantics(&semantics)
            .unwrap_or_else(|_| ToolCatalog::from_specs(Vec::new()));
        let (ingest_outcome, merged_tool_calls) = ingest_model_response_envelopes(
            response.assistant_text.as_deref(),
            &response.tool_calls,
            allow_tool_call,
            &catalog,
            &semantics,
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
                    metadata: MessageMeta::new().with_extra("envelope_ingest", json!("failed")),
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
                    metadata: MessageMeta::new().with_extra("envelope_parse_failure", json!(true)),
                });
                let cfg = g.config.clone();
                g.guardrails.count_tool_executed(1);
                g.guardrails
                    .on_tool_turn_finished(&cfg, 1, true, false, false, "agent_engine");
                let last_tool_idx = g.session.messages.len().checked_sub(1);
                inject_tool_budget_after_batch(&mut g, last_tool_idx);
                return;
            }
            EnvelopeIngestOutcome::ToolsBlocked {
                nudge_json_for_model,
                visible_assistant_text,
                user_fallback_text,
            } => {
                let assistant_id = format!(
                    "a{}",
                    g.session
                        .messages
                        .iter()
                        .filter(|message| message.role == AgentRole::Assistant)
                        .count()
                );
                let can_nudge = g.tools_blocked_nudges_this_turn == 0
                    && g.model_requests_this_turn < g.config.max_model_requests_per_turn;

                if !can_nudge {
                    g.session.messages.push(AgentMessage {
                        id: assistant_id,
                        role: AgentRole::Assistant,
                        content: user_fallback_text.clone(),
                        metadata: MessageMeta::new()
                            .with_extra("tools_blocked_fallback", json!(true)),
                    });
                    if g.config.enable_state_ledger {
                        if let Some(message) = g.session.messages.last().cloned() {
                            g.state_ledger.observe_assistant_message(&message);
                        }
                    }
                    g.emit(AgentEvent::VisibleAssistantText {
                        text: user_fallback_text.clone(),
                    });
                    // The fallback text is the final user-facing answer for this turn.
                    // The provider finish reason may be ToolUse/Other for the blocked
                    // attempt; finishing with it would fail the loop after a graceful
                    // fallback, so force Stop semantics.
                    g.finish_after_model_stop(FinishReason::Stop);
                    return;
                }

                let assistant_id_clone = assistant_id.clone();
                g.session.messages.push(AgentMessage {
                    id: assistant_id,
                    role: AgentRole::Assistant,
                    content: visible_assistant_text.clone(),
                    metadata: MessageMeta::new()
                        .with_extra("envelope_ingest", json!("tools_blocked")),
                });
                if g.config.enable_state_ledger {
                    if let Some(message) = g.session.messages.last().cloned() {
                        g.state_ledger.observe_assistant_message(&message);
                    }
                }
                g.session.messages.push(AgentMessage {
                    id: format!("tool-nudge-tools-blocked-{assistant_id_clone}"),
                    role: AgentRole::Tool,
                    content: nudge_json_for_model.clone(),
                    metadata: MessageMeta::new().with_extra("tools_blocked_nudge", json!(true)),
                });
                let last_tool_idx = g.session.messages.len().checked_sub(1);
                inject_tool_budget_after_batch(&mut g, last_tool_idx);
                g.tools_blocked_nudges_this_turn += 1;
                g.guardrails.reset_envelope_parse_failures();
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
        let mut metadata = MessageMeta::new();
        if let Some(ingest_obj) = ingest_metadata.as_object() {
            for (k, v) in ingest_obj {
                metadata.set_extra(k.clone(), v.clone());
            }
        }
        if let Some(reasoning_content) = response
            .provider_reasoning_content
            .as_deref()
            .filter(|content| !content.is_empty())
        {
            metadata.set_extra(
                "provider_reasoning",
                json!({ "reasoning_content": reasoning_content }),
            );
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

        g.last_model_response_greedy_tool_names = merged_tool_calls
            .iter()
            .map(|c| normalize_tool_name_hint(&c.name))
            .collect();

        if merged_tool_calls.is_empty() {
            g.finish_after_model_stop(response.finish_reason);
            return;
        }

        let mut repaired: Vec<ToolCallRecord> = Vec::new();
        let mut rejects = 0usize;
        let parallel = merged_tool_calls.len();
        g.pending_tool_turn_request_order = merged_tool_calls.clone();
        g.pending_reject_tool_by_call_id.clear();

        for raw in &merged_tool_calls {
            // Tool-discovery guard: reject any call whose tool name is not part of the
            // currently active set (the discovery tool itself is always callable).
            // Only enforce when active_tool_names has actually been initialized — empty
            // active set means the host has not opted into the new contract yet.
            let discovery = g.config.tool_semantics.discovery.clone();
            let discovery_name = discovery.as_ref().map(|d| d.tool_name.as_str());
            let discovery_always_callable = discovery
                .as_ref()
                .map(|d| d.always_callable && raw.name == d.tool_name)
                .unwrap_or(false);
            let companion_callable = is_companion_callable_tool(&discovery, &raw.name);
            if companion_callable {
                merge_active_tool_names(&mut g.active_tool_names, [raw.name.clone()]);
            }
            let callable_now = !g.active_tool_names.is_empty()
                && (g.active_tool_names.iter().any(|n| n == &raw.name)
                    || discovery_always_callable
                    || companion_callable);
            if !g.active_tool_names.is_empty() && !callable_now {
                let ti = g.turn_index;
                g.emit(AgentEvent::ToolSoftFailure {
                    tool_name: raw.name.clone(),
                    turn_index: ti,
                });
                let mut available = g.active_tool_names.clone();
                if let Some(name) = discovery_name {
                    if !available.iter().any(|n| n == name) {
                        available.insert(0, name.to_string());
                    }
                }
                let error = if let Some(name) = discovery_name {
                    format!("Tool '{}' is not active. Call {name} first.", raw.name)
                } else {
                    format!("Tool '{}' is not active.", raw.name)
                };
                let err = serde_json::json!({
                    "error": error,
                    "available_tools": available,
                })
                .to_string();
                let call_id = raw.id.clone().unwrap_or_else(|| "noid".into());
                g.pending_reject_tool_by_call_id.insert(
                    call_id.clone(),
                    AgentMessage {
                        id: format!("tool-err-{call_id}"),
                        role: AgentRole::Tool,
                        content: err,
                        metadata: MessageMeta::new().with_call_id(call_id),
                    },
                );
                rejects += 1;
                continue;
            }
            let semantics = g.config.tool_semantics.clone();
            let (outcome, evs) = match ToolCatalog::from_semantics(&semantics) {
                Ok(catalog) => ToolRepairPipeline::repair_with_semantics(raw, &catalog, &semantics),
                Err(_) => (
                    RepairOutcome::Reject {
                        error_json_for_model: serde_json::json!({
                            "error": format!("Tool '{}' is not configured.", raw.name),
                        })
                        .to_string(),
                    },
                    Vec::new(),
                ),
            };
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
                    let call_id = raw.id.clone().unwrap_or_else(|| "noid".into());
                    g.pending_reject_tool_by_call_id.insert(
                        call_id.clone(),
                        AgentMessage {
                            id: format!("tool-err-{call_id}"),
                            role: AgentRole::Tool,
                            content: error_json_for_model,
                            metadata: MessageMeta::new().with_call_id(call_id),
                        },
                    );
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
                        "available_tools": ToolCatalog::from_semantics(&g.config.tool_semantics)
                            .map(|catalog| {
                                catalog
                                    .all()
                                    .iter()
                                    .map(|tool| tool.name.clone())
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default(),
                    })
                    .to_string();
                    let call_id = raw.id.clone().unwrap_or_else(|| "noid".into());
                    g.pending_reject_tool_by_call_id.insert(
                        call_id.clone(),
                        AgentMessage {
                            id: format!("tool-err-{call_id}"),
                            role: AgentRole::Tool,
                            content: err,
                            metadata: MessageMeta::new().with_call_id(call_id),
                        },
                    );
                    rejects += 1;
                }
            }
        }

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
        let mut last_tool_idx = None;
        for raw in std::mem::take(&mut g.pending_tool_turn_request_order) {
            let call_id = raw.id.clone().unwrap_or_else(|| "noid".into());
            if let Some(message) = g.pending_reject_tool_by_call_id.remove(&call_id) {
                g.session.messages.push(message);
                last_tool_idx = Some(g.session.messages.len() - 1);
            }
        }
        g.pending_reject_tool_by_call_id.clear();
        inject_tool_budget_after_batch(&mut g, last_tool_idx);
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

        if let Some(request) = g.pending_call_model.as_mut() {
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

        let calls = g.pending_execute.take().expect("tools");
        let skip_count = g.pending_execute_skip_guardrail_count;
        let finalize_after_tool_stop = g.pending_finalize_after_tool_stop;
        let pending_parallel = g.pending_tool_turn_parallel;
        let pending_rejects = g.pending_reject_count_this_turn;
        g.phase = EnginePhase::Idle;
        g.pending_finalize_after_tool_stop = false;

        let turn_order = std::mem::take(&mut g.pending_tool_turn_request_order);
        let mut reject_by_call_id = std::mem::take(&mut g.pending_reject_tool_by_call_id);
        // Merge any host-reported activations into the active set in first-seen order, deduped.
        for res in &results {
            merge_active_tool_names(
                &mut g.active_tool_names,
                res.activated_tool_names.iter().cloned(),
            );
        }

        // Pair results to calls by call_id; fall back to submission order for
        // results the host returned with unknown ids. Calls left without any
        // result get a synthesized error so provider replay still sees one tool
        // message per tool_call_id (OpenAI-compatible APIs 400 otherwise).
        let mut remaining: Vec<Option<ToolResultRecord>> = results.into_iter().map(Some).collect();
        let mut executed_by_call_id: HashMap<String, (ToolCallRecord, ToolResultRecord)> =
            HashMap::with_capacity(calls.len());
        for call in &calls {
            if let Some(slot) = remaining
                .iter_mut()
                .find(|slot| slot.as_ref().is_some_and(|res| res.call_id == call.call_id))
            {
                let res = slot.take().expect("matched result");
                executed_by_call_id.insert(call.call_id.clone(), (call.clone(), res));
            }
        }
        let mut unmatched = remaining.into_iter().flatten();
        for call in &calls {
            if executed_by_call_id.contains_key(&call.call_id) {
                continue;
            }
            let res = unmatched.next().map(|mut res| {
                res.call_id = call.call_id.clone();
                res
            });
            let res = res.unwrap_or_else(|| ToolResultRecord {
                call_id: call.call_id.clone(),
                result_json: json!({
                    "error": "tool_result_missing",
                    "message": format!(
                        "Host returned no result for tool '{}' (call_id {}).",
                        call.name, call.call_id
                    ),
                })
                .to_string(),
                truncated: false,
                soft_failure: true,
                activated_tool_names: Vec::new(),
                result_metadata: None,
            });
            executed_by_call_id.insert(call.call_id.clone(), (call.clone(), res));
        }

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
            let Some((call, mut res)) = executed_by_call_id.remove(&call_id) else {
                continue;
            };
            let spill_tokens_before = g
                .estimate_pressure(CompactionTrigger::PostToolResult)
                .total_tokens;
            let policy = result_policy_for(&g.config.tool_semantics, &call.name);
            let spill_stored_copy_only = policy
                .map(|policy| policy.spill_stored_copy_only)
                .unwrap_or(false);
            let skip_result_truncation = policy
                .map(|policy| policy.skip_result_truncation)
                .unwrap_or(false);
            let spilled = if spill_stored_copy_only {
                let mut stored_copy = res.clone();
                let threshold =
                    effective_tool_spill_threshold_bytes(&g.config, g.io.context_window_tokens);
                let turn_index = g.turn_index;
                spill_tool_result_if_needed(
                    &mut g.context_store,
                    &mut stored_copy,
                    &call,
                    threshold,
                    turn_index,
                )
            } else {
                let threshold =
                    effective_tool_spill_threshold_bytes(&g.config, g.io.context_window_tokens);
                let turn_index = g.turn_index;
                spill_tool_result_if_needed(
                    &mut g.context_store,
                    &mut res,
                    &call,
                    threshold,
                    turn_index,
                )
            };

            if !skip_result_truncation {
                let pressure = g.estimate_pressure(CompactionTrigger::PostToolResult);
                let budget = effective_tool_result_budget_for_window(
                    &pressure,
                    &g.capabilities,
                    &g.config,
                    g.io.context_window_tokens,
                );
                let (payload, truncated) = truncate_tool_result_json(&res.result_json, budget);
                res.result_json = payload;
                res.truncated = res.truncated || truncated;
            }

            if g.config.tool_semantics.artifact.is_some() {
                let semantics = g.config.tool_semantics.clone();
                record_artifact_progress_with_semantics(
                    &mut g.pending_artifact_ids,
                    &semantics,
                    &call.name,
                    &call.arguments_json,
                    &res.result_json,
                );
            }

            g.session.messages.push(AgentMessage {
                id: format!("tool-{}", res.call_id),
                role: AgentRole::Tool,
                content: res.result_json.clone(),
                metadata: MessageMeta::new()
                    .with_call_id(res.call_id.clone())
                    .with_tool_name(call.name)
                    .with_extra("truncated", json!(res.truncated)),
            });
            last_batch_tool_message_index = Some(g.session.messages.len() - 1);
            if g.config.enable_state_ledger {
                g.state_ledger.observe_tool_result(&res);
            }
            if let Some(reference) = spilled.as_ref() {
                activate_tools_for_spill_reference(&mut g.active_tool_names, &mut res, reference);
            }
            if spilled.is_some() && !spill_stored_copy_only {
                let spill_tokens_after = g
                    .estimate_pressure(CompactionTrigger::PostToolResult)
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

        for (_, reject_msg) in reject_by_call_id {
            g.session.messages.push(reject_msg);
            last_batch_tool_message_index = Some(g.session.messages.len() - 1);
        }
        for (_, (call, mut res)) in executed_by_call_id {
            let spill_tokens_before = g
                .estimate_pressure(CompactionTrigger::PostToolResult)
                .total_tokens;
            let threshold =
                effective_tool_spill_threshold_bytes(&g.config, g.io.context_window_tokens);
            let turn_index = g.turn_index;
            let policy = result_policy_for(&g.config.tool_semantics, &call.name);
            let spill_stored_copy_only = policy
                .map(|policy| policy.spill_stored_copy_only)
                .unwrap_or(false);
            let skip_result_truncation = policy
                .map(|policy| policy.skip_result_truncation)
                .unwrap_or(false);
            let spilled = if spill_stored_copy_only {
                let mut stored_copy = res.clone();
                spill_tool_result_if_needed(
                    &mut g.context_store,
                    &mut stored_copy,
                    &call,
                    threshold,
                    turn_index,
                )
            } else {
                spill_tool_result_if_needed(
                    &mut g.context_store,
                    &mut res,
                    &call,
                    threshold,
                    turn_index,
                )
            };
            if !skip_result_truncation {
                let pressure = g.estimate_pressure(CompactionTrigger::PostToolResult);
                let budget = effective_tool_result_budget_for_window(
                    &pressure,
                    &g.capabilities,
                    &g.config,
                    g.io.context_window_tokens,
                );
                let (payload, truncated) = truncate_tool_result_json(&res.result_json, budget);
                res.result_json = payload;
                res.truncated = res.truncated || truncated;
            }
            if g.config.tool_semantics.artifact.is_some() {
                let semantics = g.config.tool_semantics.clone();
                record_artifact_progress_with_semantics(
                    &mut g.pending_artifact_ids,
                    &semantics,
                    &call.name,
                    &call.arguments_json,
                    &res.result_json,
                );
            }
            g.session.messages.push(AgentMessage {
                id: format!("tool-{}", res.call_id),
                role: AgentRole::Tool,
                content: res.result_json.clone(),
                metadata: MessageMeta::new()
                    .with_call_id(res.call_id.clone())
                    .with_tool_name(call.name)
                    .with_extra("truncated", json!(res.truncated)),
            });
            last_batch_tool_message_index = Some(g.session.messages.len() - 1);
            if g.config.enable_state_ledger {
                g.state_ledger.observe_tool_result(&res);
            }
            if let Some(reference) = spilled.as_ref() {
                activate_tools_for_spill_reference(&mut g.active_tool_names, &mut res, reference);
            }
            if spilled.is_some() && !spill_stored_copy_only {
                let spill_tokens_after = g
                    .estimate_pressure(CompactionTrigger::PostToolResult)
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

        inject_tool_budget_after_batch(&mut g, last_batch_tool_message_index);

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
            let synth = synthetic_finalize_calls_with_semantics(
                &g.pending_artifact_ids,
                &g.config.tool_semantics,
            );
            if !synth.is_empty() {
                g.pending_execute = Some(synth);
                g.pending_execute_skip_guardrail_count = true;
                g.pending_auto_finalize_finish = true;
                g.phase = EnginePhase::AwaitingTools;
            }
        }

        let auto_finalize_queued =
            g.pending_execute_skip_guardrail_count && g.pending_execute.is_some();
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

        if summary.summary_text.trim().is_empty() {
            g.suppress_summary_tier = true;
            let trigger = ctx.trigger;
            match g.run_compaction_pass(trigger) {
                Ok(Some(_)) => {}
                Ok(None) => {}
                Err(e) => {
                    g.set_terminal(AgentAction::Failed(e));
                }
            }
            g.suppress_summary_tier = false;
            return;
        }

        let cfg = g.config.clone();
        let message_count_before = g.session.messages.len() as u32;
        if let Err(reason) = apply_summary_result(&mut g.session, &req, &summary, &cfg, &ctx) {
            if reason == "empty summary_text" {
                g.suppress_summary_tier = true;
                let trigger = ctx.trigger;
                match g.run_compaction_pass(trigger) {
                    Ok(Some(_)) => {}
                    Ok(None) => {}
                    Err(e) => {
                        g.set_terminal(AgentAction::Failed(e));
                    }
                }
                g.suppress_summary_tier = false;
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

fn session_has_append_context(
    messages: &[AgentMessage],
    artifact_squash: Option<&ArtifactSquashSemantics>,
) -> bool {
    let Some(squash) = artifact_squash else {
        return false;
    };
    let quoted_total = (!squash.total_blocks_result_field.is_empty())
        .then(|| format!("\"{}\"", squash.total_blocks_result_field));
    messages.iter().any(|message| {
        (!squash.append_tool_name.is_empty()
            && message.content.contains(squash.append_tool_name.as_str()))
            || quoted_total
                .as_ref()
                .is_some_and(|quoted| message.content.contains(quoted))
            || (!squash.append_tool_name.is_empty()
                && message
                    .metadata
                    .raw_tool_calls()
                    .iter()
                    .any(|call| call.name == squash.append_tool_name))
    })
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
            tools_schema_json: self.io.tools_schema_json.clone(),
            system_prompt: self.io.system_prompt.clone(),
            context_sidecars_estimate: String::new(),
            capabilities: self.capabilities.clone(),
            trigger,
            suppress_summary_tier: self.suppress_summary_tier,
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
        let message_count_before = self.session.messages.len() as u32;
        let mut destructive_tiers: Vec<CompactionTier> = Vec::new();
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
                self.suppress_summary_tier,
            );
            let threshold_tokens = plan.threshold_tokens;
            let ratio_threshold_tokens = effective_threshold_tokens(
                &self.config,
                self.io.context_window_tokens,
                plan.threshold_ratio,
                None,
            );
            let pressure = self.estimate_pressure(trigger);
            self.session.ledger_snapshot = Some(pressure.clone());

            let artifact_squash = self
                .config
                .tool_semantics
                .compaction
                .artifact_squash
                .as_ref();
            let has_append_context =
                session_has_append_context(&self.session.messages, artifact_squash);
            let session_has_tool_messages = self
                .session
                .messages
                .iter()
                .any(|m| m.role == AgentRole::Tool);
            let defer_compaction_failure = has_append_context
                || self.pending_finalize_after_tool_stop
                || (self.pending_execute.is_some()
                    && !self.session.compaction_log.is_empty()
                    && session_has_tool_messages);

            if matches!(trigger, CompactionTrigger::MidTurn)
                && !self.config.allow_fifo_on_mid_turn
                && !self.capabilities.summarization_allowed
                && !self.session.compaction_log.is_empty()
                && !defer_compaction_failure
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
            let has_destructive_compaction_candidate = self.capabilities.summarization_allowed
                || (session_has_tool_messages && self.config.micro_keep_recent == 0)
                || match trigger {
                    CompactionTrigger::Preflight => self.config.allow_fifo_on_preflight,
                    CompactionTrigger::MidTurn => self.config.allow_fifo_on_mid_turn,
                    CompactionTrigger::PostToolResult => self.config.allow_fifo_on_post_tool_result,
                    CompactionTrigger::Manual => true,
                };
            let absolute_fires = absolute_fires && has_destructive_compaction_candidate;
            let active_threshold_tokens = if has_destructive_compaction_candidate {
                threshold_tokens
            } else {
                ratio_threshold_tokens
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

            let tool_round_index = self.guardrails.tool_calls_executed;
            let long_loop_fires = session_has_tool_messages
                && should_compact_for_long_loop(
                    &self.config,
                    &self.capabilities,
                    tool_round_index,
                    trigger,
                );
            let entered_for_long_loop_only = long_loop_fires
                && !should_compact_at_token_threshold(&pressure, active_threshold_tokens)
                && !absolute_fires
                && !turn_fires
                && !turn_end_fires;

            if !should_compact_at_token_threshold(&pressure, active_threshold_tokens)
                && !absolute_fires
                && !turn_fires
                && !turn_end_fires
                && !long_loop_fires
            {
                if sync_applied {
                    self.emit_post_compact(
                        trigger,
                        None,
                        last_history_ref,
                        &destructive_tiers,
                        message_count_before,
                    );
                }
                return Ok(None);
            }

            self.emit(AgentEvent::PreCompact { trigger });
            self.emit(AgentEvent::ContextPressure {
                ratio: pressure.ratio,
            });

            if matches!(trigger, CompactionTrigger::MidTurn)
                && !self.config.allow_fifo_on_mid_turn
                && !self.capabilities.summarization_allowed
                && !defer_compaction_failure
            {
                return Err(AgentError::CompactionFailed {
                    tier: "pipeline".into(),
                    reason: "No compaction tier applied while context remains over threshold"
                        .into(),
                });
            }

            if self.config.enable_context_store {
                let artifact_squash = self
                    .config
                    .tool_semantics
                    .compaction
                    .artifact_squash
                    .as_ref();
                if let Some(event) = spill_tool_messages_in_session(
                    &mut self.context_store,
                    &mut self.session,
                    artifact_squash,
                    effective_tool_spill_threshold_bytes(
                        &self.config,
                        self.io.context_window_tokens,
                    ),
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
            let forced_by_non_ratio =
                !should_compact_at_token_threshold(&pressure, active_threshold_tokens)
                    && (absolute_fires || turn_fires || turn_end_fires);
            let (effective_plan, effective_ctx) = if forced_by_non_ratio {
                let mut p = plan.clone();
                p.threshold_ratio = 0.0;
                p.threshold_tokens = 0;
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
                    if is_destructive_compaction_tier(&ev.tier) {
                        destructive_tiers.push(ev.tier.clone());
                        self.destructive_tiers_this_turn += 1;
                    }
                    self.compaction_passes_this_turn += 1;
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
                    let p2 = self.estimate_pressure(trigger);
                    if exceeds_hard_prompt_ceiling(&p2)
                        && !entered_for_long_loop_only
                        && !sync_applied
                        && !defer_compaction_failure
                    {
                        return Err(AgentError::CompactionFailed {
                            tier: "pipeline".into(),
                            reason:
                                "No compaction tier applied while context remains over threshold"
                                    .into(),
                        });
                    }
                    if sync_applied {
                        self.emit_post_compact(
                            trigger,
                            None,
                            last_history_ref,
                            &destructive_tiers,
                            message_count_before,
                        );
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
        self.context_store.expire_after_turn(self.turn_index);
        // Reset per-turn telemetry counters for the new user turn.
        self.compaction_passes_this_turn = 0;
        self.destructive_tiers_this_turn = 0;
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

        let req = self.build_model_request(allow, cache_hints);

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

        let synth = synthetic_finalize_calls_with_semantics(
            &self.pending_artifact_ids,
            &self.config.tool_semantics,
        );
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

        let injected_message = AgentMessage {
            id: format!("pending-work-reminder-{}", self.turn_index),
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
        let request = self.build_model_request(allow, vec![]);
        self.pending_call_model = Some(request.clone());
        self.model_requests_this_turn += 1;
        self.phase = EnginePhase::AwaitingModel;
        Some(AgentAction::CallModel(request))
    }

    fn build_model_request(
        &mut self,
        allow_tool_call: bool,
        cache_hints: Vec<String>,
    ) -> ModelRequest {
        let session_messages: Vec<AgentMessage> = self
            .session
            .messages
            .iter()
            .filter(|m| !m.metadata.is_superseded())
            .cloned()
            .collect();
        // State-ledger anchor intentionally NOT injected at position 0 of session_messages.
        // It would mutate byte 0 of the prefix on every turn (objective/decisions grow),
        // breaking xAI/OpenAI prompt-cache prefix stability. Ledger content is already
        // shipped via the tail-volatile sidecar (`stateLedgerJson` → `compactLedger`).

        let rendered_tool_catalog_json = render_runtime_tool_view(
            &self.io.tools_schema_json,
            &self.config,
            &self.capabilities,
            &self.loop_metrics,
        )
        .unwrap_or_else(|_| self.io.tools_schema_json.clone());

        let has_catalog = !self.io.tool_catalog_json.trim().is_empty()
            && self.io.tool_catalog_json.trim() != "[]";
        let has_registry = !self.io.tool_schema_registry_json.trim().is_empty()
            && self.io.tool_schema_registry_json.trim() != "[]";

        let active_schema_json: String = if has_registry && !self.active_tool_names.is_empty() {
            render_active_schema_json(&self.io.tool_schema_registry_json, &self.active_tool_names)
        } else {
            self.io.tools_schema_json.clone()
        };

        let tool_context_index = build_tool_context_index_with_registry(
            &active_schema_json,
            &self.io.tool_catalog_json,
            &self.io.tool_schema_registry_json,
            &mut self.context_store,
            &self.config,
            &self.capabilities,
        );
        let tools_schema_json = if tool_context_index.is_some() && !has_catalog {
            "[]".to_string()
        } else {
            active_schema_json
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
                    self.turn_index,
                )
            });

        let generation = self.sidecar_builder.next_generation();
        let sidecar_budget = self.config.long_loop.sidecar_byte_budget_per_call;
        let ledger_delta = self
            .config
            .enable_state_ledger
            .then(|| {
                self.sidecar_builder.build_ledger_delta(
                    &mut self.context_store,
                    &self.state_ledger,
                    sidecar_budget,
                )
            })
            .flatten();
        let tool_index_delta = tool_context_index.as_ref().and_then(|index| {
            self.sidecar_builder.build_tool_index_payload(
                &mut self.context_store,
                index,
                sidecar_budget,
            )
        });
        let file_index_delta = file_context_index.as_ref().and_then(|index| {
            self.sidecar_builder.build_file_index_payload(
                &mut self.context_store,
                index,
                sidecar_budget,
            )
        });
        let sidecar_byte_len = ledger_delta.as_ref().map(|s| s.len()).unwrap_or(0)
            + tool_index_delta.as_ref().map(|s| s.len()).unwrap_or(0)
            + file_index_delta.as_ref().map(|s| s.len()).unwrap_or(0);
        self.sidecar_builder
            .record_rolling_cache_entry(generation, sidecar_byte_len);
        self.sidecar_builder
            .enforce_total_budget(&self.config.long_loop);

        ModelRequest {
            session_messages,
            system_prompt: self.io.system_prompt.clone(),
            rendered_tool_catalog_json,
            tools_schema_json,
            allow_tool_call,
            context_references: self
                .context_store
                .entries()
                .filter(|entry| is_browsable_context_kind(&entry.reference.kind))
                .map(|entry| entry.reference.clone())
                .collect(),
            state_ledger: self
                .config
                .enable_state_ledger
                .then(|| self.state_ledger.clone()),
            tool_context_index,
            file_context_index,
            cache_hints,
            provider_context: ProviderContextSnapshot {
                transcript_revision: self.session.transcript_revision,
                sidecar_generation: generation,
                sidecar_byte_len: sidecar_byte_len.min(u32::MAX as usize) as u32,
                compaction_passes_this_turn: self.compaction_passes_this_turn,
                destructive_tiers_this_turn: self.destructive_tiers_this_turn,
                pressure_ratio_at_call_model_millis: {
                    let ratio = self
                        .session
                        .ledger_snapshot
                        .as_ref()
                        .map(|p| p.ratio)
                        .unwrap_or_else(|| {
                            self.estimate_pressure(CompactionTrigger::Preflight).ratio
                        });
                    (ratio * 1000.0).round().clamp(0.0, u32::MAX as f32) as u32
                },
            },
            state_ledger_delta_json: if generation == 0 { None } else { ledger_delta },
            tool_context_index_delta_json: if generation == 0 {
                None
            } else {
                tool_index_delta
            },
            file_context_index_delta_json: if generation == 0 {
                None
            } else {
                file_index_delta
            },
            retry_after_ms: None,
            model_retry_attempt: None,
        }
    }

    fn set_terminal_or_pending(&mut self, action: AgentAction) {
        match action {
            AgentAction::CallModel(_) => {}
            other => self.set_terminal(other),
        }
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
    use crate::turn_io::RawToolCall;

    fn io_small_window() -> SessionIoParams {
        SessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 256,
            tool_catalog_json: "[]".into(),
            tool_schema_registry_json: "[]".into(),
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        }
    }

    fn io_large_window() -> SessionIoParams {
        SessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tool_catalog_json: "[]".into(),
            tool_schema_registry_json: "[]".into(),
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        }
    }

    #[test]
    #[cfg(feature = "lalr-harness")]
    fn create_session_wires_long_loop_policy_from_context_tier() {
        use crate::long_loop_policy::LongLoopPolicy;
        use crate::policy::ContextPolicyTier;

        let eng = AgentEngine::new();
        let hosted = eng.create_session(
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
        );
        assert_eq!(
            hosted.lalr_config().long_loop,
            LongLoopPolicy::hosted_mutable_defaults()
        );

        let fixed_kv = eng.create_session(
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
        );
        assert_eq!(
            fixed_kv.lalr_config().long_loop,
            LongLoopPolicy::fixed_kv_defaults()
        );
    }

    #[test]
    fn submit_model_response_preserves_provider_reasoning_metadata() {
        let eng = AgentEngine::new();
        let h = eng.create_session(
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
        );

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
            assistant
                .metadata
                .extra("provider_reasoning")
                .and_then(|value| value.get("reasoning_content"))
                .and_then(|value| value.as_str()),
            Some("need tool output first"),
        );
    }

    #[test]
    fn three_tool_happy_path() {
        let eng = AgentEngine::new();
        let h = eng.create_session(
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
        let h = eng.create_session(
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
        );
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
        let h = eng.create_session(
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
        );
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
        let h = eng.create_session(
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
        let h = eng.create_session(
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
        let eng = AgentEngine::new();
        let h = eng.create_session(
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
        );
        let _ = eng.next_action(&h);
        assert!(!h.lock_inner().session.compaction_log.is_empty());
    }

    #[test]
    fn kv_fixed_emits_invalidated_on_compact() {
        let big_pair = "hello ".repeat(400);
        let eng = AgentEngine::new();
        let h = eng.create_session(
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
        );
        let _ = eng.next_action(&h);
        let evs = eng.drain_events(&h);
        assert!(evs
            .iter()
            .any(|e| matches!(e, AgentEvent::KvCacheInvalidated { .. })));
    }

    #[test]
    fn artifact_auto_finalize_then_finished() {
        let eng = AgentEngine::new();
        let h = eng.create_session(
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
        let h = eng.create_session(
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
        let h = eng.create_session(
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
        );
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
        let h = eng.create_session(
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
        );
        let _ = eng.next_action(&h);
        assert!(!eng.drain_events(&h).is_empty());
        assert!(eng.drain_events(&h).is_empty());
    }

    fn frg14_squash(
        append_tool_name: &str,
        total_blocks_result_field: &str,
    ) -> ArtifactSquashSemantics {
        ArtifactSquashSemantics {
            append_tool_name: append_tool_name.into(),
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

    #[test]
    fn frg14_append_context_custom_content_match() {
        let squash = frg14_squash("write_doc", "chunkCount");
        let messages = vec![frg14_msg(
            r#"call:write_doc{"docId":"1","chunks":[]}"#,
            MessageMeta::new(),
        )];
        assert!(session_has_append_context(&messages, Some(&squash)));
    }

    #[test]
    fn frg14_append_context_custom_raw_tool_call_match() {
        let squash = frg14_squash("write_doc", "chunkCount");
        let messages = vec![frg14_msg(
            "plain assistant text",
            MessageMeta::new().with_raw_tool_calls(vec![RawToolCall {
                id: None,
                name: "write_doc".into(),
                arguments_json: "{}".into(),
            }]),
        )];
        assert!(session_has_append_context(&messages, Some(&squash)));
    }

    #[test]
    fn frg14_append_context_custom_total_field_match() {
        let squash = frg14_squash("write_doc", "chunkCount");
        let messages = vec![frg14_msg(
            r#"{"status":"success","chunkCount":2}"#,
            MessageMeta::new(),
        )];
        assert!(session_has_append_context(&messages, Some(&squash)));
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

    #[test]
    fn frg14_append_context_enclave_matches_legacy_literals() {
        let squash = frg14_squash("append_blocks", "totalBlocks");
        let via_tool = vec![frg14_msg("call:append_blocks{}", MessageMeta::new())];
        assert!(session_has_append_context(&via_tool, Some(&squash)));
        let via_total = vec![frg14_msg(r#"{"totalBlocks":1}"#, MessageMeta::new())];
        assert!(session_has_append_context(&via_total, Some(&squash)));
    }
}
