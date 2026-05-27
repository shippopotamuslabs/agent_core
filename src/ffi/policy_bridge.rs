//! LiteRT / Kotlin policy helpers (AGENT-034) — small UniFFI surface separate from the session engine.

use super::types::{FfiToolParseBatch, FfiToolParseField, FfiToolParseFailure, FfiToolParseFailureReason};
use crate::guardrails::is_soft_failure_result_json;
use crate::repair::ToolRepairPipeline;
use crate::tool_parse;
use crate::tool_semantics::{load_semantics_with_session_catalog, ToolSemanticsConfig};
use crate::truncate::truncate_tool_result_json;
use crate::turn_io::RawToolCall;

#[derive(Debug, Clone, uniffi::Record)]
pub struct RepairResult {
    /// Resolved tool name after catalog + fuzzy repair.
    pub resolved_name: String,
    /// Possibly coerced JSON arguments.
    pub arguments_json: String,
    /// True when the name or arguments were changed by repair.
    pub repaired: bool,
}

fn load_policy_semantics(
    tool_semantics_json: &str,
    tool_catalog_json: &str,
) -> Result<(ToolSemanticsConfig, crate::catalog::ToolCatalog), String> {
    load_semantics_with_session_catalog(tool_semantics_json, tool_catalog_json).map_err(|error| {
        eprintln!("agent_core: {error}");
        error
    })
}

#[uniffi::export]
pub fn repair_tool(
    name: &str,
    args_json: &str,
    tool_semantics_json: &str,
    tool_catalog_json: &str,
) -> RepairResult {
    let (semantics, catalog) = match load_policy_semantics(tool_semantics_json, tool_catalog_json) {
        Ok(loaded) => loaded,
        Err(error) => {
            return RepairResult {
                resolved_name: name.to_string(),
                arguments_json: format!(r#"{{"error":"{error}"}}"#),
                repaired: false,
            };
        }
    };
    let raw = RawToolCall {
        id: Some("policy".into()),
        name: name.to_string(),
        arguments_json: args_json.to_string(),
    };
    let (outcome, _) = ToolRepairPipeline::repair_with_semantics(&raw, &catalog, &semantics);
    match outcome {
        crate::repair::RepairOutcome::Repaired(rec) => RepairResult {
            repaired: rec.repair_metadata.is_some()
                || rec.name != name.trim()
                || rec.arguments_json != args_json,
            resolved_name: rec.name,
            arguments_json: rec.arguments_json,
        },
        crate::repair::RepairOutcome::Reject {
            error_json_for_model,
        } => RepairResult {
            resolved_name: name.to_string(),
            arguments_json: error_json_for_model,
            repaired: false,
        },
        crate::repair::RepairOutcome::Suggest {
            original,
            closest_name,
        } => RepairResult {
            resolved_name: original,
            arguments_json: format!(
                "{{\"error\":\"Ambiguous tool (closest match: {closest_name})\"}}"
            ),
            repaired: false,
        },
    }
}

#[uniffi::export]
pub fn truncate_result(json: &str, budget_bytes: u64) -> String {
    let (s, _) = truncate_tool_result_json(json, budget_bytes as usize);
    s
}

#[uniffi::export]
pub fn should_allow_tool(soft_failure_streak: u32, max_streak: u32) -> bool {
    soft_failure_streak < max_streak
}

#[uniffi::export]
pub fn result_json_looks_soft_failure(json: &str) -> bool {
    is_soft_failure_result_json(json)
}

#[uniffi::export]
pub fn parse_tool_calls(
    raw: &str,
    tool_semantics_json: &str,
    tool_catalog_json: &str,
) -> FfiToolParseBatch {
    let (semantics, catalog) = match load_policy_semantics(tool_semantics_json, tool_catalog_json)
    {
        Ok(loaded) => loaded,
        Err(error) => {
            return FfiToolParseBatch {
                successes: Vec::new(),
                failures: vec![FfiToolParseFailure {
                    reason: FfiToolParseFailureReason::InvalidJson {
                        detail: error.clone(),
                    },
                    original: raw.to_string(),
                    format_hint: Some("tool catalog merge failed".into()),
                }],
                visible_prefix: String::new(),
                visible_suffix: String::new(),
            };
        }
    };
    tool_parse::parse_tool_calls_with_semantics(raw, &catalog, &semantics).into()
}

#[uniffi::export]
pub fn has_local_tool_contract(system_prompt: &str, tool_catalog_json: &str) -> bool {
    let mut semantics = ToolSemanticsConfig::agnostic();
    if let Err(error) = semantics.merge_catalog_json(tool_catalog_json) {
        eprintln!("agent_core: tool catalog merge failed: {error:?}");
        return false;
    }
    let catalog = match crate::catalog::ToolCatalog::from_semantics(&semantics) {
        Ok(catalog) => catalog,
        Err(error) => {
            eprintln!("agent_core: invalid merged tool catalog: {error:?}");
            return false;
        }
    };
    tool_parse::has_local_tool_contract(system_prompt, &catalog)
}

#[uniffi::export]
pub fn coerce_provider_arguments(
    tool_name: &str,
    args_json: &str,
    tool_semantics_json: &str,
    tool_catalog_json: &str,
) -> FfiToolParseField {
    let (semantics, catalog) = match load_policy_semantics(tool_semantics_json, tool_catalog_json)
    {
        Ok(loaded) => loaded,
        Err(error) => {
            return FfiToolParseField {
                tool_name: tool_name.to_string(),
                arguments_json: format!(r#"{{"error":"{error}"}}"#),
                repaired: false,
                repairs: Vec::new(),
            };
        }
    };
    tool_parse::coerce_provider_arguments_with_semantics(tool_name, args_json, &catalog, &semantics)
        .into()
}
