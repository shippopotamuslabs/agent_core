#[path = "support/mod.rs"]
mod support;

use agent_core::engine::{ingest_model_response_envelopes, EnvelopeIngestOutcome};
use agent_core::tool_call_format::ToolCallFormat;
use support::enclave_test_semantics::{enclave_test_catalog, enclave_test_semantics};

#[test]
fn strong_unparseable_marker_returns_parse_failed_with_truncated_visible_text() {
    let catalog = enclave_test_catalog();
    let semantics = enclave_test_semantics();
    let (outcome, tools) = ingest_model_response_envelopes(
        ToolCallFormat::CallBrace,
        Some("Visible prefix call:web_search{"),
        &[],
        true,
        &catalog,
        &semantics,
    );

    assert!(tools.is_empty());
    match outcome {
        EnvelopeIngestOutcome::ParseFailed {
            error_json_for_model,
            visible_assistant_text,
        } => {
            // Declared-shape hint names the format in the failure.
            assert!(
                error_json_for_model.contains("Malformed"),
                "{error_json_for_model}"
            );
            assert!(
                error_json_for_model.contains("call-brace"),
                "{error_json_for_model}"
            );
            assert_eq!(visible_assistant_text, "Visible prefix");
        }
        other => panic!("expected ParseFailed, got {other:?}"),
    }
}

/// ENC-300 D4: a Dsml session fed another shape's text fails loudly naming
/// the DECLARED format — the BracketArray payload is never parsed.
#[test]
fn dsml_session_with_bracket_array_text_parse_fails_naming_dsml() {
    let catalog = enclave_test_catalog();
    let semantics = enclave_test_semantics();
    let (outcome, tools) = ingest_model_response_envelopes(
        ToolCallFormat::Dsml,
        Some(r#"[TOOL_CALLS] [{"name": "web_search", "arguments": {"query": "x"}}]"#),
        &[],
        true,
        &catalog,
        &semantics,
    );

    assert!(tools.is_empty(), "no BracketArray parse may run");
    match outcome {
        EnvelopeIngestOutcome::ParseFailed {
            error_json_for_model,
            visible_assistant_text,
        } => {
            assert!(
                error_json_for_model.contains("DSML"),
                "error must name the declared format: {error_json_for_model}"
            );
            assert!(
                !error_json_for_model.contains("web_search")
                    || error_json_for_model.contains("available_tools"),
                "no parsed call, only the failure shape: {error_json_for_model}"
            );
            assert_eq!(visible_assistant_text, "", "marker at offset 0 strips all");
        }
        other => panic!("expected ParseFailed, got {other:?}"),
    }
}

/// ENC-300 D4: Structured ingest never mines text — DSML syntax with no
/// native calls is a plain text turn, not a parse.
#[test]
fn structured_ingest_ignores_dsml_text_without_native_calls() {
    let catalog = enclave_test_catalog();
    let semantics = enclave_test_semantics();
    let text = "Thinking aloud.<|DSML|tool_calls></|DSML|tool_calls>";
    let (outcome, tools) = ingest_model_response_envelopes(
        ToolCallFormat::Structured,
        Some(text),
        &[],
        true,
        &catalog,
        &semantics,
    );

    assert!(tools.is_empty());
    assert!(
        matches!(outcome, EnvelopeIngestOutcome::NoMarkers),
        "expected NoMarkers, got {outcome:?}"
    );
}
