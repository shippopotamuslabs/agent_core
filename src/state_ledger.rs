//! Semantic state ledger: a derived cache, not ground truth.
//!
//! The ledger preserves continuity across compaction boundaries by extracting
//! durable facts (objective, constraints, decisions, open questions, etc.) from
//! the raw chat history. Compaction tiers (summary, FIFO) can rewrite history
//! freely; the ledger is a convenience cache of the newest observation per
//! fact class.
//!
//! # Precedence rule
//!
//! 1. Surviving log wins: non-superseded session messages are ground truth for
//!    anything they still contain.
//! 2. Ledger is a derived cache, newest observation wins per fact class:
//!    objective = last-write-wins; artifact anchors = keyed replace by
//!    `artifact_id` (unchanged); decisions/constraints/open_questions/
//!    touched_files = set semantics (exact normalized-text dedupe; first-seen
//!    order kept); tool_decisions = append-only event history (never retired).
//! 3. Explicit retirement tags (same declared tag protocol as existing
//!    extraction, case-insensitive): `reversal:` / `supersedes:` retire
//!    matching decisions+constraints; `resolved:` retires matching
//!    open_questions. Match = normalized full entry text (trim, collapse
//!    whitespace, case-fold, strip trailing `?`/`.` for COMPARISON key only;
//!    stored text unchanged). Retirement arms Live observations only:
//!    `merge_summary` retirement tags are ignored entirely (no retire, no
//!    append). Unmatched Live reversal/resolved appends as a decision
//!    (visible, never silent) and records the tombstone so a repeat is a
//!    no-op; a live re-assertion clears the tombstone. NO natural-language
//!    reversal inference.
//! 4. Summary is older than the log tail: `merge_summary` observations are
//!    tombstone-filtered for decisions/constraints/open_questions — retired
//!    texts (ledger tombstone set) are NOT resurrected by summary
//!    re-observation. A live user/assistant re-assertion
//!    clears the tombstone and re-adds. (Ledger gets `retired: Vec<String>`
//!    tombstones, serde default; cap not required but dedupe.)
//! 5. HistorySnapshot has no precedence: verbatim archive behind
//!    read_context_reference; it never overrides log/ledger/summary in render.
//!    summary_refs/history_refs deduped exact.

use serde::{Deserialize, Serialize};

use crate::message::{AgentMessage, AgentRole, MessageMeta, ToolCallRecord, ToolResultRecord};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactStateAnchor {
    pub artifact_id: String,
    pub total_blocks: u32,
    pub last_block_preview: String,
    pub continuation_instruction: String,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct StateLedger {
    pub objective: Option<String>,
    pub constraints: Vec<String>,
    pub decisions: Vec<String>,
    pub open_questions: Vec<String>,
    pub touched_files: Vec<String>,
    pub tool_decisions: Vec<String>,
    pub artifact_states: Vec<ArtifactStateAnchor>,
    pub summary_refs: Vec<String>,
    pub history_refs: Vec<String>,
    /// Normalized tombstone set (precedence rule 4): retired entry keys.
    /// Summary re-observations matching a tombstone are dropped; a live
    /// re-assertion clears the tombstone and re-adds the entry.
    #[serde(default)]
    pub retired: Vec<String>,
}

/// Pulled-from-text patterns. Each tag maps to a `Vec<String>` field; the
/// "objective" tag uses `Option<String>` with last-write-wins for live
/// observations (summary fills only when empty).
const PREFIX_OBJECTIVE: &[&str] = &["objective:", "goal:"];
const PREFIX_CONSTRAINT: &[&str] = &["constraint:", "constraints:"];
const PREFIX_DECISION: &[&str] = &["decision:", "decided:"];
const PREFIX_OPEN_QUESTION: &[&str] = &["open question:", "question:", "todo:"];
const PREFIX_TOUCHED_FILE: &[&str] = &["file:", "touched:"];
/// Explicit retirement tags (precedence rule 3, Live observations only).
/// `reversal:` / `supersedes:` retire matching decisions+constraints;
/// `resolved:` retires matching open_questions. Unmatched Live retirement
/// appends as a decision (and tombstones, so a repeat is a no-op). Summary
/// retirement tags are ignored entirely.
const PREFIX_REVERSAL: &[&str] = &["reversal:", "supersedes:"];
const PREFIX_RESOLVED: &[&str] = &["resolved:"];

/// Trim leading whitespace and end at the next sentence boundary
/// (`.`/`?`/`!` followed by space, or end-of-string). Returns the value plus
/// its end byte-offset within `s` so the scanner can advance past the whole
/// span (nested tags quoted inside the value never re-scan).
fn until_sentence_end(s: &str) -> (String, usize) {
    let trimmed = s.trim_start();
    let leading = s.len() - trimmed.len();
    let mut end = trimmed.len();
    let bytes = trimmed.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if matches!(*b, b'.' | b'?' | b'!') {
            // Capture the punctuation for questions so "retention policy?" keeps the ?
            let keep_punct = *b == b'?';
            let next = bytes.get(i + 1).copied().unwrap_or(b' ');
            if next == b' ' || next == b'\n' || next == b'\t' || i + 1 == bytes.len() {
                end = if keep_punct { i + 1 } else { i };
                break;
            }
        }
    }
    (trimmed[..end].trim().to_string(), leading + end)
}

