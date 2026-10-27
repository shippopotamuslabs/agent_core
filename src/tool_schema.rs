//! Tool-schema channels parsed once per session (ENC-289).
//!
//! The host supplies three parallel JSON strings: the active (callable) tool
//! schemas, the full schema registry, and the render catalog. They used to
//! travel as raw strings on [`crate::engine::SessionIoParams`] (duplicated on
//! [`crate::turn_io::ModelRequest`]) and were re-parsed from text on every
//! turn in `engine/model_request.rs`, `tool_context.rs`, and
//! `request_pipeline.rs`.
//!
//! [`ToolSchemaSet::parse`] is now the single text-parse location: it runs
//! once at session construction ([`crate::engine::SessionIoParams::new`],
//! itself fed by the FFI session edge). Per-turn code works on the parsed
//! entries only — filtering and cloning [`serde_json::Value`]s, never parsing
//! text. Strings are re-serialized from the entries solely at the
//! FFI/provider edge ([`crate::turn_io::ModelRequest::tools_schema_json`],
//! [`crate::turn_io::ModelRequest::rendered_tool_catalog_json`], and the
//! `ffi::types` conversions).
//!
//! Byte-identity notes (all preserved by the engine test suite):
//! - Registry-filtered and runtime-view renders go through the same
//!   [`serde_json::Value`] filter-then-`to_string` shaping as before, so
//!   rendered bytes are unchanged for valid inputs.
//! - The untouched session-level raw strings back pressure/budget/stash paths
//!   ([`ToolSchemaSet::active_json`], [`ToolSchemaSet::registry_json`],
//!   [`ToolSchemaSet::catalog_json`]).
//! - Degenerate inputs (invalid JSON, non-array channels, nameless entries)
//!   previously echoed raw text or failed whole parses per turn; they now
//!   render as empty (`"[]"`) from the parsed form. No test covers those
//!   inputs; hosts always send arrays.

use serde::{Deserialize, Serialize};

use crate::loop_metrics::LoopMetrics;
use crate::session::{AgentConfig, SessionCapabilities};

/// A single tool-schema entry: the lossless parsed value plus its name.
///
/// The full [`serde_json::Value`] is retained so edge serialization is
/// byte-identical to the previous filter-then-`to_string` renders. Typed
/// views (description, deferral flags) read through accessors with the same
/// defaults the old strict struct parse applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "serde_json::Value", into = "serde_json::Value")]
pub struct SchemaEntry {
    /// Tool name from the entry's `"name"` field (`""` when absent/non-string).
    pub name: String,
    /// Full entry value, preserved losslessly.
    pub value: serde_json::Value,
}

/// Name extraction mirrors the legacy construction/render sites
/// (`parse_tool_names_from_schema`, `render_active_schema_json`, and the
/// strict tool-context struct parse): only objects with a string `"name"`
/// count. Bare strings keep `name` empty here — the runtime-view gates apply
/// the legacy backward-compat rule separately in `render_runtime_view`.
impl From<serde_json::Value> for SchemaEntry {
    fn from(value: serde_json::Value) -> Self {
        let name = value
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or_default()
            .to_string();
        Self { name, value }
    }
}

impl From<SchemaEntry> for serde_json::Value {
    fn from(entry: SchemaEntry) -> Self {
        entry.value
    }
}

impl SchemaEntry {
    /// Stub entry used when the registry does not carry an activated tool,
    /// matching the legacy `render_active_schema_json` stub shape.
    pub fn stub(name: &str) -> Self {
        Self {
            name: name.to_string(),
            value: serde_json::json!({"name": name}),
        }
    }

    pub fn description(&self) -> &str {
        self.value
            .get("description")
            .and_then(|description| description.as_str())
            .unwrap_or_default()
    }

    pub fn reproducible(&self) -> bool {
        self.value
            .get("reproducible")
            .and_then(|value| value.as_bool())
            .unwrap_or(true)
    }

    pub fn deferrable(&self) -> bool {
        self.value
            .get("deferrable")
            .and_then(|value| value.as_bool())
            .unwrap_or(true)
    }

    pub fn max_result_bytes(&self) -> Option<usize> {
        self.value
            .get("max_result_bytes")
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
    }
}

/// Serialize entries as a JSON array string (provider/FFI edge shaping).
pub fn entries_json(entries: &[SchemaEntry]) -> String {
    serde_json::Value::Array(entries.iter().map(|entry| entry.value.clone()).collect()).to_string()
}

