//! TOOL-PARSE-002 stream gate scenarios SG-01 … SG-08.
//! centralized-leaked-tool-call-recovery stream gate scenarios CL-SG-*.

use agent_core::tool_call_format::ToolCallFormat;
use agent_core::tool_stream_gate::ToolStreamGate;

const DSML_SAMPLE: &str = r#"<|DSML|tool_calls><|DSML|invoke name="web_search">
<|DSML|parameter name="query" string="true">kotlin</|DSML|parameter>
</|DSML|invoke></|DSML|tool_calls>"#;

fn push_chunked(gate: &mut ToolStreamGate, raw: &str, chunk_size: usize) -> String {
    let mut emitted = String::new();
    for chunk in raw.as_bytes().chunks(chunk_size) {
        let piece = std::str::from_utf8(chunk).expect("utf8");
        emitted.push_str(&gate.push(piece).emitted_prefix);
    }
    emitted
}

#[test]
fn faal_sg_01_native_tools_flush_preamble_before_partial_tool_marker() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);

    let emitted = gate.push("Let me try with a simpler <tool").emitted_prefix;
    let finish = gate.finish(true);
    let visible = format!("{}{}", emitted, finish.trailing_flush);

    assert_eq!(
        emitted, "Let me try with a simpler ",
        "FAAL-SG-01 emitted_prefix"
    );
    assert_eq!(
        visible, "Let me try with a simpler ",
        "FAAL-SG-01 visible text"
    );
    assert!(
        finish.has_tool_marker,
        "FAAL-SG-01 unresolved partial <tool prefix is fail-closed at EOF"
    );
}

#[test]
fn faal_sg_02_native_tools_flush_preamble_before_tool_call_tail() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);

    let emitted = gate
        .push(r#"Summary.<tool_call>{"name":"append_blocks"}"#)
        .emitted_prefix;
    let finish = gate.finish(true);

    assert_eq!(emitted, "Summary.", "FAAL-SG-02 emitted_prefix");
    assert_eq!(finish.trailing_flush, "", "FAAL-SG-02 trailing_flush");
    assert!(finish.has_tool_marker, "FAAL-SG-02 has_tool_marker");
}

#[test]
fn faal_sg_05_no_native_tools_hold_back_tool_call_body() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);

    let emitted = gate
        .push(r#"<tool_call>{"name":"append_blocks","arguments":{"artifactId":"art","blocks":[]}}</tool_call>"#)
        .emitted_prefix;
    let finish = gate.finish(false);

    assert_eq!(emitted, "", "FAAL-SG-05 emitted_prefix");
    assert_eq!(finish.trailing_flush, "", "FAAL-SG-05 trailing_flush");
    assert!(finish.has_tool_marker, "FAAL-SG-05 has_tool_marker");
}

#[test]
fn faal_md_02_native_finish_flushes_preamble() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);

    let emitted = gate.push("Let me try with a simpler <tool").emitted_prefix;
    let finish = gate.finish(true);
    let visible = format!("{}{}", emitted, finish.trailing_flush);

    assert_eq!(
        visible, "Let me try with a simpler ",
        "FAAL-MD-02 visible text"
    );
}

#[test]
fn sg_01_chunked_dsml_never_emitted() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Dsml);
    let emitted = push_chunked(&mut gate, DSML_SAMPLE, 17);
    let finish = gate.finish(false);
    assert_eq!(emitted, "", "SG-01 emitted prefix");
    assert_eq!(finish.trailing_flush, "", "SG-01 trailing_flush");
    assert!(finish.has_tool_marker, "SG-01 has_tool_marker");
}

#[test]
fn sg_02_partial_angle_bracket_holdback() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);
    let push = gate.push("Hello <tool");
    assert_eq!(push.emitted_prefix, "Hello ", "SG-02 emitted_prefix");
    assert_eq!(gate.buffered_text(), "Hello <tool", "SG-02 buffered_text");
}

