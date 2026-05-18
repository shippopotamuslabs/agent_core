//! Schema-aware coercion modelled on Forge's `schema_coercion.rs`.

use crate::catalog::ToolSchemaHint;
use crate::tool_parse::json_repair::repair_json;
use serde_json::Value;

/// Coerce a JSON value in-place to match the shape implied by a tool's
/// `ToolSchemaHint`, returning the coerced value.
pub fn coerce_to_schema(value: &mut Value, hint: &ToolSchemaHint) -> Value {
    match value {
        Value::Object(map) => {
            for field in &hint.required_fields {
                if let Some(field_value) = map.get_mut(&field.name) {
                    let coerced = coerce_field(field_value.clone(), &field.ty);
                    *field_value = coerced;
                }
            }
            for (name, default_value) in &hint.defaults {
                map.entry(name.clone()).or_insert_with(|| default_value.clone());
            }
            value.clone()
        }
        other => {
            let ty = hint
                .required_fields
                .first()
                .map(|field| field.ty.as_str())
                .unwrap_or("string");
            coerce_field(other.clone(), ty)
        }
    }
}

/// Attempt to parse a string as a JSON value (object, array, number, bool, null).
pub fn try_parse_string_as_value(s: &str) -> Option<Value> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(trimmed)
        .ok()
        .or_else(|| repair_json(trimmed).ok())
}

/// Find the first `[` in `s` and try progressively shorter suffixes from the
/// last valid char boundary to find a parseable array.
pub fn extract_array_from_string(s: &str) -> Option<Value> {
    let start = s.find('[')?;
    let slice = &s[start..];
    for end in slice
        .char_indices()
        .map(|(idx, _)| idx)
        .chain(std::iter::once(slice.len()))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        if end == 0 || !slice.is_char_boundary(end) {
            continue;
        }
        let candidate = &slice[..end];
        if let Ok(Value::Array(items)) = repair_json(candidate) {
            return Some(Value::Array(items));
        }
    }
    None
}

fn coerce_field(value: Value, ty: &str) -> Value {
    let nullable = ty.ends_with('?');
    let normalized_ty = ty.trim_end_matches('?');
    match value {
        Value::String(s) if nullable && s.trim().is_empty() => Value::Null,
        Value::String(s) => coerce_string(s, normalized_ty),
        Value::Array(items) if normalized_ty == "array" => {
            Value::Array(items.into_iter().map(coerce_array_item).collect())
        }
        other => other,
    }
}

fn coerce_string(s: String, ty: &str) -> Value {
    let trimmed = s.trim();
    match ty {
        "boolean" | "bool" => match trimmed.to_ascii_lowercase().as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::String(s),
        },
        "integer" | "int" => trimmed
            .parse::<i64>()
            .map(|number| Value::Number(number.into()))
            .unwrap_or(Value::String(s)),
        "number" | "float" => trimmed
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::String(s)),
        "null" if trimmed.eq_ignore_ascii_case("null") => Value::Null,
        "array" => coerce_string_to_array(trimmed).unwrap_or_else(|| Value::String(s)),
        "object" => coerce_string_to_object(trimmed).unwrap_or_else(|| Value::String(s)),
        _ if trimmed.eq_ignore_ascii_case("null") => Value::Null,
        _ => Value::String(s),
    }
}

fn coerce_string_to_array(s: &str) -> Option<Value> {
    unwrap_stringified_json(s).and_then(|value| match value {
        Value::Array(items) => Some(Value::Array(items.into_iter().map(coerce_array_item).collect())),
        Value::String(inner) if inner.starts_with("http://") || inner.starts_with("https://") => {
            Some(Value::Array(vec![Value::String(inner.trim_start_matches('[').to_string())]))
        }
        Value::String(inner) => extract_array_from_string(&inner),
        other => extract_array_from_string(&other.to_string()),
    })
}

fn coerce_string_to_object(s: &str) -> Option<Value> {
    unwrap_stringified_json(s).and_then(|value| match value {
        Value::Object(_) => Some(value),
        Value::String(inner) => repair_json(&inner).ok().filter(Value::is_object),
        _ => None,
    })
}

fn unwrap_stringified_json(s: &str) -> Option<Value> {
    let mut current = try_parse_string_as_value(s)?;
    for _ in 0..4 {
        let Value::String(inner) = current.clone() else {
            return Some(current);
        };
        let Some(next) = try_parse_string_as_value(&inner) else {
            return Some(Value::String(inner));
        };
        current = next;
    }
    Some(current)
}

fn coerce_array_item(value: Value) -> Value {
    match value {
        Value::String(s) => {
            let trimmed = s.trim().trim_start_matches('[').to_string();
            try_parse_string_as_value(&trimmed).unwrap_or(Value::String(trimmed))
        }
        other => other,
    }
}
