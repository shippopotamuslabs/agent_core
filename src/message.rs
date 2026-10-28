use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::tool_name::ToolName;
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

/// Typed view over `AgentMessage.metadata` — a CLOSED schema (ENC-308
/// item 12): every key the engine understands is a typed field, including
/// the well-known engine keys that used to hide in the flattened `extra`
/// catch-all (`agent_tool_budget`, `truncated`, `envelope_*`,
/// `tools_blocked_*`, `provider_reasoning`). The catch-all is deleted:
/// host annotations stay host-side and are never round-tripped through
/// the engine (Sean's closed-boundary ruling).
///
/// The two read seams differ deliberately (decision D2):
/// - Stored-state reads (the `Deserialize` impl, snapshot import) are
///   tolerant: unknown keys, and known keys whose values do not fit
///   their typed field, are dropped at the seam (see `absorb`); every
///   well-formed engine-owned value survives.
/// - FFI ingress ([`MessageMeta::from_json_str_strict`]) is strict: an
///   unknown key, or a known key whose value does not fit its field, is
///   an error naming the key.
///
/// Serialization emits keys in sorted order (serde_json's map ordering),
/// exactly as plain `Value` objects and the old flattened catch-all did,
/// so persisted sessions keep their byte shape.
#[derive(Debug, Clone, Default)]
pub struct MessageMeta {
    call_id: Option<String>,
    tool_name: Option<String>,
    raw_tool_calls: Vec<RawToolCall>,
    superseded: Option<bool>,
    /// ENC-293: set when this message's content is a tool-output spill
    /// envelope (see `compact::tool_spill`). The spill exemption reads this
    /// flag instead of sniffing content for the envelope marker.
    spilled: Option<bool>,
    rewrite_of: Option<String>,
    compaction_tier: Option<String>,
    // -- Well-known engine keys, promoted from the catch-all (ENC-308). --
    agent_tool_budget: Option<bool>,
    truncated: Option<bool>,
    envelope_ingest: Option<String>,
    envelope_parse_failure: Option<bool>,
    tools_blocked_fallback: Option<bool>,
    tools_blocked_nudge: Option<bool>,
    provider_reasoning: Option<ProviderReasoning>,
}

/// The `provider_reasoning` payload: provider reasoning content captured
/// at model-response ingest. A closed object carrying the single sub-key
/// the engine reads and writes.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderReasoning {
    reasoning_content: String,
}

impl Serialize for MessageMeta {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_canonical_value().serialize(serializer)
    }
}

/// What happened to one host-provided key in [`MessageMeta::absorb`]:
/// the tolerant stored-state path drops `Misfit`/`Unknown` keys, the
/// strict FFI path rejects them by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbsorbOutcome {
    /// The value landed in its typed field.
    Landed,
    /// A known key whose value does not fit its typed field.
    Misfit,
    /// A key outside the closed schema.
    Unknown,
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

impl MessageMeta {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when no field is set.
    /// Empty metadata serializes as JSON `null`, matching the pre-typed default.
    pub fn is_empty(&self) -> bool {
        self.call_id.is_none()
            && self.tool_name.is_none()
            && self.raw_tool_calls.is_empty()
            && self.superseded.is_none()
            && self.spilled.is_none()
            && self.rewrite_of.is_none()
            && self.compaction_tier.is_none()
            && self.agent_tool_budget.is_none()
            && self.truncated.is_none()
            && self.envelope_ingest.is_none()
            && self.envelope_parse_failure.is_none()
            && self.tools_blocked_fallback.is_none()
            && self.tools_blocked_nudge.is_none()
            && self.provider_reasoning.is_none()
    }

