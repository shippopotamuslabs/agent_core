//! Turn-end envelope ingest (TOOL-PARSE-002).

use crate::engine::leaked_tool_recovery::resolve_model_turn_text;
use crate::tool_parse::envelope_failure_json;
use crate::tool_parse::parse_tool_calls;
use crate::turn_io::RawToolCall;

#[derive(Debug, Clone)]
pub enum EnvelopeIngestOutcome {
    NoMarkers,
    Parsed {
        tool_calls: Vec<RawToolCall>,
        visible_assistant_text: String,
    },
    ParseFailed {
        error_json_for_model: String,
        visible_assistant_text: String,
    },
}

/// Detect leaked envelopes in assistant text, reify tool calls, and strip marker regions.
pub fn ingest_model_response_envelopes(
    assistant_text: Option<&str>,
    native_tool_calls: &[RawToolCall],
    allow_tool_call: bool,
) -> (EnvelopeIngestOutcome, Vec<RawToolCall>) {
    let text = assistant_text.unwrap_or("");
    let resolution = resolve_model_turn_text(text, native_tool_calls, allow_tool_call);

    if !resolution.had_strong_tool_marker {
        return (EnvelopeIngestOutcome::NoMarkers, native_tool_calls.to_vec());
    }

    let execution_tools = if !resolution.native_tool_calls.is_empty() {
        resolution.native_tool_calls.clone()
    } else {
        resolution.recovered_tool_calls.clone()
    };

    if execution_tools.is_empty() {
        let batch = parse_tool_calls(text);
        if let Some(failure) = batch.failures.first() {
            return (
                EnvelopeIngestOutcome::ParseFailed {
                    error_json_for_model: envelope_failure_json(failure),
                    visible_assistant_text: resolution.visible_assistant_text,
                },
                native_tool_calls.to_vec(),
            );
        }
        return (EnvelopeIngestOutcome::NoMarkers, native_tool_calls.to_vec());
    }

    (
        EnvelopeIngestOutcome::Parsed {
            tool_calls: execution_tools.clone(),
            visible_assistant_text: resolution.visible_assistant_text.clone(),
        },
        execution_tools,
    )
}
