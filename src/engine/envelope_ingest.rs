//! Turn-end envelope ingest (TOOL-PARSE-002).

use crate::tool_parse::envelope_failure_json;
use crate::tool_parse::{parse_tool_calls, ToolParseBatch};
use crate::tool_stream_gate::first_tool_marker_start;
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
) -> (EnvelopeIngestOutcome, Vec<RawToolCall>) {
    let text = assistant_text.unwrap_or("");
    if first_tool_marker_start(text).is_none() {
        return (
            EnvelopeIngestOutcome::NoMarkers,
            native_tool_calls.to_vec(),
        );
    }

    let batch = parse_tool_calls(text);
    let visible = visible_text_from_batch(&batch, text);

    if batch.successes.is_empty() {
        if let Some(failure) = batch.failures.first() {
            return (
                EnvelopeIngestOutcome::ParseFailed {
                    error_json_for_model: envelope_failure_json(failure),
                    visible_assistant_text: visible,
                },
                native_tool_calls.to_vec(),
            );
        }
        return (
            EnvelopeIngestOutcome::NoMarkers,
            native_tool_calls.to_vec(),
        );
    }

    let mut merged = native_tool_calls.to_vec();
    for (index, field) in batch.successes.iter().enumerate() {
        merged.push(RawToolCall {
            id: Some(format!("text-tool-{}", index + 1)),
            name: field.tool_name.clone(),
            arguments_json: field.arguments_json.clone(),
        });
    }

    (
        EnvelopeIngestOutcome::Parsed {
            tool_calls: merged.clone(),
            visible_assistant_text: visible,
        },
        merged,
    )
}

fn visible_text_from_batch(batch: &ToolParseBatch, fallback: &str) -> String {
    if batch.visible_prefix.is_empty() && batch.visible_suffix.is_empty() {
        return fallback.to_string();
    }
    format!("{}{}", batch.visible_prefix, batch.visible_suffix)
}