    /// The canonical JSON object for this metadata: one entry per set
    /// field, keys in serde_json's sorted map order — byte-identical to
    /// what the pre-ENC-308 flattened catch-all emitted for the same
    /// logical metadata.
    fn to_canonical_value(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        if let Some(v) = &self.call_id {
            map.insert("call_id".into(), v.clone().into());
        }
        if let Some(v) = &self.tool_name {
            map.insert("tool_name".into(), v.clone().into());
        }
        if !self.raw_tool_calls.is_empty() {
            map.insert(
                "raw_tool_calls".into(),
                serde_json::to_value(&self.raw_tool_calls).unwrap_or(serde_json::Value::Null),
            );
        }
        if let Some(v) = self.superseded {
            map.insert("superseded".into(), v.into());
        }
        if let Some(v) = self.spilled {
            map.insert("spilled".into(), v.into());
        }
        if let Some(v) = &self.rewrite_of {
            map.insert("rewrite_of".into(), v.clone().into());
        }
        if let Some(v) = &self.compaction_tier {
            map.insert("compaction_tier".into(), v.clone().into());
        }
        if let Some(v) = self.agent_tool_budget {
            map.insert("agent_tool_budget".into(), v.into());
        }
        if let Some(v) = self.truncated {
            map.insert("truncated".into(), v.into());
        }
        if let Some(v) = &self.envelope_ingest {
            map.insert("envelope_ingest".into(), v.clone().into());
        }
        if let Some(v) = self.envelope_parse_failure {
            map.insert("envelope_parse_failure".into(), v.into());
        }
        if let Some(v) = self.tools_blocked_fallback {
            map.insert("tools_blocked_fallback".into(), v.into());
        }
        if let Some(v) = self.tools_blocked_nudge {
            map.insert("tools_blocked_nudge".into(), v.into());
        }
        if let Some(v) = &self.provider_reasoning {
            map.insert(
                "provider_reasoning".into(),
                serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
            );
        }
        serde_json::Value::Object(map)
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
    }

    /// Canonical tool name. The legacy `name` dialect key is normalized
    /// into this field on the read path (see `absorb`).
    pub fn tool_name(&self) -> Option<&str> {
        self.tool_name.as_deref()
    }

    pub fn with_tool_name(mut self, tool_name: impl Into<String>) -> Self {
        self.set_tool_name(tool_name);
        self
    }

    pub fn set_tool_name(&mut self, tool_name: impl Into<String>) {
        self.tool_name = Some(tool_name.into());
    }

    pub fn raw_tool_calls(&self) -> &[RawToolCall] {
        &self.raw_tool_calls
    }

    pub fn set_raw_tool_calls(&mut self, calls: Vec<RawToolCall>) {
        self.raw_tool_calls = calls;
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
    }

    /// True when this message's content is a tool-output spill envelope.
    pub fn is_spilled(&self) -> bool {
        self.spilled.unwrap_or(false)
    }

    /// Mark this message's content as a tool-output spill envelope.
    pub fn mark_spilled(&mut self) {
        self.spilled = Some(true);
    }

    pub fn rewrite_of(&self) -> Option<&str> {
        self.rewrite_of.as_deref()
    }

    pub fn with_rewrite_of(mut self, rewrite_of: impl Into<String>) -> Self {
        self.rewrite_of = Some(rewrite_of.into());
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
    }

