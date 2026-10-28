//! Tentative-hold stream gate: disambiguate prose vs tool syntax (TH-SG-*).

use agent_core::tool_call_format::ToolCallFormat;
use agent_core::tool_stream_gate::ToolStreamGate;

fn visible_after_push(gate: &mut ToolStreamGate, chunk: &str) -> String {
    gate.push(chunk).emitted_prefix
}

fn push_chunked(gate: &mut ToolStreamGate, raw: &str, chunk_size: usize) -> String {
    let mut emitted = String::new();
    for chunk in raw.as_bytes().chunks(chunk_size) {
        let piece = std::str::from_utf8(chunk).expect("utf8");
        emitted.push_str(&gate.push(piece).emitted_prefix);
    }
    emitted
}

// ---------------------------------------------------------------------------
// Prose disambiguation — flush early, do not wait for finish
// ---------------------------------------------------------------------------

#[test]
fn th_sg_01_call_colon_prose_flushes_on_disambiguation() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::CallBrace);
    let mut visible = String::new();
    visible.push_str(&visible_after_push(&mut gate, "Here is the answer. To "));
    assert_eq!(visible, "Here is the answer. To ", "TH-SG-01a preamble");

    visible.push_str(&visible_after_push(&mut gate, "call: "));
    assert_eq!(
        visible, "Here is the answer. To ",
        "TH-SG-01b holds at call:"
    );

    visible.push_str(&visible_after_push(&mut gate, "the API, do this next."));
    assert_eq!(
        visible, "Here is the answer. To call: the API, do this next.",
        "TH-SG-01c flushes once prose disambiguated"
    );

    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "TH-SG-01d no trailing at finish");
    assert!(!finish.has_tool_marker, "TH-SG-01e not a tool marker");
}

#[test]
fn th_sg_02_call_colon_prose_chunked_single_char() {
    let text = "To call: the API";
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::CallBrace);
    let mut visible = String::new();
    for ch in text.chars() {
        visible.push_str(&gate.push(&ch.to_string()).emitted_prefix);
    }
    assert_eq!(visible, text, "TH-SG-02 visible");
    let finish = gate.finish(false);
    assert!(!finish.has_tool_marker, "TH-SG-02 has_tool_marker");
    assert_eq!(finish.trailing_flush, "", "TH-SG-02 trailing_flush");
}

#[test]
fn th_sg_03_lt_comparison_flushes_as_prose() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);
    let visible = visible_after_push(&mut gate, "Use values where x < 100 and y > 50.");
    assert_eq!(
        visible, "Use values where x < 100 and y > 50.",
        "TH-SG-03 visible"
    );
    let finish = gate.finish(false);
    assert!(!finish.has_tool_marker, "TH-SG-03 has_tool_marker");
}

#[test]
fn th_sg_04_json_prose_still_streams_through() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::RawJson);
    let text = r#"Here is {"name":"demo"} ok"#;
    let visible = visible_after_push(&mut gate, text);
    assert_eq!(visible, text, "TH-SG-04 visible");
}

#[test]
fn th_sg_04b_non_tool_json_at_start_streams_after_first_key_disambiguates() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::RawJson);
    let text = r#"{"status":"ok","name":"demo"}"#;
    let visible = visible_after_push(&mut gate, text);
    assert_eq!(visible, text, "TH-SG-04b visible");
    let finish = gate.finish(false);
    assert!(!finish.has_tool_marker, "TH-SG-04b has_tool_marker");
}

// ---------------------------------------------------------------------------
// Broken / incomplete tool syntax — fail-closed at EOF (no leak)
// ---------------------------------------------------------------------------

#[test]
fn th_sg_05_broken_tool_call_hidden_at_eof() {
    let broken = r#"<tool_call>{"name":"append_blocks","arg"#;
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);
    let emitted = visible_after_push(&mut gate, broken);
    assert_eq!(emitted, "", "TH-SG-05 emitted during stream");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "TH-SG-05 trailing_flush");
    assert!(finish.has_tool_marker, "TH-SG-05 has_tool_marker");
    assert_eq!(
        gate.buffered_text(),
        broken,
        "TH-SG-05 raw buffer preserved"
    );
}

#[test]
fn th_sg_06_broken_partial_tool_prefix_hidden_at_eof() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);
    let emitted = visible_after_push(&mut gate, "Summary.<tool_call>{\"name\"");
    assert_eq!(emitted, "Summary.", "TH-SG-06 preamble only");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "TH-SG-06 trailing_flush");
    assert!(finish.has_tool_marker, "TH-SG-06 has_tool_marker");
}

#[test]
fn th_sg_07_call_ident_without_brace_stays_hidden_at_eof() {
    let text = "call: append_blocks";
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::CallBrace);
    let emitted = visible_after_push(&mut gate, text);
    assert_eq!(emitted, "", "TH-SG-07 emitted");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "TH-SG-07 trailing_flush");
    assert!(finish.has_tool_marker, "TH-SG-07 has_tool_marker");
}

