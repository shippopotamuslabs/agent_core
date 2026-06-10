//! Model-facing JSON for envelope parse failures (TOOL-PARSE-002).

use serde_json::json;

use crate::catalog::ToolCatalog;
use crate::tool_parse::result::{ToolParseFailure, ToolParseFailureReason};

/// Shown in chat when the model keeps emitting tool syntax after tools were disabled.
pub const TOOLS_BLOCKED_USER_FALLBACK: &str =
    "Sorry, something went wrong on my end. Please try again.";

/// Model-facing payload when tool calls are disabled but the model emitted tool syntax anyway.
pub fn tools_blocked_nudge_json() -> String {
    serde_json::to_string(&json!({
        "error": "tool_call_limit_exhausted",
        "message": "Error: tool call limit exhausted. No more tool calls will be executed \
this turn. Do not emit tool_call, DSML, or function-call syntax again. \
Answer the user in plain text now, using the information you already have. \
If the task is incomplete, say what is done and what remains."
    }))
    .unwrap_or_else(|_| {
        r#"{"error":"tool_call_limit_exhausted","message":"Error: tool call limit exhausted. No more tool calls! Answer in plain text only."}"#
            .to_string()
    })
}

/// Same top-level shape as repair reject payloads: `error` + `available_tools`.
pub fn envelope_failure_json(failure: &ToolParseFailure, catalog: &ToolCatalog) -> String {
    let message = envelope_failure_message(failure);
    let tools: Vec<&str> = catalog
        .all()
        .iter()
        .map(|spec| spec.name.as_str())
        .collect();
    serde_json::to_string(&json!({
        "error": message,
        "available_tools": tools,
    }))
    .unwrap_or_else(|_| format!(r#"{{"error":"{message}"}}"#))
}

fn envelope_failure_message(failure: &ToolParseFailure) -> String {
    match &failure.reason {
        ToolParseFailureReason::MalformedEnvelope => {
            if failure.format_hint.as_deref() == Some("dsml") {
                "Malformed DSML tool envelope".to_string()
            } else {
                "Malformed tool envelope".to_string()
            }
        }
        ToolParseFailureReason::InvalidJson { detail } => {
            format!("Malformed tool_call envelope: {detail}")
        }
        ToolParseFailureReason::UnknownTool { name } => {
            format!("Unsupported tool: {name}")
        }
        ToolParseFailureReason::UnknownParameter { tool, param } => {
            format!("Unknown parameter '{param}' for tool '{tool}'")
        }
        ToolParseFailureReason::DuplicateParameter { tool, param } => {
            format!("Duplicate parameter '{param}' for tool '{tool}'")
        }
        ToolParseFailureReason::MissingRequiredParameter { tool, param } => {
            if tool == "web_extract" && param == "urls" {
                "Tool argument 'urls' is required and must be a non-empty array".to_string()
            } else {
                format!("Tool argument '{param}' is required")
            }
        }
        ToolParseFailureReason::SchemaCoercionFailed { tool, param, .. } => {
            format!("Tool argument '{param}' is required for tool '{tool}'")
        }
    }
}
