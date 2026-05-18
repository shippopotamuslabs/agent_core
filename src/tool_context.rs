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

#[derive(Debug, Deserialize)]
struct SchemaEntry {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    reproducible: Option<bool>,
    #[serde(default)]
    deferrable: Option<bool>,
    #[serde(default)]
    max_result_bytes: Option<usize>,
}

pub fn build_tool_context_index(
    tools_schema_json: &str,
    _catalog_json: &str,
    store: &mut ContextStore,
    config: &AgentConfig,
    capabilities: &SessionCapabilities,
) -> Option<ToolContextIndex> {
    if !config.enable_tool_schema_deferral || !capabilities.tool_schema_deferral_supported {
        return None;
    }
    let entries: Vec<SchemaEntry> = serde_json::from_str(tools_schema_json).ok()?;
    if entries.is_empty() {
        return None;
    }

    // Persist the full schema JSON behind one ToolSchema reference.
    let seq = store.next_sequence();
    let id = format!("tool-schema-{seq}");
    let preview = make_preview(tools_schema_json);
    let reference = ContextReference {
        id: id.clone(),
        kind: ContextReferenceKind::ToolSchema,
        title: "tool_schema_full".into(),
        byte_len: tools_schema_json.len() as u64,
        token_estimate: (tools_schema_json.len() / 4) as u32,
        preview,
        retrieval_hint: format!("read_context_reference(\"{id}\")"),
    };
    let entry = ContextStoreEntry {
        reference: reference.clone(),
        content: tools_schema_json.to_string(),
        source_message_ids: vec![],
        created_turn_index: 0,
        expires_after_turn: None,
    };
    let stored = store.insert_entry(entry);

    let summaries: Vec<ToolContextSummary> = entries
        .into_iter()
        .map(|e| ToolContextSummary {
            name: e.name,
            description_preview: make_preview(&e.description),
            reproducible: e.reproducible.unwrap_or(true),
            deferrable: e.deferrable.unwrap_or(true),
            max_result_bytes: e.max_result_bytes,
        })
        .collect();

    Some(ToolContextIndex {
        full_schema_ref: stored.id,
        advertised_tools: summaries,
    })
}
