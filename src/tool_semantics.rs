use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::catalog::{ToolSchemaHint, ToolSpec};
use crate::tool_name::ToolName;

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

    pub fn validate(&self) -> Result<(), ToolSemanticsError> {
        // Identity is ToolName equality: specs and aliases arrive canonical,
        // so validation compares directly with no per-site folding (ENC-299:
        // the fold lives in ToolName::new).
        let mut names: HashMap<&ToolName, &ToolName> = HashMap::new();
        for tool in &self.catalog.tools {
            if names.insert(&tool.name, &tool.name).is_some() {
                return Err(ToolSemanticsError::DuplicateToolName {
                    normalized_name: tool.name.as_str().to_string(),
                });
            }
        }

        let mut aliases: HashMap<&ToolName, &ToolName> = HashMap::new();
        for tool in &self.catalog.tools {
            for alias in &tool.aliases {
                if let Some(first) = aliases.get(alias) {
                    if *first != &tool.name {
                        return Err(ToolSemanticsError::AliasCollision {
                            alias: alias.as_str().to_string(),
                            first_tool: first.as_str().to_string(),
                            second_tool: tool.name.as_str().to_string(),
                        });
                    }
                } else {
                    // ENC-299 D5: an alias colliding with another tool's
                    // canonical name is the same collision — without this, the
                    // first-alias-wins catalog index would silently shadow the
                    // canonical owner post-validation. An alias equal to its
                    // OWN canonical name is harmless and stays allowed.
                    if let Some(owner) = names.get(alias) {
                        if *owner != &tool.name {
                            return Err(ToolSemanticsError::AliasCollision {
                                alias: alias.as_str().to_string(),
                                first_tool: owner.as_str().to_string(),
                                second_tool: tool.name.as_str().to_string(),
                            });
                        }
                    }
                    aliases.insert(alias, &tool.name);
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
            // ToolName fields are non-empty by construction (blank is a
            // boundary error); the wire-name fields keep the explicit check.
            for (name, empty) in [
                (
                    "start_tool_name",
                    artifact.start_tool_name.as_str().is_empty(),
                ),
                (
                    "append_tool_name",
                    artifact.append_tool_name.as_str().is_empty(),
                ),
                ("id_field", artifact.id_field.is_empty()),
                (
                    "append_blocks_field",
                    artifact.append_blocks_field.is_empty(),
                ),
                ("completion_field", artifact.completion_field.is_empty()),
            ] {
                if empty {
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

    /// Referential validation (ENC-299 D4): every tool name referenced by a
    /// configured semantics section must resolve — either to a catalog tool
    /// (canonical name or declared alias) or to an active schema name.
    ///
    /// Only enforced when the containing section is present/non-empty, so the
    /// agnostic config passes. The engine runs this in `create_session`
    /// after [`AgentConfig::prepare_tool_semantics`](crate::session::AgentConfig::prepare_tool_semantics),
    /// where the catalog and the parsed schema set meet for the first time.
    pub fn validate_references(
        &self,
        catalog: &crate::catalog::ToolCatalog,
        schema_names: &[ToolName],
    ) -> Result<(), ToolSemanticsError> {
        let mut known: HashSet<&ToolName> = HashSet::new();
        for spec in catalog.all() {
            known.insert(&spec.name);
            for alias in &spec.aliases {
                known.insert(alias);
            }
        }
        for name in schema_names {
            if !name.is_empty() {
                known.insert(name);
            }
        }
        let check = |section: &str, name: &ToolName| -> Result<(), ToolSemanticsError> {
            if !known.contains(name) {
                return Err(ToolSemanticsError::UnknownToolReference {
                    section: section.to_string(),
                    name: name.as_str().to_string(),
                });
            }
            Ok(())
        };
        if let Some(discovery) = &self.discovery {
            check("discovery.tool_name", &discovery.tool_name)?;
            for companion in &discovery.companion_callable_tools {
                check("discovery.companion_callable_tools", companion)?;
            }
        }
        if let Some(artifact) = &self.artifact {
            check("artifact.start_tool_name", &artifact.start_tool_name)?;
            check("artifact.append_tool_name", &artifact.append_tool_name)?;
        }
        for rule in &self.argument_repairs {
            check("argument_repairs.tool_name", &rule.tool_name)?;
        }
        for shape in &self.canonical_shapes {
            check("canonical_shapes.tool_name", &shape.tool_name)?;
        }
        for policy in &self.result_policies {
            check("result_policies.tool_name", &policy.tool_name)?;
        }
        if let Some(squash) = self.compaction.artifact_squash.as_ref() {
            check(
                "compaction.artifact_squash.append_tool_name",
                &squash.append_tool_name,
            )?;
        }
        for greedy in &self.greedy_extension_tools {
            check("greedy_extension_tools.tool_name", &greedy.tool_name)?;
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
    pub tool_name: ToolName,
    pub request_field_name: String,
    pub always_callable: bool,
    /// Tools that may be called without a prior `tool_discovery_tool` activation (e.g. context retrieval).
    #[serde(default)]
    pub companion_callable_tools: Vec<ToolName>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactToolSemantics {
    pub start_tool_name: ToolName,
    pub append_tool_name: ToolName,
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
    pub tool_name: ToolName,
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
    pub tool_name: ToolName,
    pub ordered_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultPolicy {
    pub tool_name: ToolName,
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
    pub append_tool_name: ToolName,
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
    pub tool_name: ToolName,
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
    /// A semantics section references a tool that is neither a catalog tool
    /// (canonical name or alias) nor an active schema name (ENC-299 D4).
    UnknownToolReference {
        section: String,
        name: String,
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

#[cfg(test)]
pub(crate) fn test_fixture_enclave_semantics() -> ToolSemanticsConfig {
    let mut semantics: ToolSemanticsConfig =
        serde_json::from_str(include_str!("../tests/support/enclave_tool_semantics.json"))
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
    tool_name: &ToolName,
    config: &ToolSemanticsConfig,
) -> Option<ToolSchemaHint> {
    config
        .catalog
        .tools
        .iter()
        .find(|tool| tool.name == *tool_name)
        .and_then(|tool| tool.schema_hint.clone())
}

pub(crate) fn result_policy_for<'a>(
    config: &'a ToolSemanticsConfig,
    tool_name: &ToolName,
) -> Option<&'a ToolResultPolicy> {
    config
        .result_policies
        .iter()
        .find(|policy| policy.tool_name == *tool_name)
}

#[cfg(test)]
mod catalog_split_tests {
    use super::*;

    #[test]
    fn with_tool_semantics_builds_policy_catalog() {
        use crate::catalog::ToolSpec;
        use crate::tool_name::ToolName;
        fn tn(name: &str) -> ToolName {
            ToolName::new(name).expect("test tool name must be valid")
        }
        let semantics = ToolSemanticsConfig {
            catalog: ToolCatalogConfig {
                tools: vec![ToolSpec {
                    name: tn("web_search"),
                    aliases: vec![tn("websearch")],
                    reproducible: true,
                    schema_hint: None,
                }],
            },
            ..ToolSemanticsConfig::agnostic()
        };
        let config =
            crate::session::AgentConfig::with_tool_semantics(semantics).expect("valid semantics");
        assert_eq!(config.tool_catalog.len(), 1);
        assert_eq!(
            config.tool_catalog.resolve(&tn("websearch")).unwrap().name,
            tn("web_search")
        );
        assert!(!config.tool_catalog.is_empty());
    }

    #[test]
    fn with_tool_semantics_rejects_duplicate_tool_names() {
        use crate::catalog::ToolSpec;
        use crate::tool_name::ToolName;
        fn tn(name: &str) -> ToolName {
            ToolName::new(name).expect("test tool name must be valid")
        }
        let semantics = ToolSemanticsConfig {
            catalog: ToolCatalogConfig {
                tools: vec![
                    ToolSpec {
                        name: tn("web_search"),
                        aliases: Vec::new(),
                        reproducible: true,
                        schema_hint: None,
                    },
                    ToolSpec {
                        name: tn("web-search"),
                        aliases: Vec::new(),
                        reproducible: true,
                        schema_hint: None,
                    },
                ],
            },
            ..ToolSemanticsConfig::agnostic()
        };
        assert!(crate::session::AgentConfig::with_tool_semantics(semantics).is_err());
    }
}
