//! Spill oversized tool results into the [`crate::context_store::ContextStore`]
//! and replace the prompt-facing payload with a compact `context_ref` envelope.

use crate::context_store::{
    append_turn_suffix, make_preview, ContextReference, ContextReferenceKind, ContextStore,
    ContextStoreEntry,
};
use crate::events::{CompactionEvent, CompactionTier as TierEnum};
use crate::message::{AgentMessage, AgentRole};
use crate::message::{ToolCallRecord, ToolResultRecord};
use crate::session::{AgentConfig, AgentSession};
use crate::tool_semantics::ArtifactSquashSemantics;

use super::{CompactionContext, CompactionOutcome, CompactionTier};

/// Tool-output references expire this many turns after spill (inclusive turn index).
pub const DEFAULT_TOOL_OUTPUT_TTL_TURNS: u32 = 2;

/// Build the canonical reference id used for a spilled tool output.
pub fn tool_output_reference_id(call_id: &str) -> String {
    format!("tool-output-{call_id}")
}

/// Tool names to activate when a spill envelope's `retrieval_hint` references them.
pub fn activation_tools_from_retrieval_hint(retrieval_hint: &str) -> Vec<String> {
    const PREFIX: &str = "read_context_reference(\"";
    if retrieval_hint.starts_with(PREFIX) && retrieval_hint.ends_with("\")") {
        return vec!["read_context_reference".to_string()];
    }
    Vec::new()
}

/// Construct the prompt-facing JSON envelope that replaces the raw result.
///
/// The envelope intentionally contains the literal string `context_ref` so
/// downstream message-content matchers (and the test asserting on `context_ref`)
/// can detect a spilled message.
fn spill_envelope(reference: &ContextReference) -> String {
    serde_json::json!({
        "context_ref": reference.id,
        "kind": "tool_output",
        "title": reference.title,
        "byte_len": reference.byte_len,
        "preview": reference.preview,
        "retrieval_hint": reference.retrieval_hint,
    })
    .to_string()
}

/// If `result.result_json` exceeds `threshold_bytes`, persist the raw payload
/// in `store` and overwrite `result.result_json` with a compact reference
/// envelope. Returns the inserted [`ContextReference`].
pub fn spill_tool_result_if_needed(
    store: &mut ContextStore,
    result: &mut ToolResultRecord,
    call: &ToolCallRecord,
    threshold_bytes: usize,
    turn_index: u32,
) -> Option<ContextReference> {
    let len = result.result_json.len();
    if len <= threshold_bytes {
        return None;
    }
    let id = tool_output_reference_id(&result.call_id);
    let title = append_turn_suffix(
        format!("tool_output:{}:{}", call.name, result.call_id),
        turn_index,
    );
    let preview = make_preview(&result.result_json);
    let token_estimate = (len / 4) as u32;
    let reference = ContextReference {
        id: id.clone(),
        kind: ContextReferenceKind::ToolOutput,
        title,
        byte_len: len as u64,
        token_estimate,
        preview,
        retrieval_hint: format!("read_context_reference(\"{id}\")"),
    };
    let entry = ContextStoreEntry {
        reference: reference.clone(),
        content: result.result_json.clone(),
        source_message_ids: vec![format!("tool-{}", result.call_id)],
        created_turn_index: turn_index,
        expires_after_turn: Some(turn_index + DEFAULT_TOOL_OUTPUT_TTL_TURNS),
    };
    let inserted = store.insert_entry(entry);
    // Replace the prompt-facing payload with a compact envelope.
    result.result_json = spill_envelope(&inserted);
    result.truncated = true;
    Some(inserted)
}

fn tool_message_reference_id(message: &AgentMessage) -> String {
    let call_id = message.metadata.call_id().unwrap_or(message.id.as_str());
    tool_output_reference_id(call_id)
}

