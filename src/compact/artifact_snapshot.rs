//! Persist artifact continuation state before squashing append loops.

use crate::context_store::{
    append_turn_suffix, make_preview, ContextReference, ContextReferenceKind, ContextStore,
    ContextStoreEntry,
};
use crate::state_ledger::ArtifactStateAnchor;

pub fn persist_artifact_snapshot(
    store: &mut ContextStore,
    anchor: &ArtifactStateAnchor,
    turn_index: u32,
) -> ContextReference {
    let id = format!("artifact-snapshot-{}", anchor.artifact_id);
    let content = serde_json::to_string(anchor).unwrap_or_else(|_| {
        format!(
            "{}:{}:{}",
            anchor.artifact_id, anchor.total_blocks, anchor.last_block_preview
        )
    });
    let reference = ContextReference {
        id: id.clone(),
        kind: ContextReferenceKind::ArtifactSnapshot,
        title: append_turn_suffix(
            format!("artifact_snapshot:{}", anchor.artifact_id),
            turn_index,
        ),
        byte_len: content.len() as u64,
        token_estimate: (content.len() / 4) as u32,
        preview: make_preview(&content),
        retrieval_hint: format!("read_context_reference(\"{id}\")"),
    };
    let entry = ContextStoreEntry {
        reference: reference.clone(),
        content,
        source_message_ids: vec![anchor.artifact_id.clone()],
        created_turn_index: turn_index,
        expires_after_turn: None,
    };
    store.insert_entry(entry)
}
