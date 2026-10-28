//! LiteRT / Kotlin policy helpers (AGENT-034) — small UniFFI surface separate from the session engine.

use super::semantics::{FfiToolSemantics, FfiToolSpec};
use super::types::{
    AgentException, FfiToolParseBatch, FfiToolParseFailure, FfiToolParseFailureReason,
    FfiToolParseField,
};
use crate::guardrails::is_soft_failure_result_json;
use crate::repair::ToolRepairPipeline;
use crate::tool_parse;
use crate::tool_semantics::ToolSemanticsConfig;
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

/// Typed policy-semantics loader (ENC-297 Chunk 2): the sibling specs are
/// assigned into `semantics.catalog.tools`, then the single core constructor
/// path validates. `None` semantics is host-declared agnostic. Errors are
/// returned as strings for the infallible helpers' error payloads — never
/// `eprintln`d.
fn load_policy_semantics(
    tool_semantics: Option<FfiToolSemantics>,
    tool_specs: Vec<FfiToolSpec>,
) -> Result<(ToolSemanticsConfig, crate::catalog::ToolCatalog), String> {
    fn detail(error: AgentException) -> String {
        match error {
            AgentException::Internal { detail } => detail,
            other => format!("{other:?}"),
        }
    }
    let mut semantics = match tool_semantics {
        Some(tree) => tree.into_core().map_err(detail)?,
        None => ToolSemanticsConfig::agnostic(),
    };
    semantics.catalog.tools = tool_specs
        .into_iter()
        .map(|spec| spec.into_core())
        .collect::<Result<Vec<_>, _>>()
        .map_err(detail)?;
    let catalog = crate::catalog::ToolCatalog::from_semantics(&semantics)
        .map_err(|error| format!("invalid tool semantics: {error:?}"))?;
    Ok((semantics, catalog))
}

#[uniffi::export]
pub fn repair_tool(
    name: &str,
    args_json: &str,
    tool_semantics: Option<FfiToolSemantics>,
    tool_specs: Vec<FfiToolSpec>,
) -> RepairResult {
    let (semantics, catalog) = match load_policy_semantics(tool_semantics, tool_specs) {
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
    tool_semantics: Option<FfiToolSemantics>,
    tool_specs: Vec<FfiToolSpec>,
) -> FfiToolParseBatch {
    let (semantics, catalog) = match load_policy_semantics(tool_semantics, tool_specs) {
        Ok(loaded) => loaded,
        Err(error) => {
            return FfiToolParseBatch {
                successes: Vec::new(),
                failures: vec![FfiToolParseFailure {
                    reason: FfiToolParseFailureReason::InvalidJson {
                        detail: error.clone(),
                    },
                    original: raw.to_string(),
                    format_hint: Some("typed tool semantics conversion failed".into()),
                }],
                visible_prefix: String::new(),
                visible_suffix: String::new(),
            };
        }
    };
    tool_parse::parse_tool_calls_with_semantics(raw, &catalog, &semantics).into()
}

#[uniffi::export]
pub fn has_local_tool_contract(
    system_prompt: &str,
    tool_semantics: Option<FfiToolSemantics>,
    tool_specs: Vec<FfiToolSpec>,
) -> bool {
    // Infallible contract preserved (ENC-297): invalid typed input yields
    // false, never an eprintln.
    let (_semantics, catalog) = match load_policy_semantics(tool_semantics, tool_specs) {
        Ok(loaded) => loaded,
        Err(_) => return false,
    };
    tool_parse::has_local_tool_contract(system_prompt, &catalog)
}

#[uniffi::export]
pub fn coerce_provider_arguments(
    tool_name: &str,
    args_json: &str,
    tool_semantics: Option<FfiToolSemantics>,
    tool_specs: Vec<FfiToolSpec>,
) -> FfiToolParseField {
    let (semantics, catalog) = match load_policy_semantics(tool_semantics, tool_specs) {
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
    use crate::catalog::ToolSpec;

    fn spec(name: &str) -> FfiToolSpec {
        FfiToolSpec {
            name: name.into(),
            aliases: Vec::new(),
            reproducible: true,
            schema_hint: None,
        }
    }

    fn duplicate_specs() -> Vec<FfiToolSpec> {
        vec![spec("web_search"), spec("web-search")]
    }

    #[test]
    fn repair_tool_error_payload_is_valid_json_on_invalid_specs() {
        // Duplicate (normalized-equal) spec names fail core validation; the
        // error text lands in the JSON payload (properly escaped).
        let result = repair_tool("web_search", "{}", None, duplicate_specs());
        let parsed: serde_json::Value =
            serde_json::from_str(&result.arguments_json).expect("error payload must be valid JSON");
        assert!(parsed.get("error").is_some(), "payload: {parsed}");
        assert!(!result.repaired);
    }

    #[test]
    fn coerce_provider_arguments_error_payload_is_valid_json() {
        let field = coerce_provider_arguments("web_search", "{}", None, duplicate_specs());
        let parsed: serde_json::Value =
            serde_json::from_str(&field.arguments_json).expect("error payload must be valid JSON");
        assert!(parsed.get("error").is_some(), "payload: {parsed}");
    }

    #[test]
    fn typed_specs_repair_without_json_strings() {
        // websearch is an alias of web_search in the typed specs: no JSON
        // strings cross, and repair resolves through the typed catalog.
        let mut aliased = spec("web_search");
        aliased.aliases = vec!["websearch".into()];
        let result = repair_tool("websearch", "{}", None, vec![aliased]);
        assert!(result.repaired, "payload: {}", result.arguments_json);
        assert_eq!(result.resolved_name, "web_search");
    }

    #[test]
    fn core_spec_type_still_convertible_for_policy_tests() {
        // Keeps the core ToolSpec import load-bearing for policy tests.
        let core = ToolSpec {
            name: "x".into(),
            aliases: Vec::new(),
            reproducible: true,
            schema_hint: None,
        };
        assert_eq!(FfiToolSpec::from(&core).name, "x");
    }
}
