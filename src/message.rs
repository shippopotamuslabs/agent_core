use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    System,
    User,
    Assistant,
    Tool,
    CompactSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: String,
    pub role: AgentRole,
    pub content: String,
    /// Opaque host-provided metadata (e.g. vision asset refs, turn tags).
    /// Defaults to JSON null; no required keys.
    #[serde(default)]
    pub metadata: serde_json::Value,
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
        let msg = AgentMessage {
            id: "msg-1".into(),
            role: AgentRole::User,
            content: "hello".into(),
            metadata: json!({ "tag": "vision" }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: AgentMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, msg.id);
        assert_eq!(back.role, msg.role);
        assert_eq!(back.content, msg.content);
    }

    #[test]
    fn agent_message_metadata_defaults_null() {
        let json = r#"{"id":"x","role":"user","content":"hi"}"#;
        let msg: AgentMessage = serde_json::from_str(json).unwrap();
        assert!(msg.metadata.is_null());
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
