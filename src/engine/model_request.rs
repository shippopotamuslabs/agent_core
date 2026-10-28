//! Model-request construction (ENC-286 slice 2).
//!
//! Load-bearing precedent for the decomposition: `build_model_request` runs on
//! a disjoint-field borrow split of `InnerSession` instead of the whole guard.
//! It takes no `&mut InnerSession` and never locks the session Mutex — callers
//! pass each field (or small bundle) explicitly, so simultaneous `&` / `&mut`
//! borrows of disjoint fields are checked by the borrow checker at the call
//! site.

use super::state::SessionIoParams;
use crate::compact::{estimate_session_pressure, CompactionContext};
use crate::context_store::{is_browsable_context_kind, ContextStore};
use crate::events::CompactionTrigger;
use crate::file_context::condense_file_context;
use crate::session::{AgentConfig, AgentSession, SessionCapabilities};
use crate::sidecar_builder::{ProviderContextSnapshot, SidecarBuilder};
use crate::state_ledger::StateLedger;
use crate::tool_context::build_tool_context_index_from_set;
use crate::turn_io::ModelRequest;

/// Disjoint-field view of `InnerSession` needed to build a `ModelRequest`.
///
/// Construction sites list every field explicitly; nothing here can reach the
/// phase machine or the session Mutex.
pub(super) struct ModelRequestInputs<'a> {
    pub session: &'a AgentSession,
    pub config: &'a AgentConfig,
    pub capabilities: &'a SessionCapabilities,
    pub io: &'a SessionIoParams,
    pub active_tool_names: &'a [String],
    pub context_store: &'a mut ContextStore,
    pub state_ledger: &'a StateLedger,
    pub sidecar_builder: &'a mut SidecarBuilder,
    pub turn_index: u32,
    pub compaction_passes_this_turn: u32,
    pub destructive_tiers_this_turn: u32,
    pub suppress_summary_tier: bool,
    pub allow_tool_call: bool,
    pub cache_hints: Vec<String>,
    pub loop_metrics: &'a crate::loop_metrics::LoopMetrics,
}

pub(super) fn build_model_request(inputs: ModelRequestInputs<'_>) -> ModelRequest {
    let ModelRequestInputs {
        session,
        config,
        capabilities,
        io,
        active_tool_names,
        context_store,
        state_ledger,
        sidecar_builder,
        turn_index,
        compaction_passes_this_turn,
        destructive_tiers_this_turn,
        suppress_summary_tier,
        allow_tool_call,
        cache_hints,
        loop_metrics,
    } = inputs;
    let session_messages: Vec<crate::message::AgentMessage> = session
        .messages
        .iter()
        .filter(|m| !m.metadata.is_superseded())
        .cloned()
        .collect();
    // State-ledger anchor intentionally NOT injected at position 0 of session_messages.
    // It would mutate byte 0 of the prefix on every turn (objective/decisions grow),
    // breaking xAI/OpenAI prompt-cache prefix stability. Ledger content is already
    // shipped via the tail-volatile sidecar (`stateLedgerJson` → `compactLedger`).

    // All renders below work on the session-parsed entries: no schema text is
    // parsed per turn. `rendered_runtime_tools` mirrors
    // `request_pipeline::render_runtime_tool_view` (FRG-36 gates) over the
    // parsed active channel; serialize at the edge. (ENC-297: catalog
    // presence comes from the typed `ToolCatalog` on the config, not from
    // schema-channel text.)
    let rendered_runtime_tools = io
        .schemas
        .render_runtime_view(config, capabilities, loop_metrics);

    // ENC-297 D5: the gate re-sources to typed catalog presence (non-empty
    // tools). Equivalent inputs keep exact gate semantics; the pinned delta
    // is that legacy catalog-channel text (e.g. a semantics-file '{}' object)
    // no longer counts as a catalog.
    let has_catalog = !config.tool_catalog.is_empty();

    // ENC-296: render from the active channel (registry deleted). Names the
    // active channel does not carry — tools activated mid-session — render
    // as name stubs; the host wires their full schemas provider-side.
    let active_schemas = io.schemas.render_active_schemas(active_tool_names);

    let tool_context_index = build_tool_context_index_from_set(
        &active_schemas,
        &config.tool_catalog,
        config,
        capabilities,
    );
    let tool_schemas = if tool_context_index.is_some() && !has_catalog {
        Vec::new()
    } else {
        active_schemas
    };

    let file_context_index =
        session
            .session_context
            .included_files_json()
            .and_then(|included_files_json| {
                condense_file_context(
                    &included_files_json,
                    context_store,
                    config,
                    capabilities,
                    turn_index,
                )
            });

    let generation = sidecar_builder.next_generation();
    let sidecar_budget = config.long_loop.sidecar_byte_budget_per_call;
    let ledger_delta = config
        .enable_state_ledger
        .then(|| sidecar_builder.build_ledger_delta(context_store, state_ledger, sidecar_budget))
        .flatten();
    let tool_index_delta = tool_context_index.as_ref().and_then(|index| {
        sidecar_builder.build_tool_index_payload(context_store, index, sidecar_budget)
    });
    let file_index_delta = file_context_index.as_ref().and_then(|index| {
        sidecar_builder.build_file_index_payload(context_store, index, sidecar_budget)
    });
    let sidecar_byte_len = ledger_delta.as_ref().map(|s| s.len()).unwrap_or(0)
        + tool_index_delta.as_ref().map(|s| s.len()).unwrap_or(0)
        + file_index_delta.as_ref().map(|s| s.len()).unwrap_or(0);
    sidecar_builder.record_rolling_cache_entry(generation, sidecar_byte_len);
    sidecar_builder.enforce_total_budget(&config.long_loop);

    ModelRequest {
        session_messages,
        system_prompt: io.system_prompt.clone(),
        rendered_runtime_tools,
        tool_schemas,
        allow_tool_call,
        context_references: context_store
            .entries()
            .filter(|entry| is_browsable_context_kind(&entry.reference.kind))
            .map(|entry| entry.reference.clone())
            .collect(),
        state_ledger: config.enable_state_ledger.then(|| state_ledger.clone()),
        tool_context_index,
        file_context_index,
        cache_hints,
        provider_context: ProviderContextSnapshot {
            transcript_revision: session.transcript_revision,
            sidecar_generation: generation,
            sidecar_byte_len: sidecar_byte_len.min(u32::MAX as usize) as u32,
            compaction_passes_this_turn,
            destructive_tiers_this_turn,
            pressure_ratio_at_call_model_millis: {
                let ratio = session
                    .ledger_snapshot
                    .as_ref()
                    .map(|p| p.ratio)
                    .unwrap_or_else(|| {
                        estimate_session_pressure(
                            session,
                            config,
                            &preflight_ctx_of(io, capabilities, suppress_summary_tier),
                        )
                        .ratio
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

fn preflight_ctx_of(
    io: &SessionIoParams,
    capabilities: &SessionCapabilities,
    suppress_summary_tier: bool,
) -> CompactionContext {
    CompactionContext {
        provider: io.provider.clone(),
        context_window_tokens: io.context_window_tokens,
        tools_schema_json: io.schemas.active_json().to_string(),
        system_prompt: io.system_prompt.clone(),
        context_sidecars_estimate: String::new(),
        capabilities: capabilities.clone(),
        trigger: CompactionTrigger::Preflight,
        suppress_summary_tier,
    }
}
