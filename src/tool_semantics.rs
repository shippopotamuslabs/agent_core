use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::catalog::{FieldHint, ToolSchemaHint, ToolSpec};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSemanticsConfig {
    pub catalog: ToolCatalogConfig,
    pub disclosure: Option<ToolDisclosureSemantics>,
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
            disclosure: None,
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

    pub fn enclave_compat() -> Self {
        let raw = include_str!("../resources/tool_catalog.json");
        #[derive(Deserialize)]
        struct CatalogFile {
            tools: Vec<ToolSpec>,
        }
        let catalog = serde_json::from_str::<CatalogFile>(raw)
            .map(|file| ToolCatalogConfig { tools: file.tools })
            .unwrap_or_else(|_| ToolCatalogConfig { tools: Vec::new() });
        Self {
            catalog,
            disclosure: Some(ToolDisclosureSemantics {
                tool_name: "disclose_tool_schemas".into(),
                request_field_name: "tool_names".into(),
                always_callable: true,
            }),
            artifact: Some(ArtifactToolSemantics {
                start_tool_name: "start_artifact".into(),
                append_tool_name: "append_blocks".into(),
                id_field: "artifactId".into(),
                append_blocks_field: "blocks".into(),
                completion_field: "isComplete".into(),
                status_field: "status".into(),
                success_status_value: "success".into(),
                synthetic_call_id_prefix: "auto-finalize-".into(),
                empty_append_payload_value: serde_json::json!([]),
            }),
            argument_repairs: vec![
                ToolArgumentRepairRule {
                    tool_name: "append_blocks".into(),
                    aliases: vec![
                        ParameterAlias {
                            alias: "artifact_id".into(),
                            canonical: "artifactId".into(),
                        },
                        ParameterAlias {
                            alias: "is_complete".into(),
                            canonical: "isComplete".into(),
                        },
                        ParameterAlias {
                            alias: "complete".into(),
                            canonical: "isComplete".into(),
                        },
                    ],
                    special_values: vec![SpecialValueRepair::ValidateStringifiedJsonArray {
                        field: "blocks".into(),
                    }],
                },
                ToolArgumentRepairRule {
                    tool_name: "search_current_artifact".into(),
                    aliases: vec![ParameterAlias {
                        alias: "artifact_id".into(),
                        canonical: "artifactId".into(),
                    }],
                    special_values: Vec::new(),
                },
                ToolArgumentRepairRule {
                    tool_name: "search_chat_history".into(),
                    aliases: Vec::new(),
                    special_values: vec![SpecialValueRepair::ScalarToArray {
                        source: "query".into(),
                        target: "queries".into(),
                    }],
                },
                ToolArgumentRepairRule {
                    tool_name: "web_extract".into(),
                    aliases: Vec::new(),
                    special_values: vec![SpecialValueRepair::StripLeadingBracketFromArrayStrings {
                        field: "urls".into(),
                    }],
                },
            ],
            canonical_shapes: vec![
                ToolCanonicalShape {
                    tool_name: "web_search".into(),
                    ordered_fields: vec!["query".into()],
                },
                ToolCanonicalShape {
                    tool_name: "web_extract".into(),
                    ordered_fields: vec!["urls".into(), "extract_depth".into(), "format".into()],
                },
                ToolCanonicalShape {
                    tool_name: "search_chat_history".into(),
                    ordered_fields: vec!["queries".into()],
                },
                ToolCanonicalShape {
                    tool_name: "append_blocks".into(),
                    ordered_fields: vec!["artifactId".into(), "blocks".into(), "isComplete".into()],
                },
            ],
            result_policies: vec![ToolResultPolicy {
                tool_name: "append_blocks".into(),
                spill_stored_copy_only: true,
                skip_result_truncation: true,
                preserve_in_micro_compaction: true,
            }],
            compaction: ToolCompactionSemantics {
                artifact_squash: Some(ArtifactSquashSemantics {
                    append_tool_name: "append_blocks".into(),
                    id_field: "artifactId".into(),
                    blocks_field: "blocks".into(),
                    total_blocks_result_field: "totalBlocks".into(),
                    status_field: "status".into(),
                    success_status_value: "success".into(),
                    summary_template: "Artifact generation in progress. {total} blocks written so far. Last block: `{snippet}`. Continue appending.".into(),
                    continuation_instruction: "Continue appending.".into(),
                    snippet_max_chars: 200,
                }),
            },
            greedy_extension_tools: vec![ToolGreedyExtensionSemantics {
                tool_name: "append_blocks".into(),
                extra_turns: 20,
            }],
        }
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

        if let Some(disclosure) = &self.disclosure {
            if disclosure.request_field_name.is_empty() {
                return Err(ToolSemanticsError::InvalidDisclosureConfig {
                    message: "disclosure request_field_name must not be empty".into(),
                });
            }
            if disclosure.tool_name.is_empty() {
                return Err(ToolSemanticsError::InvalidDisclosureConfig {
                    message: "disclosure tool_name must not be empty".into(),
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
pub struct ToolDisclosureSemantics {
    pub tool_name: String,
    pub request_field_name: String,
    pub always_callable: bool,
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
    InvalidDisclosureConfig {
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
        .or_else(|| {
            if tool_name == "web_extract" {
                let mut defaults = HashMap::new();
                defaults.insert(
                    "extract_depth".to_string(),
                    serde_json::Value::String("basic".to_string()),
                );
                defaults.insert(
                    "format".to_string(),
                    serde_json::Value::String("markdown".to_string()),
                );
                Some(ToolSchemaHint {
                    required_fields: vec![FieldHint {
                        name: "urls".to_string(),
                        ty: "array".to_string(),
                        description: "URLs to extract".to_string(),
                    }],
                    defaults,
                })
            } else if tool_name == "search_chat_history" {
                Some(ToolSchemaHint {
                    required_fields: vec![FieldHint {
                        name: "queries".to_string(),
                        ty: "array".to_string(),
                        description: "Queries to search".to_string(),
                    }],
                    defaults: HashMap::new(),
                })
            } else {
                None
            }
        })
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