/// The three tool-schema channels in parsed form.
///
/// Parsed exactly once per session by [`ToolSchemaSet::parse`]; per-turn code
/// borrows these entries and never parses schema text.
#[derive(Debug, Clone, Default)]
pub struct ToolSchemaSet {
    /// Currently active (callable) schemas — the `tools_schema_json` channel.
    pub active: Vec<SchemaEntry>,
    /// Full schema registry — the `tool_schema_registry_json` channel.
    pub registry: Vec<SchemaEntry>,
    /// Render catalog — the `tool_catalog_json` channel.
    pub catalog: Vec<SchemaEntry>,
    /// Untouched session-level raw text backing pressure/budget/stash/FFI
    /// paths, so those stay byte-identical.
    raw_active: String,
    raw_registry: String,
    raw_catalog: String,
}

fn parse_channel(json: &str) -> Vec<SchemaEntry> {
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(serde_json::Value::Array(items)) => items.into_iter().map(SchemaEntry::from).collect(),
        _ => Vec::new(),
    }
}

impl ToolSchemaSet {
    /// Parse the three schema channels. THE single schema text-parse
    /// location: called once per session at construction.
    pub fn parse(active_json: &str, registry_json: &str, catalog_json: &str) -> Self {
        Self {
            active: parse_channel(active_json),
            registry: parse_channel(registry_json),
            catalog: parse_channel(catalog_json),
            raw_active: active_json.to_string(),
            raw_registry: registry_json.to_string(),
            raw_catalog: catalog_json.to_string(),
        }
    }

    /// Build a set from already-parsed schema entries (the FFI typed
    /// boundary, ENC-295): the active/registry channels arrive as typed
    /// records, so no JSON text is produced or re-parsed for them; only
    /// the catalog channel still arrives as text and is parsed here. The
    /// raw text backing the entry channels is synthesized with
    /// [`entries_json`], byte-identical to the text the string path
    /// carried (same values, same serialization).
    pub fn from_entries(
        active: Vec<SchemaEntry>,
        registry: Vec<SchemaEntry>,
        catalog_json: &str,
    ) -> Self {
        let raw_active = entries_json(&active);
        let raw_registry = entries_json(&registry);
        Self {
            active,
            registry,
            catalog: parse_channel(catalog_json),
            raw_active,
            raw_registry,
            raw_catalog: catalog_json.to_string(),
        }
    }

    /// Session-level raw text of the active channel (pressure/budget/stash).
    pub fn active_json(&self) -> &str {
        &self.raw_active
    }

    /// Session-level raw text of the registry channel (stash/FFI edge).
    pub fn registry_json(&self) -> &str {
        &self.raw_registry
    }

    /// Session-level raw text of the catalog channel (semantics merge).
    pub fn catalog_json(&self) -> &str {
        &self.raw_catalog
    }

    /// Whether a render catalog is present (legacy string-shape semantics).
    pub fn has_catalog(&self) -> bool {
        !self.raw_catalog.trim().is_empty() && self.raw_catalog.trim() != "[]"
    }

    /// Whether a schema registry is present (legacy string-shape semantics).
    pub fn has_registry(&self) -> bool {
        !self.raw_registry.trim().is_empty() && self.raw_registry.trim() != "[]"
    }

    /// Whether the catalog channel carries at least one named entry.
    pub fn catalog_has_entries(&self) -> bool {
        self.catalog.iter().any(|entry| !entry.name.is_empty())
    }

    /// Initial active tool names, in channel order (replaces the
    /// session-construction `parse_tool_names_from_schema` parse).
    pub fn initial_active_names(&self) -> Vec<String> {
        self.active
            .iter()
            .filter(|entry| !entry.name.is_empty())
            .map(|entry| entry.name.clone())
            .collect()
    }

    /// Render the active-only schemas payload by filtering the parsed
    /// registry by name, preserving `active_names` order and stubbing tools
    /// the registry does not carry. Same shaping as the legacy
    /// `render_active_schema_json`; serialize with [`entries_json`].
    pub fn render_active_schemas(&self, active_names: &[String]) -> Vec<SchemaEntry> {
        let by_name: std::collections::HashMap<&str, &SchemaEntry> = self
            .registry
            .iter()
            .filter(|entry| !entry.name.is_empty())
            .map(|entry| (entry.name.as_str(), entry))
            .collect();
        active_names
            .iter()
            .map(|name| {
                by_name
                    .get(name.as_str())
                    .map(|entry| (*entry).clone())
                    .unwrap_or_else(|| SchemaEntry::stub(name))
            })
            .collect()
    }

