use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::turn_io::RawToolCall;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    System,
    User,
    Assistant,
    Tool,
    CompactSummary,
}

/// Typed view over `AgentMessage.metadata`.
///
/// Known keys are materialized as typed fields; every other host key is
/// preserved verbatim in the flattened `extra` catch-all so unknown host
/// dialects round-trip byte-compatibly. Serialization emits keys in sorted
/// order (serde_json's map ordering), exactly as plain `Value` objects did,
/// so persisted sessions keep their shape.
///
/// Deserialization is lenient per field (see `absorb`): a known key whose
/// value does not fit its typed field is preserved verbatim in `extra`
/// instead of discarding the whole metadata object, and a known key never
/// lives in both a typed field and `extra` at once, so serialization never
/// emits a key twice.
#[derive(Debug, Clone, Serialize)]
pub struct MessageMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    raw_tool_calls: Vec<RawToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    superseded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rewrite_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compaction_tier: Option<String>,
    /// Unknown host keys, round-tripped verbatim.
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

impl<'de> Deserialize<'de> for MessageMeta {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::from_value(serde_json::Value::deserialize(
            deserializer,
        )?))
    }
}

impl Default for MessageMeta {
    fn default() -> Self {
        Self {
            call_id: None,
            tool_name: None,
            raw_tool_calls: Vec::new(),
            superseded: None,
            rewrite_of: None,
            compaction_tier: None,
            extra: serde_json::Map::new(),
        }
    }
}

impl MessageMeta {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when no typed field is set and no unknown host keys are stored.
    /// Empty metadata serializes as JSON `null`, matching the pre-typed default.
    pub fn is_empty(&self) -> bool {
        self.call_id.is_none()
            && self.tool_name.is_none()
            && self.raw_tool_calls.is_empty()
            && self.superseded.is_none()
            && self.rewrite_of.is_none()
            && self.compaction_tier.is_none()
            && self.extra.is_empty()
    }

    pub fn call_id(&self) -> Option<&str> {
        self.call_id.as_deref()
    }

    pub fn with_call_id(mut self, call_id: impl Into<String>) -> Self {
        self.set_call_id(call_id);
        self
    }

    pub fn set_call_id(&mut self, call_id: impl Into<String>) {
        self.call_id = Some(call_id.into());
        self.extra.remove("call_id");
    }

    /// Canonical tool name. The legacy `name` dialect key is normalized
    /// into this field on the read path (see `absorb`), so reads never
    /// consult the catch-all.
    pub fn tool_name(&self) -> Option<&str> {
        self.tool_name.as_deref()
    }

    pub fn with_tool_name(mut self, tool_name: impl Into<String>) -> Self {
        self.set_tool_name(tool_name);
        self
    }

    pub fn set_tool_name(&mut self, tool_name: impl Into<String>) {
        self.tool_name = Some(tool_name.into());
        self.extra.remove("tool_name");
    }

    pub fn raw_tool_calls(&self) -> &[RawToolCall] {
        &self.raw_tool_calls
    }

    pub fn set_raw_tool_calls(&mut self, calls: Vec<RawToolCall>) {
        self.raw_tool_calls = calls;
        self.extra.remove("raw_tool_calls");
    }

    pub fn with_raw_tool_calls(mut self, calls: Vec<RawToolCall>) -> Self {
        self.set_raw_tool_calls(calls);
        self
    }

    pub fn is_superseded(&self) -> bool {
        self.superseded.unwrap_or(false)
    }

    pub fn mark_superseded(&mut self) {
        self.superseded = Some(true);
        self.extra.remove("superseded");
    }

    pub fn rewrite_of(&self) -> Option<&str> {
        self.rewrite_of.as_deref()
    }

    pub fn with_rewrite_of(mut self, rewrite_of: impl Into<String>) -> Self {
        self.rewrite_of = Some(rewrite_of.into());
        self.extra.remove("rewrite_of");
        self
    }

