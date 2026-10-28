//! Char-boundary safety for tool_stream_gate streaming.

use agent_core::tool_call_format::ToolCallFormat;
use agent_core::tool_stream_gate::{first_strong_tool_marker_start, ToolStreamGate};
use std::panic::{catch_unwind, AssertUnwindSafe};

fn push_chunked_bytes(gate: &mut ToolStreamGate, raw: &str, chunk_size: usize) {
    for chunk in raw.as_bytes().chunks(chunk_size) {
        if let Ok(piece) = std::str::from_utf8(chunk) {
            gate.push(piece);
        }
    }
}

#[test]
fn spaced_dsml_marker_maps_to_original_start() {
    let original = "prefix < | DSML | tool_calls>";
    let start = first_strong_tool_marker_start(original).expect("marker start");
    assert!(original.is_char_boundary(start));
    assert!(original[start..].starts_with('<'));
}

#[test]
fn emoji_cot_with_spaced_dsml_and_chunked_push_does_not_panic() {
    let text = "📄 **Title** reasoning step\n\nI will use < | DSML | tool_calls> next";
    for chunk_size in 1..=12 {
        let text = text.to_string();
        let ok = catch_unwind(AssertUnwindSafe(|| {
            let mut gate = ToolStreamGate::new(true, ToolCallFormat::Dsml);
            push_chunked_bytes(&mut gate, &text, chunk_size);
            gate.finish(false);
        }));
        assert!(ok.is_ok(), "panicked at chunk_size={chunk_size}");
    }
}

#[test]
fn strong_marker_start_is_char_boundary_for_unicode_preamble() {
    let text = "日本語 🔥 < | DSML | invoke name=\"x\"";
    if let Some(start) = first_strong_tool_marker_start(text) {
        assert!(
            text.is_char_boundary(start),
            "start={start} not char boundary"
        );
    }
}
