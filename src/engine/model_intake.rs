//! Model-response intake (ENC-286 slice 5, depends on slice 2; FRG-47 staging).
//!
//! Owns the repair/discovery loop over `merged_tool_calls` plus the two
//! early-return ingest arms (`ParseFailed`, `ToolsBlocked`) and the
//! reject-only flush tail. Each stage returns data or a decision enum; the
//! `submit_model_response` dispatch owns phase transitions and emit ordering.
//! Nothing here takes `&mut InnerSession`, locks the Mutex, or writes `phase`.
//!
//! The per-call `tool_semantics` clone + `ToolCatalog` rebuild previously
//! inside the hot loop are hoisted: the dispatch builds the catalog once and
//! passes it in.

use std::collections::HashMap;

use serde_json::json;

use super::{gate_exemption_for, merge_active_tool_names};
use crate::agent_tool_budget::inject_tool_budget_into_session;
use crate::catalog::ToolCatalog;
use crate::events::AgentEvent;
use crate::guardrails::GuardrailState;
use crate::message::{AgentMessage, AgentRole, MessageMeta, ToolCallRecord};
use crate::repair::{RepairOutcome, ToolRepairPipeline};
use crate::session::{AgentConfig, AgentSession};
use crate::state_ledger::StateLedger;
use crate::tool_name::ToolName;
use crate::tool_semantics::ToolSemanticsConfig;
use crate::turn_io::RawToolCall;

/// FRG-47 stage: appends the assistant message for the continuing flow
/// (metadata shaping, ledger observe, hook emit, greedy-name capture).
/// Returns nothing; the dispatch continues.
pub(super) struct AppendAssistantInputs<'a> {
    pub session: &'a mut AgentSession,
    pub state_ledger: &'a mut StateLedger,
    pub enable_state_ledger: bool,
    pub events: &'a mut Vec<AgentEvent>,
    pub enable_loop_hooks: bool,
    pub lifecycle_hooks_supported: bool,
    pub greedy_tool_names: &'a mut Vec<ToolName>,
}

pub(super) fn append_assistant_message(
    inputs: AppendAssistantInputs<'_>,
    merged_tool_calls: &[RawToolCall],
    assistant_content: String,
    ingest_metadata: serde_json::Value,
    provider_reasoning_content: Option<&str>,
) {
    let AppendAssistantInputs {
        session,
        state_ledger,
        enable_state_ledger,
        events,
        enable_loop_hooks,
        lifecycle_hooks_supported,
        greedy_tool_names,
    } = inputs;
    let assistant_id = format!(
        "a{}",
        session
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
    if let Some(reasoning_content) =
        provider_reasoning_content.filter(|content| !content.is_empty())
    {
        metadata.set_extra(
            "provider_reasoning",
            json!({ "reasoning_content": reasoning_content }),
        );
    }
    session.messages.push(AgentMessage {
        id: assistant_id,
        role: AgentRole::Assistant,
        content: assistant_content,
        metadata,
    });
    if enable_state_ledger {
        if let Some(message) = session.messages.last().cloned() {
            state_ledger.observe_assistant_message(&message);
        }
    }

    // Emit after_model_response only when there are tool calls (model returned with tools)
    if enable_loop_hooks && lifecycle_hooks_supported && !merged_tool_calls.is_empty() {
        events.push(AgentEvent::HookInvoked {
            checkpoint: "after_model_response".into(),
        });
    }

    // J1: greedy capture keys off the same model-output constructor as
    // gating, so `webSearch` records `web_search` (matching policy names).
    *greedy_tool_names = merged_tool_calls
        .iter()
        .filter_map(|c| ToolName::from_model_output(&c.name).ok())
        .collect();
}

/// Gate identity for one raw model tool name (J1): the single
/// model-output constructor (prefix-strip + snake-split + identity fold).
/// A blank result is not a name at all.
fn gate_tool_name(raw: &str) -> Option<ToolName> {
    ToolName::from_model_output(raw).ok()
}

/// Inputs for the repair/discovery loop. The catalog is built once by the
/// dispatch (hoisted out of the per-call hot loop).
pub(super) struct RepairTurnInputs<'a> {
    pub semantics: &'a ToolSemanticsConfig,
    pub catalog: &'a ToolCatalog,
    pub active_tool_names: &'a mut Vec<ToolName>,
    pub turn_index: u32,
    pub default_tool_timeout_ms: Option<u64>,
    pub tool_retry_limit: u32,
    pub require_read_before_edit: bool,
}

