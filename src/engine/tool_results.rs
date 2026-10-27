//! Tool-results pipeline (ENC-286 slice 4, depends on slice 2).
//!
//! Collapses the two near-identical per-result spill/truncate/record/push
//! blocks of `submit_tool_results` into one `process_one_result`, plus a pure
//! `pair_results_to_calls` stage and an `apply_processed_tool_result` stage for
//! the message/ledger/emit application. FRG-47 phase staging lives in the
//! `submit_*` dispatch in `mod.rs`: the dispatch owns pressure reads, phase
//! transitions, and emit ordering. Everything here takes disjoint field
//! bundles — no `&mut InnerSession`, no Mutex locking, no phase writes.
//!
//! Note on pressure values: `estimate_session_pressure` is a pure function of
//! (`session`, `config`, `ctx`), none of which the spill stage touches (it
//! mutates only the `ContextStore` and the result payload). The dispatch
//! therefore reads `spill_tokens_before` before the call and re-reads after
//! iff a spill event is emitted, preserving the original event values.

use std::collections::{HashMap, HashSet};

use serde_json::json;

use super::activate_tools_for_spill_reference;
use crate::artifact_finalize::record_artifact_progress_with_semantics;
use crate::guardrails::is_soft_failure_result_json;
use crate::compact::tool_spill::spill_tool_result_if_needed;
use crate::context_store::{ContextReference, ContextStore};
use crate::events::{AgentEvent, CompactionEvent, CompactionTier, CompactionTrigger};
use crate::message::{AgentMessage, AgentRole, MessageMeta, ToolCallRecord, ToolResultRecord};
use crate::session::AgentSession;
use crate::state_ledger::StateLedger;
use crate::tool_semantics::ToolSemanticsConfig;
use crate::truncate::truncate_tool_result_json;

/// FRG-47 stage: pair host results to calls by `call_id`, falling back to
/// submission order for host results with unknown ids. Calls left without any
/// result get a synthesized error so provider replay still sees one tool
/// message per `tool_call_id` (OpenAI-compatible APIs 400 otherwise).
///
/// Pure: no session access, so the dispatch owns all phase/emit behavior.
pub(super) fn pair_results_to_calls(
    calls: &[ToolCallRecord],
    results: Vec<ToolResultRecord>,
) -> HashMap<String, (ToolCallRecord, ToolResultRecord)> {
    let mut remaining: Vec<Option<ToolResultRecord>> = results.into_iter().map(Some).collect();
    let mut executed_by_call_id: HashMap<String, (ToolCallRecord, ToolResultRecord)> =
        HashMap::with_capacity(calls.len());
    for call in calls {
        if let Some(slot) = remaining
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|res| res.call_id == call.call_id))
        {
            let res = slot.take().expect("matched result");
            executed_by_call_id.insert(call.call_id.clone(), (call.clone(), res));
        }
    }
    let mut unmatched = remaining.into_iter().flatten();
    for call in calls {
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
    executed_by_call_id
}

/// Read-only inputs for one result's spill/truncate/record preparation.
/// Pressure values are read by the dispatch and passed as plain data.
pub(super) struct ProcessOneResultInputs<'a> {
    pub semantics: &'a ToolSemanticsConfig,
    pub spill_threshold_bytes: usize,
    /// `None` when the result policy says to skip truncation.
    pub truncation_budget: Option<usize>,
    pub turn_index: u32,
    pub context_store: &'a mut ContextStore,
    pub pending_artifact_ids: &'a mut HashSet<String>,
}

/// Output of `process_one_result`: the tool message to push plus everything
/// the dispatch needs to apply ledger/emit side effects in order.
pub(super) struct ProcessedToolResult {
    pub message: AgentMessage,
    pub processed: ToolResultRecord,
    pub spill_reference: Option<ContextReference>,
    /// `spilled && !spill_stored_copy_only`: the dispatch emits the ToolSpill
    /// event and re-reads pressure only in this case.
    pub spilled_inline: bool,
}

