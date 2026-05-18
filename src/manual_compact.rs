//! Manual compact request/result (stub — see `plans/PLAN-agent-core-context-management`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualCompactRequest {
    pub focus_instruction: Option<String>,
    pub force_summary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualCompactResult {
    pub summary_ref: Option<String>,
    pub history_ref: Option<String>,
    pub tokens_before: u32,
    pub tokens_after: u32,
}
