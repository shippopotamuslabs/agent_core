//! TOOL-PARSE-001 — Rust pipeline integration & unit tests.
//!
//! See `plans/TOOL-PARSE-001/test_spec.md` for the scenario catalog.
//! Each `#[test]` is named after its scenario ID (e.g. `r_01_…`).

use agent_core::catalog::{FieldHint, ToolSchemaHint};
use agent_core::tool_parse::json_repair::repair_json;
use agent_core::tool_parse::result::ToolParseFailureReason;
use agent_core::tool_parse::schema_coerce::coerce_to_schema;
use agent_core::tool_parse::parse_tool_calls;
use serde_json::Value;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn array_hint() -> ToolSchemaHint {
    ToolSchemaHint {
        required_fields: vec![FieldHint {
            name: "urls".to_string(),
            ty: "array".to_string(),
            description: "URLs to extract".to_string(),
        }],
        defaults: HashMap::new(),
    }
}

fn optional_string_hint() -> ToolSchemaHint {
    ToolSchemaHint {
        required_fields: vec![FieldHint {
            name: "format".to_string(),
            ty: "string?".to_string(),
            description: "Optional/nullable format".to_string(),
        }],
        defaults: HashMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Rust pipeline: happy path
// ---------------------------------------------------------------------------

#[test]
fn r_01_dsml_standard_delimiters_single_invoke() {
    let raw = "<| DSML | tool_calls><| DSML | invoke name=\"web_search\">\n\
               <| DSML | parameter name=\"query\" string=\"true\">digg.com xAI completes training Grok V9</| DSML | parameter>\n\
               </| DSML | invoke></| DSML | tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(batch.successes[0].tool_name, "web_search");
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"query":"digg.com xAI completes training Grok V9"}"#
    );
    assert!(batch.failures.is_empty());
    assert_eq!(batch.visible_prefix, "");
    assert_eq!(batch.visible_suffix, "");
}

#[test]
fn r_02_dsml_fullwidth_delimiters() {
    let raw = "<｜｜DSML｜｜tool_calls><｜｜DSML｜｜invoke name=\"web_search\">\n\
               <｜｜DSML｜｜parameter name=\"query\" string=\"true\">android api 36</｜｜DSML｜｜parameter>\n\
               </｜｜DSML｜｜invoke></｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(batch.successes[0].tool_name, "web_search");
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"query":"android api 36"}"#
    );
    assert!(batch.failures.is_empty());
}

#[test]
fn r_03_dsml_invoke_string_true_repair_false() {
    let raw = "<| DSML | tool_calls><| DSML | invoke name=\"web_search\">\n\
               <| DSML | parameter name=\"query\" string=\"true\">digg.com xAI completes training Grok V9</| DSML | parameter>\n\
               </| DSML | invoke></| DSML | tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(batch.successes[0].repaired, false);
    assert!(batch.successes[0].repairs.is_empty());
}

#[test]
fn r_04_dsml_three_invokes_in_one_tool_calls_block() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_extract\">\
               <｜｜DSML｜｜parameter name=\"urls\" string=\"false\">[\"https://a.com\"]</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               <｜｜DSML｜｜invoke name=\"web_extract\">\
               <｜｜DSML｜｜parameter name=\"urls\" string=\"false\">[\"https://b.com\"]</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               <｜｜DSML｜｜invoke name=\"web_search\">\
               <｜｜DSML｜｜parameter name=\"query\" string=\"true\">kotlin parser repair</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 3);
    assert_eq!(batch.successes[0].tool_name, "web_extract");
    assert_eq!(batch.successes[1].tool_name, "web_extract");
    assert_eq!(batch.successes[2].tool_name, "web_search");
    assert_eq!(
        batch.successes[2].arguments_json,
        r#"{"query":"kotlin parser repair"}"#
    );
}

#[test]
fn r_05_two_tool_calls_blocks_in_one_response() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_search\">\
               <｜｜DSML｜｜parameter name=\"query\" string=\"true\">first query</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>\
               then some prose between blocks\
               <｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_search\">\
               <｜｜DSML｜｜parameter name=\"query\" string=\"true\">second query</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 2);
    assert_eq!(batch.successes[0].arguments_json, r#"{"query":"first query"}"#);
    assert_eq!(batch.successes[1].arguments_json, r#"{"query":"second query"}"#);
}

// ---------------------------------------------------------------------------
// Rust pipeline: error paths
// ---------------------------------------------------------------------------

#[test]
fn r_06_dsml_missing_closing_parameter_tag() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_search\">\
               <｜｜DSML｜｜parameter name=\"query\" string=\"true\">unclosed parameter body\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert!(batch.successes.is_empty());
    assert_eq!(batch.failures.len(), 1);
    assert_eq!(batch.failures[0].reason, ToolParseFailureReason::MalformedEnvelope);
    // DSML-dialect discrimination required by `test_spec.md`.
    assert_eq!(batch.failures[0].format_hint, Some("dsml".to_string()));
}

