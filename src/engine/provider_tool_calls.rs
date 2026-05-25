//! Provider-replay tool call metadata (`raw_tool_calls` on assistant messages).

use serde_json::json;

use crate::message::{AgentMessage, AgentRole, ToolCallRecord};
use crate::session::AgentSession;
use crate::turn_io::RawToolCall;

/// Builds provider-replay tool calls from executed/repaired records only.
#[allow(dead_code)]
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

/// Builds provider-replay tool calls for every model-requested call in turn order.
///
/// Includes repaired and rejected calls so OpenAI-compatible providers see a tool
/// message for each `tool_call_id` on the preceding assistant message. Repaired
/// entries use canonical names/arguments; rejects keep the raw request shape.
pub fn provider_facing_raw_tool_calls_for_turn(
    merged: &[RawToolCall],
    repaired: &[ToolCallRecord],
) -> Vec<RawToolCall> {
    let repaired_by_id: std::collections::HashMap<&str, &ToolCallRecord> = repaired
        .iter()
        .map(|rec| (rec.call_id.as_str(), rec))
        .collect();
    merged
        .iter()
        .filter_map(|raw| {
            let id = raw.id.as_deref().filter(|id| !id.is_empty())?;
            if let Some(rec) = repaired_by_id.get(id) {
                Some(RawToolCall {
                    id: Some(rec.call_id.clone()),
                    name: rec.name.clone(),
                    arguments_json: rec.arguments_json.clone(),
                })
            } else {
                Some(RawToolCall {
                    id: Some(id.to_string()),
                    name: raw.name.clone(),
                    arguments_json: raw.arguments_json.clone(),
                })
            }
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
