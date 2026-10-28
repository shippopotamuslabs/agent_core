//! Shared tool-call canonicalization API.
//!
//! Dependency direction (ENC-291): this module is the shared core —
//! [`crate::repair`] and [`crate::tool_parse`] delegate to it. It imports
//! only leaf modules ([`crate::json_repair`], [`crate::schema_coerce`],
//! [`crate::tool_name_match`]) plus [`crate::catalog`],
//! [`crate::tool_semantics`] et al, and never `repair`/`tool_parse`, so the
//! import graph among the three stays acyclic.

use serde_json::Value;

use crate::catalog::{ToolCatalog, ToolSchemaHint};
use crate::events::AgentEvent;
use crate::json_repair::repair_json;
use crate::message::RepairRecord;
use crate::schema_coerce::coerce_to_schema;
use crate::tool_name::ToolName;
use crate::tool_name_match::{best_catalog_match, FUZZY_AUTO_ACCEPT};
use crate::tool_semantics::{schema_hint_for_semantics, SpecialValueRepair, ToolSemanticsConfig};
use crate::turn_io::RawToolCall;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallCanonicalizeMode {
    ProviderIngress,
    ExecutionStrict,
}

#[derive(Debug, Clone)]
pub struct CanonicalizedToolCall {
    pub resolved_name: String,
    pub arguments_json: String,
    pub arguments_value: Value,
    pub repair_metadata: Option<RepairRecord>,
    pub argument_repairs: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum CanonicalizeToolCallOutcome {
    Ok(CanonicalizedToolCall),
    Reject {
        error_json_for_model: String,
    },
    Suggest {
        original: String,
        closest_name: String,
    },
}

#[derive(Debug, Clone)]
pub struct CanonicalizedArguments {
    pub arguments_json: String,
    pub value: Value,
    pub repaired: bool,
    pub repairs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalizeArgumentsError {
    pub message: String,
}

impl std::fmt::Display for CanonicalizeArgumentsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CanonicalizeArgumentsError {}

pub fn canonicalize_tool_call_with_semantics(
    raw: &RawToolCall,
    catalog: &ToolCatalog,
    semantics: &ToolSemanticsConfig,
    mode: ToolCallCanonicalizeMode,
) -> (CanonicalizeToolCallOutcome, Vec<AgentEvent>) {
    let mut events = Vec::new();
    let (resolved_name, repair_metadata) =
        match resolve_tool_name_with_semantics(&raw.name, catalog, semantics, mode) {
            ToolNameResolution::Resolved {
                name,
                repair_metadata,
                event,
            } => {
                if let Some(event) = event {
                    events.push(event);
                }
                (name, repair_metadata)
            }
            ToolNameResolution::Rejected { error_json } => {
                return (
                    CanonicalizeToolCallOutcome::Reject {
                        error_json_for_model: error_json,
                    },
                    events,
                );
            }
        };

    let arguments = match canonicalize_raw_arguments_with_semantics(
        &resolved_name,
        &raw.arguments_json,
        catalog,
        semantics,
        mode,
    ) {
        Ok(arguments) => arguments,
        Err(error) => {
            return (
                CanonicalizeToolCallOutcome::Reject {
                    error_json_for_model: reject_json(
                        &format!("Invalid JSON for tool arguments: {error}"),
                        catalog,
                    ),
                },
                events,
            );
        }
    };

    (
        CanonicalizeToolCallOutcome::Ok(CanonicalizedToolCall {
            resolved_name: resolved_name.as_str().to_string(),
            arguments_json: arguments.arguments_json,
            arguments_value: arguments.value,
            repair_metadata,
            argument_repairs: arguments.repairs,
        }),
        events,
    )
}

pub fn canonicalize_raw_arguments_with_semantics(
    tool_name: &ToolName,
    raw_arguments_json: &str,
    catalog: &ToolCatalog,
    semantics: &ToolSemanticsConfig,
    mode: ToolCallCanonicalizeMode,
) -> Result<CanonicalizedArguments, CanonicalizeArgumentsError> {
    let trimmed = strip_json_fences(raw_arguments_json);
    let parsed = repair_json(&trimmed).map_err(|error| CanonicalizeArgumentsError {
        message: error.to_string(),
    })?;
    let mut value = parsed;
    let mut repaired = false;
    let mut repairs = Vec::new();

    apply_semantic_aliases(
        tool_name,
        semantics,
        &mut value,
        &mut repaired,
        &mut repairs,
    );
    validate_semantic_stringified_arrays(tool_name, semantics, &value)?;

    if let Some(hint) = schema_hint_for_semantics(tool_name, semantics).or_else(|| {
        catalog
            .resolve(tool_name)
            .and_then(|spec| spec.schema_hint.clone())
    }) {
        let before = value.clone();
        value = coerce_to_schema(&mut value, &hint);
        if value != before {
            repaired = true;
            if !only_schema_defaults_added(&before, &value, &hint) {
                push_repair(&mut repairs, "coerced arguments to catalog schema");
            }
        }
    }

    apply_semantic_special_values(
        tool_name,
        semantics,
        &mut value,
        &mut repaired,
        &mut repairs,
    );
    if matches!(mode, ToolCallCanonicalizeMode::ExecutionStrict) {
        validate_required_types(tool_name, &value, catalog)?;
    }
    if matches!(mode, ToolCallCanonicalizeMode::ExecutionStrict) {
        validate_required_types_with_semantics(tool_name, &value, semantics)?;
    }
    let originally_valid = serde_json::from_str::<Value>(&trimmed).is_ok();
    if !originally_valid {
        repaired = true;
        push_repair(&mut repairs, "repaired invalid JSON argument payload");
    }
    let arguments_json = canonical_arguments_json_with_semantics(tool_name, &value, semantics);
    Ok(CanonicalizedArguments {
        arguments_json,
        value,
        repaired,
        repairs,
    })
}

fn only_schema_defaults_added(before: &Value, after: &Value, hint: &ToolSchemaHint) -> bool {
    let (Some(before_object), Some(after_object)) = (before.as_object(), after.as_object()) else {
        return false;
    };
    for (key, before_value) in before_object {
        if after_object.get(key) != Some(before_value) {
            return false;
        }
    }
    for (key, after_value) in after_object {
        if before_object.contains_key(key) {
            continue;
        }
        if hint.defaults.get(key) != Some(after_value) {
            return false;
        }
    }
    true
}

enum ToolNameResolution {
    Resolved {
        name: ToolName,
        repair_metadata: Option<RepairRecord>,
        event: Option<AgentEvent>,
    },
    Rejected {
        error_json: String,
    },
}

fn resolve_tool_name_with_semantics(
    raw: &str,
    catalog: &ToolCatalog,
    semantics: &ToolSemanticsConfig,
    mode: ToolCallCanonicalizeMode,
) -> ToolNameResolution {
    let stripped = strip_tool_prefixes(raw);
    let normalized = normalize_to_snake_case(&stripped);
    // The snake pipeline maps raw model output into the canonical identity
    // space; ToolName::new applies the identity (b)-fold. A blank result
    // (raw was punctuation/whitespace only) is not a name at all.
    let name = ToolName::new(&normalized).ok();

    if let Some(name) = name.as_ref() {
        if let Some(spec) = catalog.resolve(name) {
            return ToolNameResolution::Resolved {
                name: spec.name.clone(),
                repair_metadata: None,
                event: None,
            };
        }
    }

    if catalog.all().is_empty() || matches!(mode, ToolCallCanonicalizeMode::ProviderIngress) {
        if let Some(name) = name {
            return ToolNameResolution::Resolved {
                name,
                repair_metadata: None,
                event: None,
            };
        }
    }

    if let Some((best, score)) = best_catalog_match(&normalized, catalog) {
        if score >= FUZZY_AUTO_ACCEPT {
            return ToolNameResolution::Resolved {
                name: best.clone(),
                repair_metadata: Some(RepairRecord {
                    original_name: raw.trim().to_string(),
                    applied_aliases: vec![],
                    confidence: score as f32,
                }),
                event: Some(AgentEvent::ToolRepaired {
                    original_name: raw.to_string(),
                    resolved_name: best.as_str().to_string(),
                    confidence: score as f32,
                }),
            };
        }
        return ToolNameResolution::Rejected {
            error_json: reject_json(
                &format!(
                    "Unknown tool '{}' (best match '{}' score {:.2} < {:.2})",
                    raw,
                    best.as_str(),
                    score,
                    FUZZY_AUTO_ACCEPT
                ),
                catalog,
            ),
        };
    }

    let available = semantics
        .catalog
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    ToolNameResolution::Rejected {
        error_json: serde_json::json!({
            "error": format!("Unknown tool '{raw}' after normalization '{normalized}'"),
            "available_tools": available,
        })
        .to_string(),
    }
}

fn strip_tool_prefixes(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    loop {
        if let Some(rest) = s.strip_prefix("functions.") {
            s = rest.to_string();
        } else if let Some(rest) = s.strip_prefix("tools.") {
            s = rest.to_string();
        } else {
            return s;
        }
    }
}

fn normalize_to_snake_case(input: &str) -> String {
    let mut result = String::new();
    let chars: Vec<char> = input.trim().chars().collect();
    for (index, &ch) in chars.iter().enumerate() {
        if ch == '-' || ch == ' ' {
            if !result.ends_with('_') && !result.is_empty() {
                result.push('_');
            }
            continue;
        }
        if ch.is_uppercase() {
            let prev_lower = index > 0 && chars[index - 1].is_lowercase();
            let next_lower = index + 1 < chars.len() && chars[index + 1].is_lowercase();
            if (prev_lower || (index > 0 && chars[index - 1].is_ascii_uppercase() && next_lower))
                && !result.ends_with('_')
                && !result.is_empty()
            {
                result.push('_');
            }
            for lower in ch.to_lowercase() {
                result.push(lower);
            }
        } else if ch.is_alphanumeric() || ch == '_' {
            result.push(ch);
        }
    }
    while result.contains("__") {
        result = result.replace("__", "_");
    }
    result.trim_matches('_').to_string()
}

fn move_alias(
    object: &mut serde_json::Map<String, Value>,
    alias: &str,
    canonical: &str,
    repaired: &mut bool,
    repairs: &mut Vec<String>,
) {
    let Some(value) = object.remove(alias) else {
        return;
    };
    object.entry(canonical.to_string()).or_insert(value);
    *repaired = true;
    push_repair(
        repairs,
        &format!("normalized parameter alias {alias} to {canonical}"),
    );
}

fn semantic_rule<'a>(
    tool: &ToolName,
    semantics: &'a ToolSemanticsConfig,
) -> Option<&'a crate::tool_semantics::ToolArgumentRepairRule> {
    // Both sides are canonical identities (rule names validated at the
    // boundary, inputs canonicalized by the repair pipeline), so this is
    // plain equality — no folding (ENC-299).
    semantics
        .argument_repairs
        .iter()
        .find(|rule| rule.tool_name == *tool)
}

