//! LiteRT / Kotlin policy helpers (AGENT-034) — small UniFFI surface separate from the session engine.

use super::types::{
    FfiToolParseBatch, FfiToolParseFailure, FfiToolParseFailureReason, FfiToolParseField,
};
use crate::guardrails::is_soft_failure_result_json;
use crate::repair::ToolRepairPipeline;
use crate::tool_parse;
use crate::catalog::ToolSpec;
use crate::tool_semantics::{ToolCatalogConfig, ToolSemanticsConfig};
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

/// ENC-297 Chunk 1 interim: parse the two JSON strings directly (the core
/// merge helper is deleted). Replicates the old merge exactly — catalog JSON
/// (array or `{tools:}` object form) fills an empty semantics catalog, then
/// validates — until Chunk 2 deletes both JSON strings for typed records.
fn load_policy_semantics(
    tool_semantics_json: &str,
    tool_catalog_json: &str,
) -> Result<(ToolSemanticsConfig, crate::catalog::ToolCatalog), String> {
    let mut semantics = ToolSemanticsConfig::from_json(tool_semantics_json)
        .map_err(|error| format!("invalid tool_semantics_json: {error}"))?;
    let trimmed = tool_catalog_json.trim();
    if !trimmed.is_empty() && trimmed != "[]" {
        #[derive(serde::Deserialize)]
        struct CatalogFile {
            tools: Vec<ToolSpec>,
        }
        let tools = if trimmed.starts_with('[') {
            serde_json::from_str(trimmed)
                .map_err(|error| format!("invalid tool catalog json: {error}"))?
        } else {
            serde_json::from_str::<CatalogFile>(trimmed)
                .map_err(|error| format!("invalid tool catalog json: {error}"))?
                .tools
        };
        if semantics.catalog.tools.is_empty() {
            semantics.catalog = ToolCatalogConfig { tools };
        }
        semantics
            .validate()
            .map_err(|error| format!("tool catalog merge failed: {error:?}"))?;
    }
    let catalog = crate::catalog::ToolCatalog::from_semantics(&semantics)
        .map_err(|error| format!("invalid merged tool catalog: {error:?}"))?;
    Ok((semantics, catalog))
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
                arguments_json: serde_json::json!({ "error": error }).to_string(),
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
            arguments_json: serde_json::json!({
                "error": format!("Ambiguous tool (closest match: {closest_name})"),
            })
            .to_string(),
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
    let (semantics, catalog) = match load_policy_semantics(tool_semantics_json, tool_catalog_json) {
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
    // ENC-297 Chunk 1 interim: agnostic semantics plus the catalog JSON
    // parsed directly (the core merge helper is deleted; typed records
    // arrive in Chunk 2). Invalid input yields false — the infallible
    // contract is preserved, minus the eprintln.
    let mut semantics = ToolSemanticsConfig::agnostic();
    let trimmed = tool_catalog_json.trim();
    if !trimmed.is_empty() && trimmed != "[]" {
        #[derive(serde::Deserialize)]
        struct CatalogFile {
            tools: Vec<ToolSpec>,
        }
        let tools: Vec<ToolSpec> = if trimmed.starts_with('[') {
            match serde_json::from_str(trimmed) {
                Ok(tools) => tools,
                Err(_) => return false,
            }
        } else {
            match serde_json::from_str::<CatalogFile>(trimmed) {
                Ok(file) => file.tools,
                Err(_) => return false,
            }
        };
        semantics.catalog = ToolCatalogConfig { tools };
        if semantics.validate().is_err() {
            return false;
        }
    }
    let catalog = match crate::catalog::ToolCatalog::from_semantics(&semantics) {
        Ok(catalog) => catalog,
        Err(_) => return false,
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
    let (semantics, catalog) = match load_policy_semantics(tool_semantics_json, tool_catalog_json) {
        Ok(loaded) => loaded,
        Err(error) => {
            return FfiToolParseField {
                tool_name: tool_name.to_string(),
                arguments_json: serde_json::json!({ "error": error }).to_string(),
                repaired: false,
                repairs: Vec::new(),
            };
        }
    };
    tool_parse::coerce_provider_arguments_with_semantics(tool_name, args_json, &catalog, &semantics)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_tool_error_payload_is_valid_json_even_with_quotes_in_error() {
        // Malformed catalog JSON forces the load-error path; serde error text
        // contains characters that must be JSON-escaped.
        let result = repair_tool("web_search", "{}", "{ not json", r#"{"broken": "#);
        let parsed: serde_json::Value =
            serde_json::from_str(&result.arguments_json).expect("error payload must be valid JSON");
        assert!(parsed.get("error").is_some(), "payload: {parsed}");
        assert!(!result.repaired);
    }

    #[test]
    fn coerce_provider_arguments_error_payload_is_valid_json() {
        let field = coerce_provider_arguments("web_search", "{}", "{ not json", r#"{"broken": "#);
        let parsed: serde_json::Value =
            serde_json::from_str(&field.arguments_json).expect("error payload must be valid JSON");
        assert!(parsed.get("error").is_some(), "payload: {parsed}");
    }
}