#[test]
fn r_07_dsml_missing_closing_invoke_tag() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_search\">\
               <｜｜DSML｜｜parameter name=\"query\" string=\"true\">closed param</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.failures.len(), 1);
    assert_eq!(batch.failures[0].reason, ToolParseFailureReason::MalformedEnvelope);
    // DSML-dialect discrimination required by `test_spec.md`.
    assert_eq!(batch.failures[0].format_hint, Some("dsml".to_string()));
}

#[test]
fn r_08_dsml_empty_required_parameter_body() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_search\">\
               <｜｜DSML｜｜parameter name=\"query\" string=\"true\">   </｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.failures.len(), 1);
    assert_eq!(
        batch.failures[0].reason,
        ToolParseFailureReason::MissingRequiredParameter {
            tool: "web_search".to_string(),
            param: "query".to_string(),
        }
    );
}

#[test]
fn r_09_dsml_unknown_tool_name() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"unknown_tool\">\
               <｜｜DSML｜｜parameter name=\"x\" string=\"true\">y</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.failures.len(), 1);
    assert_eq!(
        batch.failures[0].reason,
        ToolParseFailureReason::UnknownTool {
            name: "unknown_tool".to_string(),
        }
    );
}

#[test]
fn r_10_dsml_unknown_parameter_name() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_search\">\
               <｜｜DSML｜｜parameter name=\"query\" string=\"true\">android</｜｜DSML｜｜parameter>\
               <｜｜DSML｜｜parameter name=\"extra\" string=\"true\">ignored</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.failures.len(), 1);
    assert_eq!(
        batch.failures[0].reason,
        ToolParseFailureReason::UnknownParameter {
            tool: "web_search".to_string(),
            param: "extra".to_string(),
        }
    );
}

#[test]
fn r_11_dsml_duplicate_parameter_name() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_search\">\
               <｜｜DSML｜｜parameter name=\"query\" string=\"true\">first</｜｜DSML｜｜parameter>\
               <｜｜DSML｜｜parameter name=\"query\" string=\"true\">second</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.failures.len(), 1);
    assert_eq!(
        batch.failures[0].reason,
        ToolParseFailureReason::DuplicateParameter {
            tool: "web_search".to_string(),
            param: "query".to_string(),
        }
    );
}

#[test]
fn r_12_dsml_unknown_parameter_precedence_over_missing_required() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_search\">\
               <｜｜DSML｜｜parameter name=\"unused\" string=\"true\">android</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.failures.len(), 1);
    assert_eq!(
        batch.failures[0].reason,
        ToolParseFailureReason::UnknownParameter {
            tool: "web_search".to_string(),
            param: "unused".to_string(),
        }
    );
}

// ---------------------------------------------------------------------------
// Rust pipeline: repair / edge cases
// ---------------------------------------------------------------------------

#[test]
fn r_13_dsml_string_true_mismatched_with_array_schema() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_extract\">\
               <｜｜DSML｜｜parameter name=\"urls\" string=\"true\">[\"[https://example.com\"]</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"urls":["https://example.com"],"extract_depth":"basic","format":"markdown"}"#
    );
    assert!(batch.successes[0].repaired);
}

#[test]
fn r_14_dsml_malformed_bracket_prefixed_url() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_extract\">\
               <｜｜DSML｜｜parameter name=\"urls\" string=\"false\">[\"[https://example.com\"]</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"urls":["https://example.com"],"extract_depth":"basic","format":"markdown"}"#
    );
    assert!(batch.successes[0].repaired);
    assert!(batch.successes[0]
        .repairs
        .iter()
        .any(|r| r == "stripped leading bracket from bracketed URL string"));
}

#[test]
fn r_15_dsml_trailing_comma_inside_json_like_parameter() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_extract\">\
               <｜｜DSML｜｜parameter name=\"urls\" string=\"false\">[\"[https://example.com\",]</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"urls":["https://example.com"],"extract_depth":"basic","format":"markdown"}"#
    );
    assert!(batch.successes[0]
        .repairs
        .iter()
        .any(|r| r == "removed trailing comma"));
}

