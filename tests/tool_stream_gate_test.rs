//! TOOL-PARSE-002 stream gate scenarios SG-01 … SG-08.

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
fn sg_01_chunked_dsml_never_emitted() {
    let mut gate = ToolStreamGate::new(true);
    let emitted = push_chunked(&mut gate, DSML_SAMPLE, 17);
    let finish = gate.finish(false);
    assert_eq!(emitted, "", "SG-01 emitted prefix");
    assert_eq!(finish.trailing_flush, "", "SG-01 trailing_flush");
    assert!(finish.has_tool_marker, "SG-01 has_tool_marker");
}

#[test]
fn sg_02_partial_angle_bracket_holdback() {
    let mut gate = ToolStreamGate::new(true);
    let push = gate.push("Hello <tool");
    assert_eq!(push.emitted_prefix, "Hello ", "SG-02 emitted_prefix");
    assert_eq!(gate.buffered_text(), "Hello <tool", "SG-02 buffered_text");
}

#[test]
fn sg_03_native_tools_flush_preamble() {
    let mut gate = ToolStreamGate::new(true);
    gate.push("Let me search.");
    let finish = gate.finish(true);
    assert_eq!(finish.trailing_flush, "Let me search.", "SG-03 trailing_flush");
    assert!(!finish.has_tool_marker, "SG-03 has_tool_marker");
}

#[test]
fn sg_04_native_tools_do_not_flush_dsml_tail() {
    let mut gate = ToolStreamGate::new(true);
    gate.push("Summary.<|DSML|tool_calls></|DSML|tool_calls>");
    let finish = gate.finish(true);
    assert_eq!(finish.trailing_flush, "", "SG-04 trailing_flush");
    assert!(finish.has_tool_marker, "SG-04 has_tool_marker");
}

#[test]
fn sg_05_allow_tool_call_false() {
    let mut gate = ToolStreamGate::new(false);
    let push = gate.push(r#"<tool_call>{"name":"web_search"}"#);
    assert_eq!(
        push.emitted_prefix,
        r#"<tool_call>{"name":"web_search"}"#,
        "SG-05 emitted_prefix"
    );
}

#[test]
fn sg_06_plain_text_finish_flush() {
    let mut gate = ToolStreamGate::new(true);
    gate.push("All done.");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "All done.", "SG-06 trailing_flush");
    assert!(!finish.has_tool_marker, "SG-06 has_tool_marker");
}

#[test]
fn sg_07_buffered_text_is_raw() {
    let mut gate = ToolStreamGate::new(true);
    let raw = "<|DSML|tool_calls></|DSML|tool_calls>";
    gate.push(raw);
    assert_eq!(gate.buffered_text(), raw, "SG-07 buffered_text");
}

#[test]
fn sg_08_mistral_tool_calls_holdback() {
    let mut gate = ToolStreamGate::new(true);
    let push = gate.push("See [TOOL_CALLS][");
    assert_eq!(push.emitted_prefix, "See ", "SG-08 emitted_prefix");
    let finish = gate.finish(false);
    assert!(finish.has_tool_marker, "SG-08 has_tool_marker");
}