/// Normalization for COMPARISON keys only (precedence rule 3): trim, collapse
/// whitespace runs, case-fold, strip trailing `?`/`.`. Stored entry text is
/// unchanged.
fn normalize_key(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .trim_end_matches(['?', '.'])
        .to_string()
}

/// Observation source (precedence rules 2, 4). Public `observe_user_message` /
/// `observe_assistant_message` are `Live`; `merge_summary` is `Summary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObserveSource {
    Live,
    Summary,
}

impl StateLedger {
    /// Set-semantics insert into a fact-class vec (precedence rules 2, 4):
    /// exact normalized-text dedupe, first-seen order kept. Summary
    /// observations matching a tombstone are dropped (never resurrected); a
    /// live re-assertion clears the tombstone and re-adds.
    fn insert_fact(
        retired: &mut Vec<String>,
        field: &mut Vec<String>,
        value: String,
        source: ObserveSource,
    ) {
        let key = normalize_key(&value);
        // A live observation is newest: it always clears a stale tombstone
        // for its key, even when the entry itself is already present (dedupe
        // no-op below). This lets a live re-assertion adopt an
        // unmatched-retirement artifact so a later reversal retires it.
        if source == ObserveSource::Live {
            retired.retain(|k| k != &key);
        }
        if field.iter().any(|e| normalize_key(e) == key) {
            return;
        }
        match source {
            ObserveSource::Summary => {
                if retired.iter().any(|k| k == &key) {
                    return;
                }
                field.push(value);
            }
            ObserveSource::Live => {
                field.push(value);
            }
        }
    }

    /// Retire matching entries from the given fact-class fields (precedence
    /// rule 3). Returns true when at least one live entry — or an existing
    /// tombstone — matched, in which case the tombstone is recorded (or kept).
    /// A tombstone hit stays silent and keeps everything: this covers both
    /// genuinely retired facts and unmatched-retirement artifacts (appended
    /// visibly AND tombstoned, so a repeat is a no-op and the entry stays).
    fn retire_matches(
        retired: &mut Vec<String>,
        fields: &mut [&mut Vec<String>],
        key: &str,
    ) -> bool {
        if retired.iter().any(|k| k == key) {
            return true;
        }
        let mut matched = false;
        for field in fields.iter_mut() {
            let before = field.len();
            field.retain(|e| normalize_key(e) != key);
            if field.len() != before {
                matched = true;
            }
        }
        if matched {
            retired.push(key.to_string());
            return true;
        }
        false
    }

