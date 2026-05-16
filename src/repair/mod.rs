//! Tool name + argument repair before execution (see AGENT-014).

mod fuzzy;

pub use fuzzy::{best_catalog_match, FUZZY_AUTO_ACCEPT};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::catalog::{FieldHint, ToolCatalog};
use crate::events::AgentEvent;
use crate::message::{RepairRecord, ToolCallRecord};
use crate::turn_io::RawToolCall;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairOutcome {
    Repaired(ToolCallRecord),
    Reject {
        error_json_for_model: String,
    },
    Suggest {
        original: String,
        closest_name: String,
    },
}

pub struct ToolRepairPipeline;

impl ToolRepairPipeline {
    pub fn repair(raw: &RawToolCall, catalog: &ToolCatalog) -> (RepairOutcome, Vec<AgentEvent>) {
        let mut events = Vec::new();
        let (resolved_name, repair_meta) = match Self::repair_name(&raw.name, catalog, &mut events) {
            Ok(x) => x,
            Err(rej) => return (rej, events),
        };

        let arguments_json = match Self::repair_arguments(&resolved_name, &raw.arguments_json, catalog) {
            Ok(s) => s,
            Err(rej) => return (rej, events),
        };

        let record = ToolCallRecord {
            call_id: raw.id.clone().unwrap_or_else(|| "call".into()),
            name: resolved_name,
            arguments_json,
            repair_metadata: repair_meta,
        };

        (RepairOutcome::Repaired(record), events)
    }

    fn repair_name(
        raw: &str,
        catalog: &ToolCatalog,
        events: &mut Vec<AgentEvent>,
    ) -> Result<(String, Option<RepairRecord>), RepairOutcome> {
        let mut s = raw.trim().to_string();
        loop {
            if let Some(r) = s.strip_prefix("functions.") {
                s = r.to_string();
            } else if let Some(r) = s.strip_prefix("tools.") {
                s = r.to_string();
            } else {
                break;
            }
        }
        let normalized = normalize_to_snake_case(&s);

        if let Some(spec) = catalog.resolve(&normalized) {
            return Ok((spec.name.clone(), None));
        }

        // Fuzzy
        if let Some((best, score)) = best_catalog_match(&normalized, catalog) {
            if score >= FUZZY_AUTO_ACCEPT {
                events.push(AgentEvent::ToolRepaired {
                    original_name: raw.to_string(),
                    resolved_name: best.to_string(),
                    confidence: score as f32,
                });
                return Ok((
                    best.to_string(),
                    Some(RepairRecord {
                        original_name: raw.trim().to_string(),
                        applied_aliases: vec![],
                        confidence: score as f32,
                    }),
                ));
            }
            return Err(RepairOutcome::Reject {
                error_json_for_model: reject_json(
                    &format!(
                        "Unknown tool '{}' (best match '{}' score {:.2} < {:.2})",
                        raw, best, score, FUZZY_AUTO_ACCEPT
                    ),
                    catalog,
                ),
            });
        }

        Err(RepairOutcome::Reject {
            error_json_for_model: reject_json(
                &format!("Unknown tool '{raw}' after normalization '{normalized}'"),
                catalog,
            ),
        })
    }

    fn repair_arguments(
        resolved_tool: &str,
        arguments_json: &str,
        catalog: &ToolCatalog,
    ) -> Result<String, RepairOutcome> {
        let trimmed = strip_json_fences(arguments_json);
        let mut value: Value = match serde_json::from_str(&trimmed) {
            Ok(v) => v,
            Err(e) => {
                return Err(RepairOutcome::Reject {
                    error_json_for_model: reject_json(
                        &format!("Invalid JSON for tool arguments: {e}"),
                        catalog,
                    ),
                });
            }
        };

        let Some(spec) = catalog.resolve(resolved_tool) else {
            return Ok(trimmed);
        };

        if let Some(hint) = &spec.schema_hint {
            if let Some(obj) = value.as_object_mut() {
                self::apply_schema_hint(obj, hint, resolved_tool, catalog)?;
            }
        }

        Ok(serde_json::to_string(&value).unwrap_or_else(|_| trimmed))
    }
}

