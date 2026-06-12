#[path = "support/mod.rs"]
mod support;

use agent_core::engine::{ingest_model_response_envelopes, EnvelopeIngestOutcome};
use support::enclave_test_semantics::{enclave_test_catalog, enclave_test_semantics};

#[test]
fn strong_unparseable_marker_returns_parse_failed_with_truncated_visible_text() {
    let catalog = enclave_test_catalog();
    let semantics = enclave_test_semantics();
    let (outcome, tools) = ingest_model_response_envelopes(
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
            assert!(error_json_for_model.contains("Malformed tool envelope"));
            assert_eq!(visible_assistant_text, "Visible prefix");
        }
        other => panic!("expected ParseFailed, got {other:?}"),
    }
}