    /// Single entry point for host-provided keys. A known key whose value
    /// fits its typed field lands there; the legacy `name` dialect key
    /// normalizes into `tool_name`, landing only while `tool_name` is
    /// unset — an explicit `tool_name` wins in every key order (the
    /// pre-ENC-308 guard: precedence is a property of the guard, not of
    /// map iteration order). Reports the
    /// outcome so callers choose their seam: the tolerant stored-state
    /// path ([`MessageMeta::from_value`]) drops `Misfit`/`Unknown` keys;
    /// the strict FFI path ([`MessageMeta::from_json_str_strict`])
    /// rejects them by name. Engine-internal producers also route
    /// through here (e.g. the ingest-metadata copy in `model_intake`):
    /// their key set is closed and engine-owned, so every key lands.
    pub(crate) fn absorb(&mut self, key: String, value: serde_json::Value) -> AbsorbOutcome {
        use AbsorbOutcome::*;
        match key.as_str() {
            "call_id" => match value {
                serde_json::Value::String(s) => {
                    self.set_call_id(s);
                    Landed
                }
                _ => Misfit,
            },
            "tool_name" => match value {
                serde_json::Value::String(s) => {
                    self.set_tool_name(s);
                    Landed
                }
                _ => Misfit,
            },
            "raw_tool_calls" => match serde_json::from_value::<Vec<RawToolCall>>(value) {
                Ok(calls) => {
                    self.set_raw_tool_calls(calls);
                    Landed
                }
                Err(_) => Misfit,
            },
            "superseded" => match value {
                serde_json::Value::Bool(b) => {
                    self.superseded = Some(b);
                    Landed
                }
                _ => Misfit,
            },
            "spilled" => match value {
                serde_json::Value::Bool(b) => {
                    self.spilled = Some(b);
                    Landed
                }
                _ => Misfit,
            },
            "rewrite_of" => match value {
                serde_json::Value::String(s) => {
                    self.rewrite_of = Some(s);
                    Landed
                }
                _ => Misfit,
            },
            "compaction_tier" => match value {
                serde_json::Value::String(s) => {
                    self.set_compaction_tier(s);
                    Landed
                }
                _ => Misfit,
            },
            "name" => match value {
                serde_json::Value::String(s) => {
                    // Legacy dialect of `tool_name` (the pre-ENC-308
                    // guard): land only while the explicit field is
                    // unset, so an explicit `tool_name` wins regardless
                    // of the order the two keys are absorbed in.
                    if self.tool_name.is_none() {
                        self.set_tool_name(s);
                    }
                    Landed
                }
                _ => Misfit,
            },
            "agent_tool_budget" => match value {
                serde_json::Value::Bool(b) => {
                    self.agent_tool_budget = Some(b);
                    Landed
                }
                _ => Misfit,
            },
            "truncated" => match value {
                serde_json::Value::Bool(b) => {
                    self.truncated = Some(b);
                    Landed
                }
                _ => Misfit,
            },
            "envelope_ingest" => match value {
                serde_json::Value::String(s) => {
                    self.envelope_ingest = Some(s);
                    Landed
                }
                _ => Misfit,
            },
            "envelope_parse_failure" => match value {
                serde_json::Value::Bool(b) => {
                    self.envelope_parse_failure = Some(b);
                    Landed
                }
                _ => Misfit,
            },
            "tools_blocked_fallback" => match value {
                serde_json::Value::Bool(b) => {
                    self.tools_blocked_fallback = Some(b);
                    Landed
                }
                _ => Misfit,
            },
            "tools_blocked_nudge" => match value {
                serde_json::Value::Bool(b) => {
                    self.tools_blocked_nudge = Some(b);
                    Landed
                }
                _ => Misfit,
            },
            "provider_reasoning" => match serde_json::from_value::<ProviderReasoning>(value) {
                Ok(reasoning) => {
                    self.provider_reasoning = Some(reasoning);
                    Landed
                }
                Err(_) => Misfit,
            },
            _ => Unknown,
        }
    }

    // -- Well-known engine keys: typed fields since ENC-308 (they lived
    // in the catch-all before, behind these same accessors). --

    /// True when this message is the synthetic agent-tool-budget row.
    pub fn agent_tool_budget(&self) -> bool {
        self.agent_tool_budget.unwrap_or(false)
    }

    /// Mark this message as the synthetic agent-tool-budget row.
    pub fn with_agent_tool_budget(mut self) -> Self {
        self.agent_tool_budget = Some(true);
        self
    }

    /// True when the tool result payload was truncated.
    pub fn truncated(&self) -> bool {
        self.truncated.unwrap_or(false)
    }

    /// Record whether the tool result payload was truncated.
    pub fn with_truncated(mut self, truncated: bool) -> Self {
        self.truncated = Some(truncated);
        self
    }

    /// Envelope ingest outcome recorded on an assistant message
    /// (e.g. "failed", "tools_blocked", "reified").
    pub fn envelope_ingest(&self) -> Option<&str> {
        self.envelope_ingest.as_deref()
    }

    /// Record the envelope ingest outcome on this message.
    pub fn with_envelope_ingest(mut self, state: impl Into<String>) -> Self {
        self.envelope_ingest = Some(state.into());
        self
    }

    /// Mark that envelope parsing failed for this message.
    pub fn with_envelope_parse_failure(mut self) -> Self {
        self.envelope_parse_failure = Some(true);
        self
    }

