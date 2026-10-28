//! Tool-schema channel parsed once per session (ENC-289; registry removed ENC-296,
//! catalog channel removed ENC-297).
//!
//! The host supplies one JSON string: the active (callable) tool schemas.
//! (ENC-296 deleted the former full schema registry channel: the host owns
//! the registry now and wires full schemas provider-side, so the engine never
//! sees, stores, or renders from it. ENC-297 deleted the render-catalog
//! channel: catalog presence now comes from the typed policy
//! [`crate::catalog::ToolCatalog`] on [`crate::session::AgentConfig`], and
//! the runtime view renders from the active channel.)
//! It used to travel as a raw string on [`crate::engine::SessionIoParams`]
//! (duplicated on [`crate::turn_io::ModelRequest`]) and was re-parsed from
//! text on every turn in `engine/model_request.rs`, `tool_context.rs`, and
//! `request_pipeline.rs`.
//!
//! [`ToolSchemaSet::parse`] is now the single text-parse location: it runs
//! once at session construction ([`crate::engine::SessionIoParams::new`],
//! itself fed by the FFI session edge). Per-turn code works on the parsed
//! entries only — filtering and cloning [`serde_json::Value`]s, never parsing
//! text. Strings are re-serialized from the entries solely at the
//! FFI/provider edge ([`crate::turn_io::ModelRequest::tools_schema_json`],
//! [`crate::turn_io::ModelRequest::rendered_runtime_tools_json`], and the
//! `ffi::types` conversions).
//!
//! Byte-identity notes (all preserved by the engine test suite):
//! - Active-channel and runtime-view renders go through the same
//!   [`serde_json::Value`] filter-then-`to_string` shaping as before, so
//!   rendered bytes are unchanged for valid inputs.
//! - The untouched session-level raw string backs pressure/budget paths
//!   ([`ToolSchemaSet::active_json`]).
//! - Degenerate inputs (invalid JSON, non-array schema channel, nameless
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
    /// Stub entry used when the active channel does not carry an activated
    /// tool (ENC-296: the registry lookup is gone; the host wires the full
    /// schema provider-side), matching the legacy `render_active_schema_json`
    /// stub shape.
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

/// The active tool-schema channel in parsed form (ENC-296: the former full
/// schema registry channel is deleted, the host owns the registry; ENC-297:
/// the render-catalog channel is deleted, catalog presence comes from the
/// typed [`crate::catalog::ToolCatalog`] on [`crate::session::AgentConfig`]).
///
/// Parsed exactly once per session by [`ToolSchemaSet::parse`]; per-turn code
/// borrows these entries and never parses schema text.
#[derive(Debug, Clone, Default)]
pub struct ToolSchemaSet {
    /// Currently active (callable) schemas — the `tools_schema_json` channel.
    pub active: Vec<SchemaEntry>,
    /// Untouched session-level raw text backing pressure/budget/FFI
    /// paths, so those stay byte-identical.
    raw_active: String,
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
    EntryNotObject { channel: &'static str, index: usize },
    /// An entry's `"name"` is missing, not a string, or empty/whitespace.
    #[error(
        "tool schema channel {channel} entry {index} has no valid \"name\" \
         (missing, not a string, or empty)"
    )]
    InvalidName { channel: &'static str, index: usize },
    /// Two entries in the same channel carry the same `"name"`.
    #[error("tool schema channel {channel} contains duplicate tool name \"{name}\"")]
    DuplicateName { channel: &'static str, name: String },
}

/// Channel label used in [`ToolSchemaError`], matching the legacy
/// `SessionIoParams` JSON field name the host supplies. (ENC-296: the
/// `tool_schema_registry_json` channel label is deleted with the registry;
/// ENC-297: the `tool_catalog_json` channel label is deleted with the
/// render-catalog channel.)
const ACTIVE_CHANNEL: &str = "tools_schema_json";

/// Validate parsed channel entries: each must be an object carrying a
/// present, string, non-empty `"name"`, unique within the channel.
/// The name half of the entry contract — present, non-blank after trim,
/// unique within the channel — checked identically for the JSON-text
/// channels and the typed FFI entries, so the two boundary paths can
/// never drift on what a valid name is. Callers map the failure onto
/// their own boundary error type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EntryNameError {
    InvalidName { index: usize },
    DuplicateName { name: String },
}

/// Stateful checker for one channel's entry sequence: feed each
/// entry's name in order.
#[derive(Debug, Default)]
pub(crate) struct EntryNameChecker {
    seen: std::collections::HashSet<String>,
}

