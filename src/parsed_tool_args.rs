//! Single-parse typed views over tool arguments/result JSON payloads (ENC-288).
//!
//! `ToolCallRecord.arguments_json`, `ToolResultRecord.result_json`, and
//! `RawToolCall.arguments_json` stay plain strings at the FFI/provider
//! boundary, but engine hot paths used to re-parse those strings with
//! `serde_json::from_str` in every consumer (`guardrails`,
//! `artifact_finalize`, compaction). These newtypes parse **once** at ingress
//! and carry the parsed [`serde_json::Value`] alongside the cached serialized
//! string; consumers use the typed accessors instead of re-parsing.

use serde_json::Value;

/// Parsed tool-call arguments: validated once, carried with the cached string.
#[derive(Debug, Clone)]
pub struct ParsedToolArgs {
    value: Value,
    json: String,
}

impl ParsedToolArgs {
    /// Ingress parse. Unparseable payloads map to `Value::Null` (matching the
    /// previous `from_str(...).ok()?` leniency in artifact consumers); the
    /// original string is still cached for the FFI/provider string views.
    pub fn parse(arguments_json: &str) -> Self {
        let value: Value = serde_json::from_str(arguments_json).unwrap_or(Value::Null);
        Self {
            value,
            json: arguments_json.to_string(),
        }
    }

    /// Build from an already-parsed value (e.g. canonicalize ingress);
    /// the cached string is serialized once here.
    pub fn from_value(value: Value) -> Self {
        let json = serde_json::to_string(&value).unwrap_or_else(|_| "null".into());
        Self { value, json }
    }

    /// Typed read of the parsed payload.
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Cached serialized string for FFI/provider boundary views.
    pub fn json(&self) -> &str {
        &self.json
    }

    /// Raw field read without re-parsing.
    pub fn get(&self, field: &str) -> Option<&Value> {
        self.value.get(field)
    }

    /// Non-empty string field read without re-parsing.
    pub fn get_str(&self, field: &str) -> Option<String> {
        self.value
            .get(field)?
            .as_str()
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
    }
}

impl Default for ParsedToolArgs {
    fn default() -> Self {
        Self {
            value: Value::Null,
            json: "null".to_string(),
        }
    }
}

/// Parsed tool-result payload: validated once, carried with the cached string.
#[derive(Debug, Clone)]
pub struct ParsedToolResult {
    value: Value,
    json: String,
}

impl ParsedToolResult {
    /// Ingress parse. Unparseable payloads map to `Value::Null` (never a soft
    /// failure, matching the previous `let Ok(...) else { return false }`
    /// behavior); the original string is still cached for string views.
    pub fn parse(result_json: &str) -> Self {
        let value: Value = serde_json::from_str(result_json).unwrap_or(Value::Null);
        Self {
            value,
            json: result_json.to_string(),
        }
    }

    /// Build from an already-parsed value; the cached string is serialized once.
    pub fn from_value(value: Value) -> Self {
        let json = serde_json::to_string(&value).unwrap_or_else(|_| "null".into());
        Self { value, json }
    }

    /// Typed read of the parsed payload.
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Cached serialized string for FFI/provider boundary views.
    pub fn json(&self) -> &str {
        &self.json
    }

    /// Raw field read without re-parsing.
    pub fn get(&self, field: &str) -> Option<&Value> {
        self.value.get(field)
    }

    /// Non-empty string field read without re-parsing.
    pub fn get_str(&self, field: &str) -> Option<String> {
        self.value
            .get(field)?
            .as_str()
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
    }

    /// JSON `true`-ish coercion (`bool` / `"true"` / `"1"` / `"yes"` / `1`),
    /// matching `artifact_finalize` completion-field semantics.
    pub fn get_bool_coerced(&self, field: &str) -> Option<bool> {
        match self.value.get(field)? {
            Value::Bool(b) => Some(*b),
            Value::String(s) => {
                Some(s.eq_ignore_ascii_case("true") || s == "1" || s.eq_ignore_ascii_case("yes"))
            }
            Value::Number(n) => n.as_u64().map(|u| u == 1).or(Some(false)),
            _ => Some(false),
        }
    }