    /// Render the runtime tool view over the parsed active channel, applying
    /// the same read-before-edit and delegation gates as
    /// `request_pipeline::render_runtime_tool_view` (FRG-36, FND-owned — that
    /// function keeps its own string-parsing public API and unit tests; this
    /// is the parse-free sibling for the per-turn engine path). Serialize
    /// with [`entries_json`].
    pub fn render_runtime_view(
        &self,
        config: &AgentConfig,
        capabilities: &SessionCapabilities,
        metrics: &LoopMetrics,
    ) -> Vec<SchemaEntry> {
        // Infallible by construction: entries were parsed once in
        // [`ToolSchemaSet::parse`]. Degenerate channels yield empty views.
        self.active
            .iter()
            .filter(|entry| {
                // Legacy `render_runtime_tool_view` naming rule, kept exact:
                // catalog entries are schema objects, but a bare string entry
                // is still honored for backward compatibility.
                let name: &str = match &entry.value {
                    serde_json::Value::String(s) => s.as_str(),
                    serde_json::Value::Object(_) => entry.name.as_str(),
                    _ => "",
                };
                // Remove patch_file and write_file when require_read_before_edit and no files read yet
                if config.require_read_before_edit
                    && metrics.files_read.is_empty()
                    && (name == "patch_file" || name == "write_file")
                {
                    return false;
                }
                // Remove task_delegate when delegation tools not supported
                if !capabilities.delegation_tools_supported && name == "task_delegate" {
                    return false;
                }
                true
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> String {
        format!(r#"{{"name":{name:?},"description":"d","parameters":{{"type":"object"}}}}"#)
    }

    #[test]
    fn parse_once_yields_all_three_channels() {
        let set = ToolSchemaSet::parse(
            &format!("[{}]", entry("web_search")),
            &format!("[{},{}]", entry("web_search"), entry("append_blocks")),
            "[]",
        );
        assert_eq!(set.initial_active_names(), vec!["web_search".to_string()]);
        assert!(!set.has_catalog());
        assert!(set.has_registry());
        assert_eq!(set.active_json(), &format!("[{}]", entry("web_search")));
    }

    #[test]
    fn degenerate_inputs_render_empty_not_raw() {
        let set = ToolSchemaSet::parse("{bogus", r#"{"a":1}"#, "[{}]");
        assert!(set.initial_active_names().is_empty());
        assert!(!set.catalog_has_entries());
        assert_eq!(entries_json(&set.active), "[]");
        assert_eq!(entries_json(&set.registry), "[]");
    }

    #[test]
    fn render_active_schemas_filters_registry_in_order_with_stubs() {
        let set = ToolSchemaSet::parse(
            "[]",
            &format!("[{},{}]", entry("b_tool"), entry("a_tool")),
            "[]",
        );
        let rendered =
            set.render_active_schemas(&["a_tool".to_string(), "missing_tool".to_string()]);
        // Full entry values are preserved (legacy Value filter-then-stringify
        // shaping, hence sorted keys); missing tools become name stubs.
        assert_eq!(
            entries_json(&rendered),
            r#"[{"description":"d","name":"a_tool","parameters":{"type":"object"}},{"name":"missing_tool"}]"#
        );
    }

    #[test]
    fn runtime_view_gates_match_request_pipeline() {
        let config = AgentConfig {
            require_read_before_edit: true,
            ..AgentConfig::default()
        };
        let set = ToolSchemaSet::parse(
            &format!("[{},{}]", entry("read_file"), entry("patch_file")),
            "[]",
            "[]",
        );
        let rendered = set.render_runtime_view(
            &config,
            &SessionCapabilities::default(),
            &LoopMetrics::default(),
        );
        let json = entries_json(&rendered);
        assert!(json.contains("read_file"));
        assert!(!json.contains("patch_file"));
    }

    #[test]
    fn runtime_view_gates_fire_on_bare_string_entries() {
        // fcg_011/fcg_016 shape: bare strings, not schema objects.
        let set =
            ToolSchemaSet::parse(r#"["web_search","patch_file","task_delegate"]"#, "[]", "[]");
        let gated = AgentConfig {
            require_read_before_edit: true,
            ..AgentConfig::default()
        };
        let rendered = set.render_runtime_view(
            &gated,
            &SessionCapabilities::default(),
            &LoopMetrics::default(),
        );
        assert_eq!(entries_json(&rendered), r#"["web_search"]"#);
        // Bare strings never become active tool names (legacy construction
        // rule): they stay invisible to activation/registry logic.
        assert!(set.initial_active_names().is_empty());
    }

    #[test]
    fn schema_entry_roundtrip_is_lossless() {
        let raw = entry("web_search");
        let parsed: SchemaEntry = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.name, "web_search");
        assert_eq!(parsed.description(), "d");
        assert!(parsed.reproducible());
        assert!(parsed.deferrable());
        let json = serde_json::to_string(&parsed).unwrap();
        // Value round-trip normalizes key order (sorted), matching the legacy
        // filter-then-stringify renders that consumed these entries.
        assert_eq!(
            json,
            r#"{"description":"d","name":"web_search","parameters":{"type":"object"}}"#
        );
        assert!(raw.contains("\"name\":\"web_search\""));
    }
}