    /// Mark that this message is the tools-blocked fallback notice.
    pub fn with_tools_blocked_fallback(mut self) -> Self {
        self.tools_blocked_fallback = Some(true);
        self
    }

    /// True when this message is the tools-blocked nudge row.
    pub fn tools_blocked_nudge(&self) -> bool {
        self.tools_blocked_nudge.unwrap_or(false)
    }

    /// Mark this message as the tools-blocked nudge row.
    pub fn with_tools_blocked_nudge(mut self) -> Self {
        self.tools_blocked_nudge = Some(true);
        self
    }

    /// Provider reasoning content captured at model-response ingest.
    pub fn provider_reasoning_content(&self) -> Option<&str> {
        self.provider_reasoning
            .as_ref()
            .map(|reasoning| reasoning.reasoning_content.as_str())
    }

    /// Record provider reasoning content on this message.
    pub fn with_provider_reasoning_content(mut self, content: impl Into<String>) -> Self {
        self.provider_reasoning = Some(ProviderReasoning {
            reasoning_content: content.into(),
        });
        self
    }

    /// Build from an untyped JSON value — the TOLERANT stored-state read
    /// (decision D2). `null` (and missing) maps to empty; objects route
    /// known keys into typed fields via `absorb`; unknown keys and
    /// ill-typed values for known keys are dropped at this seam; anything
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

    /// Strict sibling of [`Self::from_value`] (ENC-308): the closed
    /// schema enforced — an unknown key, or a known key whose value does
    /// not fit its field, is an error naming the key.
    fn from_value_strict(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        use serde::de::Error as _;
        let mut meta = Self::default();
        if let serde_json::Value::Object(map) = value {
            for (key, val) in map {
                match meta.absorb(key.clone(), val) {
                    AbsorbOutcome::Landed => {}
                    AbsorbOutcome::Unknown => {
                        return Err(serde_json::Error::custom(format!(
                            "unknown message metadata key: {key}"
                        )));
                    }
                    AbsorbOutcome::Misfit => {
                        return Err(serde_json::Error::custom(format!(
                            "message metadata key {key} has a value that does not fit its field"
                        )));
                    }
                }
            }
        }
        Ok(meta)
    }

    /// Parse a JSON-encoded metadata blob (e.g. across FFI). Unparseable input
    /// maps to empty, matching the previous `unwrap_or(Value::Null)` behavior.
    pub fn from_json_str(s: &str) -> Self {
        serde_json::from_str(s)
            .map(Self::from_value)
            .unwrap_or_default()
    }

    /// Strict sibling of [`Self::from_json_str`] (ENC-305, closed schema
    /// ENC-308): malformed JSON is a `serde_json::Error` instead of a
    /// silent empty default; a top level that is neither an object nor
    /// null is an error, never a silently emptied meta; and the object
    /// must satisfy the closed schema — unknown keys, and known keys
    /// whose values do not fit their fields, are errors naming the key
    /// (see [`MessageMeta::from_value_strict`]). Used on paths where a
    /// parse failure must surface as a typed error (the per-turn FFI
    /// request conversion, session creation); [`Self::from_json_str`] is
    /// unchanged.
    pub fn from_json_str_strict(s: &str) -> Result<Self, serde_json::Error> {
        use serde::de::Error as _;
        let value: serde_json::Value = serde_json::from_str(s)?;
        match value {
            serde_json::Value::Object(_) => Self::from_value_strict(value),
            serde_json::Value::Null => Ok(Self::default()),
            _ => Err(serde_json::Error::custom(
                "message metadata must be a JSON object or null",
            )),
        }
    }

