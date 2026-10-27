//! Build a compact [`ToolContextIndex`] from full tool-schema JSON.
//!
//! When deferral is enabled and supported, the full schema JSON is stashed in
//! the [`crate::context_store::ContextStore`] as a [`crate::context_store::ContextReferenceKind::ToolSchema`]
//! reference, and a tiny per-tool summary list is returned for the model to see.

use serde::{Deserialize, Serialize};

use crate::context_store::{
    make_preview, ContextReference, ContextReferenceKind, ContextStore, ContextStoreEntry,
};
use crate::session::{AgentConfig, SessionCapabilities};
use crate::tool_schema::{SchemaEntry, ToolSchemaSet};

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
    pub full_schema_ref: String,
    pub advertised_tools: Vec<ToolContextSummary>,
}

pub fn build_tool_context_index(
    tools_schema_json: &str,
    _catalog_json: &str,
    store: &mut ContextStore,
    config: &AgentConfig,
    capabilities: &SessionCapabilities,
) -> Option<ToolContextIndex> {
    build_tool_context_index_with_registry(
        tools_schema_json,
        "[]",
        "[]",
        store,
        config,
        capabilities,
    )
}

/// Build the tool-context index using the tool-discovery split:
/// - `tools_schema_json` represents the currently active (callable) schemas and is the
///   sole source for `advertised_tools` — discovery must be real, so deferred tools are
///   never listed up front and can only be surfaced via `tool_discovery_tool`.
/// - `catalog_json` is retained for the presence guard (a session with neither active
///   tools nor a catalog has nothing to index).
/// - `registry_json` supplies the full schema registry (persisted behind `full_schema_ref`)
///   so an activated tool's full schema can be retrieved on disclosure.
pub fn build_tool_context_index_with_registry(
    tools_schema_json: &str,
    catalog_json: &str,
    registry_json: &str,
    store: &mut ContextStore,
    config: &AgentConfig,
    capabilities: &SessionCapabilities,
) -> Option<ToolContextIndex> {
    // String-based public API preserved: parses its own input once at this
    // boundary (a justified remaining parse — see ENC-289 notes). The
    // per-turn engine path calls `build_tool_context_index_from_set` with the
    // session-parsed entries and performs no text parsing.
    //
    // This wrapper is infallible by design (a preserved public boundary
    // API, not session construction), so a contract violation falls back
    // to an empty set here; session construction is the validating path.
    let schemas = ToolSchemaSet::parse(tools_schema_json, registry_json, catalog_json)
        .unwrap_or_default();
    let active = schemas.active.clone();
    build_tool_context_index_from_set(&active, &schemas, store, config, capabilities)
}

/// Build the tool-context index from the session-parsed [`ToolSchemaSet`]
/// plus the per-turn rendered active entries. Performs no JSON text parsing.
///
/// `active` is the rendered active/callable set (registry-filtered when a
/// registry is present, else the session active channel) — the sole source
/// for `advertised_tools`, so deferred tools stay hidden until disclosure.
/// `schemas` supplies the catalog presence guard and the raw stash text.
pub fn build_tool_context_index_from_set(
    active: &[SchemaEntry],
    schemas: &ToolSchemaSet,
    store: &mut ContextStore,
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
    if !schemas.catalog_has_entries() && advertised.is_empty() {
        return None;
    }

    // Persist the registry (full schemas) when supplied; otherwise fall back to the
    // legacy single-channel persistence of the active channel. The untouched
    // session raw text is stashed so reads stay byte-identical.
    let stash_content = if schemas.has_registry() {
        schemas.registry_json().to_string()
    } else {
        schemas.active_json().to_string()
    };
    let existing_ref = store
        .entries()
        .find(|entry| {
            entry.reference.kind == ContextReferenceKind::ToolSchema
                && entry.content == stash_content
        })
        .map(|entry| entry.reference.clone());

    // Title names come from the stashed channel's parsed entries (registry
    // when present, else the rendered active set).
    let title_source: &[SchemaEntry] = if schemas.has_registry() {
        &schemas.registry
    } else {
        active
    };
    let stored = match existing_ref {
        Some(r) => r,
        None => {
            let seq = store.next_sequence();
            let id = format!("tool-schema-{seq}");
            let preview = make_preview(&stash_content);
            let tool_names: Vec<String> = title_source
                .iter()
                .filter(|entry| !entry.name.is_empty())
                .map(|entry| entry.name.clone())
                .collect();
            let title = if tool_names.is_empty() {
                "tool_schema_full".to_string()
            } else if tool_names.len() <= 3 {
                format!("tool_schema: {}", tool_names.join(", "))
            } else {
                let first_three = tool_names[..3].join(", ");
                let last = tool_names.last().unwrap();
                let count = tool_names.len();
                format!("tool_schema ({count} tools): {first_three}, ... {last}")
            };
            let reference = ContextReference {
                id: id.clone(),
                kind: ContextReferenceKind::ToolSchema,
                title,
                byte_len: stash_content.len() as u64,
                token_estimate: (stash_content.len() / 4) as u32,
                preview,
                retrieval_hint: format!("read_context_reference(\"{id}\")"),
            };
            let entry = ContextStoreEntry {
                reference: reference.clone(),
                content: stash_content,
                source_message_ids: vec![],
                created_turn_index: 0,
                expires_after_turn: None,
            };
            store.insert_entry(entry)
        }
    };

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
        full_schema_ref: stored.id,
        advertised_tools: summaries,
    })
}
