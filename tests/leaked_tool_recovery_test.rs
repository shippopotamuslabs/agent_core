//! centralized-leaked-tool-call-recovery — turn resolver scenarios CL-TR-*.

#[path = "support/mod.rs"]
mod support;

use agent_core::engine::resolve_model_turn_text;
use agent_core::turn_io::RawToolCall;
use support::enclave_test_semantics::{enclave_test_catalog, enclave_test_semantics};

fn resolve_enclave(
    buffered: &str,
    native: &[RawToolCall],
    allow_tool_call: bool,
) -> agent_core::engine::ModelTurnTextResolution {
    let catalog = enclave_test_catalog();
    let semantics = enclave_test_semantics();
    resolve_model_turn_text(buffered, native, allow_tool_call, &catalog, &semantics)
}

const APPEND_BLOCKS_TOOL_CALL: &str = r#"<tool_call>{"name":"append_blocks","arguments":{"artifactId":"art-99","blocks":[],"isComplete":false}}</tool_call>"#;

const SPACED_DSML_WEB_SEARCH: &str = r#"< | DSML | tool_calls>< | DSML | invoke name="web_search">
< | DSML | parameter name="query" string="true">kotlin</ | DSML | parameter>
</ | DSML | invoke></ | DSML | tool_calls>"#;

#[test]
fn cl_tr_01_recover_append_blocks_without_native_tools() {
    let resolution = resolve_enclave(APPEND_BLOCKS_TOOL_CALL, &[], true);
    assert_eq!(
        resolution.recovered_tool_calls.len(),
        1,
        "CL-TR-01 recovered count"
    );
    assert_eq!(
        resolution.recovered_tool_calls[0].name, "append_blocks",
        "CL-TR-01 tool name"
    );
    assert!(
        resolution.recovered_tool_calls[0]
            .arguments_json
            .contains(r#""artifactId":"art-99""#),
        "CL-TR-01 args {}",
        resolution.recovered_tool_calls[0].arguments_json
    );
    assert_eq!(
        resolution.visible_assistant_text, "",
        "CL-TR-01 visible text"
    );
}

#[test]
fn cl_tr_02_native_preferred_over_leaked_dsml() {
    let buffered = format!(
        "preamble.<|DSML|tool_calls><|DSML|invoke name=\"append_blocks\"></|DSML|invoke></|DSML|tool_calls>"
    );
    let native = vec![RawToolCall {
        id: Some("call_native".into()),
        name: "web_search".into(),
        arguments_json: r#"{"query":"kotlin"}"#.into(),
    }];
    let resolution = resolve_enclave(&buffered, &native, true);
    assert!(
        resolution.recovered_tool_calls.is_empty(),
        "CL-TR-02 recovered must be empty"
    );
    assert_eq!(
        resolution.native_tool_calls.len(),
        1,
        "CL-TR-02 native count"
    );
    assert_eq!(
        resolution.native_tool_calls[0].name, "web_search",
        "CL-TR-02 native name"
    );
}

#[test]
fn cl_tr_03_spaced_dsml_recovers_web_search() {
    let resolution = resolve_enclave(SPACED_DSML_WEB_SEARCH, &[], true);
    assert_eq!(
        resolution.recovered_tool_calls.len(),
        1,
        "CL-TR-03 recovered count"
    );
    assert_eq!(
        resolution.recovered_tool_calls[0].name, "web_search",
        "CL-TR-03 tool name"
    );
    assert!(
        resolution.recovered_tool_calls[0]
            .arguments_json
            .contains(r#""query":"kotlin""#),
        "CL-TR-03 args {}",
        resolution.recovered_tool_calls[0].arguments_json
    );
}

#[test]
fn cl_tr_04_prose_json_flushes_verbatim() {
    let buffered = r#"Configure {"name":"value","enabled":true} carefully."#;
    let resolution = resolve_enclave(buffered, &[], true);
    assert!(
        resolution.recovered_tool_calls.is_empty(),
        "CL-TR-04 recovered must be empty"
    );
    assert_eq!(
        resolution.visible_assistant_text, buffered,
        "CL-TR-04 visible text"
    );
}

#[test]
fn cl_tr_05_allow_false_still_detects_and_strips_tool_markers() {
    let buffered = r#"<tool_call>{"name":"web_search"}</tool_call>"#;
    let resolution = resolve_enclave(buffered, &[], false);
    assert_eq!(
        resolution.visible_assistant_text, "",
        "CL-TR-05 visible text"
    );
    assert!(resolution.had_strong_tool_marker, "CL-TR-05 had marker");
    assert!(
        resolution.recovered_tool_calls.is_empty(),
        "CL-TR-05 recovered must be empty"
    );
}

#[test]
fn cl_tr_06_normalize_spaced_dsml() {
    use agent_core::tool_stream_gate::normalize_spaced_dsml;
    assert_eq!(
        normalize_spaced_dsml("< | DSML | tool_calls>"),
        "<|DSML|tool_calls>",
        "CL-TR-06 single-pipe normalized"
    );
    assert_eq!(
        normalize_spaced_dsml("< | DSML | | tool_calls>"),
        "<|DSML|tool_calls>",
        "CL-TR-06 double-pipe normalized"
    );
}

#[test]
fn cl_tr_07_call_colon_in_prose_does_not_truncate_visible_text() {
    let buffered = "Here is the answer. To call: the API, do this next.";
    let resolution = resolve_enclave(buffered, &[], true);
    assert_eq!(
        resolution.visible_assistant_text, buffered,
        "CL-TR-07 visible text"
    );
    assert!(
        !resolution.had_strong_tool_marker,
        "CL-TR-07 must not treat prose call: as a tool marker"
    );
}

/// CL-TR-08: when an earlier spaced-DSML segment shifts byte offsets between
/// `normalize_spaced_dsml(text)` and the raw text, the no-successes fallback
/// must map the marker offset back to the raw text instead of slicing raw text
/// with a normalized-space offset (previously a wrong slice or UTF-8 panic).
#[test]
fn cl_tr_08_spaced_dsml_prefix_offset_does_not_panic_on_multibyte_boundary() {
    // "</ |DSML| " collapses to "</|DSML|" (2 bytes shorter), so the strong
    // marker offset in normalized text lands inside the 4-byte rocket emoji
    // when applied to the raw text.
    let buffered = "A</ |DSML| x>\u{1F680}<|DSML|tool_calls junk";
    let resolution = resolve_enclave(buffered, &[], true);
    assert!(
        resolution.had_strong_tool_marker,
        "CL-TR-08 must detect the strong marker"
    );
    assert!(
        resolution.recovered_tool_calls.is_empty(),
        "CL-TR-08 malformed envelope must not recover calls"
    );
    assert_eq!(
        resolution.visible_assistant_text, "A</ |DSML| x>\u{1F680}",
        "CL-TR-08 visible text must cut exactly at the marker in raw text"
    );
}

#[test]
fn cl_md_03_weak_json_regex_absent_from_stream_gate() {
    use agent_core::tool_stream_gate::first_strong_tool_marker_start;
    use std::fs;

    let markers_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/tool_stream_gate/markers.rs"
    );
    let content = fs::read_to_string(markers_path).expect("CL-MD-03 read markers.rs");
    assert!(
        !content.contains(r#"\{\s*["']name["']\s*:"#),
        "CL-MD-03 weak JSON regex must not be in stream gate markers"
    );
    assert_eq!(
        first_strong_tool_marker_start(r#"Tips: {"name":"x"}"#),
        None,
        "CL-MD-03 weak JSON prose must not match strong marker"
    );
}