    /// Encode for JSON persistence / FFI. Empty metadata encodes as `null`,
    /// matching the pre-typed default.
    pub fn to_json_string(&self) -> String {
        if self.is_empty() {
            "null".to_string()
        } else {
            self.to_canonical_value().to_string()
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
    /// Message metadata — a closed, typed schema ([`MessageMeta`]): the
    /// keys the engine reads and writes are its typed fields, and that
    /// set is complete. At FFI ingress an unknown key is a typed error
    /// naming the key; in stored state an unknown key is dropped at the
    /// read seam. Host annotations are not carried here — they stay
    /// host-side, keyed by message ID, and the host reattaches them to
    /// engine emissions itself. Defaults to JSON null when empty.
    ///
    /// ## Compaction Metadata Conventions:
    /// - `rewrite_of`: ID of the original message that this message replaces/rewrites.
    /// - `compaction_tier`: Name of the compaction tier that performed the rewrite (e.g. "micro").
    /// - `superseded`: Boolean flag set to `true` on the original message after it is rewritten.
    /// - `spilled`: Boolean flag set to `true` when the content is a tool-output spill envelope.
    #[serde(
        default,
        deserialize_with = "deserialize_metadata",
        serialize_with = "serialize_metadata"
    )]
    pub metadata: MessageMeta,
}