fn apply_semantic_aliases(
    tool: &ToolName,
    semantics: &ToolSemanticsConfig,
    value: &mut Value,
    repaired: &mut bool,
    repairs: &mut Vec<String>,
) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let Some(rule) = semantic_rule(tool, semantics) else {
        return;
    };
    for alias in &rule.aliases {
        move_alias(object, &alias.alias, &alias.canonical, repaired, repairs);
    }
}

fn apply_semantic_special_values(
    tool: &ToolName,
    semantics: &ToolSemanticsConfig,
    value: &mut Value,
    repaired: &mut bool,
    repairs: &mut Vec<String>,
) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let Some(rule) = semantic_rule(tool, semantics) else {
        return;
    };
    for special in &rule.special_values {
        match special {
            SpecialValueRepair::ScalarToArray { source, target } => {
                if let Some(query) = object.remove(source) {
                    object.insert(target.clone(), Value::Array(vec![query]));
                    *repaired = true;
                    push_repair(repairs, &format!("normalized {source} to {target} array"));
                }
            }
            SpecialValueRepair::StripLeadingBracketFromArrayStrings { field } => {
                if let Some(Value::Array(items)) = object.get_mut(field) {
                    for item in items {
                        if let Some(raw) = item.as_str() {
                            let stripped = raw.trim().trim_start_matches('[').to_string();
                            if stripped != raw {
                                *item = Value::String(stripped);
                                *repaired = true;
                                push_repair(
                                    repairs,
                                    "stripped leading bracket from bracketed URL string",
                                );
                            }
                        }
                    }
                }
            }
            SpecialValueRepair::ValidateStringifiedJsonArray { .. } => {}
        }
    }
}

