//! LiteRT / Kotlin policy helpers (AGENT-034) — small UniFFI surface separate from the session engine.

use crate::catalog::ToolCatalog;
use crate::guardrails::is_soft_failure_result_json;
use crate::repair::ToolRepairPipeline;
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

#[uniffi::export]
pub fn repair_tool(name: &str, args_json: &str) -> RepairResult {
    let raw = RawToolCall {
        id: Some("policy".into()),
        name: name.to_string(),
        arguments_json: args_json.to_string(),
    };
    let (outcome, _) = ToolRepairPipeline::repair(&raw, ToolCatalog::global());
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

/// Mirrors Kotlin soft-failure streak guard (`consecutiveSoftFailureTurns < tool_retry_limit`).
#[uniffi::export]
pub fn should_allow_tool(soft_failure_streak: u32, max_streak: u32) -> bool {
    soft_failure_streak < max_streak
}

#[uniffi::export]
pub fn result_json_looks_soft_failure(json: &str) -> bool {
    is_soft_failure_result_json(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_tool_fuzzy() {
        let r = repair_tool("web_seach", r#"{"query":"x"}"#);
        assert_eq!(r.resolved_name, "web_search");
        assert!(r.repaired);
    }

    #[test]
    fn truncate_result_shortens() {
        let long = "x".repeat(20_000);
        let out = truncate_result(&long, 100u64);
        assert!(out.len() <= long.len());
    }
}
