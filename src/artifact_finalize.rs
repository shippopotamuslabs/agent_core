//! Pending artifact tracking + synthetic finalize calls (AGENT-016).

use std::collections::HashSet;

use serde_json::Value;

use crate::message::ToolCallRecord;
use crate::tool_semantics::ToolSemanticsConfig;

pub fn record_artifact_progress_with_semantics(
    pending: &mut HashSet<String>,
    semantics: &ToolSemanticsConfig,
    tool_name: &str,
    arguments_json: &str,
    result_json: &str,
) {
    let Some(artifact) = &semantics.artifact else {
        return;
    };
    if tool_name == artifact.start_tool_name {
        if let Some(id) = extract_success_field(
            result_json,
            &artifact.status_field,
            &artifact.success_status_value,
            &artifact.id_field,
        ) {
            pending.insert(id);
        }
    } else if tool_name == artifact.append_tool_name {
        let Some(complete) = parse_success_bool_field(
            result_json,
            &artifact.status_field,
            &artifact.success_status_value,
            &artifact.completion_field,
        ) else {
            return;
        };
        let Some(aid) = extract_field(arguments_json, &artifact.id_field) else {
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
    result_json: &str,
    status_field: &str,
    success_status_value: &str,
    field: &str,
) -> Option<String> {
    let v: Value = serde_json::from_str(result_json).ok()?;
    let o = v.as_object()?;
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
    result_json: &str,
    status_field: &str,
    success_status_value: &str,
    field: &str,
) -> Option<bool> {
    let v: Value = serde_json::from_str(result_json).ok()?;
    let o = v.as_object()?;
    if o.get(status_field)?.as_str()? != success_status_value {
        return None;
    }
    match o.get(field)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => {
            Some(s.eq_ignore_ascii_case("true") || s == "1" || s.eq_ignore_ascii_case("yes"))
        }
        Value::Number(n) => n.as_u64().map(|u| u == 1).or(Some(false)),
        _ => Some(false),
    }
}

fn extract_field(json: &str, field: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    let id = v.get(field)?.as_str()?;
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
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
            let mut args = serde_json::Map::new();
            args.insert(artifact.id_field.clone(), Value::String(id.clone()));
            args.insert(
                artifact.append_blocks_field.clone(),
                artifact.empty_append_payload_value.clone(),
            );
            args.insert(artifact.completion_field.clone(), Value::Bool(true));
            ToolCallRecord {
                call_id: format!("{}{}", artifact.synthetic_call_id_prefix, id),
                name: artifact.append_tool_name.clone(),
                arguments_json: Value::Object(args).to_string(),
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
}