fn validate_semantic_stringified_arrays(
    tool: &ToolName,
    semantics: &ToolSemanticsConfig,
    value: &Value,
) -> Result<(), CanonicalizeArgumentsError> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let Some(rule) = semantic_rule(tool, semantics) else {
        return Ok(());
    };
    for special in &rule.special_values {
        let SpecialValueRepair::ValidateStringifiedJsonArray { field } = special else {
            continue;
        };
        let Some(Value::String(raw)) = object.get(field) else {
            continue;
        };
        match serde_json::from_str::<Value>(raw.trim()) {
            Ok(Value::Array(_)) => {}
            Ok(_) => {
                return Err(CanonicalizeArgumentsError {
                    message: format!("{field} string must contain a JSON array"),
                });
            }
            Err(error) => {
                return Err(CanonicalizeArgumentsError {
                    message: format!("{field} string contains invalid JSON: {error}"),
                });
            }
        }
    }
    Ok(())
}

fn validate_required_types(
    tool_name: &ToolName,
    value: &Value,
    catalog: &ToolCatalog,
) -> Result<(), CanonicalizeArgumentsError> {
    validate_required_types_with_hint(tool_name, value, schema_hint_for(tool_name, catalog))
}

fn validate_required_types_with_semantics(
    tool_name: &ToolName,
    value: &Value,
    semantics: &ToolSemanticsConfig,
) -> Result<(), CanonicalizeArgumentsError> {
    validate_required_types_with_hint(
        tool_name,
        value,
        schema_hint_for_semantics(tool_name, semantics),
    )
}

