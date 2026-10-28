use std::collections::HashMap;

use agent_core::catalog::{FieldHint, ToolSchemaHint, ToolSpec};
use agent_core::tool_name::ToolName;

pub fn tn(name: &str) -> ToolName {
    ToolName::new(name).expect("test tool name must be valid")
}
use agent_core::tool_semantics::{
    ArtifactSquashSemantics, ArtifactToolSemantics, DiscoveryGateExemption, ParameterAlias,
    SpecialValueRepair, ToolArgumentRepairRule, ToolCanonicalShape, ToolCompactionSemantics,
    ToolDiscoverySemantics, ToolGreedyExtensionSemantics, ToolResultPolicy, ToolSemanticsConfig,
};
use serde_json::json;

pub fn tool(name: &str) -> ToolSpec {
    ToolSpec {
        name: tn(name),
        aliases: Vec::new(),
        reproducible: true,
        schema_hint: None,
    }
}

pub fn custom_semantics() -> ToolSemanticsConfig {
    let mut defaults = HashMap::new();
    defaults.insert("done".to_string(), json!(false));
    ToolSemanticsConfig {
        catalog: agent_core::tool_semantics::ToolCatalogConfig {
            tools: vec![
                ToolSpec {
                    name: tn("begin_doc"),
                    aliases: Vec::new(),
                    reproducible: false,
                    schema_hint: None,
                },
                ToolSpec {
                    name: tn("write_doc"),
                    aliases: Vec::new(),
                    reproducible: false,
                    schema_hint: Some(ToolSchemaHint {
                        required_fields: vec![
                            FieldHint {
                                name: "docId".to_string(),
                                ty: "string".to_string(),
                                description: String::new(),
                            },
                            FieldHint {
                                name: "chunks".to_string(),
                                ty: "array".to_string(),
                                description: String::new(),
                            },
                        ],
                        defaults,
                    }),
                },
                tool("open_tools"),
                tool("search_web"),
                tool("memory_search"),
                tool("lookup_memory"),
            ],
        },
        discovery: Some(ToolDiscoverySemantics {
            tool_name: tn("open_tools"),
            request_field_name: "names".to_string(),
            gate_exemptions: vec![DiscoveryGateExemption {
                tool_name: tn("open_tools"),
                activates: false,
                reason: "discovery entry point".to_string(),
            }],
        }),
        artifact: Some(ArtifactToolSemantics {
            start_tool_name: tn("begin_doc"),
            append_tool_name: tn("write_doc"),
            id_field: "docId".to_string(),
            append_blocks_field: "chunks".to_string(),
            completion_field: "done".to_string(),
            status_field: "status".to_string(),
            success_status_value: "success".to_string(),
            synthetic_call_id_prefix: "auto-close-".to_string(),
            empty_append_payload_value: json!([]),
        }),
        argument_repairs: vec![
            ToolArgumentRepairRule {
                tool_name: tn("write_doc"),
                aliases: vec![
                    ParameterAlias {
                        alias: "artifact_id".to_string(),
                        canonical: "docId".to_string(),
                    },
                    ParameterAlias {
                        alias: "doc_id".to_string(),
                        canonical: "docId".to_string(),
                    },
                    ParameterAlias {
                        alias: "complete".to_string(),
                        canonical: "done".to_string(),
                    },
                ],
                special_values: vec![SpecialValueRepair::ValidateStringifiedJsonArray {
                    field: "chunks".to_string(),
                }],
            },
            ToolArgumentRepairRule {
                tool_name: tn("memory_search"),
                aliases: Vec::new(),
                special_values: vec![SpecialValueRepair::ScalarToArray {
                    source: "query".to_string(),
                    target: "queries".to_string(),
                }],
            },
        ],
        canonical_shapes: vec![
            ToolCanonicalShape {
                tool_name: tn("write_doc"),
                ordered_fields: vec!["docId".to_string(), "chunks".to_string(), "done".to_string()],
            },
            ToolCanonicalShape {
                tool_name: tn("memory_search"),
                ordered_fields: vec!["queries".to_string()],
            },
        ],
        result_policies: vec![ToolResultPolicy {
            tool_name: tn("write_doc"),
            spill_stored_copy_only: true,
            skip_result_truncation: true,
            preserve_in_micro_compaction: true,
        }],
        compaction: ToolCompactionSemantics {
            artifact_squash: Some(ArtifactSquashSemantics {
                append_tool_name: tn("write_doc"),
                id_field: "docId".to_string(),
                blocks_field: "chunks".to_string(),
                total_blocks_result_field: "chunkCount".to_string(),
                status_field: "status".to_string(),
                success_status_value: "success".to_string(),
                summary_template: "Artifact generation in progress. {total} blocks written so far. Last block: `{snippet}`. Continue appending.".to_string(),
                continuation_instruction: "Continue appending.".to_string(),
                snippet_max_chars: 200,
            }),
        },
        greedy_extension_tools: vec![ToolGreedyExtensionSemantics {
            tool_name: tn("write_doc"),
            extra_turns: 20,
        }],
    }
}
