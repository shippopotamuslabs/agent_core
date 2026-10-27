//! fix-artifact-agent-loop — canonicalization and execution-gate red tests.

#[path = "support/mod.rs"]
mod support;

use agent_core::canonicalize::{
    canonicalize_raw_arguments_with_semantics, canonicalize_tool_call_with_semantics,
    CanonicalizeToolCallOutcome, ToolCallCanonicalizeMode,
};
use agent_core::catalog::{ToolCatalog, ToolSpec};
use agent_core::repair::{RepairOutcome, ToolRepairPipeline};
use agent_core::tool_parse::coerce_provider_arguments_with_semantics;
use agent_core::tool_semantics::ToolSemanticsConfig;
use agent_core::turn_io::RawToolCall;
use serde_json::Value;
use support::enclave_test_semantics::{
    enclave_test_catalog, enclave_test_semantics, parse_enclave_tool_calls,
};

fn enclave_env() -> (ToolCatalog, ToolSemanticsConfig) {
    let semantics = enclave_test_semantics();
    let catalog = enclave_test_catalog();
    (catalog, semantics)
}

fn append_blocks_raw(arguments_json: &str) -> RawToolCall {
    RawToolCall {
        id: Some("call-append-blocks".to_string()),
        name: "append_blocks".to_string(),
        arguments_json: arguments_json.to_string(),
    }
}

fn assert_blocks_array(arguments_json: &str, expected_text: &str, scenario: &str) {
    let value: Value = serde_json::from_str(arguments_json)
        .unwrap_or_else(|err| panic!("{scenario} arguments_json must parse: {err}"));
    let blocks = value["blocks"]
        .as_array()
        .unwrap_or_else(|| panic!("{scenario} blocks must be a JSON array"));
    assert_eq!(blocks.len(), 1, "{scenario} blocks length");
    assert_eq!(blocks[0]["text"], expected_text, "{scenario} block text");
}

#[test]
fn faal_cp_01_stringified_blocks_become_array() {
    let (catalog, semantics) = enclave_env();
    let result = canonicalize_raw_arguments_with_semantics(
        "append_blocks",
        r#"{"artifactId":"art-cp-1","blocks":"[{\"type\":\"paragraph\",\"text\":\"Hello\"}]","isComplete":false}"#,
        &catalog,
        &semantics,
        ToolCallCanonicalizeMode::ExecutionStrict,
    )
    .expect("FAAL-CP-01 canonicalize_raw_arguments succeeds");

    assert_blocks_array(&result.arguments_json, "Hello", "FAAL-CP-01");
    assert_eq!(
        result.value["artifactId"], "art-cp-1",
        "FAAL-CP-01 artifactId"
    );
}

#[test]
fn faal_cp_02_provider_ingress_allows_unknown_execution_rejects() {
    let (catalog, semantics) = enclave_env();
    let raw = RawToolCall {
        id: Some("unknown-1".to_string()),
        name: "totally_unknown_tool_xyz".to_string(),
        arguments_json: "{}".to_string(),
    };

    let (ingress, _) = canonicalize_tool_call_with_semantics(
        &raw,
        &catalog,
        &semantics,
        ToolCallCanonicalizeMode::ProviderIngress,
    );
    assert!(
        matches!(ingress, CanonicalizeToolCallOutcome::Ok(_)),
        "FAAL-CP-02 ingress"
    );

    let (strict, _) = canonicalize_tool_call_with_semantics(
        &raw,
        &catalog,
        &semantics,
        ToolCallCanonicalizeMode::ExecutionStrict,
    );
    match strict {
        CanonicalizeToolCallOutcome::Reject {
            error_json_for_model,
        } => assert!(
            error_json_for_model.contains("Unknown tool"),
            "FAAL-CP-02 strict reject message",
        ),
        other => panic!("FAAL-CP-02 expected Reject, got {other:?}"),
    }
}