/// Outcome of the repair/discovery loop. Event emission stays ordered by the
/// dispatch: all stage-collected events are returned for the dispatch to emit.
pub(super) struct RepairTurnOutcome {
    pub repaired: Vec<ToolCallRecord>,
    pub reject_by_call_id: HashMap<String, AgentMessage>,
    pub parallel: usize,
    pub rejects: usize,
    pub events: Vec<AgentEvent>,
}

pub(super) fn repair_tool_turn(
    merged_tool_calls: &[RawToolCall],
    inputs: RepairTurnInputs<'_>,
) -> RepairTurnOutcome {
    let RepairTurnInputs {
        semantics,
        catalog,
        active_tool_names,
        turn_index,
        default_tool_timeout_ms,
        tool_retry_limit,
        require_read_before_edit,
    } = inputs;
    // Hoisted out of the per-call loop (was cloned + rebuilt per call).
    let discovery = semantics.discovery.clone();
    let discovery_name = discovery.as_ref().map(|d| d.tool_name.clone());

    let mut repaired: Vec<ToolCallRecord> = Vec::new();
    let mut reject_by_call_id: HashMap<String, AgentMessage> = HashMap::new();
    let mut events: Vec<AgentEvent> = Vec::new();
    let mut rejects = 0usize;
    let parallel = merged_tool_calls.len();

    for raw in merged_tool_calls {
        // Tool-discovery guard: reject any call whose tool name is not part of
        // the currently active set (the discovery tool itself is always callable).
        // Only enforce when active_tool_names has actually been initialized — empty
        // active set means the host has not opted into the new contract yet.
        // Both sides are canonical identities, so this is plain equality.
        let tool_name = gate_tool_name(&raw.name);
        // ENC-303: one exemption list drives both halves. `activates == true`
        // merges into the active set (companion style); either value makes
        // the tool callable now without prior discovery activation.
        let exemption = tool_name
            .as_ref()
            .and_then(|name| gate_exemption_for(&discovery, name));
        if exemption == Some(true) {
            if let Some(name) = tool_name.clone() {
                merge_active_tool_names(active_tool_names, [name]);
            }
        }
        let callable_now = !active_tool_names.is_empty()
            && (tool_name
                .as_ref()
                .is_some_and(|name| active_tool_names.iter().any(|active| active == name))
                || exemption.is_some());
        if !active_tool_names.is_empty() && !callable_now {
            events.push(AgentEvent::ToolSoftFailure {
                tool_name: raw.name.clone(),
                turn_index,
            });
            let mut available = active_tool_names.clone();
            if let Some(name) = discovery_name.clone() {
                if !available.iter().any(|n| n == &name) {
                    available.insert(0, name);
                }
            }
            let error = if let Some(name) = discovery_name.as_ref() {
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
            reject_by_call_id.insert(
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
        // ENC-297: `catalog` is the construction-time validated policy
        // catalog, so this validity check runs on the semantics directly
        // instead of rebuilding (and silently defaulting) per tool call.
        let (outcome, evs) = match semantics.validate() {
            Ok(_) => ToolRepairPipeline::repair_with_semantics(raw, catalog, semantics),
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
            events.push(ev);
        }
        match outcome {
            RepairOutcome::Repaired(mut rec) => {
                if default_tool_timeout_ms.is_some() {
                    rec.attempt_metadata = Some(crate::message::ToolAttemptMetadata {
                        attempt_index: turn_index + 1,
                        attempts_remaining: Some(
                            tool_retry_limit
                                .saturating_sub(turn_index)
                                .saturating_sub(1),
                        ),
                        timeout_ms: default_tool_timeout_ms,
                        policy_denied: false,
                        requires_prior_read: require_read_before_edit,
                    });
                }
                repaired.push(rec);
            }
            RepairOutcome::Reject {
                error_json_for_model,
            } => {
                events.push(AgentEvent::ToolSoftFailure {
                    tool_name: raw.name.clone(),
                    turn_index,
                });
                let call_id = raw.id.clone().unwrap_or_else(|| "noid".into());
                reject_by_call_id.insert(
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
                events.push(AgentEvent::ToolSoftFailure {
                    tool_name: raw.name.clone(),
                    turn_index,
                });
                let err = serde_json::json!({
                    "error": format!("Ambiguous tool '{original}' (closest '{closest_name}')"),
                    "available_tools": catalog
                        .all()
                        .iter()
                        .map(|tool| tool.name.clone())
                        .collect::<Vec<_>>(),
                })
                .to_string();
                let call_id = raw.id.clone().unwrap_or_else(|| "noid".into());
                reject_by_call_id.insert(
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

    RepairTurnOutcome {
        repaired,
        reject_by_call_id,
        parallel,
        rejects,
        events,
    }
}

/// Shared fields for the early-return ingest arms.
pub(super) struct IngestArmInputs<'a> {
    pub session: &'a mut AgentSession,
    pub guardrails: &'a mut GuardrailState,
    pub config: &'a AgentConfig,
    pub state_ledger: &'a mut StateLedger,
    pub events: &'a mut Vec<AgentEvent>,
    pub turn_index: u32,
    pub last_model_response_greedy_tool_names: &'a [ToolName],
}

/// FRG-47 stage: the `ParseFailed` ingest arm. Applies message/ledger/
/// guardrail/budget effects; the dispatch returns afterwards (no phase write).
pub(super) fn apply_ingest_parse_failed(
    inputs: IngestArmInputs<'_>,
    visible_assistant_text: String,
    error_json_for_model: String,
) {
    let IngestArmInputs {
        session,
        guardrails,
        config,
        state_ledger,
        events: _,
        turn_index,
        last_model_response_greedy_tool_names,
    } = inputs;
    guardrails.record_envelope_parse_failure();
    let assistant_id = format!(
        "a{}",
        session
            .messages
            .iter()
            .filter(|message| message.role == AgentRole::Assistant)
            .count()
    );
    let assistant_id_clone = assistant_id.clone();
    session.messages.push(AgentMessage {
        id: assistant_id,
        role: AgentRole::Assistant,
        content: visible_assistant_text,
        metadata: MessageMeta::new().with_envelope_ingest("failed"),
    });
    if config.enable_state_ledger {
        if let Some(message) = session.messages.last().cloned() {
            state_ledger.observe_assistant_message(&message);
        }
    }
    session.messages.push(AgentMessage {
        id: format!("tool-err-envelope-{assistant_id_clone}"),
        role: AgentRole::Tool,
        content: error_json_for_model,
        metadata: MessageMeta::new().with_envelope_parse_failure(),
    });
    let cfg = config.clone();
    guardrails.count_tool_executed(1);
    guardrails.on_tool_turn_finished(&cfg, 1, true, false, false, "agent_engine");
    let last_tool_idx = session.messages.len().checked_sub(1);
    let budget_meta =
        guardrails.tool_budget_snapshot(config, last_model_response_greedy_tool_names);
    inject_tool_budget_into_session(session, turn_index, &budget_meta, last_tool_idx);
}

/// FRG-47 stage: the `ToolsBlocked` ingest arm. Returns what the dispatch must
/// do next; the stage itself writes no phase.
pub(super) enum ToolsBlockedEffect {
    /// Nudge applied; the turn continues on the next model call.
    Nudged,
    /// Graceful fallback already pushed as the final answer; the dispatch must
    /// finish the turn with `Stop` semantics and return.
    FallbackFinished,
}

pub(super) fn apply_ingest_tools_blocked(
    inputs: IngestArmInputs<'_>,
    visible_assistant_text: String,
    user_fallback_text: String,
    nudge_json_for_model: String,
    tools_blocked_nudges_this_turn: &mut u32,
    model_requests_this_turn: u32,
    max_model_requests_per_turn: u32,
) -> ToolsBlockedEffect {
    let IngestArmInputs {
        session,
        guardrails,
        config,
        state_ledger,
        events,
        turn_index,
        last_model_response_greedy_tool_names,
    } = inputs;
    let assistant_id = format!(
        "a{}",
        session
            .messages
            .iter()
            .filter(|message| message.role == AgentRole::Assistant)
            .count()
    );
    let can_nudge = *tools_blocked_nudges_this_turn == 0
        && model_requests_this_turn < max_model_requests_per_turn;

    if !can_nudge {
        session.messages.push(AgentMessage {
            id: assistant_id,
            role: AgentRole::Assistant,
            content: user_fallback_text.clone(),
            metadata: MessageMeta::new().with_tools_blocked_fallback(),
        });
        if config.enable_state_ledger {
            if let Some(message) = session.messages.last().cloned() {
                state_ledger.observe_assistant_message(&message);
            }
        }
        events.push(AgentEvent::VisibleAssistantText {
            text: user_fallback_text.clone(),
        });
        return ToolsBlockedEffect::FallbackFinished;
    }

    let assistant_id_clone = assistant_id.clone();
    session.messages.push(AgentMessage {
        id: assistant_id,
        role: AgentRole::Assistant,
        content: visible_assistant_text,
        metadata: MessageMeta::new().with_envelope_ingest("tools_blocked"),
    });
    if config.enable_state_ledger {
        if let Some(message) = session.messages.last().cloned() {
            state_ledger.observe_assistant_message(&message);
        }
    }
    session.messages.push(AgentMessage {
        id: format!("tool-nudge-tools-blocked-{assistant_id_clone}"),
        role: AgentRole::Tool,
        content: nudge_json_for_model,
        metadata: MessageMeta::new().with_tools_blocked_nudge(),
    });
    let last_tool_idx = session.messages.len().checked_sub(1);
    let budget_meta =
        guardrails.tool_budget_snapshot(config, last_model_response_greedy_tool_names);
    inject_tool_budget_into_session(session, turn_index, &budget_meta, last_tool_idx);
    *tools_blocked_nudges_this_turn += 1;
    guardrails.reset_envelope_parse_failures();
    ToolsBlockedEffect::Nudged
}

/// Inputs for the reject-only flush tail.
pub(super) struct RejectFlushInputs<'a> {
    pub session: &'a mut AgentSession,
    pub guardrails: &'a mut GuardrailState,
    pub config: &'a AgentConfig,
    pub pending_tool_turn_request_order: &'a mut Vec<RawToolCall>,
    pub pending_reject_tool_by_call_id: &'a mut HashMap<String, AgentMessage>,
    pub turn_index: u32,
    pub parallel: usize,
    pub rejects: usize,
    pub last_model_response_greedy_tool_names: &'a [ToolName],
}

/// FRG-47 stage: the reject-only synthetic turn tail. Applies guardrail
/// counts, flushes reject messages in request order, and injects the budget.
/// The dispatch returns afterwards (phase already `Idle`).
pub(super) fn flush_reject_only_turn(inputs: RejectFlushInputs<'_>) {
    let RejectFlushInputs {
        session,
        guardrails,
        config,
        pending_tool_turn_request_order,
        pending_reject_tool_by_call_id,
        turn_index,
        parallel,
        rejects,
        last_model_response_greedy_tool_names,
    } = inputs;
    let cfg = config.clone();
    guardrails.count_tool_executed(rejects as u32);
    guardrails.on_tool_turn_finished(&cfg, parallel, parallel > 0, false, false, "agent_engine");
    let mut last_tool_idx = None;
    for raw in std::mem::take(pending_tool_turn_request_order) {
        let call_id = raw.id.clone().unwrap_or_else(|| "noid".into());
        if let Some(message) = pending_reject_tool_by_call_id.remove(&call_id) {
            session.messages.push(message);
            last_tool_idx = Some(session.messages.len() - 1);
        }
    }
    pending_reject_tool_by_call_id.clear();
    let budget_meta =
        guardrails.tool_budget_snapshot(config, last_model_response_greedy_tool_names);
    inject_tool_budget_into_session(session, turn_index, &budget_meta, last_tool_idx);
}