#[test]
fn sg_03_native_tools_flush_preamble() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Structured);
    let emitted = gate.push("Let me search.").emitted_prefix;
    assert_eq!(emitted, "Let me search.", "SG-03 emitted during stream");
    let finish = gate.finish(true);
    assert_eq!(finish.trailing_flush, "", "SG-03 trailing_flush");
    assert!(!finish.has_tool_marker, "SG-03 has_tool_marker");
}

#[test]
fn sg_10_native_tools_flush_long_chunked_preamble_before_tool_calls() {
    let preamble =
        "I'll immediately proceed to append_blocks with artifact hbWxQYaTV7WyJ39rLRiq9to9LCu2 \
         and populate the document with the requested sections.";
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Structured);
    let mut emitted = String::new();
    for chunk in preamble.as_bytes().chunks(17) {
        let piece = std::str::from_utf8(chunk).expect("utf8");
        emitted.push_str(&gate.push(piece).emitted_prefix);
    }
    let finish = gate.finish(true);
    let visible = format!("{}{}", emitted, finish.trailing_flush);
    assert_eq!(visible, preamble, "SG-10 visible text");
    assert!(!finish.has_tool_marker, "SG-10 has_tool_marker");
}

#[test]
fn sg_04_native_tools_do_not_flush_dsml_tail() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Dsml);
    gate.push("Summary.<|DSML|tool_calls></|DSML|tool_calls>");
    let finish = gate.finish(true);
    assert_eq!(finish.trailing_flush, "", "SG-04 trailing_flush");
    assert!(finish.has_tool_marker, "SG-04 has_tool_marker");
}

#[test]
fn sg_05_allow_tool_call_false_still_holds_back_tool_markers() {
    let mut gate = ToolStreamGate::new(false, ToolCallFormat::XmlToolCall);
    let push = gate.push(r#"<tool_call>{"name":"web_search"}"#);
    assert_eq!(push.emitted_prefix, "", "SG-05 emitted_prefix");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "SG-05 trailing_flush");
    assert!(finish.has_tool_marker, "SG-05 has_tool_marker");
}

#[test]
fn sg_05b_allow_tool_call_false_flushes_plain_prose() {
    let mut gate = ToolStreamGate::new(false, ToolCallFormat::Structured);
    let emitted = gate.push("All done.").emitted_prefix;
    assert_eq!(emitted, "All done.", "SG-05b emitted during stream");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "SG-05b trailing_flush");
    assert!(!finish.has_tool_marker, "SG-05b has_tool_marker");
}

#[test]
fn sg_06_plain_text_finish_flush() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Structured);
    let emitted = gate.push("All done.").emitted_prefix;
    assert_eq!(emitted, "All done.", "SG-06 emitted during stream");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "SG-06 trailing_flush");
    assert!(!finish.has_tool_marker, "SG-06 has_tool_marker");
}

#[test]
fn sg_07_buffered_text_is_raw() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Dsml);
    let raw = "<|DSML|tool_calls></|DSML|tool_calls>";
    gate.push(raw);
    assert_eq!(gate.buffered_text(), raw, "SG-07 buffered_text");
}

#[test]
fn sg_08_bracket_array_holdback() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::BracketArray);
    let push = gate.push("See [TOOL_CALLS][");
    assert_eq!(push.emitted_prefix, "See ", "SG-08 emitted_prefix");
    let finish = gate.finish(false);
    assert!(finish.has_tool_marker, "SG-08 has_tool_marker");
}

#[test]
fn sg_09b_bracket_array_gate_streams_xml_tool_call_text_unheld() {
    // ENC-300 m6: a non-Structured gate never holds another shape's text —
    // `<tool_call>` streams straight through a BracketArray gate.
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::BracketArray);
    let raw = r#"<tool_call>{"name":"web_search"}</tool_call>"#;
    let emitted = push_chunked(&mut gate, raw, 7);
    assert_eq!(emitted, raw, "SG-09b wrong-format text streams unheld");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "SG-09b trailing_flush");
    assert!(
        !finish.has_tool_marker,
        "SG-09b no BracketArray marker at finish"
    );
}

