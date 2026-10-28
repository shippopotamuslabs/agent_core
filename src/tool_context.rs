//! Build a compact [`ToolContextIndex`] from the active tool schemas.
//!
//! When deferral is enabled and supported, a tiny per-tool summary list is
//! returned for the model to see. (ENC-296: the engine no longer stashes any
//! schema text in the [`crate::context_store::ContextStore`] — the registry
//! channel is deleted, the host owns the registry, and disclosure-time full
//! schemas are wired provider-side by the host. The index is
//! advertisement-only.)

use serde::{Deserialize, Serialize};

use crate::context_store::make_preview;
use crate::session::{AgentConfig, SessionCapabilities};
use crate::tool_schema::SchemaEntry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolContextSummary {
    pub name: String,
    pub description_preview: String,
    pub reproducible: bool,
    pub deferrable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_result_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolContextIndex {
    pub advertised_tools: Vec<ToolContextSummary>,
}

/// Build the tool-context index from the per-turn rendered active entries.
/// Performs no JSON text parsing: the session-parsed set is the only
/// input, so there is one (strict) error contract for schema data.
///
/// `active` is the rendered active/callable set — the sole source for
/// `advertised_tools`, so deferred tools stay hidden until disclosure.
/// `has_catalog_entries` is the catalog presence guard (a session with
/// neither active tools nor a catalog has nothing to index). Nothing is
/// stashed in the store (ENC-296): the host owns the registry and serves
/// full schemas provider-side, so there is no engine-side retrieval
/// reference to mint.
pub fn build_tool_context_index_from_set(
    active: &[SchemaEntry],
    has_catalog_entries: bool,
    config: &AgentConfig,
    capabilities: &SessionCapabilities,
) -> Option<ToolContextIndex> {
    if !config.enable_tool_schema_deferral || !capabilities.tool_schema_deferral_supported {
        return None;
    }

    // Presence guard: a session with neither active tools nor a catalog has
    // nothing to index. Nameless entries count as absent, matching the legacy
    // strict-struct parse (which failed closed on them).
    let advertised: Vec<&SchemaEntry> = active
        .iter()
        .filter(|entry| !entry.name.is_empty())
        .collect();
    if !has_catalog_entries && advertised.is_empty() {
        return None;
    }

    // Advertise only the active/callable tools. Deferred catalog tools stay hidden until
    // the model activates them through `tool_discovery_tool` (capability search).
    let summaries: Vec<ToolContextSummary> = advertised
        .into_iter()
        .map(|entry| ToolContextSummary {
            name: entry.name.clone(),
            description_preview: make_preview(entry.description()),
            reproducible: entry.reproducible(),
            deferrable: entry.deferrable(),
            max_result_bytes: entry.max_result_bytes(),
        })
        .collect();

    Some(ToolContextIndex {
        advertised_tools: summaries,
    })
}