impl EntryNameChecker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn check(&mut self, index: usize, name: &str) -> Result<(), EntryNameError> {
        if name.trim().is_empty() {
            return Err(EntryNameError::InvalidName { index });
        }
        if !self.seen.insert(name.to_string()) {
            return Err(EntryNameError::DuplicateName {
                name: name.to_string(),
            });
        }
        Ok(())
    }
}

fn validate_entries(
    channel: &'static str,
    items: Vec<serde_json::Value>,
) -> Result<Vec<SchemaEntry>, ToolSchemaError> {
    let mut names = EntryNameChecker::new();
    let mut entries = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        if !item.is_object() {
            return Err(ToolSchemaError::EntryNotObject { channel, index });
        }
        let name = item
            .get("name")
            .and_then(|name| name.as_str())
            .ok_or(ToolSchemaError::InvalidName { channel, index })?;
        names.check(index, name).map_err(|error| match error {
            EntryNameError::InvalidName { index } => {
                ToolSchemaError::InvalidName { channel, index }
            }
            EntryNameError::DuplicateName { name } => {
                ToolSchemaError::DuplicateName { channel, name }
            }
        })?;
        entries.push(SchemaEntry::from(item));
    }
    Ok(entries)
}

/// Parse the active provider-schema channel strictly: the text must be a
/// JSON array of validly named schema objects. Empty or whitespace-only
/// text is the legacy "channel absent" case and yields no entries.
fn parse_schema_channel(
    channel: &'static str,
    json: &str,
) -> Result<Vec<SchemaEntry>, ToolSchemaError> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| ToolSchemaError::InvalidJson {
            channel,
            message: error.to_string(),
        })?;
    match value {
        serde_json::Value::Array(items) => validate_entries(channel, items),
        _ => Err(ToolSchemaError::NotAnArray { channel }),
    }
}

impl ToolSchemaSet {
    /// Parse the active schema channel, enforcing the host contract.
    /// THE single schema text-parse location: called once per session at
    /// construction. Returns [`ToolSchemaError`] on the first contract
    /// violation; no partial set is produced.
    pub fn parse(active_json: &str) -> Result<Self, ToolSchemaError> {
        Ok(Self {
            active: parse_schema_channel(ACTIVE_CHANNEL, active_json)?,
            raw_active: active_json.to_string(),
        })
    }

    /// Parse the active channel leniently: a contract violation degrades to
    /// an empty channel (no entries, raw text normalized to `"[]"`).
    ///
    /// This is the lenient sibling for the infallible string-boundary
    /// render helper in `tool_context`. Session construction uses the strict
    /// [`ToolSchemaSet::parse`] and fails hard on any violation.
    pub fn parse_lenient(active_json: &str) -> Self {
        let (active, raw_active) = match parse_schema_channel(ACTIVE_CHANNEL, active_json) {
            Ok(entries) => (entries, active_json.to_string()),
            Err(_) => (Vec::new(), "[]".to_string()),
        };
        Self { active, raw_active }
    }

    /// Build a set from already-parsed schema entries (the FFI typed
    /// boundary, ENC-295): the active channel arrives as typed records,
    /// so no JSON text is produced or re-parsed for it.
    /// The raw text backing the entry channel is synthesized with
    /// [`entries_json`], byte-identical to the text the string path
    /// carried (same values, same serialization).
    pub fn from_entries(active: Vec<SchemaEntry>) -> Self {
        let raw_active = entries_json(&active);
        Self { active, raw_active }
    }