#[test]
fn sg_09_call_brace_native_call_never_emitted() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::CallBrace);
    let emitted = push_chunked(
        &mut gate,
        r#"call:append_blocks{artifactId: <|"|>artifact-1<|"|>, blocks: <|"|>[{"type":"paragraph","text":"large"}]<|"|>}"#,
        11,
    );
    let finish = gate.finish(false);
    assert_eq!(emitted, "", "SG-09 emitted prefix");
    assert_eq!(finish.trailing_flush, "", "SG-09 trailing_flush");
    assert!(finish.has_tool_marker, "SG-09 has_tool_marker");
}

// ---------------------------------------------------------------------------
// centralized-leaked-tool-call-recovery — CL-SG-*
// ---------------------------------------------------------------------------

const CL_SG_01_RAW: &str = r#"<tool_call>{"name":"append_blocks","arguments":{"artifactId":"art-1","blocks":[],"isComplete":false}}</tool_call>"#;

#[test]
fn cl_sg_01_chunked_tool_call_never_emitted() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);
    let emitted = push_chunked(&mut gate, CL_SG_01_RAW, 16);
    let finish = gate.finish(false);
    assert_eq!(emitted, "", "CL-SG-01 emitted prefix");
    assert_eq!(finish.trailing_flush, "", "CL-SG-01 trailing_flush");
    assert!(finish.has_tool_marker, "CL-SG-01 has_tool_marker");
}

#[test]
fn cl_sg_02_partial_angle_holdback() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);
    let push = gate.push("Hello <tool");
    assert_eq!(push.emitted_prefix, "Hello ", "CL-SG-02 emitted_prefix");
    assert_eq!(
        gate.buffered_text(),
        "Hello <tool",
        "CL-SG-02 buffered_text"
    );
}

#[test]
fn cl_sg_03_native_tools_flush_preamble() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Structured);
    let emitted = gate.push("Let me search.").emitted_prefix;
    assert_eq!(emitted, "Let me search.", "CL-SG-03 emitted during stream");
    let finish = gate.finish(true);
    assert_eq!(finish.trailing_flush, "", "CL-SG-03 trailing_flush");
    assert!(!finish.has_tool_marker, "CL-SG-03 has_tool_marker");
}

#[test]
fn cl_sg_04_json_prose_streams_through() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::RawJson);
    let push = gate.push(r#"Here is {"name":"demo"} ok"#);
    assert_eq!(
        push.emitted_prefix, r#"Here is {"name":"demo"} ok"#,
        "CL-SG-04 emitted_prefix"
    );
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "CL-SG-04 trailing_flush");
    assert!(!finish.has_tool_marker, "CL-SG-04 has_tool_marker");
}

#[test]
fn cl_sg_05_allow_false_still_holds_back_tool_markers() {
    let mut gate = ToolStreamGate::new(false, ToolCallFormat::XmlToolCall);
    let push = gate.push(r#"<tool_call>{"name":"web_search"}"#);
    assert_eq!(push.emitted_prefix, "", "CL-SG-05 emitted_prefix");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "CL-SG-05 trailing_flush");
    assert!(finish.has_tool_marker, "CL-SG-05 has_tool_marker");
}

#[test]
fn cl_sg_06_plain_text_finish_flush() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Structured);
    let emitted = gate.push("All done.").emitted_prefix;
    assert_eq!(emitted, "All done.", "CL-SG-06 emitted during stream");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "CL-SG-06 trailing_flush");
    assert!(!finish.has_tool_marker, "CL-SG-06 has_tool_marker");
}

#[test]
fn cl_sg_07_buffered_text_retains_raw_leak() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);
    let raw = CL_SG_01_RAW;
    gate.push(raw);
    assert_eq!(gate.buffered_text(), raw, "CL-SG-07 buffered_text");
}

#[test]
fn cl_sg_08_tool_calls_holdback() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::BracketArray);
    let push = gate.push("See [TOOL_CALLS][");
    assert_eq!(push.emitted_prefix, "See ", "CL-SG-08 emitted_prefix");
    let finish = gate.finish(false);
    assert!(finish.has_tool_marker, "CL-SG-08 has_tool_marker");
}

