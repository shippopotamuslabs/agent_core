//! Tool name + argument repair before execution (see AGENT-014).

mod fuzzy;

pub use fuzzy::{best_catalog_match, FUZZY_AUTO_ACCEPT};

use serde::{Deserialize, Serialize};

use crate::canonicalize::{
    canonicalize_tool_call, canonicalize_tool_call_with_semantics, CanonicalizeToolCallOutcome,
    ToolCallCanonicalizeMode,
};
use crate::catalog::ToolCatalog;
use crate::events::AgentEvent;
use crate::message::ToolCallRecord;
use crate::tool_semantics::ToolSemanticsConfig;
use crate::turn_io::RawToolCall;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairOutcome {
    Repaired(ToolCallRecord),
    Reject {
        error_json_for_model: String,
    },
    Suggest {
        original: String,
        closest_name: String,
    },
}

pub struct ToolRepairPipeline;

impl ToolRepairPipeline {
    pub fn repair(raw: &RawToolCall, catalog: &ToolCatalog) -> (RepairOutcome, Vec<AgentEvent>) {
        let (outcome, events) =
            canonicalize_tool_call(raw, catalog, ToolCallCanonicalizeMode::ExecutionStrict);
        let outcome = match outcome {
            CanonicalizeToolCallOutcome::Ok(call) => RepairOutcome::Repaired(ToolCallRecord {
                call_id: raw.id.clone().unwrap_or_else(|| "call".into()),
                name: call.resolved_name,
                arguments_json: call.arguments_json,
                attempt_metadata: None,
                repair_metadata: call.repair_metadata,
            }),
            CanonicalizeToolCallOutcome::Reject {
                error_json_for_model,
            } => RepairOutcome::Reject {
                error_json_for_model,
            },
            CanonicalizeToolCallOutcome::Suggest {
                original,
                closest_name,
            } => RepairOutcome::Suggest {
                original,
                closest_name,
            },
        };
        (outcome, events)
    }