#[test]
fn faal_cp_03_provider_ingress_matches_execution_strict_arguments() {
    let (catalog, semantics) = enclave_env();
    let args = r#"{"artifactId":"art-parity-1","blocks":"[{\"type\":\"paragraph\",\"text\":\"Hi\"}]","isComplete":true}"#;
    let ingress =
        coerce_provider_arguments_with_semantics("append_blocks", args, &catalog, &semantics);
    let (strict, _) = canonicalize_tool_call_with_semantics(
        &append_blocks_raw(args),
        &catalog,
        &semantics,
        ToolCallCanonicalizeMode::ExecutionStrict,
    );
    let strict = match strict {
        CanonicalizeToolCallOutcome::Ok(call) => call,
        other => panic!("FAAL-CP-03 expected Ok, got {other:?}"),
    };

    let ingress_json: Value =
        serde_json::from_str(&ingress.arguments_json).expect("FAAL-CP-03 ingress JSON");
    let strict_json: Value =
        serde_json::from_str(&strict.arguments_json).expect("FAAL-CP-03 strict JSON");
    assert_eq!(ingress_json, strict_json, "FAAL-CP-03 semantic parity");
    assert!(ingress_json["blocks"].is_array(), "FAAL-CP-03 blocks array");
    assert_eq!(ingress_json["isComplete"], true, "FAAL-CP-03 isComplete");
}

#[test]
fn faal_cp_04_double_canonicalize_is_stable() {
    let (catalog, semantics) = enclave_env();
    let first = canonicalize_raw_arguments_with_semantics(
        "append_blocks",
        r#"{"artifactId":"art-cp-4","blocks":"[{\"type\":\"paragraph\",\"text\":\"Stable\"}]","isComplete":false}"#,
        &catalog,
        &semantics,
        ToolCallCanonicalizeMode::ExecutionStrict,
    )
    .expect("FAAL-CP-04 first canonicalize");
    let snapshot = first.arguments_json.clone();
    let mut value = first.value;

    let second = canonicalize_raw_arguments_with_semantics(
        "append_blocks",
        &serde_json::to_string(&value).expect("FAAL-CP-04 value json"),
        &catalog,
        &semantics,
        ToolCallCanonicalizeMode::ExecutionStrict,
    )
    .expect("FAAL-CP-04 second canonicalize");

    assert_eq!(second.arguments_json, snapshot, "FAAL-CP-04 stable JSON");
}

#[test]
fn faal_rc_01_repair_uses_canonicalize_for_stringified_blocks() {
    let (catalog, semantics) = enclave_env();
    let (outcome, _) = ToolRepairPipeline::repair_with_semantics(
        &append_blocks_raw(
            r#"{"artifactId":"art-coerce-1","blocks":"[{\"type\":\"paragraph\",\"text\":\"Hello\"}]","isComplete":false}"#,
        ),
        &catalog,
        &semantics,
    );

    match outcome {
        RepairOutcome::Repaired(record) => {
            assert_blocks_array(&record.arguments_json, "Hello", "FAAL-RC-01")
        }
        other => panic!("FAAL-RC-01 expected Repaired, got {other:?}"),
    }
}

#[test]
fn faal_rc_02_repair_rejects_malformed_blocks_string() {
    let (catalog, semantics) = enclave_env();
    let (outcome, _) = ToolRepairPipeline::repair_with_semantics(
        &append_blocks_raw(
            r#"{"artifactId":"art-bad-1","blocks":"[{not valid json","isComplete":false}"#,
        ),
        &catalog,
        &semantics,
    );

    match outcome {
        RepairOutcome::Reject {
            error_json_for_model,
        } => assert!(
            error_json_for_model.contains("Invalid JSON")
                || error_json_for_model.contains("blocks"),
            "FAAL-RC-02 reject detail",
        ),
        other => panic!("FAAL-RC-02 expected Reject, got {other:?}"),
    }
}

#[test]
fn faal_rc_03_bad_ingress_arguments_are_still_rejected_at_execution() {
    let (catalog, semantics) = enclave_env();
    let ingress = coerce_provider_arguments_with_semantics(
        "append_blocks",
        r#"{"artifactId":"art-bad-ingress","blocks":"[{not valid json","isComplete":false}"#,
        &catalog,
        &semantics,
    );
    assert_ne!(
        ingress.arguments_json, "{}",
        "FAAL-RC-03 must not silently mask bad args"
    );
    assert!(
        !ingress.repairs.is_empty(),
        "FAAL-RC-03 repairs record parse failure"
    );

    let (outcome, _) = ToolRepairPipeline::repair_with_semantics(
        &RawToolCall {
            id: Some("bad-ingress".to_string()),
            name: ingress.tool_name,
            arguments_json: ingress.arguments_json,
        },
        &catalog,
        &semantics,
    );
    assert!(
        matches!(outcome, RepairOutcome::Reject { .. }),
        "FAAL-RC-03 execution reject"
    );
}