fn reject_json(msg: &str, catalog: &ToolCatalog) -> String {
    let tools: Vec<&str> = catalog.all().iter().map(|s| s.name.as_str()).collect();
    serde_json::to_string(&json!({
        "error": msg,
        "available_tools": tools,
    }))
    .unwrap_or_else(|_| format!(r#"{{"error":"{msg}"}}"#))
}

fn strip_json_fences(s: &str) -> String {
    let mut t = s.trim().to_string();
    while t.ends_with("```") {
        t = t[..t.len() - 3].trim().to_string();
    }
    if let Some(rest) = t.strip_prefix("```json") {
        return strip_json_fences(rest);
    }
    if let Some(rest) = t.strip_prefix("```") {
        return strip_json_fences(rest);
    }
    if let Some(idx) = t.find(|c| c == '{' || c == '[') {
        return t[idx..].trim().to_string();
    }
    t
}

fn normalize_to_snake_case(input: &str) -> String {
    let mut r = String::new();
    let chars: Vec<char> = input.trim().chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '-' || c == ' ' {
            if !r.ends_with('_') && !r.is_empty() {
                r.push('_');
            }
            continue;
        }
        if c.is_uppercase() {
            let prev_lower = i > 0 && chars[i - 1].is_lowercase();
            let next_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();
            if prev_lower || (i > 0 && chars[i - 1].is_ascii_uppercase() && next_lower) {
                if !r.ends_with('_') && !r.is_empty() {
                    r.push('_');
                }
            }
            for lc in c.to_lowercase() {
                r.push(lc);
            }
        } else if c.is_alphanumeric() || c == '_' {
            r.push(c);
        }
    }
    while r.contains("__") {
        r = r.replace("__", "_");
    }
    r.trim_matches('_').to_string()
}

fn apply_schema_hint(
    obj: &mut Map<String, Value>,
    hint: &crate::catalog::ToolSchemaHint,
    tool_name: &str,
    catalog: &ToolCatalog,
) -> Result<(), RepairOutcome> {
    for field in &hint.required_fields {
        if obj.contains_key(&field.name) {
            coerce_field(obj, field)?;
            continue;
        }
        if let Some(def) = hint.defaults.get(&field.name) {
            obj.insert(field.name.clone(), def.clone());
        } else {
            return Err(RepairOutcome::Reject {
                error_json_for_model: reject_json(
                    &format!(
                        "Missing required field '{}' for tool '{}' (no default in catalog hint)",
                        field.name, tool_name
                    ),
                    catalog,
                ),
            });
        }
    }
    for (k, v) in &hint.defaults {
        obj.entry(k.clone()).or_insert_with(|| v.clone());
    }
    Ok(())
}

fn coerce_field(obj: &mut Map<String, Value>, field: &FieldHint) -> Result<(), RepairOutcome> {
    let Some(v) = obj.get_mut(&field.name) else {
        return Ok(());
    };
    let ty = field.ty.to_lowercase();
    if ty.contains("bool") {
        if let Some(s) = v.as_str() {
            if s == "true" {
                *v = Value::Bool(true);
            } else if s == "false" {
                *v = Value::Bool(false);
            }
        }
    } else if ty.contains("i32")
        || ty.contains("i64")
        || ty.contains("int")
        || ty.contains("number")
        || ty.contains("float")
        || ty.contains("double")
    {
        if let Some(s) = v.as_str() {
            if let Ok(n) = s.parse::<i64>() {
                *v = Value::Number(n.into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn_io::RawToolCall;

    #[test]
    fn web_seach_fuzzy_repairs_to_web_search() {
        let raw = RawToolCall {
            id: Some("c1".into()),
            name: "web_seach".into(),
            arguments_json: r#"{"query":"rust"}"#.into(),
        };
        let (out, evs) = ToolRepairPipeline::repair(&raw, ToolCatalog::global());
        match out {
            RepairOutcome::Repaired(rec) => {
                assert_eq!(rec.name, "web_search");
                assert_eq!(evs.len(), 1);
            }
            o => panic!("expected Repaired, got {o:?}"),
        }
    }

    #[test]
    fn web_search_uppercase_normalizes() {
        let raw = RawToolCall {
            id: None,
            name: "WEB_SEARCH".into(),
            arguments_json: "{}".into(),
        };
        let (out, evs) = ToolRepairPipeline::repair(&raw, ToolCatalog::global());
        match out {
            RepairOutcome::Repaired(rec) => {
                assert_eq!(rec.name, "web_search");
                assert!(evs.is_empty());
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn functions_prefix_stripped() {
        let raw = RawToolCall {
            id: None,
            name: "functions.web_search".into(),
            arguments_json: "{}".into(),
        };
        let (out, _) = ToolRepairPipeline::repair(&raw, ToolCatalog::global());
        assert!(matches!(out, RepairOutcome::Repaired(r) if r.name == "web_search"));
    }

    #[test]
    fn reject_includes_available_tools() {
        let raw = RawToolCall {
            id: None,
            name: "totally_unknown_tool_xyz".into(),
            arguments_json: "{}".into(),
        };
        match ToolRepairPipeline::repair(&raw, ToolCatalog::global()).0 {
            RepairOutcome::Reject { error_json_for_model } => {
                let v: Value = serde_json::from_str(&error_json_for_model).unwrap();
                assert!(v.get("error").is_some());
                let tools = v.get("available_tools").and_then(|t| t.as_array()).unwrap();
                assert_eq!(tools.len(), ToolCatalog::global().all().len());
            }
            o => panic!("expected Reject {o:?}"),
        }
    }

    #[test]
    fn argument_json_with_code_fence() {
        let raw = RawToolCall {
            id: None,
            name: "web_search".into(),
            arguments_json: "```json\n{\"query\":\"x\"}\n```".into(),
        };
        let (out, _) = ToolRepairPipeline::repair(&raw, ToolCatalog::global());
        match out {
            RepairOutcome::Repaired(rec) => {
                let v: Value = serde_json::from_str(&rec.arguments_json).unwrap();
                assert_eq!(v["query"], "x");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn append_blocks_injects_is_complete_default() {
        let raw = RawToolCall {
            id: None,
            name: "append_blocks".into(),
            arguments_json: r#"{"artifactId":"a","blocks":[]}"#.into(),
        };
        let (out, _) = ToolRepairPipeline::repair(&raw, ToolCatalog::global());
        match out {
            RepairOutcome::Repaired(rec) => {
                let v: Value = serde_json::from_str(&rec.arguments_json).unwrap();
                assert_eq!(v["isComplete"], false);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn fuzzy_score_below_cutoff_rejects() {
        let cat = ToolCatalog::global();
        let needle = "zzzzz_no_real_match_yyyyy";
        let (_, score) = best_catalog_match(needle, cat).unwrap();
        assert!(
            score < FUZZY_AUTO_ACCEPT,
            "needle should score below cutoff, got {score}"
        );
        let raw = RawToolCall {
            id: None,
            name: needle.into(),
            arguments_json: "{}".into(),
        };
        assert!(matches!(
            ToolRepairPipeline::repair(&raw, cat).0,
            RepairOutcome::Reject { .. }
        ));
    }
}
