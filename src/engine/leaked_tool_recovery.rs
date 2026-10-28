//! Centralized turn-end leaked tool-call resolution (centralized-leaked-tool-call-recovery).

use crate::catalog::ToolCatalog;
use crate::tool_call_format::ToolCallFormat;
use crate::tool_parse::parse_tool_calls_with_semantics;
use crate::tool_semantics::ToolSemanticsConfig;
use crate::tool_stream_gate::{
    declared_marker_start, normalize_spaced_dsml, original_offset_for_normalized_offset,
    other_format_marker_start,
};
use crate::turn_io::RawToolCall;

/// Turn-end classification result for model text + native tools.
#[derive(Debug, Clone)]
pub struct ModelTurnTextResolution {
    pub visible_assistant_text: String,
    pub native_tool_calls: Vec<RawToolCall>,
    pub recovered_tool_calls: Vec<RawToolCall>,
    pub had_strong_tool_marker: bool,
    /// Declared-format markers absent but another envelope shape's strong
    /// markers present: the turn must fail loudly naming the declared format
    /// (no fallback parse of the other shape).
    pub wrong_format_marker: bool,
}

/// Classify buffered assistant text vs native structured tool calls at model turn end.
///
/// Declared-format contract (ENC-300 D4):
/// * `Structured`: native calls only; body text is never mined, truncated,
///   or leak-recovered.
/// * Envelope(`fmt`): only `fmt`'s parser runs on text; text-parsed calls are
///   the calls and the native channel is ignored. Declared markers present
///   but malformed surfaces as `ParseFailed` downstream. Other-format markers
///   with declared markers absent sets `wrong_format_marker`. No markers at
///   all is a plain text turn (native passes through).
pub fn resolve_model_turn_text(
    format: ToolCallFormat,
    buffered_text: &str,
    native_tool_calls: &[RawToolCall],
    allow_tool_call: bool,
    catalog: &ToolCatalog,
    semantics: &ToolSemanticsConfig,
) -> ModelTurnTextResolution {
    if format.is_structured() {
        return resolve_structured(buffered_text, native_tool_calls, allow_tool_call);
    }
    resolve_envelope(
        format,
        buffered_text,
        native_tool_calls,
        allow_tool_call,
        catalog,
        semantics,
    )
}

/// Structured sessions: native calls only. Body text is never mined,
/// truncated, or leak-recovered; the tools-disabled + native-calls
/// `ToolsBlocked` detection is preserved.
fn resolve_structured(
    buffered_text: &str,
    native_tool_calls: &[RawToolCall],
    allow_tool_call: bool,
) -> ModelTurnTextResolution {
    ModelTurnTextResolution {
        visible_assistant_text: buffered_text.to_string(),
        native_tool_calls: native_tool_calls.to_vec(),
        recovered_tool_calls: Vec::new(),
        had_strong_tool_marker: !allow_tool_call && !native_tool_calls.is_empty(),
        wrong_format_marker: false,
    }
}

fn resolve_envelope(
    format: ToolCallFormat,
    buffered_text: &str,
    native_tool_calls: &[RawToolCall],
    allow_tool_call: bool,
    catalog: &ToolCatalog,
    semantics: &ToolSemanticsConfig,
) -> ModelTurnTextResolution {
    let normalized = normalize_spaced_dsml(buffered_text);
    let declared_start = declared_marker_start(&normalized, 0, format);
    let other_start = if declared_start.is_none() {
        other_format_marker_start(&normalized, format)
    } else {
        None
    };
    let blocked_native = !allow_tool_call && !native_tool_calls.is_empty();
    let had_marker = declared_start.is_some() || other_start.is_some() || blocked_native;

    if !had_marker {
        // No markers at all: plain text turn, native passes through.
        return ModelTurnTextResolution {
            visible_assistant_text: buffered_text.to_string(),
            native_tool_calls: native_tool_calls.to_vec(),
            recovered_tool_calls: Vec::new(),
            had_strong_tool_marker: false,
            wrong_format_marker: false,
        };
    }

    if let Some(start) = declared_start {
        let batch = parse_tool_calls_with_semantics(format, &normalized, catalog, semantics);
        let visible_assistant_text = if !batch.successes.is_empty() {
            visible_text_from_batch(&batch, buffered_text)
        } else {
            truncate_before_normalized_marker(buffered_text, start)
        };

        if !allow_tool_call {
            return ModelTurnTextResolution {
                visible_assistant_text,
                native_tool_calls: native_tool_calls.to_vec(),
                recovered_tool_calls: Vec::new(),
                had_strong_tool_marker: true,
                wrong_format_marker: false,
            };
        }

        // Text-parsed calls are the calls; the native channel is ignored.
        let recovered_tool_calls = batch
            .successes
            .iter()
            .enumerate()
            .map(|(index, field)| RawToolCall {
                id: Some(format!("text-tool-{}", index + 1)),
                name: field.tool_name.clone(),
                arguments_json: field.arguments_json.clone(),
            })
            .collect();
        return ModelTurnTextResolution {
            visible_assistant_text,
            native_tool_calls: Vec::new(),
            recovered_tool_calls,
            had_strong_tool_marker: true,
            wrong_format_marker: false,
        };
    }

    if let Some(start) = other_start {
        // Wrong-format syntax with declared markers absent: never parsed
        // (no fallback); the ingest layer fails loudly naming the declared
        // format. Visible text is truncated before the offending syntax.
        return ModelTurnTextResolution {
            visible_assistant_text: truncate_before_normalized_marker(buffered_text, start),
            native_tool_calls: if allow_tool_call {
                Vec::new()
            } else {
                native_tool_calls.to_vec()
            },
            recovered_tool_calls: Vec::new(),
            had_strong_tool_marker: true,
            wrong_format_marker: true,
        };
    }

    // Blocked native calls with no text markers at all.
    ModelTurnTextResolution {
        visible_assistant_text: buffered_text.to_string(),
        native_tool_calls: native_tool_calls.to_vec(),
        recovered_tool_calls: Vec::new(),
        had_strong_tool_marker: true,
        wrong_format_marker: false,
    }
}

/// Truncate raw text before a marker offset measured in
/// `normalize_spaced_dsml` space: spaced-DSML collapse can shift offsets
/// relative to the raw text (and across multi-byte chars), so map back
/// before slicing.
fn truncate_before_normalized_marker(buffered_text: &str, normalized_start: usize) -> String {
    original_offset_for_normalized_offset(buffered_text, normalized_start)
        .map(|start| buffered_text[..start].trim_end().to_string())
        .unwrap_or_default()
}

fn visible_text_from_batch(batch: &crate::tool_parse::ToolParseBatch, fallback: &str) -> String {
    if !batch.successes.is_empty() {
        return format!("{}{}", batch.visible_prefix, batch.visible_suffix);
    }
    if batch.visible_prefix.is_empty() && batch.visible_suffix.is_empty() {
        return fallback.to_string();
    }
    format!("{}{}", batch.visible_prefix, batch.visible_suffix)
}
