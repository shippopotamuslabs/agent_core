//! Tool-result and message-content truncation before prompts hit providers (AGENT-019).

use crate::token_counter::{default_counter, TokenCounter};

const TRUNCATION_NOTE: &str = "\n[Result truncated to fit context budget]";
pub const MESSAGE_TRUNCATION_NOTE: &str = "\n[... earlier content truncated to fit context ...]";

/// Truncate UTF-8 `json` to fit within `budget_bytes`, preserving character boundaries.
/// Appends [`TRUNCATION_NOTE`] when truncated.
pub fn truncate_tool_result_json(json: &str, budget_bytes: usize) -> (String, bool) {
    if json.len() <= budget_bytes {
        return (json.to_string(), false);
    }
    if budget_bytes == 0 {
        return (TRUNCATION_NOTE.trim_start().to_string(), true);
    }
    let note_len = TRUNCATION_NOTE.len();
    let target = budget_bytes.saturating_sub(note_len);
    let mut end = target.min(json.len());
    while end > 0 && !json.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + note_len);
    out.push_str(&json[..end]);
    out.push_str(TRUNCATION_NOTE);
    (out, true)
}

/// Truncate UTF-8 `text` to at most `max_tokens` using the default counter.
/// Identical to the pre-ENC-292 shared-`tiktoken`-encoder behavior.
pub fn truncate_message_content_to_tokens(text: &str, max_tokens: u32) -> (String, bool) {
    truncate_message_content_to_tokens_with(default_counter(), text, max_tokens)
}

/// Truncate UTF-8 `text` to at most `max_tokens` using an explicit
/// host-supplied [`TokenCounter`] (ENC-292 port).
pub fn truncate_message_content_to_tokens_with(
    counter: &dyn TokenCounter,
    text: &str,
    max_tokens: u32,
) -> (String, bool) {
    counter.truncate_text_to_tokens(text, max_tokens, MESSAGE_TRUNCATION_NOTE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{count_tokens, truncate_budget_per_result, PromptBudgetParts, TokenLedger};
    use crate::message::{AgentMessage, AgentRole};
    use crate::session::{AgentConfig, AgentSession, SessionCapabilities};

    #[test]
    fn under_budget_unchanged() {
        let j = r#"{"ok":true}"#;
        let (s, t) = truncate_tool_result_json(j, 4096);
        assert_eq!(s, j);
        assert!(!t);
    }

    #[test]
    fn oversized_truncates_with_note() {
        let body = "x".repeat(5000);
        let j = format!(r#"{{"blob":"{body}"}}"#);
        let budget = 256;
        let (s, t) = truncate_tool_result_json(&j, budget);
        assert!(t);
        assert!(s.len() <= budget);
        assert!(s.contains(TRUNCATION_NOTE.trim_start()));
    }

    #[test]
    fn truncates_on_char_boundary() {
        let j = "😀".repeat(100); // 4 bytes each
        let (s, t) = truncate_tool_result_json(&j, 50);
        assert!(t);
        assert!(s.is_char_boundary(s.len() - TRUNCATION_NOTE.len()));
    }

    #[test]
    fn ledger_uses_truncated_payload() {
        let huge = "word ".repeat(2000);
        let json = format!(r#"{{"data":"{huge}"}}"#);
        let caps = SessionCapabilities {
            summarization_allowed: true,
            kv_fixed: false,
            tool_result_budget_tokens: Some(8),
            ..Default::default()
        };
        let config = AgentConfig::default();
        let budget = truncate_budget_per_result(&caps, &config);
        let (truncated, _) = truncate_tool_result_json(&json, budget);

        let tool_msg = AgentMessage {
            id: "t1".into(),
            role: AgentRole::Tool,
            content: truncated.clone(),
            metadata: serde_json::Value::Null,
        };
        let session_trunc = AgentSession::new(vec![tool_msg], serde_json::Value::Null);
        let session_full = AgentSession::new(
            vec![AgentMessage {
                id: "t1".into(),
                role: AgentRole::Tool,
                content: json.clone(),
                metadata: serde_json::Value::Null,
            }],
            serde_json::Value::Null,
        );
        let cfg = AgentConfig::default();
        let parts = PromptBudgetParts::from_legacy("anthropic", 100_000, "[]");
        let t_tokens = TokenLedger::estimate(&session_trunc, &cfg, &parts).total_tokens;
        let f_tokens = TokenLedger::estimate(&session_full, &cfg, &parts).total_tokens;
        assert!(t_tokens < f_tokens);
    }

    #[test]
    fn message_truncation_preserves_head_and_note() {
        let body = "alpha ".repeat(400);
        let (truncated, changed) = truncate_message_content_to_tokens(&body, 32);
        assert!(changed);
        assert!(truncated.contains(MESSAGE_TRUNCATION_NOTE.trim()));
        assert!(count_tokens(&truncated) <= 32);
    }

    #[test]
    fn message_truncation_under_budget_unchanged() {
        let text = "short message";
        let (out, changed) = truncate_message_content_to_tokens(text, 64);
        assert!(!changed);
        assert_eq!(out, text);
    }

    #[test]
    fn cloud_budget_path_uses_strict_default() {
        let caps = SessionCapabilities {
            summarization_allowed: true,
            kv_fixed: false,
            tool_result_budget_tokens: None,
            ..Default::default()
        };
        let config = AgentConfig::default();
        assert_eq!(
            truncate_budget_per_result(&caps, &config),
            config.tool_result_budget_cloud_bytes
        );
    }
}
