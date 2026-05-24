use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub name: String,
    pub aliases: Vec<String>,
    pub reproducible: bool,
    pub schema_hint: Option<ToolSchemaHint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CatalogFile {
    tools: Vec<ToolSpec>,
}

pub struct ToolCatalog {
    tools: Vec<ToolSpec>,
    index: HashMap<String, usize>,
}

static CATALOG: Lazy<ToolCatalog> = Lazy::new(|| {
    let raw = include_str!("../resources/tool_catalog.json");
    let file: CatalogFile = serde_json::from_str(raw).expect("tool_catalog.json is malformed");
    ToolCatalog::from_specs(file.tools)
});

impl ToolCatalog {
    fn from_specs(tools: Vec<ToolSpec>) -> Self {
        let mut index = HashMap::new();
        for (i, spec) in tools.iter().enumerate() {
            index.insert(normalize(&spec.name), i);
            for alias in &spec.aliases {
                index.entry(normalize(alias)).or_insert(i);
            }
        }
        ToolCatalog { tools, index }
    }

    pub fn global() -> &'static ToolCatalog {
        &CATALOG
    }

    /// Returns the canonical `ToolSpec` for `name` or any registered alias.
    pub fn resolve(&self, name: &str) -> Option<&ToolSpec> {
        self.index.get(&normalize(name)).map(|&i| &self.tools[i])
    }

    pub fn all(&self) -> &[ToolSpec] {
        &self.tools
    }
}

fn normalize(s: &str) -> String {
    s.to_lowercase().replace(['-', ' '], "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_loads_15_tools() {
        assert_eq!(ToolCatalog::global().all().len(), 15);
    }

    #[test]
    fn disclose_tool_schemas_in_global_catalog() {
        let spec = ToolCatalog::global().resolve("disclose_tool_schemas").unwrap();
        assert!(spec.reproducible);
        let hint = spec.schema_hint.as_ref().unwrap();
        assert!(
            hint.required_fields
                .iter()
                .any(|field| field.name == "tool_names")
        );
    }

    #[test]
    fn resolve_canonical_name() {
        let cat = ToolCatalog::global();
        let spec = cat.resolve("web_search").unwrap();
        assert_eq!(spec.name, "web_search");
        assert!(spec.reproducible);
    }

    #[test]
    fn resolve_alias() {
        let cat = ToolCatalog::global();
        let spec = cat.resolve("websearch").unwrap();
        assert_eq!(spec.name, "web_search");
    }

    #[test]
    fn resolve_web_fetch_alias() {
        let cat = ToolCatalog::global();
        let spec = cat.resolve("web_fetch").unwrap();
        assert_eq!(spec.name, "web_extract");
    }

    #[test]
    fn resolve_unknown_returns_none() {
        assert!(ToolCatalog::global().resolve("does_not_exist").is_none());
    }

    #[test]
    fn reproducible_classification() {
        let cat = ToolCatalog::global();
        let reproducible = [
            "disclose_tool_schemas",
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
                cat.resolve(name).unwrap().reproducible,
                "{name} should be reproducible"
            );
        }
        for name in &non_reproducible {
            assert!(
                !cat.resolve(name).unwrap().reproducible,
                "{name} should not be reproducible"
            );
        }
    }

    #[test]
    fn start_artifact_schema_hint() {
        let cat = ToolCatalog::global();
        let spec = cat.resolve("start_artifact").unwrap();
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
        let cat = ToolCatalog::global();
        let spec = cat.resolve("append_blocks").unwrap();
        let hint = spec.schema_hint.as_ref().unwrap();
        assert_eq!(
            hint.defaults.get("isComplete"),
            Some(&serde_json::Value::Bool(false))
        );
    }

    #[test]
    fn search_x_recent_schema_hint_covers_boolean_defaults() {
        let cat = ToolCatalog::global();
        let spec = cat.resolve("search_x_recent").unwrap();
        let hint = spec.schema_hint.as_ref().unwrap();
        assert_eq!(hint.defaults.get("has_geo"), Some(&serde_json::Value::Bool(false)));
        assert_eq!(hint.defaults.get("has_media"), Some(&serde_json::Value::Bool(false)));
    }

    #[test]
    fn normalize_is_case_and_separator_insensitive() {
        let cat = ToolCatalog::global();
        assert!(cat.resolve("Web_Search").is_some());
        assert!(cat.resolve("WEB-SEARCH").is_some());
    }

    #[test]
    fn x_search_alias_resolves() {
        let cat = ToolCatalog::global();
        let spec = cat.resolve("x_search").unwrap();
        assert_eq!(spec.name, "search_x_recent");
    }
}