    pub fn compaction_tier(&self) -> Option<&str> {
        self.compaction_tier.as_deref()
    }

    pub fn with_compaction_tier(mut self, tier: impl Into<String>) -> Self {
        self.set_compaction_tier(tier);
        self
    }

    pub fn set_compaction_tier(&mut self, tier: impl Into<String>) {
        self.compaction_tier = Some(tier.into());
        self.extra.remove("compaction_tier");
    }

    /// Read an unknown host key from the catch-all.
    pub fn extra(&self, key: &str) -> Option<&serde_json::Value> {
        self.extra.get(key)
    }

    /// Store a host key (builder). Known keys route to their typed field
    /// when the value fits (see `absorb`); anything else is stored verbatim
    /// in the catch-all.
    pub fn with_extra(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.set_extra(key, value);
        self
    }

    /// Store a host key. Routes through `absorb`, so a known key can never
    /// shadow its typed field inside the catch-all.
    pub fn set_extra(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.absorb(key.into(), value);
    }

    /// Single entry point for host-provided keys. A known key whose value
    /// fits its typed field lands there; a known key whose value does not
    /// fit is preserved verbatim in the catch-all (never silently dropped,
    /// and never duplicated alongside an already-set typed field). The
    /// legacy `name` dialect key normalizes into `tool_name`. Unknown keys
    /// are stored verbatim.
    fn absorb(&mut self, key: String, value: serde_json::Value) {
        match key.as_str() {
            "call_id" => match value {
                serde_json::Value::String(s) => self.set_call_id(s),
                _ if self.call_id.is_none() => {
                    self.extra.insert(key, value);
                }
                _ => {}
            },
            "tool_name" => match value {
                serde_json::Value::String(s) => self.set_tool_name(s),
                _ if self.tool_name.is_none() => {
                    self.extra.insert(key, value);
                }
                _ => {}
            },
            "raw_tool_calls" => match serde_json::from_value::<Vec<RawToolCall>>(value.clone()) {
                Ok(calls) => self.set_raw_tool_calls(calls),
                Err(_) if self.raw_tool_calls.is_empty() => {
                    self.extra.insert(key, value);
                }
                Err(_) => {}
            },
            "superseded" => match value {
                serde_json::Value::Bool(b) => {
                    self.superseded = Some(b);
                    self.extra.remove("superseded");
                }
                _ if self.superseded.is_none() => {
                    self.extra.insert(key, value);
                }
                _ => {}
            },
            "rewrite_of" => match value {
                serde_json::Value::String(s) => {
                    self.rewrite_of = Some(s);
                    self.extra.remove("rewrite_of");
                }
                _ if self.rewrite_of.is_none() => {
                    self.extra.insert(key, value);
                }
                _ => {}
            },
            "compaction_tier" => match value {
                serde_json::Value::String(s) => self.set_compaction_tier(s),
                _ if self.compaction_tier.is_none() => {
                    self.extra.insert(key, value);
                }
                _ => {}
            },
            "name" if self.tool_name.is_none() => match value {
                serde_json::Value::String(s) => self.set_tool_name(s),
                _ => {
                    self.extra.insert(key, value);
                }
            },
            _ => {
                self.extra.insert(key, value);
            }
        }
    }

    // -- Well-known host keys: stored in the catch-all, accessed typed. --

