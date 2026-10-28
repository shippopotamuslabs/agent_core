use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::tool_name::ToolName;
use crate::tool_semantics::{ToolSemanticsConfig, ToolSemanticsError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldHint {
    pub name: String,
    pub ty: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchemaHint {
    pub required_fields: Vec<FieldHint>,
    pub defaults: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: ToolName,
    pub aliases: Vec<ToolName>,
    pub reproducible: bool,
    pub schema_hint: Option<ToolSchemaHint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
struct CatalogFile {
    tools: Vec<ToolSpec>,
}

/// Policy catalog: the validated executable tool set for a session (ENC-297).
///
/// Built once at config construction by [`AgentConfig::with_tool_semantics`](crate::session::AgentConfig::with_tool_semantics)
/// (via [`ToolCatalog::from_semantics`]) and rebuilt at session creation inside
/// [`ToolUniverse::build`](crate::tool_universe::ToolUniverse::build) — the
/// construction-time validation point; per-turn engine code borrows the
/// universe's catalog and never rebuilds it.
///
/// Identity is [`ToolName`] equality: specs and aliases are already canonical
/// at construction, so lookup performs no folding of its own (ENC-299: the
/// old per-site `normalize` fold is deleted; the fold lives in
/// [`ToolName::new`]).
#[derive(Debug, Clone, Default)]
pub struct ToolCatalog {
    tools: Vec<ToolSpec>,
    index: HashMap<ToolName, usize>,
}

impl ToolCatalog {
    pub(crate) fn from_specs(tools: Vec<ToolSpec>) -> Self {
        let mut index = HashMap::new();
        for (i, spec) in tools.iter().enumerate() {
            index.insert(spec.name.clone(), i);
            for alias in &spec.aliases {
                index.entry(alias.clone()).or_insert(i);
            }
        }
        ToolCatalog { tools, index }
    }

    pub fn from_semantics(config: &ToolSemanticsConfig) -> Result<ToolCatalog, ToolSemanticsError> {
        config.validate()?;
        Ok(ToolCatalog::from_specs(config.catalog.tools.clone()))
    }

    /// Returns the canonical `ToolSpec` for `name` or any registered alias.
    /// The caller passes an already-canonical [`ToolName`] (raw model output
    /// is canonicalized by the repair pipeline first).
    pub fn resolve(&self, name: &ToolName) -> Option<&ToolSpec> {
        self.index.get(name).map(|&i| &self.tools[i])
    }

    pub fn all(&self) -> &[ToolSpec] {
        &self.tools
    }

    /// Presence gate (ENC-297 D5): true when the host supplied at least one tool spec.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tn(name: &str) -> ToolName {
        ToolName::new(name).expect("test tool name must be valid")
    }

    fn fixture_catalog() -> ToolCatalog {
        let raw = include_str!("../resources/tool_catalog.json");
        let file: CatalogFile = serde_json::from_str(raw).expect("tool_catalog.json");
        ToolCatalog::from_specs(file.tools)
    }
    #[test]
    fn catalog_loads_20_tools() {
        assert_eq!(fixture_catalog().all().len(), 20);
    }

    #[test]
    fn discovery_tool_in_global_catalog_has_no_legacy_alias() {
        let catalog = fixture_catalog();
        let spec = catalog.resolve(&tn("tool_discovery_tool")).unwrap();
        assert!(spec.reproducible);
        let hint = spec.schema_hint.as_ref().unwrap();
        assert!(hint
            .required_fields
            .iter()
            .any(|field| field.name == "capability"));
        assert!(catalog.resolve(&tn("search_tools")).is_none());
    }

    #[test]
    fn resolve_canonical_name() {
        let cat = fixture_catalog();
        let spec = cat.resolve(&tn("web_search")).unwrap();
        assert_eq!(spec.name.as_str(), "web_search");
        assert!(spec.reproducible);
    }

    #[test]
    fn resolve_alias() {
        let cat = fixture_catalog();
        let spec = cat.resolve(&tn("websearch")).unwrap();
        assert_eq!(spec.name.as_str(), "web_search");
    }

    #[test]
    fn resolve_web_fetch_alias() {
        let cat = fixture_catalog();
        let spec = cat.resolve(&tn("web_fetch")).unwrap();
        assert_eq!(spec.name.as_str(), "web_extract");
    }

    #[test]
    fn resolve_unknown_returns_none() {
        assert!(fixture_catalog().resolve(&tn("does_not_exist")).is_none());
    }

    #[test]
    fn reproducible_classification() {
        let cat = fixture_catalog();
        let reproducible = [
            "tool_discovery_tool",
            "web_search",
            "web_extract",
            "attached_image_inspect",
            "search_chat_history",
            "search_chat",
            "get_message_context",
            "search_current_artifact",
            "search_x_recent",
        ];
        let non_reproducible = [
            "start_artifact",
            "append_blocks",
            "manage_memories",
            "generate_image",
            "generate_video",
            "generate_music",
        ];
        for name in &reproducible {
            assert!(
                cat.resolve(&tn(name)).unwrap().reproducible,
                "{name} should be reproducible"
            );
        }
        for name in &non_reproducible {
            assert!(
                !cat.resolve(&tn(name)).unwrap().reproducible,
                "{name} should not be reproducible"
            );
        }
    }

    #[test]
    fn start_artifact_schema_hint() {
        let cat = fixture_catalog();
        let spec = cat.resolve(&tn("start_artifact")).unwrap();
        let hint = spec.schema_hint.as_ref().unwrap();
        let fields: Vec<&str> = hint
            .required_fields
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(fields.contains(&"title"));
    }

    #[test]
    fn append_blocks_default_is_complete_false() {
        let cat = fixture_catalog();
        let spec = cat.resolve(&tn("append_blocks")).unwrap();
        let hint = spec.schema_hint.as_ref().unwrap();
        assert_eq!(
            hint.defaults.get("isComplete"),
            Some(&serde_json::Value::Bool(false))
        );
    }

    #[test]
    fn search_x_recent_schema_hint_covers_boolean_defaults() {
        let cat = fixture_catalog();
        let spec = cat.resolve(&tn("search_x_recent")).unwrap();
        let hint = spec.schema_hint.as_ref().unwrap();
        assert_eq!(
            hint.defaults.get("has_geo"),
            Some(&serde_json::Value::Bool(false))
        );
        assert_eq!(
            hint.defaults.get("has_media"),
            Some(&serde_json::Value::Bool(false))
        );
    }

    #[test]
    fn resolve_is_case_and_separator_insensitive() {
        // The (b)-fold lives in ToolName::new, not in resolve: lookup itself
        // performs no folding.
        let cat = fixture_catalog();
        assert!(cat.resolve(&tn("Web_Search")).is_some());
        assert!(cat.resolve(&tn("WEB-SEARCH")).is_some());
    }

    #[test]
    fn x_search_alias_resolves() {
        let cat = fixture_catalog();
        let spec = cat.resolve(&tn("x_search")).unwrap();
        assert_eq!(spec.name.as_str(), "search_x_recent");
    }
}