#[test]
fn cl_sg_09_call_append_blocks_held_back() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::CallBrace);
    let raw = r#"call:append_blocks{artifactId:"art-1",blocks:[{"type":"paragraph","text":"x"}]}"#;
    let emitted = push_chunked(&mut gate, raw, 11);
    let finish = gate.finish(false);
    assert_eq!(emitted, "", "CL-SG-09 emitted prefix");
    assert!(finish.has_tool_marker, "CL-SG-09 has_tool_marker");
}

/// ENC-300 D5: Structured is pure pass-through — even full DSML syntax
/// streams unheld, finishes with no marker, and flushes everything.
#[test]
fn structured_gate_passes_dsml_through_unheld() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Structured);
    let emitted = push_chunked(&mut gate, DSML_SAMPLE, 17);
    assert_eq!(
        emitted, DSML_SAMPLE,
        "Structured emits everything during stream"
    );
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "Structured trailing_flush");
    assert!(
        !finish.has_tool_marker,
        "Structured never reports a tool marker"
    );
}

/// ENC-300 D5: a Dsml gate holds its own markers but streams another
/// shape's text through unheld.
#[test]
fn dsml_gate_streams_bracket_array_unheld() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Dsml);
    let raw = r#"See [TOOL_CALLS] [{"name":"web_search"}] ok"#;
    let emitted = push_chunked(&mut gate, raw, 7);
    assert_eq!(emitted, raw, "other-format text streams through unheld");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "trailing_flush");
    assert!(
        !finish.has_tool_marker,
        "other-format text is no marker for a Dsml gate"
    );
}

/// ENC-300 D5: an XmlToolCall gate streams DSML text through unheld —
/// `<` alone is not a hold without the declared `<tool_call` prefix.
#[test]
fn xml_gate_streams_dsml_unheld() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);
    let emitted = push_chunked(&mut gate, DSML_SAMPLE, 17);
    assert_eq!(
        emitted, DSML_SAMPLE,
        "DSML streams through an XmlToolCall gate"
    );
    let finish = gate.finish(false);
    assert!(
        !finish.has_tool_marker,
        "DSML is no marker for an XmlToolCall gate"
    );
}

/// ENC-300 D5: a CallBrace gate streams `<tool_call>` text through unheld.
#[test]
fn callbrace_gate_streams_tool_call_unheld() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::CallBrace);
    let raw = r#"<tool_call>{"name":"web_search","arguments":{"query":"x"}}</tool_call>"#;
    let emitted = push_chunked(&mut gate, raw, 9);
    assert_eq!(
        emitted, raw,
        "XmlToolCall text streams through a CallBrace gate"
    );
    let finish = gate.finish(false);
    assert!(
        !finish.has_tool_marker,
        "<tool_call> is no marker for a CallBrace gate"
    );
}

#[test]
fn multibyte_char_at_holdback_tail_boundary_does_not_panic() {
    // Regression: the tail search window starts at len - TOOL_MARKER_HOLDBACK_CHARS,
    // raw byte arithmetic that can land inside a multi-byte char (e.g. an en dash).
    // This used to panic in earliest_tentative_trigger_for's slice.
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Structured);
    // 100 ASCII bytes, then a 3-byte en dash at bytes 100..103, then 62 ASCII bytes:
    // total 165 bytes, so the 64-byte tail window starts at byte 101 — inside the dash.
    let text = format!("{}–{}", "a".repeat(100), "b".repeat(62));

    let emitted = gate.push(&text).emitted_prefix;
    let finish = gate.finish(false);

    assert_eq!(format!("{}{}", emitted, finish.trailing_flush), text);
    assert!(!finish.has_tool_marker);
}

#[test]
fn multibyte_chars_streamed_in_small_chunks_do_not_panic() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Structured);
    let text = format!(
        "Sean — quick situational read on this: {} – and more — text {}",
        "x".repeat(80),
        "y".repeat(80)
    );
    let mut emitted = String::new();
    let chars: Vec<char> = text.chars().collect();
    for chunk in chars.chunks(7) {
        let piece: String = chunk.iter().collect();
        emitted.push_str(&gate.push(&piece).emitted_prefix);
    }
    let finish = gate.finish(false);
    assert_eq!(format!("{}{}", emitted, finish.trailing_flush), text);
}
