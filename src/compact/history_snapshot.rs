//! Persist transcript ranges into the [`crate::context_store::ContextStore`]
//! before destructive compaction (summary or FIFO).

use crate::context_store::{
    make_preview, ContextReference, ContextReferenceKind, ContextStore, ContextStoreEntry,
};
use crate::events::CompactionTrigger;
use crate::message::AgentMessage;
use crate::session::{AgentConfig, AgentSession};

use super::{CompactionContext, CompactionOutcome, CompactionTier};

/// Mint a stable id for a history snapshot derived from the message id range.
pub fn history_snapshot_reference_id(messages: &[AgentMessage]) -> String {
    let first = messages.first().map(|m| m.id.as_str()).unwrap_or("none");
    let last = messages.last().map(|m| m.id.as_str()).unwrap_or("none");
    format!("history-{first}-{last}")
}

fn serialize_range(messages: &[AgentMessage]) -> String {
    serde_json::to_string(messages).unwrap_or_else(|_| {
        // Fallback: line-delimited "role: content".
        messages
            .iter()
            .map(|m| format!("{:?}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// Snapshot the supplied transcript range into the context store. Returns the
/// inserted reference (or `None` for an empty range).
pub fn snapshot_history_range(
    store: &mut ContextStore,
    messages: &[AgentMessage],
    source: CompactionTrigger,
    turn_index: u32,
) -> Option<ContextReference> {
    if messages.is_empty() {
        return None;
    }
    let id = history_snapshot_reference_id(messages);
    let content = serialize_range(messages);
    let len = content.len();
    let title = format!(
        "history_snapshot:{:?}:{}..{}",
        source,
        messages.first().unwrap().id,
        messages.last().unwrap().id
    );
    let reference = ContextReference {
        id: id.clone(),
        kind: ContextReferenceKind::HistorySnapshot,
        title,
        byte_len: len as u64,
        token_estimate: (len / 4) as u32,
        preview: make_preview(&content),
        retrieval_hint: format!("read_context_reference(\"{id}\")"),
    };
    let source_ids: Vec<String> = messages.iter().map(|m| m.id.clone()).collect();
    let entry = ContextStoreEntry {
        reference: reference.clone(),
        content,
        source_message_ids: source_ids,
        created_turn_index: turn_index,
        expires_after_turn: None,
    };
    Some(store.insert_entry(entry))
}

pub struct HistorySnapshotCompactionTier;

impl Default for HistorySnapshotCompactionTier {
    fn default() -> Self {
        Self
    }
}

impl CompactionTier for HistorySnapshotCompactionTier {
    fn apply(
        &self,
        session: AgentSession,
        _config: &AgentConfig,
        _ctx: &CompactionContext,
    ) -> CompactionOutcome {
        // The engine owns ContextStore, so this stateful tier is applied as an
        // engine prelude. Keeping a concrete tier preserves planner/pipeline
        // parity and prevents silent plan drops.
        let _ = session;
        CompactionOutcome::NotApplicable
    }
}