#[test]
fn r_16_dsml_preserves_nested_brackets_escapes_newlines() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_search\">\
               <｜｜DSML｜｜parameter name=\"query\" string=\"true\">find [docs] with \\\"quoted\\\" phrase\\nnext line</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"query":"find [docs] with \"quoted\" phrase\nnext line"}"#
    );
}

#[test]
fn r_17_markdown_fenced_tool_calls() {
    let raw = "I will call the tool now.\n\n\
               ```xml\n\
               <｜｜DSML｜｜tool_calls><｜｜DSML｜｜invoke name=\"web_search\">\n\
               <｜｜DSML｜｜parameter name=\"query\" string=\"true\">android api 36</｜｜DSML｜｜parameter>\n\
               </｜｜DSML｜｜invoke></｜｜DSML｜｜tool_calls>\n\
               ```\n\n\
               Done.";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"query":"android api 36"}"#
    );
    assert_eq!(batch.visible_prefix, "I will call the tool now.\n\n");
    assert_eq!(batch.visible_suffix, "\n\nDone.");
}

#[test]
fn r_18_mistral_tool_calls_single_element() {
    let raw =
        r#"[TOOL_CALLS] [{"name": "web_search", "arguments": {"query": "kotlin coroutines"}}]"#;
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(batch.successes[0].tool_name, "web_search");
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"query":"kotlin coroutines"}"#
    );
}

#[test]
fn r_19_mistral_tool_calls_multi_element() {
    let raw = r#"[TOOL_CALLS] [{"name":"web_search","arguments":{"query":"news"}},{"name":"search_chat_history","arguments":{"query":"old chats"}}]"#;
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 2);
    assert_eq!(batch.successes[1].tool_name, "search_chat_history");
    assert!(batch.successes[1]
        .arguments_json
        .contains(r#""queries":["old chats"]"#));
}

#[test]
fn r_20_mistral_sentinel_without_json_array() {
    let raw = "[TOOL_CALLS] not_json_here";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.failures.len(), 1);
    assert_eq!(batch.failures[0].reason, ToolParseFailureReason::MalformedEnvelope);
}

#[test]
fn r_21_mistral_with_visible_prefix() {
    let raw = "I will search now.\n[TOOL_CALLS] [{\"name\": \"web_search\", \"arguments\": {\"query\": \"android 15\"}}]";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(batch.visible_prefix, "I will search now.");
    assert_eq!(batch.visible_suffix, "");
}

#[test]
fn r_22_tool_call_wrapping_json_object() {
    let raw = r#"<tool_call>{"name":"web_search","arguments":{"query":"pocket crew android"}}</tool_call>"#;
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"query":"pocket crew android"}"#
    );
}

#[test]
fn r_23_tool_call_wrapping_json_array_two_elements() {
    let raw = r#"<tool_call>[{"name":"web_search","arguments":{"query":"q1"}},{"name":"web_extract","arguments":{"urls":["https://a.com"]}}]</tool_call>"#;
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 2);
    assert_eq!(batch.successes[0].arguments_json, r#"{"query":"q1"}"#);
    assert!(batch.successes[1]
        .arguments_json
        .contains(r#""urls":["https://a.com"]"#));
    assert!(batch.successes[1]
        .arguments_json
        .contains(r#""extract_depth":"basic""#));
    assert!(batch.successes[1]
        .arguments_json
        .contains(r#""format":"markdown""#));
}

#[test]
fn r_24_glm_arg_key_arg_value_form() {
    let raw = r#"<tool_call> web_search <arg_key>query</arg_key> <arg_value>"weather in London"</arg_value> </tool_call>"#;
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(batch.successes[0].tool_name, "web_search");
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"query":"weather in London"}"#
    );
}

#[test]
fn r_25_cdata_payload() {
    let raw = r#"<![CDATA[<tool> {"name": "web_search", "arguments": {"query": "weather in London"}} </tool>]]>"#;
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"query":"weather in London"}"#
    );
}

#[test]
fn r_26_raw_json_in_prose() {
    let raw = "I will fetch that.\n\
               {\"name\":\"web_extract\",\"arguments\":{\"urls\":[\"https://example.com\"]}}\n\
               Done.";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(batch.successes[0].tool_name, "web_extract");
    assert!(batch.successes[0]
        .arguments_json
        .contains(r#""urls":["https://example.com"]"#));
    assert_eq!(batch.visible_prefix, "I will fetch that.\n");
    assert_eq!(batch.visible_suffix, "\nDone.");
}

#[test]
fn r_27_parameters_key_normalized() {
    let raw = r#"<tool_call>{"name":"web_search","parameters":{"query":"llama 3.2 benchmarks"}}</tool_call>"#;
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"query":"llama 3.2 benchmarks"}"#
    );
}

#[test]
fn r_28_single_quoted_json_repaired() {
    let raw = r#"<tool_call> {'name': 'web_search', 'arguments': {'query': 'weather in London'}} </tool_call>"#;
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"query":"weather in London"}"#
    );
    assert!(batch.successes[0]
        .repairs
        .iter()
        .any(|r| r == "converted single quotes to double quotes"));
}

