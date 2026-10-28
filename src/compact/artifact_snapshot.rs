//! Persist artifact continuation state before squashing append loops.
//!
//! ENC-302: the persisted snapshot is a derived observation cache, not
//! ground truth. The stored count is the newest observed value (`unknown`
//! when unobserved) and may be stale; the host artifact is the source of
//! truth. Never branch control flow on the cached count.

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
    // ENC-302 m3: the old `unwrap_or_else` unknown-fallback was deleted as
    // unreachable — `ArtifactStateAnchor` is a plain serde struct (strings +
    // Option<u32>), whose serialization to a JSON string cannot fail, so the
    // fallback could never render. An unknown count still persists through
    // the normal path as `"observed_total_blocks":null`.
    let content = serde_json::to_string(anchor).expect("artifact anchor serializes to JSON");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// ENC-302 m3: an unknown-count anchor persists through the normal JSON
    /// path (no fallback needed) as `"observed_total_blocks":null`.
    #[test]
    fn enc302_snapshot_persists_unknown_count_as_null() {
        let mut store = ContextStore::default();
        let anchor = ArtifactStateAnchor {
            artifact_id: "art-1".into(),
            observed_total_blocks: None,
            last_block_preview: "gamma".into(),
            continuation_instruction: "Continue appending.".into(),
        };
        let reference = persist_artifact_snapshot(&mut store, &anchor, 0);
        assert_eq!(reference.id, "artifact-snapshot-art-1");
        let stored = store.get(&reference.id).expect("snapshot must be stored");
        assert!(
            stored.content.contains("\"observed_total_blocks\":null"),
            "unknown count must persist as null: {}",
            stored.content
        );
        assert!(stored.content.contains("Continue appending."));
    }
}
