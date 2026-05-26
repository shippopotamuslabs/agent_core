//! Regression for user-reported DSML leak (Qwen 3.7 Max / OpenRouter).

#[path = "support/mod.rs"]
mod support;

use agent_core::engine::resolve_model_turn_text;
use support::enclave_test_semantics::{enclave_test_catalog, enclave_test_semantics};
use support::enclave_test_semantics::parse_enclave_tool_calls;
use agent_core::tool_stream_gate::{
    first_strong_tool_marker_start, normalize_spaced_dsml, ToolStreamGate,
};

const LEAKED_DOUBLE_PIPE: &str = r#"< | DSML | | tool_calls>< | DSML | | invoke name="web_search">< | DSML | | parameter name="query" string="true">tom's guide qwen 3.7 max preview review what gets right wrong 2026</ | DSML | | parameter></ | DSML | | invoke>< | DSML | | invoke name="web_search">< | DSML | | parameter name="query" string="true">qwen3.7 max vs claude opus verbosity coding assistant token output comparison analysis</ | DSML | | parameter></ | DSML | | invoke></ | DSML | | tool_calls>"#;

#[test]
fn double_pipe_dsml_is_normalized_recovered_and_hidden_from_stream() {
    assert_eq!(
        normalize_spaced_dsml(LEAKED_DOUBLE_PIPE),
        concat!(
            "<|DSML|tool_calls><|DSML|invoke name=\"web_search\">",
            "<|DSML|parameter name=\"query\" string=\"true\">tom's guide qwen 3.7 max preview review what gets right wrong 2026</|DSML|parameter>",
            "</|DSML|invoke><|DSML|invoke name=\"web_search\">",
            "<|DSML|parameter name=\"query\" string=\"true\">qwen3.7 max vs claude opus verbosity coding assistant token output comparison analysis</|DSML|parameter>",
            "</|DSML|invoke></|DSML|tool_calls>"
        )
    );
    assert!(first_strong_tool_marker_start(LEAKED_DOUBLE_PIPE).is_some());

    let mut gate = ToolStreamGate::new(true);
    let emitted = gate.push(LEAKED_DOUBLE_PIPE).emitted_prefix;
    let finish = gate.finish(false);
    assert_eq!(emitted, "");
    assert_eq!(finish.trailing_flush, "");
    assert!(finish.has_tool_marker);

    let batch = parse_enclave_tool_calls(&normalize_spaced_dsml(LEAKED_DOUBLE_PIPE));
    assert_eq!(batch.successes.len(), 2);

    let catalog = enclave_test_catalog();
    let semantics = enclave_test_semantics();
    let resolution = resolve_model_turn_text(LEAKED_DOUBLE_PIPE, &[], true, &catalog, &semantics);
    assert_eq!(resolution.recovered_tool_calls.len(), 2);
    assert_eq!(resolution.visible_assistant_text, "");
}

#[test]
fn allow_false_still_hides_dsml_from_stream() {
    let mut gate = ToolStreamGate::new(false);
    let emitted = gate.push(LEAKED_DOUBLE_PIPE).emitted_prefix;
    let finish = gate.finish(false);
    assert_eq!(emitted, "");
    assert_eq!(finish.trailing_flush, "");
    assert!(finish.has_tool_marker);
    assert_eq!(gate.buffered_text(), LEAKED_DOUBLE_PIPE);
}
