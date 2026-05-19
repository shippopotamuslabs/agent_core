//! Pending artifact tracking + synthetic finalize calls (AGENT-016).

use std::collections::HashSet;

use serde_json::Value;

use crate::message::ToolCallRecord;

/// Updates `pending` from a completed tool invocation (mirrors `LlmToolingOrchestrator`).
pub fn record_artifact_progress(
    pending: &mut HashSet<String>,
    tool_name: &str,
    arguments_json: &str,
    result_json: &str,
) {
    match tool_name {
        "start_artifact" => {
            if let Some(id) = extract_start_success_artifact_id(result_json) {
                pending.insert(id);
            }
        }
        "append_blocks" => {
            let Some(complete) = parse_append_blocks_success_is_complete(result_json) else {
                return;
            };
            let Some(aid) = extract_artifact_id_from_append_args(arguments_json) else {
                return;
            };
            if complete {
                pending.remove(&aid);
            } else {
                pending.insert(aid);
            }
        }
        _ => {}
    }
}

fn extract_start_success_artifact_id(result_json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(result_json).ok()?;
    let o = v.as_object()?;
    if o.get("status")?.as_str()? != "success" {
        return None;
    }
    let id = o.get("artifactId")?.as_str()?;
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

fn parse_append_blocks_success_is_complete(result_json: &str) -> Option<bool> {
    let v: Value = serde_json::from_str(result_json).ok()?;
    let o = v.as_object()?;
    if o.get("status")?.as_str()? != "success" {
        return None;
    }
    match o.get("isComplete")? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => {
            Some(s.eq_ignore_ascii_case("true") || s == "1" || s.eq_ignore_ascii_case("yes"))
        }
        Value::Number(n) => n.as_u64().map(|u| u == 1).or(Some(false)),
        _ => Some(false),
    }
}

fn extract_artifact_id_from_append_args(arguments_json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(arguments_json).ok()?;
    let id = v.get("artifactId")?.as_str()?;
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// Synthetic finalize calls (not counted toward guardrail tool budgets).
pub fn synthetic_finalize_calls(pending: &HashSet<String>) -> Vec<ToolCallRecord> {
    pending
        .iter()
        .map(|id| ToolCallRecord {
            call_id: format!("auto-finalize-{id}"),
            name: "append_blocks".into(),
            arguments_json: format!(r#"{{"artifactId":"{id}","blocks":[],"isComplete":true}}"#),
            attempt_metadata: None,
            repair_metadata: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_then_incomplete_append_keeps_pending() {
        let mut p = HashSet::new();
        record_artifact_progress(
            &mut p,
            "start_artifact",
            "{}",
            r#"{"status":"success","artifactId":"art-1"}"#,
        );
        assert!(p.contains("art-1"));
        record_artifact_progress(
            &mut p,
            "append_blocks",
            r#"{"artifactId":"art-1","blocks":[]}"#,
            r#"{"status":"success","isComplete":false}"#,
        );
        assert!(p.contains("art-1"));
    }

    #[test]
    fn complete_append_removes() {
        let mut p = HashSet::from(["art-1".into()]);
        record_artifact_progress(
            &mut p,
            "append_blocks",
            r#"{"artifactId":"art-1","blocks":[]}"#,
            r#"{"status":"success","isComplete":true}"#,
        );
        assert!(p.is_empty());
    }

    #[test]
    fn synthetic_finalize_shapes() {
        let p: HashSet<_> = ["a".into(), "b".into()].into_iter().collect();
        let calls = synthetic_finalize_calls(&p);
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|c| c.name == "append_blocks"));
        assert!(calls
            .iter()
            .all(|c| c.call_id.starts_with("auto-finalize-")));
    }
}
