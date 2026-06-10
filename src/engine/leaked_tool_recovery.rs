//! Centralized turn-end leaked tool-call resolution (centralized-leaked-tool-call-recovery).

use crate::catalog::ToolCatalog;
use crate::tool_parse::parse_tool_calls_with_semantics;
use crate::tool_semantics::ToolSemanticsConfig;
use crate::tool_stream_gate::{
    first_strong_tool_marker_start, normalize_spaced_dsml, original_offset_for_normalized_offset,
};
use crate::turn_io::RawToolCall;

/// Turn-end classification result for model text + native tools.
#[derive(Debug, Clone)]
pub struct ModelTurnTextResolution {
    pub visible_assistant_text: String,
    pub native_tool_calls: Vec<RawToolCall>,
    pub recovered_tool_calls: Vec<RawToolCall>,
    pub had_strong_tool_marker: bool,
}

/// Classify buffered assistant text vs native structured tool calls at model turn end.
pub fn resolve_model_turn_text(
    buffered_text: &str,
    native_tool_calls: &[RawToolCall],
    allow_tool_call: bool,
    catalog: &ToolCatalog,
    semantics: &ToolSemanticsConfig,
) -> ModelTurnTextResolution {
    let normalized = normalize_spaced_dsml(buffered_text);
    let text_markers = first_strong_tool_marker_start(&normalized).is_some();
    let had_strong_tool_marker =
        text_markers || (!allow_tool_call && !native_tool_calls.is_empty());

    if !had_strong_tool_marker {
        return ModelTurnTextResolution {
            visible_assistant_text: buffered_text.to_string(),
            native_tool_calls: native_tool_calls.to_vec(),
            recovered_tool_calls: Vec::new(),
            had_strong_tool_marker: false,
        };
    }

    let batch = parse_tool_calls_with_semantics(
        if text_markers {
            &normalized
        } else {
            buffered_text
        },
        catalog,
        semantics,
    );
    let visible_assistant_text = if text_markers {
        if !batch.successes.is_empty() {
            visible_text_from_batch(&batch, buffered_text)
        } else {
            // Marker offsets are byte positions in `normalized`; spaced-DSML
            // collapse can shift them relative to `buffered_text`, so map back
            // before slicing the raw text.
            first_strong_tool_marker_start(&normalized)
                .and_then(|start| original_offset_for_normalized_offset(buffered_text, start))
                .map(|start| buffered_text[..start].trim_end().to_string())
                .unwrap_or_default()
        }
    } else {
        buffered_text.to_string()
    };

    if !allow_tool_call {
        return ModelTurnTextResolution {
            visible_assistant_text,
            native_tool_calls: native_tool_calls.to_vec(),
            recovered_tool_calls: Vec::new(),
            had_strong_tool_marker: true,
        };
    }

    if !native_tool_calls.is_empty() {
        return ModelTurnTextResolution {
            visible_assistant_text,
            native_tool_calls: native_tool_calls.to_vec(),
            recovered_tool_calls: Vec::new(),
            had_strong_tool_marker: true,
        };
    }

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

    ModelTurnTextResolution {
        visible_assistant_text,
        native_tool_calls: Vec::new(),
        recovered_tool_calls,
        had_strong_tool_marker: true,
    }
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
