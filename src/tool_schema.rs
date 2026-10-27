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
//! - Degenerate inputs (invalid JSON, non-array schema channels, nameless
//!   or duplicate entries) are rejected at session construction:
//!   [`ToolSchemaSet::parse`] returns a [`ToolSchemaError`] and no session
//!   is created. Hosts always send arrays of named schema objects; anything
//!   else is a contract violation, not a render case.

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
    ///
    /// Construction through [`ToolSchemaSet::parse`] validates the name
    /// (present, a string, non-empty, unique per channel) before an entry is
    /// built; direct serde construction keeps this lenient extraction.
    pub name: String,
    /// Full entry value, preserved losslessly.
    pub value: serde_json::Value,
}

/// Name extraction mirrors the legacy construction/render sites
/// (`parse_tool_names_from_schema`, `render_active_schema_json`, and the
/// strict tool-context struct parse): only objects with a string `"name"`
/// count. Bare-string entries extract no name here and are rejected by
/// [`ToolSchemaSet::parse`] as non-object entries.
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

/// Why a tool-schema channel was rejected at session construction.
///
/// The schema channels are a contract between the host and the engine:
/// arrays of schema objects, each carrying a valid, unique `"name"`.
/// [`ToolSchemaSet::parse`] enforces that contract once, at the boundary,
/// and the error surfaces to the caller (through `AgentError` on the
/// session-construction paths) instead of degrading silently per turn.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolSchemaError {
    /// Channel text is not valid JSON.
    #[error("tool schema channel {channel} is not valid JSON: {message}")]
    InvalidJson {
        channel: &'static str,
        message: String,
    },
    /// Channel is valid JSON but its top level is not an array.
    #[error("tool schema channel {channel} must be a JSON array of tool schema objects")]
    NotAnArray { channel: &'static str },
    /// An entry in the channel array is not a JSON object.
    #[error("tool schema channel {channel} entry {index} is not a schema object")]
    EntryNotObject {
        channel: &'static str,
        index: usize,
    },
    /// An entry's `"name"` is missing, not a string, or empty/whitespace.
    #[error(
        "tool schema channel {channel} entry {index} has no valid \"name\" \
         (missing, not a string, or empty)"
    )]
    InvalidName {
        channel: &'static str,
        index: usize,
    },
    /// Two entries in the same channel carry the same `"name"`.
    #[error("tool schema channel {channel} contains duplicate tool name \"{name}\"")]
    DuplicateName {
        channel: &'static str,
        name: String,
    },
}

/// Channel labels used in [`ToolSchemaError`], matching the legacy
/// `SessionIoParams` JSON field names the host supplies.
const ACTIVE_CHANNEL: &str = "tools_schema_json";
const REGISTRY_CHANNEL: &str = "tool_schema_registry_json";
const CATALOG_CHANNEL: &str = "tool_catalog_json";

/// Validate parsed channel entries: each must be an object carrying a
/// present, string, non-empty `"name"`, unique within the channel.
fn validate_entries(
    channel: &'static str,
    items: Vec<serde_json::Value>,
) -> Result<Vec<SchemaEntry>, ToolSchemaError> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut entries = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        if !item.is_object() {
            return Err(ToolSchemaError::EntryNotObject { channel, index });
        }
        let name = item
            .get("name")
            .and_then(|name| name.as_str())
            .filter(|name| !name.trim().is_empty())
            .ok_or(ToolSchemaError::InvalidName { channel, index })?;
        if !seen.insert(name.to_string()) {
            return Err(ToolSchemaError::DuplicateName {
                channel,
                name: name.to_string(),
            });
        }
        entries.push(SchemaEntry::from(item));
    }
    Ok(entries)
}

/// Parse one provider-schema channel (active / registry) strictly: the
/// text must be a JSON array of validly named schema objects. Empty or
/// whitespace-only text is the legacy "channel absent" case and yields no
/// entries.
fn parse_schema_channel(
    channel: &'static str,
    json: &str,
) -> Result<Vec<SchemaEntry>, ToolSchemaError> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value = serde_json::from_str(json).map_err(|error| {
        ToolSchemaError::InvalidJson {
            channel,
            message: error.to_string(),
        }
    })?;
    match value {
        serde_json::Value::Array(items) => validate_entries(channel, items),
        _ => Err(ToolSchemaError::NotAnArray { channel }),
    }
}

/// Parse the catalog channel. An array is validated exactly like the
/// provider-schema channels; a JSON object is the semantics file form,
/// which carries no schema entries on this channel (the typed semantics
/// parse consumes it separately) and is accepted as-is.
fn parse_catalog_channel(json: &str) -> Result<Vec<SchemaEntry>, ToolSchemaError> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value = serde_json::from_str(json).map_err(|error| {
        ToolSchemaError::InvalidJson {
            channel: CATALOG_CHANNEL,
            message: error.to_string(),
        }
    })?;
    match value {
        serde_json::Value::Array(items) => validate_entries(CATALOG_CHANNEL, items),
        serde_json::Value::Object(_) => Ok(Vec::new()),
        _ => Err(ToolSchemaError::NotAnArray {
            channel: CATALOG_CHANNEL,
        }),
    }
}

