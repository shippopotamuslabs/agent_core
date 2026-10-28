use std::collections::{HashMap, HashSet};

#[path = "support/mod.rs"]
mod support;

use agent_core::artifact_finalize::{
    record_artifact_progress_with_semantics, synthetic_finalize_calls_with_semantics,
};
use agent_core::canonicalize::{
    canonicalize_raw_arguments_with_semantics, ToolCallCanonicalizeMode,
};
use agent_core::catalog::{FieldHint, ToolCatalog, ToolSchemaHint, ToolSpec};
use agent_core::tool_name::ToolName;

fn tn(name: &str) -> ToolName {
    ToolName::new(name).expect("test tool name must be valid")
}
use agent_core::guardrails::GuardrailState;
use agent_core::repair::{RepairOutcome, ToolRepairPipeline};
use agent_core::session::AgentConfig;
use agent_core::tool_call_format::ToolCallFormat;
use agent_core::tool_parse::parse_tool_calls_with_semantics;
use agent_core::tool_semantics::{
    ArtifactSquashSemantics, ArtifactToolSemantics, DiscoveryGateExemption, ParameterAlias,
    SpecialValueRepair, ToolArgumentRepairRule, ToolCanonicalShape, ToolCompactionSemantics,
    ToolDiscoverySemantics, ToolGreedyExtensionSemantics, ToolResultPolicy, ToolSemanticsConfig,
    ToolSemanticsError,
};
use agent_core::turn_io::RawToolCall;
use serde_json::{json, Value};
use support::enclave_test_semantics::{enclave_test_catalog, enclave_test_semantics};

fn tool(name: &str) -> ToolSpec {
    ToolSpec {
        name: tn(name),
        aliases: Vec::new(),
        reproducible: true,
        schema_hint: None,
    }
}

