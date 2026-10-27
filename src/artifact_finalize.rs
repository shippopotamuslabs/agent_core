//! Pending artifact tracking + synthetic finalize calls (AGENT-016).

use std::collections::HashSet;

use serde_json::Value;

use crate::message::ToolCallRecord;
use crate::parsed_tool_args::{ParsedToolArgs, ParsedToolResult};
use crate::tool_semantics::ToolSemanticsConfig;

/// Boundary entry: parses both payloads once, then delegates to
/// [`record_artifact_progress_with_parsed`]. Hot paths should parse once and
/// call the parsed entry directly.
pub fn record_artifact_progress_with_semantics(
    pending: &mut HashSet<String>,
    semantics: &ToolSemanticsConfig,
    tool_name: &str,
    arguments_json: &str,
    result_json: &str,
) {
    let args = ParsedToolArgs::parse(arguments_json);
    let result = ParsedToolResult::parse(result_json);
    record_artifact_progress_with_parsed(pending, semantics, tool_name, &args, &result);
}

/// Hot-path entry over already-parsed payloads (no re-parse).
pub fn record_artifact_progress_with_parsed(
    pending: &mut HashSet<String>,
    semantics: &ToolSemanticsConfig,
    tool_name: &str,
    args: &ParsedToolArgs,
    result: &ParsedToolResult,
) {
    let Some(artifact) = &semantics.artifact else {
        return;
    };
    if tool_name == artifact.start_tool_name {
        if let Some(id) = extract_success_field(
            result.value(),
            &artifact.status_field,
            &artifact.success_status_value,
            &artifact.id_field,
        ) {
            pending.insert(id);
        }
    } else if tool_name == artifact.append_tool_name {
        let Some(complete) = parse_success_bool_field(
            result,
            &artifact.status_field,
            &artifact.success_status_value,
            &artifact.completion_field,
        ) else {
            return;
        };
        let Some(aid) = args.get_str(&artifact.id_field) else {
            return;
        };
        if complete {
            pending.remove(&aid);
        } else {
            pending.insert(aid);
        }
    }
}

fn extract_success_field(
    result: &Value,
    status_field: &str,
    success_status_value: &str,
    field: &str,
) -> Option<String> {
    let o = result.as_object()?;
    if o.get(status_field)?.as_str()? != success_status_value {
        return None;
    }
    let id = o.get(field)?.as_str()?;
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

fn parse_success_bool_field(
    result: &ParsedToolResult,
    status_field: &str,
    success_status_value: &str,
    field: &str,
) -> Option<bool> {
    let o = result.value().as_object()?;
    if o.get(status_field)?.as_str()? != success_status_value {
        return None;
    }
    result.get_bool_coerced(field)
}

/// Renders the synthetic finalize call's arguments JSON for one pending
/// artifact (ENC-290): the three finalize values keyed by the configured
/// wire names (`id_field` / `append_blocks_field` / `completion_field`),
/// serialized exactly once. The [`ToolSemanticsConfig`] `Value` payload
/// (`empty_append_payload_value`) is cloned exactly once, here at the
/// typed boundary — no other site clones config `Value`s into call
/// arguments.
pub fn finalize_arguments_json(
    artifact: &crate::tool_semantics::ArtifactToolSemantics,
    id: &str,
) -> String {
    let mut args = serde_json::Map::with_capacity(3);
    args.insert(artifact.id_field.clone(), Value::String(id.to_string()));
    args.insert(
        artifact.append_blocks_field.clone(),
        artifact.empty_append_payload_value.clone(),
    );
    args.insert(artifact.completion_field.clone(), Value::Bool(true));
    Value::Object(args).to_string()
}

pub fn synthetic_finalize_calls_with_semantics(
    pending: &HashSet<String>,
    semantics: &ToolSemanticsConfig,
) -> Vec<ToolCallRecord> {
    let Some(artifact) = &semantics.artifact else {
        return Vec::new();
    };
    pending
        .iter()
        .map(|id| {
            let arguments_json = finalize_arguments_json(artifact, id);
            ToolCallRecord {
                call_id: format!("{}{}", artifact.synthetic_call_id_prefix, id),
                name: artifact.append_tool_name.clone(),
                arguments_json,
                attempt_metadata: None,
                repair_metadata: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_then_incomplete_append_keeps_pending() {
        let semantics = crate::tool_semantics::test_fixture_enclave_semantics();
        let mut p = HashSet::new();
        record_artifact_progress_with_semantics(
            &mut p,
            &semantics,
            "start_artifact",
            "{}",
            r#"{"status":"success","artifactId":"art-1"}"#,
        );
        assert!(p.contains("art-1"));
        record_artifact_progress_with_semantics(
            &mut p,
            &semantics,
            "append_blocks",
            r#"{"artifactId":"art-1","blocks":[]}"#,
            r#"{"status":"success","isComplete":false}"#,
        );
        assert!(p.contains("art-1"));
    }

    #[test]
    fn complete_append_removes() {
        let semantics = crate::tool_semantics::test_fixture_enclave_semantics();
        let mut p = HashSet::from(["art-1".into()]);
        record_artifact_progress_with_semantics(
            &mut p,
            &semantics,
            "append_blocks",
            r#"{"artifactId":"art-1","blocks":[]}"#,
            r#"{"status":"success","isComplete":true}"#,
        );
        assert!(p.is_empty());
    }

    #[test]
    fn synthetic_finalize_shapes() {
        let semantics = crate::tool_semantics::test_fixture_enclave_semantics();
        let p: HashSet<_> = ["a".into(), "b".into()].into_iter().collect();
        let calls = synthetic_finalize_calls_with_semantics(&p, &semantics);
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|c| c.name == "append_blocks"));
        assert!(calls
            .iter()
            .all(|c| c.call_id.starts_with("auto-finalize-")));
    }

    #[test]
    fn synthetic_finalize_args_byte_identical() {
        // ENC-290: the typed finalize path must emit exactly the JSON the
        // direct-`Value` construction produced.
        let semantics = crate::tool_semantics::test_fixture_enclave_semantics();
        let artifact = semantics.artifact.as_ref().expect("artifact semantics");
        let json = finalize_arguments_json(artifact, "a");
        assert_eq!(json, r#"{"artifactId":"a","blocks":[],"isComplete":true}"#);

        let p: HashSet<_> = ["a".into()].into_iter().collect();
        let calls = synthetic_finalize_calls_with_semantics(&p, &semantics);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].arguments_json,
            r#"{"artifactId":"a","blocks":[],"isComplete":true}"#
        );
        assert_eq!(calls[0].call_id, "auto-finalize-a");
    }
}
