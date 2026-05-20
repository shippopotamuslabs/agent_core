//! Provider-replay tool call metadata (`raw_tool_calls` on assistant messages).

use serde_json::json;

use crate::message::{AgentMessage, AgentRole, ToolCallRecord};
use crate::session::AgentSession;
use crate::turn_io::RawToolCall;

/// Builds provider-replay tool calls from executed/repaired records only.
pub fn provider_facing_raw_tool_calls(repaired: &[ToolCallRecord]) -> Vec<RawToolCall> {
    repaired
        .iter()
        .map(|rec| RawToolCall {
            id: Some(rec.call_id.clone()),
            name: rec.name.clone(),
            arguments_json: rec.arguments_json.clone(),
        })
        .collect()
}

/// Updates `raw_tool_calls` on the most recently pushed assistant message in-session.
pub fn patch_last_assistant_raw_tool_calls(session: &mut AgentSession, calls: Vec<RawToolCall>) {
    let value = serde_json::to_value(&calls).unwrap_or(serde_json::Value::Null);
    let Some(message) = last_assistant_message_mut(&mut session.messages) else {
        return;
    };
    if let Some(obj) = message.metadata.as_object_mut() {
        obj.insert("raw_tool_calls".to_string(), value);
    } else {
        message.metadata = json!({ "raw_tool_calls": value });
    }
}

fn last_assistant_message_mut(messages: &mut [AgentMessage]) -> Option<&mut AgentMessage> {
    messages
        .iter_mut()
        .rev()
        .find(|message| message.role == AgentRole::Assistant)
}