    /// Session-level raw text of the active channel (pressure/budget).
    pub fn active_json(&self) -> &str {
        &self.raw_active
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

    /// Render the active-only schemas payload by looking each name up in
    /// the parsed active channel, preserving `active_names` order and
    /// stubbing tools the active channel does not carry (ENC-296: the host
    /// owns the registry and wires full schemas provider-side, so an
    /// activated-but-not-active tool renders as a name stub here). Same
    /// shaping as the legacy `render_active_schema_json`; serialize with
    /// [`entries_json`].
    pub fn render_active_schemas(&self, active_names: &[String]) -> Vec<SchemaEntry> {
        let by_name: std::collections::HashMap<&str, &SchemaEntry> = self
            .active
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
        // The gate policy itself is shared with
        // `request_pipeline::render_runtime_tool_view` via
        // [`runtime_gate_allows`], so the two renders cannot drift.
        self.active
            .iter()
            .filter(|entry| runtime_gate_allows(entry.name.as_str(), config, capabilities, metrics))
            .cloned()
            .collect()
    }
}

/// The FRG-36 runtime-view gate policy, shared by
/// [`ToolSchemaSet::render_runtime_view`] (parsed entries, per-turn path)
/// and `request_pipeline::render_runtime_tool_view` (raw catalog text,
/// string boundary): edit tools stay hidden until something has been
/// read when `require_read_before_edit` is on, and the delegation tool
/// stays hidden when the session does not support delegation.
pub fn runtime_gate_allows(
    name: &str,
    config: &AgentConfig,
    capabilities: &SessionCapabilities,
    metrics: &LoopMetrics,
) -> bool {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> String {
        format!(r#"{{"name":{name:?},"description":"d","parameters":{{"type":"object"}}}}"#)
    }

    #[test]
    fn parse_once_yields_active_channel() {
        // ENC-296: the registry channel is deleted and the host owns the
        // registry; ENC-297: the render-catalog channel is deleted and
        // catalog presence comes from the typed ToolCatalog.
        let set =
            ToolSchemaSet::parse(&format!("[{}]", entry("web_search"))).expect("valid channel");
        assert_eq!(set.initial_active_names(), vec!["web_search".to_string()]);
        assert_eq!(set.active_json(), &format!("[{}]", entry("web_search")));
    }

    #[test]
    fn degenerate_inputs_are_rejected_at_parse() {
        // Invalid JSON text.
        assert!(matches!(
            ToolSchemaSet::parse("{bogus"),
            Err(ToolSchemaError::InvalidJson {
                channel: "tools_schema_json",
                ..
            })
        ));
        // Valid JSON, wrong top level for the active channel.
        assert!(matches!(
            ToolSchemaSet::parse(r#"{"a":1}"#),
            Err(ToolSchemaError::NotAnArray {
                channel: "tools_schema_json"
            })
        ));
        // Entry without a name, and entry with a whitespace-only name.
        assert!(matches!(
            ToolSchemaSet::parse("[{}]"),
            Err(ToolSchemaError::InvalidName {
                channel: "tools_schema_json",
                index: 0
            })
        ));
        assert!(matches!(
            ToolSchemaSet::parse(r#"[{"name":"  "}]"#),
            Err(ToolSchemaError::InvalidName { index: 0, .. })
        ));
        // Duplicate names within the channel.
        let duplicates = format!("[{},{}]", entry("dup_tool"), entry("dup_tool"));
        assert!(matches!(
            ToolSchemaSet::parse(&duplicates),
            Err(ToolSchemaError::DuplicateName { name, .. }) if name == "dup_tool"
        ));
        // Empty / whitespace-only text is the absent-channel case: Ok.
        let set = ToolSchemaSet::parse("").expect("absent channel");
        assert!(set.initial_active_names().is_empty());
        assert_eq!(entries_json(&set.active), "[]");
    }

    #[test]
    fn parse_lenient_degrades_bad_channel_to_empty() {
        let set = ToolSchemaSet::parse_lenient(&format!("[{}]", entry("web_search")));
        assert_eq!(set.initial_active_names(), vec!["web_search".to_string()]);
        // A malformed active channel degrades to empty (raw text normalized).
        let set = ToolSchemaSet::parse_lenient("{bogus");
        assert!(set.active.is_empty());
        assert_eq!(set.active_json(), "[]");
    }

    #[test]
    fn bare_string_entries_are_rejected_at_parse() {
        // The fcg_011/fcg_016 fixture shape (bare strings, not schema
        // objects) is a contract violation now, caught at parse instead of
        // being honored by the runtime-view gates.
        assert!(matches!(
            ToolSchemaSet::parse(r#"["web_search","patch_file"]"#),
            Err(ToolSchemaError::EntryNotObject {
                channel: "tools_schema_json",
                index: 0
            })
        ));
    }

    #[test]
    fn render_active_schemas_reads_active_channel_in_order_with_stubs() {
        // ENC-296: render sources the active channel (registry deleted); a
        // name missing from active renders as a name stub for the host to
        // fill provider-side.
        let set = ToolSchemaSet::parse(&format!("[{},{}]", entry("b_tool"), entry("a_tool")))
            .expect("valid channel");
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
        let set =
            ToolSchemaSet::parse(&format!("[{},{}]", entry("read_file"), entry("patch_file")))
                .expect("valid channel");
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