fn custom_semantics() -> ToolSemanticsConfig {
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
            gate_exemptions: vec![agent_core::tool_semantics::DiscoveryGateExemption {
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

fn catalog_from(semantics: &ToolSemanticsConfig) -> ToolCatalog {
    ToolCatalog::from_semantics(semantics).expect("semantics catalog must build")
}

fn assert_json_eq(actual: &str, expected: Value) {
    let actual: Value = serde_json::from_str(actual).expect("actual JSON must parse");
    assert_eq!(actual, expected);
}

#[test]
fn ts_001_agnostic_default_contains_no_enclave_tool_semantics() {
    let config = AgentConfig::default();

    assert!(config.tool_semantics.discovery.is_none());
    assert!(config.tool_semantics.artifact.is_none());
    assert_eq!(config.tool_semantics.greedy_extension_tools.len(), 0);
}

#[test]
fn ts_002_enclave_compatibility_config_preserves_existing_semantic_names() {
    let semantics = enclave_test_semantics();
    let discovery = semantics.discovery.expect("discovery semantics");
    let artifact = semantics.artifact.expect("artifact semantics");

    assert_eq!(discovery.tool_name.as_str(), "tool_discovery_tool");
    assert_eq!(discovery.request_field_name, "capability");
    assert_eq!(artifact.start_tool_name.as_str(), "start_artifact");
    assert_eq!(artifact.append_tool_name.as_str(), "append_blocks");
    assert_eq!(artifact.id_field, "artifactId");
    assert_eq!(artifact.completion_field, "isComplete");
}

#[test]
fn ts_003_catalog_builds_from_host_semantics_without_global_catalog() {
    let semantics = custom_semantics();
    let catalog = ToolCatalog::from_semantics(&semantics).expect("catalog");

    assert_eq!(
        catalog.resolve(&tn("begin_doc")).unwrap().name.as_str(),
        "begin_doc"
    );
    assert_eq!(
        catalog.resolve(&tn("write_doc")).unwrap().name.as_str(),
        "write_doc"
    );
    assert!(catalog.resolve(&tn("append_blocks")).is_none());
}

#[test]
fn ts_004_duplicate_canonical_tool_names_are_rejected() {
    let mut semantics = ToolSemanticsConfig::agnostic();
    semantics.catalog.tools = vec![tool("write_doc"), tool("write-doc")];

    assert_eq!(
        semantics.validate(),
        Err(ToolSemanticsError::DuplicateToolName {
            normalized_name: "write_doc".to_string(),
        }),
    );
}

#[test]
fn ts_005_alias_collisions_are_rejected() {
    let mut first = tool("write_doc");
    first.aliases = vec![tn("writer")];
    let mut second = tool("compose_doc");
    second.aliases = vec![tn("writer")];
    let mut semantics = ToolSemanticsConfig::agnostic();
    semantics.catalog.tools = vec![first, second];

    assert_eq!(
        semantics.validate(),
        Err(ToolSemanticsError::AliasCollision {
            alias: "writer".to_string(),
            first_tool: "write_doc".to_string(),
            second_tool: "compose_doc".to_string(),
        }),
    );
}

fn exemption(tool: &str, activates: bool, reason: &str) -> DiscoveryGateExemption {
    DiscoveryGateExemption {
        tool_name: tn(tool),
        activates,
        reason: reason.to_string(),
    }
}

fn discovery_with(exemptions: Vec<DiscoveryGateExemption>) -> ToolSemanticsConfig {
    let mut semantics = ToolSemanticsConfig::agnostic();
    semantics.discovery = Some(ToolDiscoverySemantics {
        tool_name: tn("open_tools"),
        request_field_name: "names".to_string(),
        gate_exemptions: exemptions,
    });
    semantics
}

#[test]
fn ts_033_blank_gate_exemption_reason_is_rejected() {
    let semantics = discovery_with(vec![exemption("open_tools", false, "   ")]);

    assert_eq!(
        semantics.validate(),
        Err(ToolSemanticsError::InvalidDiscoveryConfig {
            message:
                "discovery gate_exemptions entry for 'open_tools' must carry a non-blank reason"
                    .to_string(),
        }),
    );
}

#[test]
fn ts_034_duplicate_gate_exemption_is_rejected() {
    let semantics = discovery_with(vec![
        exemption("open_tools", false, "discovery entry point"),
        exemption("open_tools", true, "second claim"),
    ]);

    assert_eq!(
        semantics.validate(),
        Err(ToolSemanticsError::InvalidDiscoveryConfig {
            message: "duplicate discovery gate_exemptions entry for 'open_tools'".to_string(),
        }),
    );
}

#[test]
fn ts_006_blank_artifact_tool_name_is_rejected_at_parse() {
    // ENC-299: ToolName construction is fallible, so a blank tool_name never
    // reaches validate() — serde rejects it at the JSON boundary instead.
    let json = serde_json::json!({
        "catalog": {"tools": []},
        "discovery": null,
        "artifact": {
            "start_tool_name": "begin_doc",
            "append_tool_name": "",
            "id_field": "docId",
            "append_blocks_field": "chunks",
            "completion_field": "done",
            "status_field": "status",
            "success_status_value": "success",
            "synthetic_call_id_prefix": "auto-close-",
            "empty_append_payload_value": []
        },
        "argument_repairs": [],
        "canonical_shapes": [],
        "result_policies": [],
        "compaction": {"artifact_squash": null},
        "greedy_extension_tools": []
    });
    let raw = serde_json::to_string(&json).expect("test JSON serializes");
    let result = ToolSemanticsConfig::from_json(&raw);
    let error = result.expect_err("blank append_tool_name must fail at parse");
    // m5 layer pin: the rejection surfaces at the serde boundary and names
    // the offending field — not a bare line/column, not a validate() error.
    let message = error.to_string();
    assert!(
        message.contains("artifact.append_tool_name"),
        "serde error must name the field, got {message:?}"
    );
    assert!(
        message.contains("tool name must not be blank"),
        "serde error must name the rejecting layer, got {message:?}"
    );
}

#[test]
fn ts_007_global_catalog_remains_compatibility_only() {
    let catalog = enclave_test_catalog();

    assert_eq!(
        catalog.resolve(&tn("append_blocks")).unwrap().name.as_str(),
        "append_blocks"
    );
    assert_eq!(ToolSemanticsConfig::agnostic().catalog.tools.len(), 0);
}

#[test]
fn ts_008_custom_alias_normalizes_artifact_id_field() {
    let semantics = custom_semantics();
    let result = canonicalize_raw_arguments_with_semantics(
        &tn("write_doc"),
        r#"{"artifact_id":"doc-1","chunks":[]}"#,
        &catalog_from(&semantics),
        &semantics,
        ToolCallCanonicalizeMode::ExecutionStrict,
    )
    .expect("canonicalized arguments");

    assert_json_eq(
        &result.arguments_json,
        json!({"docId":"doc-1","chunks":[],"done":false}),
    );
    assert!(result
        .repairs
        .contains(&"normalized parameter alias artifact_id to docId".to_string()));
}

#[test]
fn ts_009_custom_completion_alias_normalizes_completion_field() {
    let semantics = custom_semantics();
    let result = canonicalize_raw_arguments_with_semantics(
        &tn("write_doc"),
        r#"{"docId":"doc-1","chunks":[],"complete":true}"#,
        &catalog_from(&semantics),
        &semantics,
        ToolCallCanonicalizeMode::ExecutionStrict,
    )
    .expect("canonicalized arguments");

    assert_json_eq(
        &result.arguments_json,
        json!({"docId":"doc-1","chunks":[],"done":true}),
    );
    assert!(result
        .repairs
        .contains(&"normalized parameter alias complete to done".to_string()));
}

#[test]
fn ts_010_custom_default_injects_completion_false() {
    let semantics = custom_semantics();
    let result = canonicalize_raw_arguments_with_semantics(
        &tn("write_doc"),
        r#"{"docId":"doc-1","chunks":[]}"#,
        &catalog_from(&semantics),
        &semantics,
        ToolCallCanonicalizeMode::ExecutionStrict,
    )
    .expect("canonicalized arguments");

    assert_json_eq(
        &result.arguments_json,
        json!({"docId":"doc-1","chunks":[],"done":false}),
    );
    assert!(result.repaired);
}

#[test]
fn ts_011_stringified_custom_blocks_array_is_accepted() {
    let semantics = custom_semantics();
    let result = canonicalize_raw_arguments_with_semantics(
        &tn("write_doc"),
        r#"{"docId":"doc-1","chunks":"[{\"text\":\"Hello\"}]","done":false}"#,
        &catalog_from(&semantics),
        &semantics,
        ToolCallCanonicalizeMode::ExecutionStrict,
    )
    .expect("canonicalized arguments");

    assert_json_eq(
        &result.arguments_json,
        json!({"docId":"doc-1","chunks":[{"text":"Hello"}],"done":false}),
    );
    assert!(result
        .repairs
        .contains(&"coerced arguments to catalog schema".to_string()));
}

#[test]
fn ts_012_malformed_stringified_custom_blocks_array_is_rejected() {
    let semantics = custom_semantics();
    let result = canonicalize_raw_arguments_with_semantics(
        &tn("write_doc"),
        r#"{"docId":"doc-1","chunks":"[{not valid json","done":false}"#,
        &catalog_from(&semantics),
        &semantics,
        ToolCallCanonicalizeMode::ExecutionStrict,
    );

    assert_eq!(
        result.unwrap_err().message,
        "chunks string contains invalid JSON: key must be a string at line 1 column 3",
    );
}

#[test]
fn ts_013_custom_scalar_query_normalizes_to_queries_array() {
    let semantics = custom_semantics();
    let result = canonicalize_raw_arguments_with_semantics(
        &tn("memory_search"),
        r#"{"query":"budget"}"#,
        &catalog_from(&semantics),
        &semantics,
        ToolCallCanonicalizeMode::ExecutionStrict,
    )
    .expect("canonicalized arguments");

    assert_json_eq(&result.arguments_json, json!({"queries":["budget"]}));
    assert!(result
        .repairs
        .contains(&"normalized query to queries array".to_string()));
}

#[test]
fn ts_014_existing_repair_wrapper_remains_enclave_compatible() {
    let raw = RawToolCall {
        id: Some("call-1".to_string()),
        name: "append_blocks".to_string(),
        arguments_json: r#"{"artifactId":"a","blocks":[]}"#.to_string(),
    };

    let semantics = enclave_test_semantics();
    let catalog = enclave_test_catalog();
    let (outcome, _) = ToolRepairPipeline::repair_with_semantics(&raw, &catalog, &semantics);
    match outcome {
        RepairOutcome::Repaired(record) => {
            assert_eq!(record.name, "append_blocks");
            assert_json_eq(
                &record.arguments_json,
                json!({"artifactId":"a","blocks":[],"isComplete":false}),
            );
        }
        other => panic!("expected repaired append_blocks, got {other:?}"),
    }
}

#[test]
fn ts_015_semantics_aware_repair_accepts_custom_tool_names() {
    let semantics = custom_semantics();
    let raw = RawToolCall {
        id: Some("call-1".to_string()),
        name: "write_doc".to_string(),
        arguments_json: r#"{"docId":"doc-1","chunks":[]}"#.to_string(),
    };

    let (outcome, _) =
        ToolRepairPipeline::repair_with_semantics(&raw, &catalog_from(&semantics), &semantics);
    match outcome {
        RepairOutcome::Repaired(record) => {
            assert_eq!(record.name, "write_doc");
            assert_json_eq(
                &record.arguments_json,
                json!({"docId":"doc-1","chunks":[],"done":false}),
            );
        }
        other => panic!("expected repaired write_doc, got {other:?}"),
    }
}

#[test]
fn ts_016_provider_parse_uses_supplied_custom_catalog() {
    let semantics = custom_semantics();
    let batch = parse_tool_calls_with_semantics(
        ToolCallFormat::XmlToolCall,
        r#"<tool_call>{"name":"lookup_memory","arguments":{"query":"alpha"}}</tool_call>"#,
        &catalog_from(&semantics),
        &semantics,
    );

    assert_eq!(batch.successes.len(), 1);
    assert_eq!(batch.successes[0].tool_name, "lookup_memory");
    assert_eq!(batch.failures.len(), 0);
}

#[test]
fn ts_020_custom_artifact_start_result_records_pending_id() {
    let semantics = custom_semantics();
    let mut pending = HashSet::new();

    record_artifact_progress_with_semantics(
        &mut pending,
        &semantics,
        "begin_doc",
        "{}",
        r#"{"status":"success","docId":"doc-1"}"#,
    );

    assert_eq!(
        pending.into_iter().collect::<Vec<_>>(),
        vec!["doc-1".to_string()]
    );
}

#[test]
fn ts_021_custom_incomplete_append_keeps_pending_id() {
    let semantics = custom_semantics();
    let mut pending = HashSet::from(["doc-1".to_string()]);

    record_artifact_progress_with_semantics(
        &mut pending,
        &semantics,
        "write_doc",
        r#"{"docId":"doc-1","chunks":[]}"#,
        r#"{"status":"success","done":false}"#,
    );

    assert_eq!(
        pending.into_iter().collect::<Vec<_>>(),
        vec!["doc-1".to_string()]
    );
}

#[test]
fn ts_022_custom_complete_append_removes_pending_id() {
    let semantics = custom_semantics();
    let mut pending = HashSet::from(["doc-1".to_string()]);

    record_artifact_progress_with_semantics(
        &mut pending,
        &semantics,
        "write_doc",
        r#"{"docId":"doc-1","chunks":[]}"#,
        r#"{"status":"success","done":true}"#,
    );

    assert!(pending.is_empty());
}

#[test]
fn ts_023_synthetic_finalize_uses_custom_append_tool_and_fields() {
    let semantics = custom_semantics();
    let pending = HashSet::from(["doc-7".to_string()]);

    let calls = synthetic_finalize_calls_with_semantics(&pending, &semantics);

    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call_id, "auto-close-doc-7");
    assert_eq!(calls[0].name, "write_doc");
    assert_json_eq(
        &calls[0].arguments_json,
        json!({"docId":"doc-7","chunks":[],"done":true}),
    );
}

#[test]
fn ts_027_custom_greedy_tool_extends_guardrail_cap() {
    let mut guardrails = GuardrailState::default();
    guardrails.count_tool_executed(20);
    let config = AgentConfig {
        max_turns: 20,
        tool_semantics: custom_semantics(),
        ..AgentConfig::default()
    };
    let last_tools = vec![tn("write_doc")];

    assert_eq!(
        guardrails.effective_max_tool_calls(&config, &last_tools),
        40,
    );
    assert!(guardrails.allow_tool_call(&config, &last_tools));
}

#[test]
fn ts_028_no_greedy_config_keeps_base_guardrail_cap() {
    let mut guardrails = GuardrailState::default();
    guardrails.count_tool_executed(20);
    let config = AgentConfig {
        max_turns: 20,
        ..AgentConfig::default()
    };
    let last_tools = vec![tn("write_doc")];

    assert_eq!(
        guardrails.effective_max_tool_calls(&config, &last_tools),
        20,
    );
    assert!(!guardrails.allow_tool_call(&config, &last_tools));
}

#[test]
fn ts_032_hard_coded_append_blocks_repair_fails_custom_only_config() {
    let semantics = custom_semantics();
    let raw = RawToolCall {
        id: Some("call-1".to_string()),
        name: "write_doc".to_string(),
        arguments_json: r#"{"doc_id":"doc-1","chunks":[],"complete":true}"#.to_string(),
    };

    let (outcome, _) =
        ToolRepairPipeline::repair_with_semantics(&raw, &catalog_from(&semantics), &semantics);
    match outcome {
        RepairOutcome::Repaired(record) => {
            assert_eq!(record.name, "write_doc");
            let value: Value = serde_json::from_str(&record.arguments_json).unwrap();
            assert_eq!(value, json!({"docId":"doc-1","chunks":[],"done":true}));
            assert!(value.get("artifactId").is_none());
            assert!(value.get("isComplete").is_none());
        }
        other => panic!("expected repaired write_doc, got {other:?}"),
    }
}
