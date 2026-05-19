use crate::message::{ToolCallRecord, ToolResultRecord};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSignatureWindow {
    pub signature: String,
    pub repetition_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWorkItem {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoomLoopMatch {
    pub signature: String,
    pub repetition_count: u32,
}

#[derive(Debug, Clone, Default)]
pub struct LoopMetrics {
    pub turn_count: u32,
    pub message_count: u32,
    pub files_read: BTreeSet<String>,
    pub repeated_tool_signatures: Vec<ToolSignatureWindow>,
    pub tool_attempts: BTreeMap<String, u32>,
    pub pending_work_items: Vec<PendingWorkItem>,
}

impl LoopMetrics {
    pub fn record_tool_call(&mut self, call: &ToolCallRecord) {
        *self.tool_attempts.entry(call.name.clone()).or_insert(0) += 1;
        let signature = format!("{}:{}", call.name, call.arguments_json);
        if let Some(entry) = self
            .repeated_tool_signatures
            .iter_mut()
            .find(|w| w.signature == signature)
        {
            entry.repetition_count += 1;
        } else {
            self.repeated_tool_signatures.push(ToolSignatureWindow {
                signature,
                repetition_count: 1,
            });
        }
    }

    pub fn record_tool_result(&mut self, _result: &ToolResultRecord) {
        // no-op for now
    }

    pub fn record_file_read(&mut self, path: &str) {
        self.files_read.insert(path.to_string());
    }

    pub fn can_edit_path(&self, path: &str) -> bool {
        self.files_read.contains(path)
    }

    pub fn detect_doom_loop(&self, threshold: usize) -> Option<DoomLoopMatch> {
        self.repeated_tool_signatures
            .iter()
            .find(|w| w.repetition_count >= threshold as u32)
            .map(|w| DoomLoopMatch {
                signature: w.signature.clone(),
                repetition_count: w.repetition_count,
            })
    }

    pub fn has_pending_work(&self) -> bool {
        !self.pending_work_items.is_empty()
    }
}