pub fn spill_tool_messages_in_session(
    store: &mut ContextStore,
    session: &mut AgentSession,
    artifact_squash: Option<&ArtifactSquashSemantics>,
    threshold_bytes: usize,
    turn_index: u32,
    trigger: crate::events::CompactionTrigger,
) -> Option<CompactionEvent> {
    let before = session
        .messages
        .iter()
        .map(|message| (message.content.len() / 4) as u32)
        .sum();
    let mut changed = false;
    let quoted_total = artifact_squash
        .filter(|squash| !squash.total_blocks_result_field.is_empty())
        .map(|squash| format!("\"{}\"", squash.total_blocks_result_field));

    for message in &mut session.messages {
        if message.role != AgentRole::Tool
            || message.content.len() <= threshold_bytes
            || message.content.contains("\"context_ref\"")
            || quoted_total
                .as_ref()
                .is_some_and(|quoted| message.content.contains(quoted))
        {
            continue;
        }
        let id = tool_message_reference_id(message);
        let content = message.content.clone();
        let reference = ContextReference {
            id: id.clone(),
            kind: ContextReferenceKind::ToolOutput,
            title: append_turn_suffix(format!("tool_output:{}", message.id), turn_index),
            byte_len: content.len() as u64,
            token_estimate: (content.len() / 4) as u32,
            preview: make_preview(&content),
            retrieval_hint: format!("read_context_reference(\"{id}\")"),
        };
        let entry = ContextStoreEntry {
            reference: reference.clone(),
            content,
            source_message_ids: vec![message.id.clone()],
            created_turn_index: turn_index,
            expires_after_turn: Some(turn_index + DEFAULT_TOOL_OUTPUT_TTL_TURNS),
        };
        let inserted = store.insert_entry(entry);
        message.content = spill_envelope(&inserted);
        changed = true;
    }

    changed.then(|| CompactionEvent {
        tier: TierEnum::ToolSpill,
        tokens_before: before,
        tokens_after: session
            .messages
            .iter()
            .map(|message| (message.content.len() / 4) as u32)
            .sum(),
        trigger,
    })
}

pub struct ToolSpillCompactionTier;

impl Default for ToolSpillCompactionTier {
    fn default() -> Self {
        Self
    }
}

impl CompactionTier for ToolSpillCompactionTier {
    fn apply(
        &self,
        session: AgentSession,
        _config: &AgentConfig,
        _ctx: &CompactionContext,
    ) -> CompactionOutcome {
        // ContextStore is engine-owned; the engine applies this stateful tier
        // before the stateless pipeline executes.
        let _ = session;
        CompactionOutcome::NotApplicable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::MessageMeta;
    use crate::tool_semantics::ArtifactSquashSemantics;

    fn frg14_squash(append_tool_name: &str, total_field: &str) -> ArtifactSquashSemantics {
        ArtifactSquashSemantics {
            append_tool_name: append_tool_name.into(),
            id_field: "docId".into(),
            blocks_field: "chunks".into(),
            total_blocks_result_field: total_field.into(),
            status_field: "status".into(),
            success_status_value: "success".into(),
            summary_template: "t".into(),
            continuation_instruction: "c".into(),
            snippet_max_chars: 200,
        }
    }

    fn frg14_tool_session(content: &str) -> AgentSession {
        AgentSession::new(
            vec![AgentMessage {
                id: "t1".into(),
                role: AgentRole::Tool,
                content: content.into(),
                metadata: MessageMeta::new(),
            }],
            serde_json::Value::Null,
        )
    }

    #[test]
    fn frg14_spill_custom_total_field_exempt() {
        let squash = frg14_squash("write_doc", "chunkCount");
        let content =
            r#"{"status":"success","chunkCount":3,"extra_padding":"0123456789"}"#.to_string();
        let mut session = frg14_tool_session(&content);
        let mut store = ContextStore::default();
        let event = spill_tool_messages_in_session(
            &mut store,
            &mut session,
            Some(&squash),
            16,
            0,
            crate::events::CompactionTrigger::PostToolResult,
        );
        assert!(event.is_none());
        assert_eq!(session.messages[0].content, content);
    }

    #[test]
    fn frg14_spill_none_spills_legacy_total() {
        let content =
            r#"{"status":"success","totalBlocks":3,"extra_padding":"0123456789"}"#.to_string();
        let mut session = frg14_tool_session(&content);
        let mut store = ContextStore::default();
        let event = spill_tool_messages_in_session(
            &mut store,
            &mut session,
            None,
            16,
            0,
            crate::events::CompactionTrigger::PostToolResult,
        );
        assert!(event.is_some());
        assert!(session.messages[0].content.contains("context_ref"));
    }

    #[test]
    fn frg14_spill_none_spills_custom_total() {
        let content =
            r#"{"status":"success","chunkCount":3,"extra_padding":"0123456789"}"#.to_string();
        let mut session = frg14_tool_session(&content);
        let mut store = ContextStore::default();
        let event = spill_tool_messages_in_session(
            &mut store,
            &mut session,
            None,
            16,
            0,
            crate::events::CompactionTrigger::PostToolResult,
        );
        assert!(event.is_some());
        assert!(session.messages[0].content.contains("context_ref"));
    }

    #[test]
    fn frg14_spill_enclave_exempts_legacy_total() {
        let squash = frg14_squash("append_blocks", "totalBlocks");
        let content =
            r#"{"status":"success","totalBlocks":3,"extra_padding":"0123456789"}"#.to_string();
        let mut session = frg14_tool_session(&content);
        let mut store = ContextStore::default();
        let event = spill_tool_messages_in_session(
            &mut store,
            &mut session,
            Some(&squash),
            16,
            0,
            crate::events::CompactionTrigger::PostToolResult,
        );
        assert!(event.is_none());
        assert_eq!(session.messages[0].content, content);
    }
}