    pub fn repair_with_semantics(
        raw: &RawToolCall,
        catalog: &ToolCatalog,
        semantics: &ToolSemanticsConfig,
    ) -> (RepairOutcome, Vec<AgentEvent>) {
        let (outcome, events) = canonicalize_tool_call_with_semantics(
            raw,
            catalog,
            semantics,
            ToolCallCanonicalizeMode::ExecutionStrict,
        );
        let outcome = match outcome {
            CanonicalizeToolCallOutcome::Ok(call) => RepairOutcome::Repaired(ToolCallRecord {
                call_id: raw.id.clone().unwrap_or_else(|| "call".into()),
                name: call.resolved_name,
                arguments_json: call.arguments_json,
                attempt_metadata: None,
                repair_metadata: call.repair_metadata,
            }),
            CanonicalizeToolCallOutcome::Reject {
                error_json_for_model,
            } => RepairOutcome::Reject {
                error_json_for_model,
            },
            CanonicalizeToolCallOutcome::Suggest {
                original,
                closest_name,
            } => RepairOutcome::Suggest {
                original,
                closest_name,
            },
        };
        (outcome, events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_semantics::ToolSemanticsConfig;
    use crate::turn_io::RawToolCall;
    use serde_json::Value;

    fn enclave_repair_env() -> (ToolCatalog, ToolSemanticsConfig) {
        let semantics = crate::tool_semantics::test_fixture_enclave_semantics();
        let catalog = ToolCatalog::from_semantics(&semantics).expect("enclave catalog");
        (catalog, semantics)
    }

    #[test]
    fn search_tools_repair_succeeds_in_global_catalog() {
        let raw = RawToolCall {
            id: Some("c1".into()),
            name: "search_tools".into(),
            arguments_json: r#"{"query":"start artifact"}"#.into(),
        };
        let (catalog, semantics) = enclave_repair_env();
        let (out, _) = ToolRepairPipeline::repair_with_semantics(&raw, &catalog, &semantics);
        match out {
            RepairOutcome::Repaired(rec) => assert_eq!(rec.name, "search_tools"),
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    #[test]
    fn web_seach_fuzzy_repairs_to_web_search() {
        let raw = RawToolCall {
            id: Some("c1".into()),
            name: "web_seach".into(),
            arguments_json: r#"{"query":"rust"}"#.into(),
        };
        let (catalog, semantics) = enclave_repair_env();
        let (out, evs) = ToolRepairPipeline::repair_with_semantics(&raw, &catalog, &semantics);
        match out {
            RepairOutcome::Repaired(rec) => {
                assert_eq!(rec.name, "web_search");
                assert_eq!(evs.len(), 1);
            }
            o => panic!("expected Repaired, got {o:?}"),
        }
    }

    #[test]
    fn web_search_uppercase_normalizes() {
        let raw = RawToolCall {
            id: None,
            name: "WEB_SEARCH".into(),
            arguments_json: r#"{"query":"rust"}"#.into(),
        };
        let (catalog, semantics) = enclave_repair_env();
        let (out, evs) = ToolRepairPipeline::repair_with_semantics(&raw, &catalog, &semantics);
        match out {
            RepairOutcome::Repaired(rec) => {
                assert_eq!(rec.name, "web_search");
                assert!(evs.is_empty());
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn functions_prefix_stripped() {
        let raw = RawToolCall {
            id: None,
            name: "functions.web_search".into(),
            arguments_json: r#"{"query":"rust"}"#.into(),
        };
        let (catalog, semantics) = enclave_repair_env();
        let (out, _) = ToolRepairPipeline::repair_with_semantics(&raw, &catalog, &semantics);
        assert!(matches!(out, RepairOutcome::Repaired(r) if r.name == "web_search"));
    }

    #[test]
    fn reject_includes_available_tools() {
        let raw = RawToolCall {
            id: None,
            name: "totally_unknown_tool_xyz".into(),
            arguments_json: "{}".into(),
        };
        let (catalog, semantics) = enclave_repair_env();
        match ToolRepairPipeline::repair_with_semantics(&raw, &catalog, &semantics).0 {
            RepairOutcome::Reject {
                error_json_for_model,
            } => {
                let v: Value = serde_json::from_str(&error_json_for_model).unwrap();
                assert!(v.get("error").is_some());
                let tools = v.get("available_tools").and_then(|t| t.as_array()).unwrap();
                assert_eq!(tools.len(), catalog.all().len());
            }
            o => panic!("expected Reject {o:?}"),
        }
    }

    #[test]
    fn argument_json_with_code_fence() {
        let raw = RawToolCall {
            id: None,
            name: "web_search".into(),
            arguments_json: "```json\n{\"query\":\"x\"}\n```".into(),
        };
        let (catalog, semantics) = enclave_repair_env();
        let (out, _) = ToolRepairPipeline::repair_with_semantics(&raw, &catalog, &semantics);
        match out {
            RepairOutcome::Repaired(rec) => {
                let v: Value = serde_json::from_str(&rec.arguments_json).unwrap();
                assert_eq!(v["query"], "x");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn append_blocks_injects_is_complete_default() {
        let raw = RawToolCall {
            id: None,
            name: "append_blocks".into(),
            arguments_json: r#"{"artifactId":"a","blocks":[]}"#.into(),
        };
        let (catalog, semantics) = enclave_repair_env();
        let (out, _) = ToolRepairPipeline::repair_with_semantics(&raw, &catalog, &semantics);
        match out {
            RepairOutcome::Repaired(rec) => {
                let v: Value = serde_json::from_str(&rec.arguments_json).unwrap();
                assert_eq!(v["isComplete"], false);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn append_blocks_complete_alias_sets_is_complete() {
        let raw = RawToolCall {
            id: None,
            name: "append_blocks".into(),
            arguments_json: r#"{"artifactId":"a","blocks":[],"complete":true}"#.into(),
        };
        let (catalog, semantics) = enclave_repair_env();
        let (out, _) = ToolRepairPipeline::repair_with_semantics(&raw, &catalog, &semantics);
        match out {
            RepairOutcome::Repaired(rec) => {
                let v: Value = serde_json::from_str(&rec.arguments_json).unwrap();
                assert_eq!(v["isComplete"], true);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn append_blocks_string_is_complete_coerced_on_repair() {
        let raw = RawToolCall {
            id: None,
            name: "append_blocks".into(),
            arguments_json: r#"{"artifactId":"a","blocks":[],"isComplete":"true"}"#.into(),
        };
        let (catalog, semantics) = enclave_repair_env();
        let (out, _) = ToolRepairPipeline::repair_with_semantics(&raw, &catalog, &semantics);
        match out {
            RepairOutcome::Repaired(rec) => {
                let v: Value = serde_json::from_str(&rec.arguments_json).unwrap();
                assert_eq!(v["isComplete"], true);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn fuzzy_score_below_cutoff_rejects() {
        let (cat, semantics) = enclave_repair_env();
        let needle = "zzzzz_no_real_match_yyyyy";
        let (_, score) = best_catalog_match(needle, &cat).unwrap();
        assert!(
            score < FUZZY_AUTO_ACCEPT,
            "needle should score below cutoff, got {score}"
        );
        let raw = RawToolCall {
            id: None,
            name: needle.into(),
            arguments_json: "{}".into(),
        };
        assert!(matches!(
            ToolRepairPipeline::repair_with_semantics(&raw, &cat, &semantics).0,
            RepairOutcome::Reject { .. }
        ));
    }
}