#[test]
fn th_sg_08_chunked_dsml_leak_still_hidden() {
    let raw = r#"< | DSML | | tool_calls>< | DSML | | invoke name="web_search"></ | DSML | | tool_calls>"#;
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Dsml);
    let emitted = push_chunked(&mut gate, raw, 13);
    assert_eq!(emitted, "", "TH-SG-08 emitted");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "TH-SG-08 trailing_flush");
    assert!(finish.has_tool_marker, "TH-SG-08 has_tool_marker");
}

// ---------------------------------------------------------------------------
// Confirmed tool syntax — never emitted to chat
// ---------------------------------------------------------------------------

#[test]
fn th_sg_09_confirmed_tool_call_never_emitted() {
    let raw = r#"<tool_call>{"name":"web_search","arguments":{"query":"kotlin"}}</tool_call>"#;
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);
    let emitted = push_chunked(&mut gate, raw, 11);
    assert_eq!(emitted, "", "TH-SG-09 emitted");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "TH-SG-09 trailing_flush");
    assert!(finish.has_tool_marker, "TH-SG-09 has_tool_marker");
}

#[test]
fn th_sg_10_call_brace_syntax_confirmed_and_hidden() {
    let raw = r#"call:append_blocks{artifactId:"art-1",blocks:[]}"#;
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::CallBrace);
    let emitted = push_chunked(&mut gate, raw, 7);
    assert_eq!(emitted, "", "TH-SG-10 emitted");
    let finish = gate.finish(false);
    assert!(finish.has_tool_marker, "TH-SG-10 has_tool_marker");
}

#[test]
fn th_sg_10b_bare_json_tool_call_is_hidden_when_chunked() {
    let raw = r#"{"name":"web_search","arguments":{"query":"kotlin"}}"#;
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::RawJson);
    let emitted = push_chunked(&mut gate, raw, 1);
    assert_eq!(emitted, "", "TH-SG-10b emitted");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "TH-SG-10b trailing_flush");
    assert!(finish.has_tool_marker, "TH-SG-10b has_tool_marker");
}

#[test]
fn th_sg_11_native_tools_flush_preamble_before_partial_marker() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);
    let emitted = visible_after_push(&mut gate, "Let me try with a simpler <tool");
    assert_eq!(emitted, "Let me try with a simpler ", "TH-SG-11 emitted");
    let finish = gate.finish(true);
    let visible = format!("{}{}", emitted, finish.trailing_flush);
    assert_eq!(visible, "Let me try with a simpler ", "TH-SG-11 visible");
}

#[test]
fn th_sg_12_native_tools_flush_preamble_before_confirmed_tool_call() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);
    let emitted = visible_after_push(&mut gate, r#"Summary.<tool_call>{"name":"x"}"#);
    assert_eq!(emitted, "Summary.", "TH-SG-12 emitted");
    let finish = gate.finish(true);
    assert_eq!(finish.trailing_flush, "", "TH-SG-12 trailing_flush");
    assert!(finish.has_tool_marker, "TH-SG-12 has_tool_marker");
}

// ---------------------------------------------------------------------------
// Buffer integrity + allow_tool_call policy
// ---------------------------------------------------------------------------

#[test]
fn th_sg_13_buffered_text_always_retains_raw_stream() {
    let raw = r#"<tool_call>{"name":"web_search"}</tool_call>"#;
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::XmlToolCall);
    gate.push(raw);
    assert_eq!(gate.buffered_text(), raw, "TH-SG-13 buffered_text");
}

#[test]
fn th_sg_14_allow_tool_call_false_still_hides_confirmed_leak() {
    let mut gate = ToolStreamGate::new(false, ToolCallFormat::XmlToolCall);
    let emitted = visible_after_push(&mut gate, r#"<tool_call>{"name":"web_search"}"#);
    assert_eq!(emitted, "", "TH-SG-14 emitted");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "TH-SG-14 trailing_flush");
    assert!(finish.has_tool_marker, "TH-SG-14 has_tool_marker");
}

#[test]
fn th_sg_15_plain_text_flushes_during_stream() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::Structured);
    let emitted = visible_after_push(&mut gate, "All done.");
    assert_eq!(emitted, "All done.", "TH-SG-15 emitted during stream");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "TH-SG-15 trailing_flush");
    assert!(!finish.has_tool_marker, "TH-SG-15 has_tool_marker");
}

#[test]
fn th_sg_16_bracket_array_holdback() {
    let mut gate = ToolStreamGate::new(true, ToolCallFormat::BracketArray);
    let emitted = visible_after_push(&mut gate, "See [TOOL_CALLS][");
    assert_eq!(emitted, "See ", "TH-SG-16 emitted");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "TH-SG-16 trailing_flush");
    assert!(finish.has_tool_marker, "TH-SG-16 has_tool_marker");
}
