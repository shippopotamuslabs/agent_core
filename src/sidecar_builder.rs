//! Bounded sidecar deltas for state ledger, tool index, and file index.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::context_store::{
    make_preview, ContextReference, ContextReferenceKind, ContextStore, ContextStoreEntry,
};
use crate::file_context::FileContextIndex;
use crate::long_loop_policy::LongLoopPolicy;
use crate::state_ledger::StateLedger;
use crate::tool_context::ToolContextIndex;

/// Provider-visible sidecar metadata on each `ModelRequest`.
///
/// Also carries per-call telemetry fields that enable post-deploy cache-hit A/B
/// comparison: `pressure_ratio_at_call_model` measures how full the context window
/// was just before the model call; `compaction_passes_this_turn` and
/// `destructive_tiers_this_turn` record how much compaction work happened this turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderContextSnapshot {
    pub transcript_revision: u64,
    pub sidecar_generation: u32,
    pub sidecar_byte_len: u32,
    /// Number of compaction passes run so far this user turn (includes all triggers).
    #[serde(default)]
    pub compaction_passes_this_turn: u32,
    /// Number of compaction tiers that performed a destructive history rewrite this turn
    /// (Summary + Fifo). Used with `cache_read_tokens / input_tokens` to measure cache-hit
    /// degradation caused by history rewrites.
    #[serde(default)]
    pub destructive_tiers_this_turn: u32,
    /// Context pressure ratio (`total_tokens / context_window_tokens`) at the time the
    /// model was called. Stored as a fixed-point value scaled by 1000 (e.g. `825` == 0.825)
    /// to avoid f32 serialization drift. Use `value as f32 / 1000.0` to recover the ratio.
    #[serde(default)]
    pub pressure_ratio_at_call_model_millis: u32,
}

/// Builds bounded JSON sidecar payloads per model call.
#[derive(Debug, Clone, Default)]
pub struct SidecarBuilder {
    generation: u32,
    rolling_cache: Vec<RollingCacheEntry>,
    last_ledger_json: Option<String>,
}

#[derive(Debug, Clone)]
struct RollingCacheEntry {
    generation: u32,
    byte_len: usize,
}

impl SidecarBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_generation(&mut self) -> u32 {
        let gen = self.generation;
        self.generation = self.generation.saturating_add(1);
        gen
    }

    pub fn current_generation(&self) -> u32 {
        self.generation
    }

    pub fn build_ledger_delta(
        &mut self,
        store: &mut ContextStore,
        ledger: &StateLedger,
        budget: usize,
    ) -> Option<String> {
        let full = serde_json::to_string(ledger).ok()?;
        let payload = match self.last_ledger_json.as_ref() {
            None => full.clone(),
            Some(previous) => ledger_delta_json(previous, &full),
        };

        let bounded = enforce_byte_budget(
            store,
            &payload,
            budget,
            ContextReferenceKind::ArtifactSnapshot,
            "ledger",
            "ledger_snapshot",
        );
        self.last_ledger_json = Some(full);
        Some(bounded)
    }

    pub fn build_tool_index_payload(
        &mut self,
        store: &mut ContextStore,
        index: &ToolContextIndex,
        budget: usize,
    ) -> Option<String> {
        // ENC-296: the index payload is advertisement-only (no schema text)
        // and browsable by design; oversized payloads spill under the
        // generic ArtifactSnapshot kind with an explicit tool-index id prefix.
        let payload = serde_json::to_string(index).ok()?;
        Some(enforce_byte_budget(
            store,
            &payload,
            budget,
            ContextReferenceKind::ArtifactSnapshot,
            "tool-index",
            "tool_context_index",
        ))
    }

    pub fn build_file_index_payload(
        &mut self,
        store: &mut ContextStore,
        index: &FileContextIndex,
        budget: usize,
    ) -> Option<String> {
        let payload = serde_json::to_string(index).ok()?;
        Some(enforce_byte_budget(
            store,
            &payload,
            budget,
            ContextReferenceKind::FileOutline,
            "file-index",
            "file_context_index",
        ))
    }

    pub fn enforce_total_budget(&mut self, policy: &LongLoopPolicy) {
        while self.rolling_cache_total_bytes() > policy.sidecar_total_byte_budget
            && !self.rolling_cache.is_empty()
        {
            self.rolling_cache.remove(0);
        }
    }

    pub fn rolling_cache_total_bytes(&self) -> usize {
        self.rolling_cache.iter().map(|e| e.byte_len).sum()
    }

    pub fn rolling_cache_has_generation(&self, generation: u32) -> bool {
        self.rolling_cache
            .iter()
            .any(|e| e.generation == generation)
    }

    pub fn record_rolling_cache_entry(&mut self, generation: u32, byte_len: usize) {
        self.rolling_cache.push(RollingCacheEntry {
            generation,
            byte_len,
        });
    }
}

fn insert_spilled_sidecar(
    store: &mut ContextStore,
    payload: &str,
    kind: ContextReferenceKind,
    id_prefix: &str,
    title: &str,
) -> String {
    let seq = store.next_sequence();
    let id = format!("ctxref-{}-{}", id_prefix, seq);
    let preview = make_preview(payload);
    let reference = ContextReference {
        id: id.clone(),
        kind,
        title: title.to_string(),
        byte_len: payload.len() as u64,
        token_estimate: (payload.len() / 4) as u32,
        preview,
        retrieval_hint: format!("read_context_reference(\"{id}\")"),
    };
    let entry = ContextStoreEntry {
        reference,
        content: payload.to_string(),
        source_message_ids: vec![],
        created_turn_index: 0,
        expires_after_turn: None,
    };
    store.insert_entry(entry);
    id
}

fn enforce_byte_budget(
    store: &mut ContextStore,
    payload: &str,
    budget: usize,
    kind: ContextReferenceKind,
    id_prefix: &str,
    title: &str,
) -> String {
    if payload.len() <= budget {
        return payload.to_string();
    }
    let ref_id = insert_spilled_sidecar(store, payload, kind, id_prefix, title);
    let truncated = json!({
        "refs": [ref_id],
        "truncated": true,
    });
    let mut out = serde_json::to_string(&truncated).unwrap_or_else(|_| "{}".into());
    while out.len() > budget && out.len() > 2 {
        out.pop();
    }
    out
}

fn ledger_delta_json(previous_full: &str, current_full: &str) -> String {
    let prev: Value = serde_json::from_str(previous_full).unwrap_or(Value::Null);
    let curr: Value = serde_json::from_str(current_full).unwrap_or(Value::Null);
    let mut delta = serde_json::Map::new();

    if let (Some(prev_obj), Some(curr_obj)) = (prev.as_object(), curr.as_object()) {
        for (key, curr_val) in curr_obj {
            let prev_val = prev_obj.get(key);
            if prev_val != Some(curr_val) {
                delta.insert(key.clone(), curr_val.clone());
            }
        }
    }

    serde_json::to_string(&Value::Object(delta)).unwrap_or_else(|_| "{}".into())
}