    fn observe_text(&mut self, content: &str, source: ObserveSource) {
        // We scan line-by-line and also allow inline ". Decision: …" sentences
        // (M4 puts multiple "Decision:" tags on a single line).
        let mut search_from = 0usize;
        // Must be to_ascii_lowercase: full Unicode lowercasing can change byte
        // lengths, and the match offsets below index into the original `content`.
        let lower = content.to_ascii_lowercase();
        // Walk through, finding any of the relevant prefixes.
        let prefixes: &[(&[&str], &str)] = &[
            (PREFIX_OBJECTIVE, "objective"),
            (PREFIX_CONSTRAINT, "constraint"),
            (PREFIX_DECISION, "decision"),
            (PREFIX_OPEN_QUESTION, "open_question"),
            (PREFIX_TOUCHED_FILE, "touched_file"),
            (PREFIX_REVERSAL, "reversal"),
            (PREFIX_RESOLVED, "resolved"),
        ];
        let content_bytes = content.as_bytes();
        while search_from < content.len() {
            // Find the earliest prefix match starting at `search_from`. A
            // tag fires only on a left word-boundary (start of text or a
            // non-alphanumeric predecessor) so `Unresolved:` / `Indecision:`
            // never false-fire on the `resolved:` / `decision:` suffix.
            let mut earliest: Option<(usize, usize, &str)> = None; // (pos, prefix_len, kind)
            for (list, kind) in prefixes {
                for p in *list {
                    let mut off = search_from;
                    while off < lower.len() {
                        match lower[off..].find(p) {
                            Some(rel) => {
                                let abs = off + rel;
                                if abs == 0 || !content_bytes[abs - 1].is_ascii_alphanumeric() {
                                    match earliest {
                                        Some((cur, _, _)) if cur <= abs => {}
                                        _ => earliest = Some((abs, p.len(), kind)),
                                    }
                                    break;
                                }
                                off = abs + 1;
                            }
                            None => break,
                        }
                    }
                }
            }
            let Some((pos, plen, kind)) = earliest else {
                break;
            };
            let after = &content[pos + plen..];
            let (value, value_end) = until_sentence_end(after);
            // Advance past the extracted value span so tags nested/quoted
            // inside a value never re-scan; multi-tag lines still extract
            // every tag because the next tag starts after this value ends.
            search_from = pos + plen + value_end;
            // Retirement arms Live observations only (rule 3): a Summary
            // retirement tag is ignored entirely — no retire, no append.
            if (kind == "reversal" || kind == "resolved") && source != ObserveSource::Live {
                continue;
            }
            if !value.is_empty() {
                match kind {
                    "objective" => match source {
                        // Live is newest: last-write-wins. Summary is older
                        // than the log tail: fills only when empty.
                        ObserveSource::Live => {
                            self.objective = Some(value);
                        }
                        ObserveSource::Summary => {
                            if self.objective.is_none() {
                                self.objective = Some(value);
                            }
                        }
                    },
                    "constraint" => {
                        Self::insert_fact(&mut self.retired, &mut self.constraints, value, source)
                    }
                    "decision" => {
                        Self::insert_fact(&mut self.retired, &mut self.decisions, value, source)
                    }
                    "open_question" => Self::insert_fact(
                        &mut self.retired,
                        &mut self.open_questions,
                        value,
                        source,
                    ),
                    "touched_file" => {
                        // Tombstones cover decisions/constraints/open_questions
                        // only (rule 4): touched files are plain set semantics.
                        let key = normalize_key(&value);
                        if !self.touched_files.iter().any(|e| normalize_key(e) == key) {
                            self.touched_files.push(value);
                        }
                    }
                    "reversal" => {
                        let key = normalize_key(&value);
                        let matched = Self::retire_matches(
                            &mut self.retired,
                            &mut [&mut self.constraints, &mut self.decisions],
                            &key,
                        );
                        if !matched {
                            // Unmatched retirement stays visible: append as a
                            // decision instead of silently dropping it, and
                            // record the tombstone so a repeat is a no-op
                            // (the appended entry stays).
                            Self::insert_fact(
                                &mut self.retired,
                                &mut self.decisions,
                                value,
                                source,
                            );
                            if !self.retired.iter().any(|k| k == &key) {
                                self.retired.push(key);
                            }
                        }
                    }
                    "resolved" => {
                        let key = normalize_key(&value);
                        let matched = Self::retire_matches(
                            &mut self.retired,
                            &mut [&mut self.open_questions],
                            &key,
                        );
                        if !matched {
                            // Same visibility rule as unmatched reversal.
                            Self::insert_fact(
                                &mut self.retired,
                                &mut self.decisions,
                                value,
                                source,
                            );
                            if !self.retired.iter().any(|k| k == &key) {
                                self.retired.push(key);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    pub fn observe_user_message(&mut self, message: &AgentMessage) {
        if matches!(message.role, AgentRole::User) {
            self.observe_text(&message.content, ObserveSource::Live);
        }
    }

    pub fn observe_assistant_message(&mut self, message: &AgentMessage) {
        if matches!(message.role, AgentRole::Assistant) {
            self.observe_text(&message.content, ObserveSource::Live);
        }
    }

    pub fn observe_tool_call(&mut self, call: &ToolCallRecord) {
        self.tool_decisions.push(format!("call:{}", call.name));
    }

    pub fn observe_tool_result(&mut self, result: &ToolResultRecord) {
        let status = if result.soft_failure {
            "soft_failure"
        } else if result.truncated {
            "truncated"
        } else {
            "ok"
        };
        self.tool_decisions
            .push(format!("result:{}:{}", result.call_id, status));
    }

    pub fn observe_artifact_anchor(&mut self, anchor: ArtifactStateAnchor) {
        if let Some(existing) = self
            .artifact_states
            .iter_mut()
            .find(|a| a.artifact_id == anchor.artifact_id)
        {
            *existing = anchor;
        } else {
            self.artifact_states.push(anchor);
        }
    }

    pub fn merge_summary(&mut self, summary_text: &str, summary_ref: Option<&str>) {
        if let Some(r) = summary_ref {
            // HistorySnapshot has no precedence (rule 5): refs deduped exact.
            if !self.summary_refs.iter().any(|e| e == r) {
                self.summary_refs.push(r.to_string());
            }
        }
        // Surface any embedded structured tags from the summary text itself.
        // Summary is older than the log tail (rule 4): tombstone-filtered,
        // and it never clobbers a live objective.
        self.observe_text(summary_text, ObserveSource::Summary);
    }

    pub fn add_history_ref(&mut self, history_ref: &str) {
        if !self.history_refs.iter().any(|e| e == history_ref) {
            self.history_refs.push(history_ref.to_string());
        }
    }

    pub fn as_anchor_message(&self) -> Option<AgentMessage> {
        // Render a compact one-message anchor only when there is something to
        // anchor.
        if self.objective.is_none()
            && self.constraints.is_empty()
            && self.decisions.is_empty()
            && self.open_questions.is_empty()
            && self.touched_files.is_empty()
            && self.artifact_states.is_empty()
        {
            return None;
        }
        let mut body = String::new();
        if let Some(obj) = &self.objective {
            body.push_str(&format!("Objective: {obj}\n"));
        }
        if !self.constraints.is_empty() {
            body.push_str(&format!("Constraints: {}\n", self.constraints.join("; ")));
        }
        if !self.decisions.is_empty() {
            body.push_str(&format!("Decisions: {}\n", self.decisions.join("; ")));
        }
        if !self.open_questions.is_empty() {
            body.push_str(&format!(
                "Open questions: {}\n",
                self.open_questions.join("; ")
            ));
        }
        if !self.touched_files.is_empty() {
            body.push_str(&format!(
                "Touched files: {}\n",
                self.touched_files.join("; ")
            ));
        }
        if !self.artifact_states.is_empty() {
            for a in &self.artifact_states {
                body.push_str(&format!(
                    "Artifact {}: {} blocks, last `{}`. {}\n",
                    a.artifact_id, a.total_blocks, a.last_block_preview, a.continuation_instruction
                ));
            }
        }
        Some(AgentMessage {
            id: "state-ledger-anchor".into(),
            role: AgentRole::CompactSummary,
            content: body.trim_end().to_string(),
            metadata: MessageMeta::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(content: &str) -> AgentMessage {
        AgentMessage {
            id: "u".into(),
            role: AgentRole::User,
            content: content.into(),
            metadata: MessageMeta::new(),
        }
    }

    #[test]
    fn observe_text_with_length_changing_lowercase_chars_does_not_panic() {
        // Regression: full Unicode to_lowercase() can change byte lengths
        // ('İ' is 2 bytes, its lowercase "i̇" is 3), shifting match offsets
        // that are applied back to the original string.
        let mut l = StateLedger::default();
        let content = format!("{} Decision: ship it.", "İ".repeat(10));
        l.observe_user_message(&user(&content));
        assert_eq!(l.decisions, vec!["ship it".to_string()]);
    }

    #[test]
    fn observe_user_extracts_objective_and_constraints() {
        let mut l = StateLedger::default();
        l.observe_user_message(&user(
            "Objective: build context manager. Constraint: do not edit contracts. \
             Decision: put ArtifactSquash before FIFO. Open question: retention policy?",
        ));
        assert_eq!(l.objective.as_deref(), Some("build context manager"));
        assert_eq!(l.constraints, vec!["do not edit contracts".to_string()]);
        assert_eq!(
            l.decisions,
            vec!["put ArtifactSquash before FIFO".to_string()]
        );
        assert_eq!(l.open_questions, vec!["retention policy?".to_string()]);
    }
}
