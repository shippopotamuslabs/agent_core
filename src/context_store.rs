//! In-memory context store for spilled tool output, terminal output, history
//! snapshots, artifact snapshots, and file outlines/chunks.
//!
//! The store is engine-owned (lives on [`crate::engine::state::InnerSession`])
//! and exposes deterministic-ish IDs that compaction tiers can attach to model
//! requests as [`ContextReference`] handles. The host (or future model tool)
//! retrieves the full payload via the engine's read/search APIs.
//!
//! (ENC-296: the former host-owned `ToolSchema` / `ToolCatalogIndex` kinds
//! are deleted with the tool registry — the host owns the registry and no
//! schema text is stashed here. The remaining kinds are engine-created.)
//!
//! ### ENC-292 decision: no storage port for `ContextStore`
//!
//! `ContextStore` is deliberately **not** behind a host-supplied storage port.
//! It is session-scoped in-memory application state, not platform
//! infrastructure: entries live and die with the owning session (deduplicated
//! by content, expired by turn index), it performs no I/O, has no platform
//! variance (no filesystem/keychain/network access), and every constructor is
//! already plain Rust (`ContextStore::default()`). Porting it would add host
//! surface and FFI callback round-trips for zero isolation benefit. If a host
//! ever needs cross-session persistence, that belongs in a new snapshot
//! import/export API on the engine — not a storage trait inside the session.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextReferenceKind {
    ToolOutput,
    TerminalOutput,
    HistorySnapshot,
    ArtifactSnapshot,
    ToolIndex,
    FileOutline,
    FileChunk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReference {
    pub id: String,
    pub kind: ContextReferenceKind,
    pub title: String,
    pub byte_len: u64,
    pub token_estimate: u32,
    pub preview: String,
    pub retrieval_hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextStoreEntry {
    pub reference: ContextReference,
    pub content: String,
    pub source_message_ids: Vec<String>,
    pub created_turn_index: u32,
    pub expires_after_turn: Option<u32>,
}

const PREVIEW_MAX_CHARS: usize = 160;

/// Suffix appended to browsable reference titles for turn-aware freshness in Kotlin hosts.
pub const CONTEXT_REFERENCE_TURN_SUFFIX: &str = "@turn:";

/// Appends [`CONTEXT_REFERENCE_TURN_SUFFIX`] + turn index when the title does not already carry turn metadata.
pub fn append_turn_suffix(title: impl AsRef<str>, turn_index: u32) -> String {
    let title = title.as_ref();
    if title.contains(CONTEXT_REFERENCE_TURN_SUFFIX) {
        title.to_string()
    } else {
        format!("{title} {CONTEXT_REFERENCE_TURN_SUFFIX}{turn_index}")
    }
}

/// Kinds exposed in the model-facing browsable context-reference sidecar.
pub fn is_browsable_context_kind(kind: &ContextReferenceKind) -> bool {
    matches!(
        kind,
        ContextReferenceKind::ToolOutput
            | ContextReferenceKind::FileOutline
            | ContextReferenceKind::FileChunk
            | ContextReferenceKind::HistorySnapshot
            | ContextReferenceKind::TerminalOutput
            | ContextReferenceKind::ArtifactSnapshot
            | ContextReferenceKind::ToolIndex
    )
}

fn content_fingerprint(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// Build a short preview of the content (single-line, head-clipped).
pub fn make_preview(content: &str) -> String {
    let one_line: String = content
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    let chars: Vec<char> = one_line.chars().collect();
    if chars.len() <= PREVIEW_MAX_CHARS {
        chars.into_iter().collect()
    } else {
        let head: String = chars[..PREVIEW_MAX_CHARS - 1].iter().collect();
        format!("{head}…")
    }
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct ContextStore {
    /// Stable insertion-ordered map keyed by reference id.
    entries: BTreeMap<String, ContextStoreEntry>,
    /// Monotonic counter used by callers that need to mint sequence IDs.
    next_seq: u64,
}

impl ContextStore {
    pub fn insert_entry(&mut self, entry: ContextStoreEntry) -> ContextReference {
        let fingerprint = content_fingerprint(&entry.content);
        let to_remove: Vec<String> = self
            .entries
            .iter()
            .filter_map(|(id, existing)| {
                if content_fingerprint(&existing.content) == fingerprint
                    && existing.content == entry.content
                {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();
        for id in to_remove {
            self.entries.remove(&id);
        }
        let reference = entry.reference.clone();
        self.entries.insert(reference.id.clone(), entry);
        reference
    }

    /// Restore-time invariant check (ENC-305 fix pass, M3), run by
    /// snapshot import before the store is installed in a live session.
    ///
    /// The live invariants [`ContextStore::insert_entry`] maintains are
    /// re-checked, never re-established: every map key must equal its
    /// entry's `reference.id`, and `next_seq` must be greater than every
    /// numeric suffix `N` among keys shaped `ctxref-*-N` (the minted-ID
    /// shape, see `sidecar_builder`), so a future minted ID can never
    /// collide with — and silently overwrite — a restored entry.
    /// Content dedup is deliberately NOT re-run and entries are NOT
    /// replayed through `insert_entry`: entries are data, and replaying
    /// could silently drop them.
    pub fn validate_restored(&self) -> Result<(), crate::tool_semantics::ToolSemanticsError> {
        use crate::tool_semantics::ToolSemanticsError;
        for (key, entry) in &self.entries {
            if key != &entry.reference.id {
                return Err(ToolSemanticsError::InvalidSnapshot {
                    message: format!(
                        "context store key '{key}' does not match its entry reference id '{}'",
                        entry.reference.id
                    ),
                });
            }
        }
        let mut max_suffix: Option<u64> = None;
        for key in self.entries.keys() {
            let Some(rest) = key.strip_prefix("ctxref-") else {
                continue;
            };
            let Some((_, suffix)) = rest.rsplit_once('-') else {
                continue;
            };
            if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if let Ok(n) = suffix.parse::<u64>() {
                max_suffix = Some(max_suffix.map_or(n, |max: u64| max.max(n)));
            }
        }
        if let Some(max_suffix) = max_suffix {
            if self.next_seq <= max_suffix {
                return Err(ToolSemanticsError::InvalidSnapshot {
                    message: format!(
                        "context store next_seq {} must be greater than the largest \
                         ctxref suffix {max_suffix} among restored entry ids",
                        self.next_seq
                    ),
                });
            }
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&ContextStoreEntry> {
        self.entries.get(id)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }

    /// Mint a new sequential ID and return it. Used by callers that want
    /// deterministic-per-store IDs (e.g. tool schema deferral indices).
    pub fn next_sequence(&mut self) -> u64 {
        let id = self.next_seq;
        self.next_seq += 1;
        id
    }

    /// Naive content+title+id substring search. Returns at most `limit` refs.
    pub fn search(&self, query: &str, limit: usize) -> Vec<ContextReference> {
        if query.is_empty() {
            return self
                .entries
                .values()
                .take(limit)
                .map(|e| e.reference.clone())
                .collect();
        }
        let needle = query.to_lowercase();
        let mut hits: Vec<ContextReference> = Vec::new();
        for entry in self.entries.values() {
            if hits.len() >= limit {
                break;
            }
            let r = &entry.reference;
            let haystack = format!(
                "{} {} {} {}",
                r.id.to_lowercase(),
                r.title.to_lowercase(),
                r.preview.to_lowercase(),
                entry.content.to_lowercase()
            );
            if haystack.contains(&needle) {
                hits.push(r.clone());
            }
        }
        hits
    }

    /// Drop entries whose `expires_after_turn` is <= the supplied index.
    /// Returns the IDs that were removed.
    pub fn expire_after_turn(&mut self, current_turn_index: u32) -> Vec<String> {
        let to_remove: Vec<String> = self
            .entries
            .iter()
            .filter_map(|(id, entry)| match entry.expires_after_turn {
                Some(exp) if exp <= current_turn_index => Some(id.clone()),
                _ => None,
            })
            .collect();
        for id in &to_remove {
            self.entries.remove(id);
        }
        to_remove
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All current entries, useful for tests and serialization helpers.
    pub fn entries(&self) -> impl Iterator<Item = &ContextStoreEntry> {
        self.entries.values()
    }

    pub fn has_history_snapshot_for_ids(&self, message_ids: &[String]) -> bool {
        self.entries.values().any(|entry| {
            entry.reference.kind == ContextReferenceKind::HistorySnapshot
                && message_ids
                    .iter()
                    .all(|id| entry.source_message_ids.iter().any(|source| source == id))
        })
    }
}

pub fn insert_terminal_output(
    store: &mut ContextStore,
    id: impl Into<String>,
    title: impl Into<String>,
    content: impl Into<String>,
    turn_index: u32,
) -> ContextReference {
    let id = id.into();
    let title = title.into();
    let content = content.into();
    let reference = ContextReference {
        id: id.clone(),
        kind: ContextReferenceKind::TerminalOutput,
        title: append_turn_suffix(title, turn_index),
        byte_len: content.len() as u64,
        token_estimate: (content.len() / 4) as u32,
        preview: make_preview(&content),
        retrieval_hint: format!("read_context_reference(\"{id}\")"),
    };
    let entry = ContextStoreEntry {
        reference: reference.clone(),
        content,
        source_message_ids: vec![],
        created_turn_index: turn_index,
        expires_after_turn: None,
    };
    store.insert_entry(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kind: ContextReferenceKind, content: &str) -> ContextStoreEntry {
        ContextStoreEntry {
            reference: ContextReference {
                id: id.into(),
                kind,
                title: id.into(),
                byte_len: content.len() as u64,
                token_estimate: (content.len() / 4) as u32,
                preview: make_preview(content),
                retrieval_hint: format!("read({id})"),
            },
            content: content.into(),
            source_message_ids: vec![],
            created_turn_index: 0,
            expires_after_turn: None,
        }
    }

    #[test]
    fn insert_get_round_trip() {
        let mut store = ContextStore::default();
        let r = store.insert_entry(entry("a", ContextReferenceKind::ToolOutput, "hi"));
        assert_eq!(r.id, "a");
        assert_eq!(store.get("a").unwrap().content, "hi");
    }

    #[test]
    fn search_returns_matching_kind() {
        let mut store = ContextStore::default();
        store.insert_entry(entry(
            "a",
            ContextReferenceKind::ToolOutput,
            "alpha rare_token",
        ));
        store.insert_entry(entry("b", ContextReferenceKind::FileChunk, "beta only"));
        let hits = store.search("rare_token", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "a");
    }

    #[test]
    fn dedup_keeps_newest_reference_for_duplicate_content() {
        let mut store = ContextStore::default();
        store.insert_entry(entry(
            "old",
            ContextReferenceKind::ToolOutput,
            "dup payload",
        ));
        store.insert_entry(entry(
            "new",
            ContextReferenceKind::ToolOutput,
            "dup payload",
        ));
        assert!(store.get("old").is_none());
        assert_eq!(store.get("new").unwrap().content, "dup payload");
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn dedup_preserves_distinct_content_with_same_fingerprint_collision_guard() {
        let mut store = ContextStore::default();
        store.insert_entry(entry("a", ContextReferenceKind::ToolOutput, "payload-a"));
        store.insert_entry(entry("b", ContextReferenceKind::ToolOutput, "payload-b"));
        assert!(store.get("a").is_some());
        assert!(store.get("b").is_some());
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn is_browsable_context_kind_covers_all_engine_created_refs() {
        // ENC-296: the host-owned ToolSchema/ToolCatalogIndex kinds are
        // deleted, so every remaining (engine-created) kind is browsable.
        // Re-pinned from the old exclusion assertions: the contract is now
        // that no infrastructure kind survives outside this list.
        // The exhaustive match below has no wildcard arm on purpose: adding
        // a future variant fails compilation here until it is classified.
        for kind in [
            ContextReferenceKind::ToolOutput,
            ContextReferenceKind::TerminalOutput,
            ContextReferenceKind::HistorySnapshot,
            ContextReferenceKind::ArtifactSnapshot,
            ContextReferenceKind::ToolIndex,
            ContextReferenceKind::FileOutline,
            ContextReferenceKind::FileChunk,
        ] {
            match kind {
                ContextReferenceKind::ToolOutput
                | ContextReferenceKind::TerminalOutput
                | ContextReferenceKind::HistorySnapshot
                | ContextReferenceKind::ArtifactSnapshot
                | ContextReferenceKind::ToolIndex
                | ContextReferenceKind::FileOutline
                | ContextReferenceKind::FileChunk => assert!(
                    is_browsable_context_kind(&kind),
                    "{kind:?} must be browsable"
                ),
            }
        }
    }

    #[test]
    fn validate_restored_accepts_live_invariants() {
        // M3: a store built through insert_entry (plus minted-seq usage)
        // passes restore validation.
        let mut store = ContextStore::default();
        store.insert_entry(entry(
            "ctxref-sidecar-0",
            ContextReferenceKind::ToolOutput,
            "payload",
        ));
        let _ = store.next_sequence();
        store.insert_entry(entry(
            "term-1",
            ContextReferenceKind::TerminalOutput,
            "other",
        ));
        store.validate_restored().expect("live store must validate");
    }

    #[test]
    fn validate_restored_rejects_key_id_mismatch() {
        // M3: entries are installed verbatim by serde, bypassing
        // insert_entry, so a key != reference.id store is constructible
        // and must be caught here.
        let mut store = ContextStore::default();
        store.entries.insert(
            "wrong-key".to_string(),
            entry("real-id", ContextReferenceKind::ToolOutput, "payload"),
        );
        let error = store
            .validate_restored()
            .expect_err("key/id mismatch must fail");
        assert!(
            matches!(
                error,
                crate::tool_semantics::ToolSemanticsError::InvalidSnapshot { ref message }
                if message.contains("wrong-key") && message.contains("real-id")
            ),
            "expected InvalidSnapshot naming key and id, got {error:?}"
        );
    }

    #[test]
    fn validate_restored_rejects_next_seq_behind_minted_suffix() {
        // M3: next_seq reset to 0 would let a future minted
        // ctxref-sidecar-0 overwrite the restored entry.
        let mut store = ContextStore::default();
        store.insert_entry(entry(
            "ctxref-sidecar-3",
            ContextReferenceKind::ToolOutput,
            "payload",
        ));
        store.next_seq = 3;
        let error = store
            .validate_restored()
            .expect_err("next_seq not ahead of suffixes must fail");
        assert!(
            matches!(
                error,
                crate::tool_semantics::ToolSemanticsError::InvalidSnapshot { .. }
            ),
            "expected InvalidSnapshot, got {error:?}"
        );
        store.next_seq = 4;
        store.validate_restored().expect("next_seq ahead must pass");
        // Non-ctxref keys and non-numeric suffixes impose no constraint.
        store.next_seq = 0;
        let mut store = ContextStore::default();
        store.insert_entry(entry("term-9", ContextReferenceKind::TerminalOutput, "x"));
        store.insert_entry(entry(
            "ctxref-note-x",
            ContextReferenceKind::ToolOutput,
            "y",
        ));
        store.validate_restored().expect("unshaped keys must pass");
    }

    #[test]
    fn expire_after_turn_drops_old_entries() {
        let mut store = ContextStore::default();
        let mut e1 = entry("a", ContextReferenceKind::HistorySnapshot, "x");
        e1.expires_after_turn = Some(2);
        store.insert_entry(e1);
        let mut e2 = entry("b", ContextReferenceKind::HistorySnapshot, "y");
        e2.expires_after_turn = Some(10);
        store.insert_entry(e2);
        let removed = store.expire_after_turn(5);
        assert_eq!(removed, vec!["a".to_string()]);
        assert!(store.get("a").is_none());
        assert!(store.get("b").is_some());
    }
}
