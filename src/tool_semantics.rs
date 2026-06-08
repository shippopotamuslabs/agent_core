use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::catalog::{ToolSchemaHint, ToolSpec};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSemanticsConfig {
    pub catalog: ToolCatalogConfig,
    pub discovery: Option<ToolDiscoverySemantics>,
    pub artifact: Option<ArtifactToolSemantics>,
    pub argument_repairs: Vec<ToolArgumentRepairRule>,
    pub canonical_shapes: Vec<ToolCanonicalShape>,
    pub result_policies: Vec<ToolResultPolicy>,
    pub compaction: ToolCompactionSemantics,
    pub greedy_extension_tools: Vec<ToolGreedyExtensionSemantics>,
}

impl ToolSemanticsConfig {
    pub fn agnostic() -> Self {
        Self {
            catalog: ToolCatalogConfig { tools: Vec::new() },
            discovery: None,
            artifact: None,
            argument_repairs: Vec::new(),
            canonical_shapes: Vec::new(),
            result_policies: Vec::new(),
            compaction: ToolCompactionSemantics {
                artifact_squash: None,
            },
            greedy_extension_tools: Vec::new(),
        }
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Merges host-provided catalog JSON into `catalog.tools` when the config catalog is empty.
    pub fn merge_catalog_json(&mut self, catalog_json: &str) -> Result<(), ToolSemanticsError> {
        let trimmed = catalog_json.trim();
        if trimmed.is_empty() || trimmed == "[]" {
            return Ok(());
        }
        #[derive(Deserialize)]
        struct CatalogFile {
            tools: Vec<ToolSpec>,
        }
        let tools = if trimmed.starts_with('[') {
            serde_json::from_str(trimmed).map_err(|error| {
                ToolSemanticsError::InvalidResultPolicy {
                    message: format!("invalid tool catalog json: {error}"),
                }
            })?
        } else {
            serde_json::from_str::<CatalogFile>(trimmed)
                .map_err(|error| ToolSemanticsError::InvalidResultPolicy {
                    message: format!("invalid tool catalog json: {error}"),
                })?
                .tools
        };
        if self.catalog.tools.is_empty() {
            self.catalog.tools = tools;
        }
        self.validate()
    }

    pub fn validate(&self) -> Result<(), ToolSemanticsError> {
        let mut names: HashMap<String, String> = HashMap::new();
        for tool in &self.catalog.tools {
            let normalized = normalize_tool_name(&tool.name);
            if names.insert(normalized.clone(), tool.name.clone()).is_some() {
                return Err(ToolSemanticsError::DuplicateToolName {
                    normalized_name: normalized,
                });
            }
        }

        let mut aliases: HashMap<String, String> = HashMap::new();
        for tool in &self.catalog.tools {
            for alias in &tool.aliases {
                let normalized = normalize_tool_name(alias);
                if let Some(first) = aliases.get(&normalized) {
                    if first != &tool.name {
                        return Err(ToolSemanticsError::AliasCollision {
                            alias: alias.clone(),
                            first_tool: first.clone(),
                            second_tool: tool.name.clone(),
                        });
                    }
                } else {
                    aliases.insert(normalized, tool.name.clone());
                }
            }
        }

        if let Some(discovery) = &self.discovery {
            if discovery.request_field_name.is_empty() {
                return Err(ToolSemanticsError::InvalidDiscoveryConfig {
                    message: "discovery request_field_name must not be empty".into(),
                });
            }
            if discovery.tool_name.is_empty() {
                return Err(ToolSemanticsError::InvalidDiscoveryConfig {
                    message: "discovery tool_name must not be empty".into(),
                });
            }
        }

        if let Some(artifact) = &self.artifact {
            for (name, value) in [
                ("start_tool_name", &artifact.start_tool_name),
                ("append_tool_name", &artifact.append_tool_name),
                ("id_field", &artifact.id_field),
                ("append_blocks_field", &artifact.append_blocks_field),
                ("completion_field", &artifact.completion_field),
            ] {
                if value.is_empty() {
                    return Err(ToolSemanticsError::InvalidArtifactConfig {
                        message: format!("artifact {name} must not be empty"),
                    });
                }
            }
        }

        for policy in &self.result_policies {
            if policy.tool_name.is_empty() {
                return Err(ToolSemanticsError::InvalidResultPolicy {
                    message: "result policy tool_name must not be empty".into(),
                });
            }
        }

        Ok(())
    }
}

impl Default for ToolSemanticsConfig {
    fn default() -> Self {
        Self::agnostic()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCatalogConfig {
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDiscoverySemantics {
    pub tool_name: String,
    pub request_field_name: String,
    pub always_callable: bool,
    /// Tools that may be called without a prior `search_tools` activation (e.g. context retrieval).
    #[serde(default)]
    pub companion_callable_tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactToolSemantics {
    pub start_tool_name: String,
    pub append_tool_name: String,
    pub id_field: String,
    pub append_blocks_field: String,
    pub completion_field: String,
    pub status_field: String,
    pub success_status_value: String,
    pub synthetic_call_id_prefix: String,
    pub empty_append_payload_value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolArgumentRepairRule {
    pub tool_name: String,
    pub aliases: Vec<ParameterAlias>,
    pub special_values: Vec<SpecialValueRepair>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterAlias {
    pub alias: String,
    pub canonical: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpecialValueRepair {
    ScalarToArray { source: String, target: String },
    StripLeadingBracketFromArrayStrings { field: String },
    ValidateStringifiedJsonArray { field: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCanonicalShape {
    pub tool_name: String,
    pub ordered_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultPolicy {
    pub tool_name: String,
    pub spill_stored_copy_only: bool,
    pub skip_result_truncation: bool,
    pub preserve_in_micro_compaction: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCompactionSemantics {
    pub artifact_squash: Option<ArtifactSquashSemantics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSquashSemantics {
    pub append_tool_name: String,
    pub id_field: String,
    pub blocks_field: String,
    pub total_blocks_result_field: String,
    pub status_field: String,
    pub success_status_value: String,
    pub summary_template: String,
    pub continuation_instruction: String,
    pub snippet_max_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolGreedyExtensionSemantics {
    pub tool_name: String,
    pub extra_turns: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSemanticsError {
    DuplicateToolName {
        normalized_name: String,
    },
    AliasCollision {
        alias: String,
        first_tool: String,
        second_tool: String,
    },
    InvalidDiscoveryConfig {
        message: String,
    },
    InvalidArtifactConfig {
        message: String,
    },
    InvalidResultPolicy {
        message: String,
    },
}

pub(crate) fn normalize_tool_name(s: &str) -> String {
    s.to_lowercase().replace(['-', ' '], "_")
}

#[cfg(test)]
mod merge_catalog_tests {
    use super::*;

    #[test]
    fn legacy_kotlin_catalog_shape_fails_merge() {
        let mut semantics = ToolSemanticsConfig::agnostic();
        let legacy =
            r#"[{"name":"web_search","description":"x","disclosureMode":"PRELOADED"}]"#;
        assert!(semantics.merge_catalog_json(legacy).is_err());
    }

    #[test]
    fn tool_spec_array_merges_into_empty_semantics_catalog() {
        let mut semantics = ToolSemanticsConfig::agnostic();
        let specs = r#"[{"name":"web_search","aliases":["websearch"],"reproducible":true,"schema_hint":null}]"#;
        semantics.merge_catalog_json(specs).expect("merge");
        assert_eq!(semantics.catalog.tools.len(), 1);
        assert_eq!(semantics.catalog.tools[0].name, "web_search");
    }
}

#[cfg(test)]
pub(crate) fn test_fixture_enclave_semantics() -> ToolSemanticsConfig {
    let mut semantics: ToolSemanticsConfig = serde_json::from_str(include_str!(
        "../tests/support/enclave_tool_semantics.json"
    ))
    .expect("enclave_tool_semantics.json");
    let raw = include_str!("../resources/tool_catalog.json");
    #[derive(Deserialize)]
    struct CatalogFile {
        tools: Vec<ToolSpec>,
    }
    let catalog = serde_json::from_str::<CatalogFile>(raw).expect("tool_catalog.json");
    semantics.catalog = ToolCatalogConfig {
        tools: catalog.tools,
    };
    semantics
}

pub(crate) fn schema_hint_for_semantics(
    tool_name: &str,
    config: &ToolSemanticsConfig,
) -> Option<ToolSchemaHint> {
    config
        .catalog
        .tools
        .iter()
        .find(|tool| normalize_tool_name(&tool.name) == normalize_tool_name(tool_name))
        .and_then(|tool| tool.schema_hint.clone())
}

pub(crate) fn result_policy_for<'a>(
    config: &'a ToolSemanticsConfig,
    tool_name: &str,
) -> Option<&'a ToolResultPolicy> {
    config
        .result_policies
        .iter()
        .find(|policy| normalize_tool_name(&policy.tool_name) == normalize_tool_name(tool_name))
}

/// Loads host semantics JSON and merges session catalog JSON into an executable catalog.
pub(crate) fn load_semantics_with_session_catalog(
    tool_semantics_json: &str,
    tool_catalog_json: &str,
) -> Result<(ToolSemanticsConfig, crate::catalog::ToolCatalog), String> {
    let mut semantics = ToolSemanticsConfig::from_json(tool_semantics_json)
        .map_err(|error| format!("invalid tool_semantics_json: {error}"))?;
    semantics
        .merge_catalog_json(tool_catalog_json)
        .map_err(|error| format!("tool catalog merge failed: {error:?}"))?;
    let catalog = crate::catalog::ToolCatalog::from_semantics(&semantics)
        .map_err(|error| format!("invalid merged tool catalog: {error:?}"))?;
    Ok((semantics, catalog))
}