    /// Kotlin-compatible soft failure on the already-parsed payload: JSON
    /// object with a top-level `error` key, `status: "error"`, or `data: null`
    /// with no `results` array.
    pub fn is_soft_failure(&self) -> bool {
        is_soft_failure_value(&self.value)
    }
}

impl Default for ParsedToolResult {
    fn default() -> Self {
        Self {
            value: Value::Null,
            json: "null".to_string(),
        }
    }
}

/// Soft-failure predicate over an already-parsed result value (no re-parse).
pub fn is_soft_failure_value(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };

    if obj.contains_key("error") {
        return true;
    }

    if obj
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("error"))
    {
        return true;
    }

    obj.get("data").is_some_and(Value::is_null) && !obj.contains_key("results")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_caches_string_and_value() {
        let parsed = ParsedToolArgs::parse(r#"{"artifactId":"a"}"#);
        assert_eq!(parsed.json(), r#"{"artifactId":"a"}"#);
        assert_eq!(parsed.get_str("artifactId"), Some("a".into()));
        assert_eq!(parsed.get_str("missing"), None);
    }

    #[test]
    fn parse_invalid_maps_to_null_but_keeps_string() {
        let parsed = ParsedToolArgs::parse("{bogus");
        assert_eq!(parsed.value(), &Value::Null);
        assert_eq!(parsed.json(), "{bogus");
        assert_eq!(parsed.get_str("x"), None);
    }

    #[test]
    fn from_value_serializes_once() {
        let parsed = ParsedToolResult::from_value(json!({"status": "success"}));
        assert_eq!(parsed.get_str("status"), Some("success".into()));
        assert_eq!(
            serde_json::from_str::<Value>(parsed.json()).unwrap(),
            json!({"status": "success"})
        );
    }

    #[test]
    fn soft_failure_parity_with_legacy_shapes() {
        for (payload, expected) in [
            (r#"{"error":"x"}"#, true),
            (r#"{"status":"error"}"#, true),
            (r#"{"status":"ERROR"}"#, true),
            (r#"{"data":null}"#, true),
            (r#"{"results":[]}"#, false),
            (r#"{"data":null,"results":[]}"#, false),
            (r#"{"status":"success"}"#, false),
            ("{bogus", false),
        ] {
            assert_eq!(
                ParsedToolResult::parse(payload).is_soft_failure(),
                expected,
                "payload: {payload}"
            );
        }
    }

    #[test]
    fn bool_coercion_matches_artifact_finalize_semantics() {
        let parsed = ParsedToolResult::parse(
            r#"{"a":true,"b":"true","c":"1","d":"yes","e":1,"f":0,"g":"no","h":[]}"#,
        );
        assert_eq!(parsed.get_bool_coerced("a"), Some(true));
        assert_eq!(parsed.get_bool_coerced("b"), Some(true));
        assert_eq!(parsed.get_bool_coerced("c"), Some(true));
        assert_eq!(parsed.get_bool_coerced("d"), Some(true));
        assert_eq!(parsed.get_bool_coerced("e"), Some(true));
        assert_eq!(parsed.get_bool_coerced("f"), Some(false));
        assert_eq!(parsed.get_bool_coerced("g"), Some(false));
        assert_eq!(parsed.get_bool_coerced("h"), Some(false));
        assert_eq!(parsed.get_bool_coerced("missing"), None);
    }

    #[test]
    fn canonicalize_value_roundtrips_through_parsed_args() {
        let value = json!({"artifactId": "a", "blocks": [], "isComplete": false});
        let parsed = ParsedToolArgs::from_value(value.clone());
        assert_eq!(parsed.value(), &value);
        assert_eq!(parsed.get_str("artifactId"), Some("a".into()));
    }
}
