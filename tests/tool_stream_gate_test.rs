//! TOOL-PARSE-002 stream gate scenarios SG-01 … SG-08.
//! centralized-leaked-tool-call-recovery stream gate scenarios CL-SG-*.

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
    let mut gate = ToolStreamGate::new(true);

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
    assert!(finish.has_tool_marker, "FAAL-SG-01 has_tool_marker");
}

#[test]
fn faal_sg_02_native_tools_flush_preamble_before_tool_call_tail() {
    let mut gate = ToolStreamGate::new(true);

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
    let mut gate = ToolStreamGate::new(true);

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
    let mut gate = ToolStreamGate::new(true);

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
    assert_eq!(
        finish.trailing_flush, "Let me search.",
        "SG-03 trailing_flush"
    );
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
fn sg_05_allow_tool_call_false_still_holds_back_tool_markers() {
    let mut gate = ToolStreamGate::new(false);
    let push = gate.push(r#"<tool_call>{"name":"web_search"}"#);
    assert_eq!(push.emitted_prefix, "", "SG-05 emitted_prefix");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "SG-05 trailing_flush");
    assert!(finish.has_tool_marker, "SG-05 has_tool_marker");
}

#[test]
fn sg_05b_allow_tool_call_false_flushes_plain_prose() {
    let mut gate = ToolStreamGate::new(false);
    gate.push("All done.");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "All done.", "SG-05b trailing_flush");
    assert!(!finish.has_tool_marker, "SG-05b has_tool_marker");
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

#[test]
fn sg_09_litert_native_call_never_emitted() {
    let mut gate = ToolStreamGate::new(true);
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
    let mut gate = ToolStreamGate::new(true);
    let emitted = push_chunked(&mut gate, CL_SG_01_RAW, 16);
    let finish = gate.finish(false);
    assert_eq!(emitted, "", "CL-SG-01 emitted prefix");
    assert_eq!(finish.trailing_flush, "", "CL-SG-01 trailing_flush");
    assert!(finish.has_tool_marker, "CL-SG-01 has_tool_marker");
}

#[test]
fn cl_sg_02_partial_angle_holdback() {
    let mut gate = ToolStreamGate::new(true);
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
    let mut gate = ToolStreamGate::new(true);
    gate.push("Let me search.");
    let finish = gate.finish(true);
    assert_eq!(
        finish.trailing_flush, "Let me search.",
        "CL-SG-03 trailing_flush"
    );
    assert!(!finish.has_tool_marker, "CL-SG-03 has_tool_marker");
}

#[test]
fn cl_sg_04_json_prose_streams_through() {
    let mut gate = ToolStreamGate::new(true);
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
    let mut gate = ToolStreamGate::new(false);
    let push = gate.push(r#"<tool_call>{"name":"web_search"}"#);
    assert_eq!(push.emitted_prefix, "", "CL-SG-05 emitted_prefix");
    let finish = gate.finish(false);
    assert_eq!(finish.trailing_flush, "", "CL-SG-05 trailing_flush");
    assert!(finish.has_tool_marker, "CL-SG-05 has_tool_marker");
}

#[test]
fn cl_sg_06_plain_text_finish_flush() {
    let mut gate = ToolStreamGate::new(true);
    gate.push("All done.");
    let finish = gate.finish(false);
    assert_eq!(
        finish.trailing_flush, "All done.",
        "CL-SG-06 trailing_flush"
    );
    assert!(!finish.has_tool_marker, "CL-SG-06 has_tool_marker");
}

#[test]
fn cl_sg_07_buffered_text_retains_raw_leak() {
    let mut gate = ToolStreamGate::new(true);
    let raw = CL_SG_01_RAW;
    gate.push(raw);
    assert_eq!(gate.buffered_text(), raw, "CL-SG-07 buffered_text");
}

#[test]
fn cl_sg_08_tool_calls_holdback() {
    let mut gate = ToolStreamGate::new(true);
    let push = gate.push("See [TOOL_CALLS][");
    assert_eq!(push.emitted_prefix, "See ", "CL-SG-08 emitted_prefix");
    let finish = gate.finish(false);
    assert!(finish.has_tool_marker, "CL-SG-08 has_tool_marker");
}

#[test]
fn cl_sg_09_call_append_blocks_held_back() {
    let mut gate = ToolStreamGate::new(true);
    let raw = r#"call:append_blocks{artifactId:"art-1",blocks:[{"type":"paragraph","text":"x"}]}"#;
    let emitted = push_chunked(&mut gate, raw, 11);
    let finish = gate.finish(false);
    assert_eq!(emitted, "", "CL-SG-09 emitted prefix");
    assert!(finish.has_tool_marker, "CL-SG-09 has_tool_marker");
}