/// Shared body for the required-field type validators; only the hint source
/// (catalog vs. semantics) differs between callers.
fn validate_required_types_with_hint(
    tool_name: &ToolName,
    value: &Value,
    hint: Option<ToolSchemaHint>,
) -> Result<(), CanonicalizeArgumentsError> {
    let Some(hint) = hint else {
        return Ok(());
    };
    let Some(object) = value.as_object() else {
        return Ok(());
    };

    for field in &hint.required_fields {
        let Some(field_value) = object.get(&field.name) else {
            if !hint.defaults.contains_key(&field.name) {
                return Err(CanonicalizeArgumentsError {
                    message: format!(
                        "Missing required field '{}' for tool '{}'",
                        field.name, tool_name
                    ),
                });
            }
            continue;
        };
        if !matches_schema_type(field_value, &field.ty) {
            return Err(CanonicalizeArgumentsError {
                message: format!(
                    "Field '{}' for tool '{}' could not be coerced to {}",
                    field.name, tool_name, field.ty
                ),
            });
        }
    }
    Ok(())
}

fn schema_hint_for(tool_name: &ToolName, catalog: &ToolCatalog) -> Option<ToolSchemaHint> {
    catalog
        .resolve(tool_name)
        .and_then(|spec| spec.schema_hint.clone())
}

fn canonical_arguments_json_with_semantics(
    tool: &ToolName,
    value: &Value,
    semantics: &ToolSemanticsConfig,
) -> String {
    let Some(shape) = semantics
        .canonical_shapes
        .iter()
        .find(|shape| shape.tool_name == *tool)
    else {
        return value.to_string();
    };
    let Some(object) = value.as_object() else {
        return value.to_string();
    };

    let mut out = serde_json::Map::new();
    for field in &shape.ordered_fields {
        if let Some(field_value) = object.get(field) {
            out.insert(field.clone(), field_value.clone());
        }
    }
    for (field, field_value) in object {
        if !out.contains_key(field) {
            out.insert(field.clone(), field_value.clone());
        }
    }
    Value::Object(out).to_string()
}

fn matches_schema_type(value: &Value, ty: &str) -> bool {
    match ty.trim_end_matches('?').to_ascii_lowercase().as_str() {
        "array" => value.is_array(),
        "object" => value.is_object(),
        "boolean" | "bool" => value.is_boolean(),
        "integer" | "int" => value.as_i64().is_some(),
        "number" | "float" | "double" => value.as_f64().is_some(),
        "string" => value.is_string(),
        _ => true,
    }
}

fn reject_json(message: &str, catalog: &ToolCatalog) -> String {
    let tools: Vec<&str> = catalog
        .all()
        .iter()
        .map(|spec| spec.name.as_str())
        .collect();
    serde_json::json!({
        "error": message,
        "available_tools": tools,
    })
    .to_string()
}

fn strip_json_fences(s: &str) -> String {
    let mut trimmed = s.trim().to_string();
    while trimmed.ends_with("```") {
        trimmed = trimmed[..trimmed.len() - 3].trim().to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("```json") {
        return strip_json_fences(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        return strip_json_fences(rest);
    }
    if let Some(index) = trimmed.find(['{', '[']) {
        return trimmed[index..].trim().to_string();
    }
    trimmed
}

fn push_repair(repairs: &mut Vec<String>, repair: &str) {
    if !repairs.iter().any(|existing| existing == repair) {
        repairs.push(repair.to_string());
    }
}