#[test]
fn r_29_smart_quotes_normalized() {
    let raw = "<tool_call> {\"name\": \"web_search\", \"arguments\": {\"query\": \"cost of \u{201C}bread\u{201D} in Paris\"}} </tool_call>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"query":"cost of \"bread\" in Paris"}"#
    );
}

#[test]
fn r_30_mutation_defense_string_true_on_urls_plus_catalog_defaults() {
    let raw = "<｜｜DSML｜｜tool_calls>\
               <｜｜DSML｜｜invoke name=\"web_extract\">\
               <｜｜DSML｜｜parameter name=\"urls\" string=\"true\">[\"[https://example.com\"]</｜｜DSML｜｜parameter>\
               </｜｜DSML｜｜invoke>\
               </｜｜DSML｜｜tool_calls>";
    let batch = parse_tool_calls(raw);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"urls":["https://example.com"],"extract_depth":"basic","format":"markdown"}"#
    );
    assert!(batch.successes[0].repaired);
}

#[test]
fn catalog_append_blocks_uses_schema_required_fields_and_default() {
    let raw = r#"<tool_call>{"name":"append_blocks","arguments":{"artifactId":"artifact-1","blocks":[{"type":"heading","level":1,"text":"Research Document"}]}}</tool_call>"#;
    let batch = parse_tool_calls(raw);
    assert!(batch.failures.is_empty(), "unexpected failures: {:?}", batch.failures);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(batch.successes[0].tool_name, "append_blocks");
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"artifactId":"artifact-1","blocks":[{"level":1,"text":"Research Document","type":"heading"}],"isComplete":false}"#
    );
}