#[test]
fn frg14_slice1_no_shape_passthrough_preserves_extra_fields() {
    let mut semantics = ToolSemanticsConfig::agnostic();
    semantics.catalog.tools = vec![
        ToolSpec {
            name: "web_search".to_string(),
            aliases: Vec::new(),
            reproducible: true,
            schema_hint: None,
        },
        ToolSpec {
            name: "web_extract".to_string(),
            aliases: Vec::new(),
            reproducible: true,
            schema_hint: None,
        },
    ];
    assert!(
        semantics.canonical_shapes.is_empty(),
        "FRG14-SLICE1 test requires no canonical_shapes"
    );
    let catalog = ToolCatalog::from_semantics(&semantics).expect("FRG14-SLICE1 catalog");

    let search = canonicalize_raw_arguments_with_semantics(
        "web_search",
        r#"{"query":"x","extra":1}"#,
        &catalog,
        &semantics,
        ToolCallCanonicalizeMode::ProviderIngress,
    )
    .expect("FRG14-SLICE1 web_search canonicalize");
    let search_value: Value =
        serde_json::from_str(&search.arguments_json).expect("FRG14-SLICE1 search JSON");
    assert_eq!(search_value["query"], "x", "FRG14-SLICE1 search query");
    assert_eq!(
        search_value["extra"], 1,
        "FRG14-SLICE1 search extra preserved"
    );

    let extract = canonicalize_raw_arguments_with_semantics(
        "web_extract",
        r#"{"urls":["u"],"extra":true}"#,
        &catalog,
        &semantics,
        ToolCallCanonicalizeMode::ProviderIngress,
    )
    .expect("FRG14-SLICE1 web_extract canonicalize");
    let extract_value: Value =
        serde_json::from_str(&extract.arguments_json).expect("FRG14-SLICE1 extract JSON");
    assert_eq!(
        extract_value["urls"],
        serde_json::json!(["u"]),
        "FRG14-SLICE1 extract urls"
    );
    assert_eq!(
        extract_value["extra"], true,
        "FRG14-SLICE1 extract extra preserved"
    );
    assert!(
        extract_value.get("extract_depth").is_none(),
        "FRG14-SLICE1 no injected extract_depth"
    );
    assert!(
        extract_value.get("format").is_none(),
        "FRG14-SLICE1 no injected format"
    );
}

#[test]
fn faal_md_01_repair_provider_and_parse_paths_all_emit_array_blocks() {
    let (catalog, semantics) = enclave_env();
    let args = r#"{"artifactId":"art-md-1","blocks":"[{\"type\":\"paragraph\",\"text\":\"Parity\"}]","isComplete":false}"#;
    let ingress =
        coerce_provider_arguments_with_semantics("append_blocks", args, &catalog, &semantics);
    let (repair, _) =
        ToolRepairPipeline::repair_with_semantics(&append_blocks_raw(args), &catalog, &semantics);
    let raw_text =
        format!(r#"<tool_call>{{"name":"append_blocks","arguments":{args}}}</tool_call>"#,);
    let parsed = parse_enclave_tool_calls(&raw_text);

    assert_blocks_array(&ingress.arguments_json, "Parity", "FAAL-MD-01 ingress");
    match repair {
        RepairOutcome::Repaired(record) => {
            assert_blocks_array(&record.arguments_json, "Parity", "FAAL-MD-01 repair")
        }
        other => panic!("FAAL-MD-01 expected Repaired, got {other:?}"),
    }
    assert_eq!(parsed.successes.len(), 1, "FAAL-MD-01 parsed successes");
    assert_blocks_array(
        &parsed.successes[0].arguments_json,
        "Parity",
        "FAAL-MD-01 parse_tool_calls",
    );
}
