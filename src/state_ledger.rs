//! Semantic state ledger.
//!
//! The ledger preserves continuity across compaction boundaries by extracting
//! durable facts (objective, constraints, decisions, open questions, etc.) from
//! the raw chat history. Compaction tiers (summary, FIFO) can rewrite history
//! freely while the ledger remains the source of truth for "what we agreed".

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
}

/// Pulled-from-text patterns. Each tag maps to a `Vec<String>` field; the
/// "objective" tag uses `Option<String>` and is set on first sighting only.
const PREFIX_OBJECTIVE: &[&str] = &["objective:", "goal:"];
const PREFIX_CONSTRAINT: &[&str] = &["constraint:", "constraints:"];
const PREFIX_DECISION: &[&str] = &["decision:", "decided:"];
const PREFIX_OPEN_QUESTION: &[&str] = &["open question:", "question:", "todo:"];
const PREFIX_TOUCHED_FILE: &[&str] = &["file:", "touched:"];

/// Trim leading whitespace and end at the next sentence boundary
/// (`.`/`?`/`!` followed by space, or end-of-string).
fn until_sentence_end(s: &str) -> String {
    let trimmed = s.trim_start();
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
    trimmed[..end].trim().to_string()
}

fn parse_lines(text: &str) -> Vec<&str> {
    text.split('\n').collect()
}

impl StateLedger {
    fn observe_text(&mut self, content: &str) {
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
        ];
        while search_from < content.len() {
            // Find the earliest prefix match starting at `search_from`.
            let mut earliest: Option<(usize, usize, &str)> = None; // (pos, prefix_len, kind)
            for (list, kind) in prefixes {
                for p in *list {
                    if let Some(rel) = lower[search_from..].find(p) {
                        let abs = search_from + rel;
                        let len = p.len();
                        match earliest {
                            Some((cur, _, _)) if cur <= abs => {}
                            _ => earliest = Some((abs, len, kind)),
                        }
                    }
                }
            }
            let Some((pos, plen, kind)) = earliest else {
                break;
            };
            let after = &content[pos + plen..];
            let value = until_sentence_end(after);
            if !value.is_empty() {
                match kind {
                    "objective" => {
                        if self.objective.is_none() {
                            self.objective = Some(value);
                        }
                    }
                    "constraint" => self.constraints.push(value),
                    "decision" => self.decisions.push(value),
                    "open_question" => self.open_questions.push(value),
                    "touched_file" => self.touched_files.push(value),
                    _ => {}
                }
            }
            search_from = pos + plen;
        }
        // Suppress unused-variable lint in case the function path widens later.
        let _ = parse_lines;
    }

    pub fn observe_user_message(&mut self, message: &AgentMessage) {
        if matches!(message.role, AgentRole::User) {
            self.observe_text(&message.content);
        }
    }

    pub fn observe_assistant_message(&mut self, message: &AgentMessage) {
        if matches!(message.role, AgentRole::Assistant) {
            self.observe_text(&message.content);
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
            self.summary_refs.push(r.to_string());
        }
        // Surface any embedded structured tags from the summary text itself.
        self.observe_text(summary_text);
    }

    pub fn add_history_ref(&mut self, history_ref: &str) {
        self.history_refs.push(history_ref.to_string());
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
