use crate::message::AgentMessage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub session_messages: Vec<AgentMessage>,
    pub system_prompt: String,
    /// Raw JSON blob of the tool schemas in provider-specific format.
    /// Engine passes through opaquely; transport assembles it.
    pub tools_schema_json: String,
    #[serde(default)]
    pub rendered_tool_catalog_json: String,
    pub allow_tool_call: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_references: Vec<crate::context_store::ContextReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_ledger: Option<crate::state_ledger::StateLedger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_context_index: Option<crate::tool_context::ToolContextIndex>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_context_index: Option<crate::file_context::FileContextIndex>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    ToolUse,
    Stop,
    MaxTokens,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub arguments_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_text: Option<String>,
    pub tool_calls: Vec<RawToolCall>,
    pub finish_reason: FinishReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_reasoning_content: Option<String>,
}

impl ModelResponse {
    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }

    pub fn text_only(text: impl Into<String>) -> Self {
        Self {
            assistant_text: Some(text.into()),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            provider_reasoning_content: None,
        }
    }
}

/// Sent from engine to host when the summary compaction tier needs a round-trip.
/// Defined here (not in compact/) so `AgentAction` in `action.rs` can reference it
/// without a circular dependency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryRequest {
    pub messages_to_summarize: Vec<AgentMessage>,
    pub instruction: String,
    /// ID of the last message in the range to summarize. Echoed back in `SummaryResult`.
    pub last_message_id: String,
    #[serde(default = "default_summary_trigger")]
    pub trigger: crate::events::CompactionTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_instruction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_ledger_snapshot: Option<crate::state_ledger::StateLedger>,
}

fn default_summary_trigger() -> crate::events::CompactionTrigger {
    crate::events::CompactionTrigger::Preflight
}

/// Host returns this after performing the summarization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryResult {
    pub summary_text: String,
    /// Must match `SummaryRequest.last_message_id` — validated by engine.
    pub last_message_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_message(id: &str) -> AgentMessage {
        AgentMessage {
            id: id.into(),
            role: crate::message::AgentRole::User,
            content: "test".into(),
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn model_response_empty_roundtrip() {
        let r = ModelResponse {
            assistant_text: None,
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            provider_reasoning_content: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: ModelResponse = serde_json::from_str(&json).unwrap();
        assert!(!back.has_tool_calls());
        assert!(back.assistant_text.is_none());
    }

    #[test]
    fn model_response_parallel_tool_calls() {
        let r = ModelResponse {
            assistant_text: None,
            tool_calls: vec![
                RawToolCall {
                    id: Some("c1".into()),
                    name: "web_search".into(),
                    arguments_json: "{}".into(),
                },
                RawToolCall {
                    id: Some("c2".into()),
                    name: "web_extract".into(),
                    arguments_json: "{}".into(),
                },
            ],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        };
        assert!(r.has_tool_calls());
        assert_eq!(r.tool_calls.len(), 2);
        let json = serde_json::to_string(&r).unwrap();
        let back: ModelResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tool_calls[0].name, "web_search");
        assert_eq!(back.tool_calls[1].name, "web_extract");
    }

    #[test]
    fn finish_reason_other_roundtrip() {
        let r = FinishReason::Other("content_filter".into());
        let json = serde_json::to_string(&r).unwrap();
        let back: FinishReason = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, FinishReason::Other(s) if s == "content_filter"));
    }

    #[test]
    fn raw_tool_call_no_id_omits_field() {
        let tc = RawToolCall {
            id: None,
            name: "web_search".into(),
            arguments_json: "{}".into(),
        };
        let json = serde_json::to_string(&tc).unwrap();
        assert!(!json.contains("\"id\""));
    }

    #[test]
    fn model_request_roundtrip() {
        let req = ModelRequest {
            session_messages: vec![make_message("m1")],
            system_prompt: "You are helpful.".into(),
            tools_schema_json: "[]".into(),
            rendered_tool_catalog_json: "[]".into(),
            allow_tool_call: true,
            context_references: vec![],
            state_ledger: None,
            tool_context_index: None,
            file_context_index: None,
            cache_hints: vec![],
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ModelRequest = serde_json::from_str(&json).unwrap();
        assert!(back.allow_tool_call);
        assert_eq!(back.session_messages.len(), 1);
    }

    #[test]
    fn summary_request_roundtrip() {
        let req = SummaryRequest {
            messages_to_summarize: vec![make_message("m1"), make_message("m2")],
            instruction: "Summarize preserving tool decisions.".into(),
            last_message_id: "m2".into(),
            trigger: crate::events::CompactionTrigger::Preflight,
            focus_instruction: None,
            history_ref: None,
            state_ledger_snapshot: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: SummaryRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.last_message_id, "m2");
        assert_eq!(back.messages_to_summarize.len(), 2);
    }
}
