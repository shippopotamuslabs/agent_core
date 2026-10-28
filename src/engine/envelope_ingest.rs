//! Turn-end envelope ingest (TOOL-PARSE-002).

use crate::catalog::ToolCatalog;
use crate::engine::leaked_tool_recovery::resolve_model_turn_text;
use crate::tool_call_format::ToolCallFormat;
use crate::tool_parse::envelope_error::{tools_blocked_nudge_json, TOOLS_BLOCKED_USER_FALLBACK};
use crate::tool_parse::envelope_failure_json;
use crate::tool_parse::parse_tool_calls_with_semantics;
use crate::tool_parse::{ToolParseFailure, ToolParseFailureReason};
use crate::tool_semantics::ToolSemanticsConfig;
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
    /// Tool syntax detected while `allow_tool_call` is false.
    ToolsBlocked {
        nudge_json_for_model: String,
        visible_assistant_text: String,
        user_fallback_text: String,
    },
}

/// Detect leaked envelopes in assistant text, reify tool calls, and strip marker regions.
///
/// Declared-format contract (ENC-300 D4):
/// * `Structured`: native calls only; body text is never mined or truncated.
///   `ToolsBlocked` when tools are disabled and native calls are present is
///   preserved.
/// * Envelope(`fmt`): only `fmt`'s parser runs on text; text-parsed calls are
///   the calls and the native channel is ignored. Declared markers present
///   but malformed => `ParseFailed`. Other-format markers with declared
///   markers absent => `ParseFailed` naming the declared format (loud, no
///   fallback). No markers at all => plain text turn.
pub fn ingest_model_response_envelopes(
    format: ToolCallFormat,
    assistant_text: Option<&str>,
    native_tool_calls: &[RawToolCall],
    allow_tool_call: bool,
    catalog: &ToolCatalog,
    semantics: &ToolSemanticsConfig,
) -> (EnvelopeIngestOutcome, Vec<RawToolCall>) {
    let text = assistant_text.unwrap_or("");

    if format.is_structured() {
        // Structured: native calls only; body text is never mined/truncated
        // and leak recovery never runs.
        if !allow_tool_call && !native_tool_calls.is_empty() {
            return (
                EnvelopeIngestOutcome::ToolsBlocked {
                    nudge_json_for_model: tools_blocked_nudge_json(),
                    visible_assistant_text: text.to_string(),
                    user_fallback_text: TOOLS_BLOCKED_USER_FALLBACK.to_string(),
                },
                Vec::new(),
            );
        }
        return (EnvelopeIngestOutcome::NoMarkers, native_tool_calls.to_vec());
    }

    let resolution = resolve_model_turn_text(
        format,
        text,
        native_tool_calls,
        allow_tool_call,
        catalog,
        semantics,
    );

    if !allow_tool_call {
        if resolution.had_strong_tool_marker {
            return (
                EnvelopeIngestOutcome::ToolsBlocked {
                    nudge_json_for_model: tools_blocked_nudge_json(),
                    visible_assistant_text: resolution.visible_assistant_text,
                    user_fallback_text: TOOLS_BLOCKED_USER_FALLBACK.to_string(),
                },
                Vec::new(),
            );
        }
        return (EnvelopeIngestOutcome::NoMarkers, native_tool_calls.to_vec());
    }

    if resolution.wrong_format_marker {
        // Loud, no fallback: other-format syntax while the declared format's
        // markers are absent. The error names the declared format.
        let failure = ToolParseFailure {
            reason: ToolParseFailureReason::MalformedEnvelope,
            original: text.to_string(),
            format_hint: format.format_hint().map(str::to_string),
        };
        return (
            EnvelopeIngestOutcome::ParseFailed {
                error_json_for_model: envelope_failure_json(&failure, catalog),
                visible_assistant_text: resolution.visible_assistant_text,
            },
            Vec::new(),
        );
    }

    if !resolution.had_strong_tool_marker {
        return (EnvelopeIngestOutcome::NoMarkers, native_tool_calls.to_vec());
    }

    if !resolution.recovered_tool_calls.is_empty() {
        let execution_tools = resolution.recovered_tool_calls.clone();
        return (
            EnvelopeIngestOutcome::Parsed {
                tool_calls: execution_tools.clone(),
                visible_assistant_text: resolution.visible_assistant_text.clone(),
            },
            execution_tools,
        );
    }

    // Declared markers present but malformed: surface the declared parser's
    // failure (existing ParseFailed shape).
    let batch = parse_tool_calls_with_semantics(format, text, catalog, semantics);
    if let Some(failure) = batch.failures.first() {
        return (
            EnvelopeIngestOutcome::ParseFailed {
                error_json_for_model: envelope_failure_json(failure, catalog),
                visible_assistant_text: resolution.visible_assistant_text,
            },
            Vec::new(),
        );
    }
    let failure = ToolParseFailure {
        reason: ToolParseFailureReason::MalformedEnvelope,
        original: text.to_string(),
        format_hint: format.format_hint().map(str::to_string),
    };
    (
        EnvelopeIngestOutcome::ParseFailed {
            error_json_for_model: envelope_failure_json(&failure, catalog),
            visible_assistant_text: resolution.visible_assistant_text,
        },
        Vec::new(),
    )
}
