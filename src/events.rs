use serde::{Deserialize, Serialize};

/// Which phase triggered a compaction pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTrigger {
    Preflight,
    MidTurn,
    PostToolResult,
}

/// Which compaction strategy ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTier {
    Micro,
    Fifo,
    ArtifactSquash,
    Summary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionEvent {
    pub tier: CompactionTier,
    pub tokens_before: u32,
    pub tokens_after: u32,
    pub trigger: CompactionTrigger,
}

// Full AgentError and AgentEvent types added in AGENT-006.