impl ToolSchemaSet {
    /// Parse the three schema channels, enforcing the host contract.
    /// THE single schema text-parse location: called once per session at
    /// construction. Returns [`ToolSchemaError`] on the first contract
    /// violation; no partial set is produced.
    pub fn parse(
        active_json: &str,
        registry_json: &str,
        catalog_json: &str,
    ) -> Result<Self, ToolSchemaError> {
        Ok(Self {
            active: parse_schema_channel(ACTIVE_CHANNEL, active_json)?,
            registry: parse_schema_channel(REGISTRY_CHANNEL, registry_json)?,
            catalog: parse_catalog_channel(catalog_json)?,
            raw_active: active_json.to_string(),
            raw_registry: registry_json.to_string(),
            raw_catalog: catalog_json.to_string(),
        })
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
        // Infallible by construction: entries were parsed and validated
        // once in [`ToolSchemaSet::parse`], so every entry here is a schema
        // object whose `entry.name` is its gate identity. There is no
        // bare-string naming rule: non-object entries never reach this set.
        self.active
            .iter()
            .filter(|entry| {
                let name: &str = entry.name.as_str();
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
        )
        .expect("valid channels");
        assert_eq!(set.initial_active_names(), vec!["web_search".to_string()]);
        assert!(!set.has_catalog());
        assert!(set.has_registry());
        assert_eq!(set.active_json(), &format!("[{}]", entry("web_search")));
    }

    #[test]
    fn degenerate_inputs_are_rejected_at_parse() {
        // Invalid JSON text.
        assert!(matches!(
            ToolSchemaSet::parse("{bogus", "[]", "[]"),
            Err(ToolSchemaError::InvalidJson {
                channel: "tools_schema_json",
                ..
            })
        ));
        // Valid JSON, wrong top level for a provider-schema channel.
        assert!(matches!(
            ToolSchemaSet::parse("[]", r#"{"a":1}"#, "[]"),
            Err(ToolSchemaError::NotAnArray {
                channel: "tool_schema_registry_json"
            })
        ));
        // Entry without a name, and entry with a whitespace-only name.
        assert!(matches!(
            ToolSchemaSet::parse("[{}]", "[]", "[]"),
            Err(ToolSchemaError::InvalidName {
                channel: "tools_schema_json",
                index: 0
            })
        ));
        assert!(matches!(
            ToolSchemaSet::parse(r#"[{"name":"  "}]"#, "[]", "[]"),
            Err(ToolSchemaError::InvalidName { index: 0, .. })
        ));
        // Duplicate names within one channel.
        let duplicates = format!("[{},{}]", entry("dup_tool"), entry("dup_tool"));
        assert!(matches!(
            ToolSchemaSet::parse(&duplicates, "[]", "[]"),
            Err(ToolSchemaError::DuplicateName { name, .. }) if name == "dup_tool"
        ));
        // Empty / whitespace-only text is the absent-channel case: Ok.
        let set = ToolSchemaSet::parse("", "  ", "").expect("absent channels");
        assert!(set.initial_active_names().is_empty());
        assert_eq!(entries_json(&set.active), "[]");
        // Catalog object form (semantics file) is accepted, yields no entries.
        let set = ToolSchemaSet::parse("[]", "[]", r#"{"tools":[]}"#).expect("catalog object");
        assert!(set.catalog.is_empty());
        assert!(!set.catalog_has_entries());
        // A catalog array is validated like the schema channels.
        let set = ToolSchemaSet::parse("[]", "[]", &format!("[{}]", entry("cat_tool")))
            .expect("catalog array");
        assert_eq!(set.catalog.len(), 1);
        assert!(set.catalog_has_entries());
    }

    #[test]
    fn bare_string_entries_are_rejected_at_parse() {
        // The fcg_011/fcg_016 fixture shape (bare strings, not schema
        // objects) is a contract violation now, caught at parse instead of
        // being honored by the runtime-view gates.
        assert!(matches!(
            ToolSchemaSet::parse(r#"["web_search","patch_file"]"#, "[]", "[]"),
            Err(ToolSchemaError::EntryNotObject {
                channel: "tools_schema_json",
                index: 0
            })
        ));
    }

    #[test]
    fn render_active_schemas_filters_registry_in_order_with_stubs() {
        let set = ToolSchemaSet::parse(
            "[]",
            &format!("[{},{}]", entry("b_tool"), entry("a_tool")),
            "[]",
        )
        .expect("valid channels");
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
        )
        .expect("valid channels");
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