    /// True when this message is the synthetic agent-tool-budget row.
    pub fn agent_tool_budget(&self) -> bool {
        self.extra
            .get("agent_tool_budget")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Mark this message as the synthetic agent-tool-budget row.
    pub fn with_agent_tool_budget(mut self) -> Self {
        self.set_extra("agent_tool_budget", serde_json::json!(true));
        self
    }

    /// True when the tool result payload was truncated.
    pub fn truncated(&self) -> bool {
        self.extra
            .get("truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Record whether the tool result payload was truncated.
    pub fn with_truncated(mut self, truncated: bool) -> Self {
        self.set_extra("truncated", serde_json::json!(truncated));
        self
    }

    /// Envelope ingest outcome recorded on an assistant message
    /// (e.g. "failed", "tools_blocked", "reified").
    pub fn envelope_ingest(&self) -> Option<&str> {
        self.extra.get("envelope_ingest").and_then(|v| v.as_str())
    }

    /// Record the envelope ingest outcome on this message.
    pub fn with_envelope_ingest(mut self, state: impl Into<String>) -> Self {
        self.set_extra("envelope_ingest", serde_json::json!(state.into()));
        self
    }

    /// Mark that envelope parsing failed for this message.
    pub fn with_envelope_parse_failure(mut self) -> Self {
        self.set_extra("envelope_parse_failure", serde_json::json!(true));
        self
    }

    /// Mark that this message is the tools-blocked fallback notice.
    pub fn with_tools_blocked_fallback(mut self) -> Self {
        self.set_extra("tools_blocked_fallback", serde_json::json!(true));
        self
    }

    /// True when this message is the tools-blocked nudge row.
    pub fn tools_blocked_nudge(&self) -> bool {
        self.extra
            .get("tools_blocked_nudge")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Mark this message as the tools-blocked nudge row.
    pub fn with_tools_blocked_nudge(mut self) -> Self {
        self.set_extra("tools_blocked_nudge", serde_json::json!(true));
        self
    }

    /// Provider reasoning content captured at model-response ingest.
    pub fn provider_reasoning_content(&self) -> Option<&str> {
        self.extra
            .get("provider_reasoning")?
            .get("reasoning_content")?
            .as_str()
    }

    /// Build from an untyped JSON value. `null` (and missing) maps to empty;
    /// objects split known keys into typed fields via `absorb`; anything
    /// else maps to empty (only objects and null were ever written by the
    /// engine).
    pub fn from_value(value: serde_json::Value) -> Self {
        let mut meta = Self::default();
        if let serde_json::Value::Object(map) = value {
            for (key, val) in map {
                meta.absorb(key, val);
            }
        }
        meta
    }

    /// Parse a JSON-encoded metadata blob (e.g. across FFI). Unparseable input
    /// maps to empty, matching the previous `unwrap_or(Value::Null)` behavior.
    pub fn from_json_str(s: &str) -> Self {
        serde_json::from_str(s)
            .map(Self::from_value)
            .unwrap_or_default()
    }

    /// Encode for JSON persistence / FFI. Empty metadata encodes as `null`,
    /// matching the pre-typed default.
    pub fn to_json_string(&self) -> String {
        if self.is_empty() {
            "null".to_string()
        } else {
            serde_json::to_string(self).unwrap_or_else(|_| "null".into())
        }
    }
}

fn deserialize_metadata<'de, D>(deserializer: D) -> Result<MessageMeta, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(MessageMeta::from_value(value))
}

fn serialize_metadata<S>(meta: &MessageMeta, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if meta.is_empty() {
        serializer.serialize_none()
    } else {
        meta.serialize(serializer)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: String,
    pub role: AgentRole,
    pub content: String,
    /// Opaque host-provided metadata (e.g. vision asset refs, turn tags).
    /// Defaults to JSON null; no required keys.
    ///
    /// ## Compaction Metadata Conventions:
    /// - `rewrite_of`: ID of the original message that this message replaces/rewrites.
    /// - `compaction_tier`: Name of the compaction tier that performed the rewrite (e.g. "micro").
    /// - `superseded`: Boolean flag set to `true` on the original message after it is rewritten.
    #[serde(
        default,
        deserialize_with = "deserialize_metadata",
        serialize_with = "serialize_metadata"
    )]
    pub metadata: MessageMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairRecord {
    pub original_name: String,
    pub applied_aliases: Vec<String>,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub call_id: String,
    pub name: String,
    pub arguments_json: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_metadata: Option<ToolAttemptMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_metadata: Option<RepairRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAttemptMetadata {
    pub attempt_index: u32,
    pub attempts_remaining: Option<u32>,
    pub timeout_ms: Option<u64>,
    pub policy_denied: bool,
    pub requires_prior_read: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultMetadata {
    pub duration_ms: Option<u64>,
    pub timed_out: bool,
    pub timeout_ms: Option<u64>,
    pub policy_denied: bool,
    pub prior_read_satisfied: bool,
    pub delegated_agent_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultRecord {
    pub call_id: String,
    pub result_json: String,
    pub truncated: bool,
    pub soft_failure: bool,
    #[serde(default)]
    pub activated_tool_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_metadata: Option<ToolResultMetadata>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_message_roundtrip() {
        let mut extra = MessageMeta::new();
        extra.set_extra("tag", json!("vision"));
        let msg = AgentMessage {
            id: "msg-1".into(),
            role: AgentRole::User,
            content: "hello".into(),
            metadata: extra,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: AgentMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, msg.id);
        assert_eq!(back.role, msg.role);
        assert_eq!(back.content, msg.content);
        assert_eq!(back.metadata.extra("tag"), Some(&json!("vision")));
    }

    #[test]
    fn agent_message_metadata_defaults_null() {
        let json = r#"{"id":"x","role":"user","content":"hi"}"#;
        let msg: AgentMessage = serde_json::from_str(json).unwrap();
        assert!(msg.metadata.is_empty());
        assert_eq!(
            serde_json::to_string(&msg).unwrap().matches("null").count(),
            1
        );
    }

    #[test]
    fn agent_message_explicit_null_roundtrips_null() {
        let json = r#"{"id":"x","role":"user","content":"hi","metadata":null}"#;
        let msg: AgentMessage = serde_json::from_str(json).unwrap();
        assert!(msg.metadata.is_empty());
        let back = serde_json::to_value(&msg).unwrap();
        assert_eq!(back["metadata"], serde_json::Value::Null);
    }

    #[test]
    fn message_meta_known_keys_roundtrip_verbatim() {
        let raw = json!({
            "call_id": "c1",
            "compaction_tier": "micro",
            "rewrite_of": "m0",
            "superseded": true,
            "tool_name": "web_search",
            "raw_tool_calls": [{"id": "c1", "name": "web_search", "arguments_json": "{}"}],
        });
        let meta = MessageMeta::from_value(raw.clone());
        assert_eq!(meta.call_id(), Some("c1"));
        assert_eq!(meta.tool_name(), Some("web_search"));
        assert_eq!(meta.raw_tool_calls().len(), 1);
        assert!(meta.is_superseded());
        assert_eq!(meta.rewrite_of(), Some("m0"));
        assert_eq!(meta.compaction_tier(), Some("micro"));
        assert_eq!(serde_json::to_value(&meta).unwrap(), raw);
    }

    #[test]
    fn message_meta_unknown_keys_roundtrip_verbatim() {
        let raw = json!({
            "tool_name": "web_search",
            "truncated": true,
            "vision_ref": "asset-1",
        });
        let meta = MessageMeta::from_value(raw.clone());
        assert_eq!(meta.tool_name(), Some("web_search"));
        assert_eq!(meta.extra("truncated"), Some(&json!(true)));
        assert_eq!(serde_json::to_value(&meta).unwrap(), raw);
    }

    #[test]
    fn message_meta_legacy_name_dialect_resolves() {
        let meta = MessageMeta::from_value(json!({ "name": "web_search" }));
        assert_eq!(meta.tool_name(), Some("web_search"));
        // The legacy key is normalized into the canonical field on read.
        assert_eq!(
            serde_json::to_value(&meta).unwrap(),
            json!({ "tool_name": "web_search" })
        );
    }

    #[test]
    fn message_meta_malformed_known_field_preserves_the_rest() {
        let meta = MessageMeta::from_value(json!({
            "call_id": 42,
            "superseded": true,
            "custom": "x",
        }));
        // One malformed field must not discard the whole metadata object.
        assert!(meta.is_superseded());
        assert_eq!(meta.extra("custom"), Some(&json!("x")));
        // The malformed value itself is preserved verbatim, not dropped.
        assert_eq!(meta.call_id(), None);
        assert_eq!(meta.extra("call_id"), Some(&json!(42)));
    }

    #[test]
    fn message_meta_malformed_raw_tool_calls_preserved() {
        let meta = MessageMeta::from_value(json!({
            "raw_tool_calls": [{ "bogus": 1 }],
            "tool_name": "web_search",
        }));
        assert_eq!(meta.tool_name(), Some("web_search"));
        assert!(meta.raw_tool_calls().is_empty());
        assert_eq!(meta.extra("raw_tool_calls"), Some(&json!([{ "bogus": 1 }])));
    }

    #[test]
    fn message_meta_set_extra_routes_known_keys_to_typed_fields() {
        let mut meta = MessageMeta::new();
        meta.set_extra("call_id", json!("c9"));
        assert_eq!(meta.call_id(), Some("c9"));
        assert_eq!(meta.extra("call_id"), None);
        assert_eq!(
            serde_json::to_value(&meta).unwrap(),
            json!({ "call_id": "c9" })
        );
    }

    #[test]
    fn message_meta_typed_setter_clears_preserved_shadow() {
        let mut meta = MessageMeta::from_value(json!({ "call_id": 42 }));
        assert_eq!(meta.extra("call_id"), Some(&json!(42)));
        meta.set_call_id("c1");
        assert_eq!(meta.extra("call_id"), None);
        assert_eq!(
            serde_json::to_value(&meta).unwrap(),
            json!({ "call_id": "c1" })
        );
    }

    #[test]
    fn message_meta_to_json_string_empty_is_null() {
        assert_eq!(MessageMeta::new().to_json_string(), "null");
        assert_eq!(MessageMeta::from_json_str("null").to_json_string(), "null");
        assert_eq!(MessageMeta::from_json_str("bogus").to_json_string(), "null");
        let meta = MessageMeta::new().with_call_id("c1");
        let s = meta.to_json_string();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&s).unwrap(),
            json!({"call_id": "c1"})
        );
    }

    #[test]
    fn tool_call_record_roundtrip() {
        let rec = ToolCallRecord {
            call_id: "c1".into(),
            name: "web_search".into(),
            arguments_json: r#"{"query":"rust"}"#.into(),
            attempt_metadata: None,
            repair_metadata: Some(RepairRecord {
                original_name: "web_seach".into(),
                applied_aliases: vec![],
                confidence: 0.9,
            }),
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: ToolCallRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.call_id, rec.call_id);
        assert_eq!(back.repair_metadata.unwrap().original_name, "web_seach");
    }

    #[test]
    fn tool_call_record_no_repair_omits_field() {
        let rec = ToolCallRecord {
            call_id: "c2".into(),
            name: "web_search".into(),
            arguments_json: "{}".into(),
            attempt_metadata: None,
            repair_metadata: None,
        };
        let json = serde_json::to_string(&rec).unwrap();
        assert!(!json.contains("repair_metadata"));
    }

    #[test]
    fn tool_result_record_roundtrip() {
        let rec = ToolResultRecord {
            call_id: "c1".into(),
            result_json: r#"{"results":[]}"#.into(),
            truncated: false,
            soft_failure: false,
            activated_tool_names: Vec::new(),
            result_metadata: None,
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: ToolResultRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.call_id, rec.call_id);
        assert!(!back.truncated);
    }

    #[test]
    fn all_roles_serialize() {
        let roles = [
            AgentRole::System,
            AgentRole::User,
            AgentRole::Assistant,
            AgentRole::Tool,
            AgentRole::CompactSummary,
        ];
        for role in &roles {
            let s = serde_json::to_string(role).unwrap();
            let back: AgentRole = serde_json::from_str(&s).unwrap();
            assert_eq!(&back, role);
        }
    }
}