impl AgentMessage {
    /// Set this message's content to a spill `envelope` and flag it as
    /// spilled, as one operation. The envelope and the flag are one fact
    /// (ENC-293: the spill exemption trusts the flag and never re-sniffs
    /// content), so every path that puts an envelope into a session goes
    /// through here and the two can never diverge.
    pub fn set_spilled_content(&mut self, envelope: String) {
        self.content = envelope;
        self.metadata.mark_spilled();
    }
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
    // m3: host-reported activations travel as canonical identities.
    // (`ToolCallRecord.name` stays `String`: it is a history record.)
    #[serde(default)]
    pub activated_tool_names: Vec<ToolName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_metadata: Option<ToolResultMetadata>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_message_roundtrip() {
        let metadata = MessageMeta::from_value(json!({
            "call_id": "c1",
            "agent_tool_budget": true,
        }));
        let msg = AgentMessage {
            id: "msg-1".into(),
            role: AgentRole::User,
            content: "hello".into(),
            metadata,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: AgentMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, msg.id);
        assert_eq!(back.role, msg.role);
        assert_eq!(back.content, msg.content);
        assert_eq!(back.metadata.call_id(), Some("c1"));
        assert!(back.metadata.agent_tool_budget());
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
    fn message_meta_unknown_keys_dropped_at_read_seam() {
        // ENC-308 item 12: the catch-all is gone. A well-known engine key
        // (`truncated`) lands in its typed field; a genuinely unknown host
        // key (`vision_ref`) is dropped at the tolerant read seam and
        // never re-serialized.
        let raw = json!({
            "tool_name": "web_search",
            "truncated": true,
            "vision_ref": "asset-1",
        });
        let meta = MessageMeta::from_value(raw);
        assert_eq!(meta.tool_name(), Some("web_search"));
        assert!(meta.truncated());
        assert_eq!(
            serde_json::to_value(&meta).unwrap(),
            json!({ "tool_name": "web_search", "truncated": true })
        );
    }

    #[test]
    fn message_meta_well_known_engine_keys_roundtrip() {
        // The keys that used to hide in the catch-all are typed fields
        // now; every one round-trips through the stored-state path.
        let raw = json!({
            "agent_tool_budget": true,
            "truncated": false,
            "envelope_ingest": "reified",
            "envelope_parse_failure": true,
            "tools_blocked_fallback": true,
            "tools_blocked_nudge": true,
            "provider_reasoning": { "reasoning_content": "because" },
        });
        let meta = MessageMeta::from_value(raw.clone());
        assert!(meta.agent_tool_budget());
        assert!(!meta.truncated());
        assert_eq!(meta.envelope_ingest(), Some("reified"));
        assert!(meta.tools_blocked_nudge());
        assert_eq!(meta.provider_reasoning_content(), Some("because"));
        assert_eq!(serde_json::to_value(&meta).unwrap(), raw);
    }

    #[test]
    fn message_meta_serialization_byte_shape_is_sorted_and_stable() {
        // Byte-stability guard: a fully populated meta encodes with keys
        // in sorted order, exactly as the pre-ENC-308 flattened catch-all
        // (and plain Value objects before it) emitted them.
        let meta = MessageMeta::new()
            .with_call_id("c1")
            .with_tool_name("web_search")
            .with_rewrite_of("m0")
            .with_compaction_tier("micro")
            .with_agent_tool_budget()
            .with_truncated(true)
            .with_envelope_ingest("failed")
            .with_envelope_parse_failure()
            .with_tools_blocked_nudge()
            .with_provider_reasoning_content("r");
        let mut meta = meta;
        meta.mark_superseded();
        meta.mark_spilled();
        assert_eq!(
            meta.to_json_string(),
            concat!(
                "{\"agent_tool_budget\":true,\"call_id\":\"c1\",",
                "\"compaction_tier\":\"micro\",\"envelope_ingest\":\"failed\",",
                "\"envelope_parse_failure\":true,",
                "\"provider_reasoning\":{\"reasoning_content\":\"r\"},",
                "\"rewrite_of\":\"m0\",\"spilled\":true,\"superseded\":true,",
                "\"tool_name\":\"web_search\",\"tools_blocked_nudge\":true,",
                "\"truncated\":true}"
            )
        );
    }

    #[test]
    fn agent_message_legacy_row_with_junk_keys_loads_typed_fields() {
        // ENC-308 acceptance (D2): a stored message written by a
        // pre-close build — typed keys, well-known engine keys, and junk
        // host keys side by side — loads through the real deserialize
        // path with every engine-owned value intact; the junk is gone on
        // re-serialize. `spilled` rides along: before ENC-308 the read
        // path parked it in the catch-all and `is_spilled()` read false
        // on restored messages; absorb now routes it to its field.
        let stored = r#"{
            "id": "m7",
            "role": "tool",
            "content": "spill envelope",
            "metadata": {
                "call_id": "c9",
                "spilled": true,
                "superseded": false,
                "agent_tool_budget": true,
                "envelope_ingest": "reified",
                "vision_ref": "asset-1",
                "host_debug": { "trace": "abc" }
            }
        }"#;
        let msg: AgentMessage = serde_json::from_str(stored).unwrap();
        assert_eq!(msg.metadata.call_id(), Some("c9"));
        assert!(msg.metadata.is_spilled());
        assert!(!msg.metadata.is_superseded());
        assert!(msg.metadata.agent_tool_budget());
        assert_eq!(msg.metadata.envelope_ingest(), Some("reified"));
        let value = serde_json::to_value(&msg).unwrap();
        let metadata = value["metadata"].as_object().unwrap();
        assert!(!metadata.contains_key("vision_ref"));
        assert!(!metadata.contains_key("host_debug"));
        assert_eq!(metadata["spilled"], json!(true));
        assert_eq!(metadata["call_id"], json!("c9"));
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
    fn message_meta_explicit_tool_name_beats_legacy_name_in_any_order() {
        // R1 (ENC-308 review): the pre-ENC-308 `name` arm was guarded on
        // `tool_name` being unset, so the explicit key won in ALL
        // orderings. The guard is what carries that precedence — not
        // serde_json's sorted-key iteration (the only order `from_value`
        // can feed the keys in this tree). Drive the same absorb seam
        // `from_value` uses, in both insertion orders:
        let mut explicit_first = MessageMeta::new();
        explicit_first.absorb("tool_name".into(), json!("explicit_tool"));
        explicit_first.absorb("name".into(), json!("legacy_tool"));
        assert_eq!(explicit_first.tool_name(), Some("explicit_tool"));

        let mut legacy_first = MessageMeta::new();
        legacy_first.absorb("name".into(), json!("legacy_tool"));
        legacy_first.absorb("tool_name".into(), json!("explicit_tool"));
        assert_eq!(legacy_first.tool_name(), Some("explicit_tool"));

        // And at the from_value level, an object carrying both keys
        // resolves to the explicit name and normalizes to it alone.
        let meta = MessageMeta::from_value(json!({
            "name": "legacy_tool",
            "tool_name": "explicit_tool",
        }));
        assert_eq!(meta.tool_name(), Some("explicit_tool"));
        assert_eq!(
            serde_json::to_value(&meta).unwrap(),
            json!({ "tool_name": "explicit_tool" })
        );
    }

    #[test]
    fn message_meta_malformed_known_field_dropped_rest_intact() {
        let meta = MessageMeta::from_value(json!({
            "call_id": 42,
            "superseded": true,
            "custom": "x",
        }));
        // One malformed field must not discard the whole metadata object:
        // the well-formed remainder still lands. The malformed value
        // itself is dropped at the seam (pre-ENC-308 it was preserved
        // verbatim in the catch-all; the catch-all is deleted).
        assert!(meta.is_superseded());
        assert_eq!(meta.call_id(), None);
        assert_eq!(
            serde_json::to_value(&meta).unwrap(),
            json!({ "superseded": true })
        );
    }

    #[test]
    fn message_meta_malformed_raw_tool_calls_dropped() {
        let meta = MessageMeta::from_value(json!({
            "raw_tool_calls": [{ "bogus": 1 }],
            "tool_name": "web_search",
        }));
        assert_eq!(meta.tool_name(), Some("web_search"));
        assert!(meta.raw_tool_calls().is_empty());
        assert_eq!(
            serde_json::to_value(&meta).unwrap(),
            json!({ "tool_name": "web_search" })
        );
    }

    #[test]
    fn message_meta_absorb_routes_and_reports_outcomes() {
        let mut meta = MessageMeta::new();
        assert_eq!(
            meta.absorb("call_id".into(), json!("c9")),
            AbsorbOutcome::Landed
        );
        assert_eq!(meta.call_id(), Some("c9"));
        assert_eq!(
            serde_json::to_value(&meta).unwrap(),
            json!({ "call_id": "c9" })
        );
        assert_eq!(
            meta.absorb("vision_ref".into(), json!("asset-1")),
            AbsorbOutcome::Unknown
        );
        assert_eq!(
            meta.absorb("superseded".into(), json!("yes")),
            AbsorbOutcome::Misfit
        );
    }

    #[test]
    fn message_meta_misfit_then_typed_setter() {
        let mut meta = MessageMeta::from_value(json!({ "call_id": 42 }));
        assert_eq!(meta.call_id(), None);
        meta.set_call_id("c1");
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
    fn message_meta_from_json_str_strict_rejects_malformed_json() {
        // ENC-305: the strict sibling surfaces malformed JSON as an error
        // while valid blobs (object and null) parse exactly as the lenient
        // path parses them.
        assert!(MessageMeta::from_json_str_strict("{not json").is_err());
        assert!(MessageMeta::from_json_str_strict("bogus").is_err());
        let meta = MessageMeta::from_json_str_strict(r#"{"call_id":"c1"}"#)
            .expect("valid object metadata must parse");
        assert_eq!(meta.call_id(), Some("c1"));
        assert!(MessageMeta::from_json_str_strict("null")
            .expect("null metadata must parse")
            .is_empty());
        // m1: a top level that is neither an object nor null is an
        // error, not a silently emptied meta.
        assert!(MessageMeta::from_json_str_strict("42").is_err());
        assert!(MessageMeta::from_json_str_strict("[1,2]").is_err());
        assert!(MessageMeta::from_json_str_strict("\"text\"").is_err());
        assert!(MessageMeta::from_json_str_strict("true").is_err());
        // The lenient path is unchanged.
        assert_eq!(MessageMeta::from_json_str("bogus").to_json_string(), "null");
    }

    #[test]
    fn message_meta_strict_rejects_unknown_and_misfit_keys_by_name() {
        // ENC-308 item 12: the strict FFI ingress path enforces the
        // closed schema — an unknown host key is an error naming it,
        // and so is a known key whose value does not fit its field.
        let err = MessageMeta::from_json_str_strict(r#"{"vision_ref":"asset-1"}"#)
            .expect_err("unknown key must reject");
        assert!(
            err.to_string().contains("vision_ref"),
            "error names the key: {err}"
        );
        let err = MessageMeta::from_json_str_strict(r#"{"call_id":42}"#)
            .expect_err("misfit value must reject");
        assert!(
            err.to_string().contains("call_id"),
            "error names the key: {err}"
        );
        // The well-known engine keys and the legacy `name` dialect are
        // inside the schema and parse strictly.
        let meta = MessageMeta::from_json_str_strict(
            r#"{"name":"web_search","agent_tool_budget":true,"spilled":true}"#,
        )
        .expect("schema keys must parse");
        assert_eq!(meta.tool_name(), Some("web_search"));
        assert!(meta.agent_tool_budget());
        assert!(meta.is_spilled());
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