/// ENC-286 slice 4 core: the single per-result pipeline (spill → truncate →
/// artifact progress → message shaping) shared by both drain loops.
pub(super) fn process_one_result(
    call: &ToolCallRecord,
    mut res: ToolResultRecord,
    inputs: ProcessOneResultInputs<'_>,
) -> ProcessedToolResult {
    let ProcessOneResultInputs {
        semantics,
        spill_threshold_bytes,
        truncation_budget,
        turn_index,
        context_store,
        pending_artifact_ids,
    } = inputs;
    let spill_stored_copy_only = crate::tool_semantics::result_policy_for(semantics, &call.name)
        .map(|policy| policy.spill_stored_copy_only)
        .unwrap_or(false);
    let spilled = if spill_stored_copy_only {
        let mut stored_copy = res.clone();
        spill_tool_result_if_needed(
            context_store,
            &mut stored_copy,
            call,
            spill_threshold_bytes,
            turn_index,
        )
    } else {
        spill_tool_result_if_needed(context_store, &mut res, call, spill_threshold_bytes, turn_index)
    };

    if let Some(budget) = truncation_budget {
        let (payload, truncated) = truncate_tool_result_json(&res.result_json, budget);
        res.result_json = payload;
        res.truncated = res.truncated || truncated;
    }

    if semantics.artifact.is_some() {
        record_artifact_progress_with_semantics(
            pending_artifact_ids,
            semantics,
            &call.name,
            &call.arguments_json,
            &res.result_json,
        );
    }

    let message = AgentMessage {
        id: format!("tool-{}", res.call_id),
        role: AgentRole::Tool,
        content: res.result_json.clone(),
        metadata: MessageMeta::new()
            .with_call_id(res.call_id.clone())
            .with_tool_name(call.name.clone())
            .with_extra("truncated", json!(res.truncated)),
    };
    let spilled_inline = spilled.is_some() && !spill_stored_copy_only;
    ProcessedToolResult {
        message,
        processed: res,
        spill_reference: spilled,
        spilled_inline,
    }
}

/// Dispatch-owned application of one processed result: message push, ledger
/// observe, spill-activation merge, and the ToolSpill compaction event.
pub(super) struct ApplyProcessedInputs<'a> {
    pub session: &'a mut AgentSession,
    pub enable_state_ledger: bool,
    pub state_ledger: &'a mut StateLedger,
    pub active_tool_names: &'a mut Vec<String>,
    pub events: &'a mut Vec<AgentEvent>,
    pub last_batch_tool_message_index: &'a mut Option<usize>,
    pub spill_tokens_before: u32,
    pub spill_tokens_after: u32,
}

pub(super) fn apply_processed_tool_result(
    inputs: ApplyProcessedInputs<'_>,
    processed: ProcessedToolResult,
) -> ToolResultRecord {
    let ApplyProcessedInputs {
        session,
        enable_state_ledger,
        state_ledger,
        active_tool_names,
        events,
        last_batch_tool_message_index,
        spill_tokens_before,
        spill_tokens_after,
    } = inputs;
    session.messages.push(processed.message);
    *last_batch_tool_message_index = Some(session.messages.len() - 1);
    let mut res = processed.processed;
    if enable_state_ledger {
        state_ledger.observe_tool_result(&res);
    }
    if let Some(reference) = processed.spill_reference.as_ref() {
        activate_tools_for_spill_reference(active_tool_names, &mut res, reference);
    }
    if processed.spilled_inline {
        events.push(AgentEvent::CompactionApplied(CompactionEvent {
            tier: CompactionTier::ToolSpill,
            tokens_before: spill_tokens_before,
            tokens_after: spill_tokens_after,
            trigger: CompactionTrigger::PostToolResult,
        }));
    }
    res
}

/// Guardrail-accounting read of a finished batch (unchanged semantics).
pub(super) fn batch_has_non_soft_outcome(processed_results: &[ToolResultRecord]) -> bool {
    processed_results
        .iter()
        .any(|res| !res.soft_failure && !is_soft_failure_result_json(&res.result_json))
}