#[test]
fn catalog_start_artifact_accepts_catalog_required_title() {
    let raw = r#"<tool_call>{"name":"start_artifact","arguments":{"title":"Research Brief"}}</tool_call>"#;
    let batch = parse_tool_calls(raw);
    assert!(batch.failures.is_empty(), "unexpected failures: {:?}", batch.failures);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(batch.successes[0].tool_name, "start_artifact");
    assert_eq!(batch.successes[0].arguments_json, r#"{"title":"Research Brief"}"#);
}

#[test]
fn catalog_manage_memories_accepts_catalog_required_action() {
    let raw = r#"<tool_call>{"name":"manage_memories","arguments":{"action":"search"}}</tool_call>"#;
    let batch = parse_tool_calls(raw);
    assert!(batch.failures.is_empty(), "unexpected failures: {:?}", batch.failures);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(batch.successes[0].tool_name, "manage_memories");
    assert_eq!(batch.successes[0].arguments_json, r#"{"action":"search"}"#);
}

#[test]
fn catalog_search_current_artifact_is_explicitly_covered() {
    let raw = r#"<tool_call>{"name":"search_current_artifact","arguments":{"artifactId":"artifact-1","query":"summary"}}</tool_call>"#;
    let batch = parse_tool_calls(raw);
    assert!(batch.failures.is_empty(), "unexpected failures: {:?}", batch.failures);
    assert_eq!(batch.successes.len(), 1);
    assert_eq!(batch.successes[0].tool_name, "search_current_artifact");
    assert_eq!(
        batch.successes[0].arguments_json,
        r#"{"artifactId":"artifact-1","query":"summary"}"#
    );
}

// ---------------------------------------------------------------------------
// Rust JSON-repair unit tests
// ---------------------------------------------------------------------------

#[test]
fn r_31_missing_comma_between_object_values() {
    let value = repair_json(r#"{"a":"foo" "b":"bar"}"#).expect("repair");
    assert_eq!(value.to_string(), r#"{"a":"foo","b":"bar"}"#);
}

#[test]
fn r_32_missing_comma_between_array_elements() {
    let value = repair_json(r#"["alpha" "beta" "gamma"]"#).expect("repair");
    assert_eq!(value.to_string(), r#"["alpha","beta","gamma"]"#);
}

#[test]
fn r_33_python_none_to_null() {
    let value = repair_json(r#"{"result": None}"#).expect("repair");
    assert_eq!(value.to_string(), r#"{"result":null}"#);
}

#[test]
fn r_34_python_true_false_to_booleans() {
    let value = repair_json(r#"{"flag": True, "other": False}"#).expect("repair");
    assert_eq!(value.to_string(), r#"{"flag":true,"other":false}"#);
}

#[test]
fn r_35_undefined_to_null() {
    let value = repair_json(r#"{"value": undefined}"#).expect("repair");
    assert_eq!(value.to_string(), r#"{"value":null}"#);
}

#[test]
fn r_36_missing_colon_between_key_and_value() {
    let value = repair_json(r#"{"key" "value"}"#).expect("repair");
    assert_eq!(value.to_string(), r#"{"key":"value"}"#);
}

#[test]
fn r_37_leading_comma_in_object() {
    let value = repair_json(r#"{, "k":"v"}"#).expect("repair");
    assert_eq!(value.to_string(), r#"{"k":"v"}"#);
}

#[test]
fn r_38_truncated_string_missing_end_quote() {
    let value = repair_json(r#"{"query":"android development"#).expect("repair");
    assert_eq!(value.to_string(), r#"{"query":"android development"}"#);
}

#[test]
fn r_39_block_and_line_comments_stripped() {
    let value =
        repair_json("{\"k\": /* comment */ \"v\", // trailing\n \"k2\":\"v2\"}").expect("repair");
    let obj = value.as_object().expect("object");
    assert_eq!(obj.get("k").and_then(Value::as_str), Some("v"));
    assert_eq!(obj.get("k2").and_then(Value::as_str), Some("v2"));
}

#[test]
fn r_40_string_concatenation_collapsed() {
    let value = repair_json(r#"{"title": "hello " + "world"}"#).expect("repair");
    assert_eq!(value.to_string(), r#"{"title":"hello world"}"#);
}

#[test]
fn r_41_newline_delimited_json_wrapped_in_array() {
    let input = "{\"name\":\"web_search\",\"arguments\":{\"query\":\"first\"}}\n\
                 {\"name\":\"web_extract\",\"arguments\":{\"urls\":[\"https://a.com\"]}}";
    let value = repair_json(input).expect("repair");
    let arr = value.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["name"].as_str(), Some("web_search"));
    assert_eq!(arr[1]["name"].as_str(), Some("web_extract"));
}

// ---------------------------------------------------------------------------
// Rust schema-coercion unit tests
// ---------------------------------------------------------------------------

#[test]
fn r_42_python_style_string_array_to_real_array() {
    let hint = array_hint();
    let mut value = Value::String("[{'href': 'https://example.com'}]".to_string());
    let coerced = coerce_to_schema(&mut value, &hint);
    let arr = coerced.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["href"].as_str(), Some("https://example.com"));
}

#[test]
fn r_43_array_string_with_trailing_garbage_extracted() {
    let hint = array_hint();
    let mut value = Value::String("[\"https://example.com\"]{\\n}".to_string());
    let coerced = coerce_to_schema(&mut value, &hint);
    let arr = coerced.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0].as_str(), Some("https://example.com"));
}

#[test]
fn r_44_double_encoded_json_string_unwrapped() {
    let hint = array_hint();
    // Two-level encoding per `test_spec.md`:
    //   * The JSON payload fed to `repair_json` is the literal string
    //     `"\"[\\\"https://example.com\\\"]\""` — i.e. a JSON string whose
    //     content is the JSON-string form of the array literal.
    //   * Parsing it once via `repair_json` yields a `Value::String` whose
    //     content is `"[\"https://example.com\"]"` (still a JSON string,
    //     not yet an array). `coerce_to_schema` must then perform one more
    //     unwrap to reach the array. This guards against a recursive-unwrap
    //     mutation where the parser only peels one layer.
    let payload = "\"\\\"[\\\\\\\"https://example.com\\\\\\\"]\\\"\"";
    let mut value = repair_json(payload).expect("repair");
    // Sanity: after one repair pass, we should still be holding a
    // JSON-string-encoded JSON array.
    assert_eq!(
        value,
        Value::String("\"[\\\"https://example.com\\\"]\"".to_string())
    );
    let coerced = coerce_to_schema(&mut value, &hint);
    assert_eq!(
        coerced,
        Value::Array(vec![Value::String("https://example.com".to_string())])
    );
}

#[test]
fn r_45_empty_string_to_null_for_nullable_field() {
    let hint = optional_string_hint();
    let mut value = Value::String("".to_string());
    let coerced = coerce_to_schema(&mut value, &hint);
    assert_eq!(coerced, Value::Null);
}

