use serde::{Deserialize, Serialize};

// ── Compaction ────────────────────────────────────────────────────────────────

/// Which phase of the turn loop triggered a compaction pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTrigger {
    Preflight,
    MidTurn,
    PostToolResult,
    Manual,
}

/// Which compaction strategy ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTier {
    Micro,
    Fifo,
    ArtifactSquash,
    Summary,
    ToolSpill,
    HistorySnapshot,
}

impl std::fmt::Display for CompactionTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompactionTier::Micro => write!(f, "micro"),
            CompactionTier::Fifo => write!(f, "fifo"),
            CompactionTier::ArtifactSquash => write!(f, "artifact_squash"),
            CompactionTier::Summary => write!(f, "summary"),
            CompactionTier::ToolSpill => write!(f, "tool_spill"),
            CompactionTier::HistorySnapshot => write!(f, "history_snapshot"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionEvent {
    pub tier: CompactionTier,
    pub tokens_before: u32,
    pub tokens_after: u32,
    pub trigger: CompactionTrigger,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
pub enum AgentError {
    #[error("Unsupported tool: {name}")]
    UnsupportedTool { name: String },

    #[error("Tool repair failed for '{name}': {reason}")]
    RepairFailed { name: String, reason: String },

    #[error("Guardrail exceeded: {kind}")]
    GuardrailExceeded { kind: String },

    #[error("Model stopped before completion: {reason}")]
    ModelStopped { reason: String },

    #[error("Compaction failed (tier={tier}): {reason}")]
    CompactionFailed { tier: String, reason: String },

    #[error("Invalid state transition from '{from}' to '{to}'")]
    InvalidTransition { from: String, to: String },

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Context reference not found: {reference_id}")]
    ContextReferenceNotFound { reference_id: String },

    #[error("Context store unavailable: {reason}")]
    ContextStoreUnavailable { reason: String },

    #[error("Policy rejected: {reason}")]
    PolicyRejected { reason: String },
}

// ── Events ────────────────────────────────────────────────────────────────────

/// Observability events emitted by the engine during a session.
/// Accumulate internally; drained by `AgentEngine::drain_events()` (AGENT-049).
/// No message content or tool arguments — IDs only, safe to log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    HookInvoked {
        checkpoint: String,
    },
    TurnStarted {
        turn_index: u32,
        allow_tool_call: bool,
    },
    TurnCompleted {
        turn_index: u32,
        tool_call_count: u32,
        terminal: bool,
    },
    CompactionApplied(CompactionEvent),
    ContextPressure {
        ratio: f32,
    },
    ToolRepaired {
        original_name: String,
        resolved_name: String,
        confidence: f32,
    },
    ToolSoftFailure {
        tool_name: String,
        turn_index: u32,
    },
    /// Emitted when a kv_fixed session compacts and the transport must rebuild
    /// its KV cache from the updated history.
    KvCacheInvalidated {
        reason: String,
    },
    PreCompact {
        trigger: CompactionTrigger,
    },
    PostCompact {
        trigger: CompactionTrigger,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary_ref: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        history_ref: Option<String>,
    },
    ToolPolicyDenied {
        tool_name: String,
    },
    ToolTimedOut {
        tool_name: String,
        timeout_ms: u64,
    },
    /// User-visible assistant text synthesized by the engine (not streamed from the model).
    VisibleAssistantText {
        text: String,
    },
}

impl std::fmt::Display for AgentEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentEvent::HookInvoked { checkpoint } => {
                write!(f, "HookInvoked({checkpoint})")
            }
            AgentEvent::TurnStarted { turn_index, .. } => {
                write!(f, "TurnStarted(turn={turn_index})")
            }
            AgentEvent::TurnCompleted {
                turn_index,
                tool_call_count,
                ..
            } => {
                write!(
                    f,
                    "TurnCompleted(turn={turn_index}, tools={tool_call_count})"
                )
            }
            AgentEvent::CompactionApplied(e) => write!(f, "CompactionApplied(tier={})", e.tier),
            AgentEvent::ContextPressure { ratio } => write!(f, "ContextPressure(ratio={ratio:.2})"),
            AgentEvent::ToolRepaired {
                original_name,
                resolved_name,
                confidence,
            } => {
                write!(
                    f,
                    "ToolRepaired({original_name}->{resolved_name} conf={confidence:.2})"
                )
            }
            AgentEvent::ToolSoftFailure {
                tool_name,
                turn_index,
            } => {
                write!(f, "ToolSoftFailure(tool={tool_name} turn={turn_index})")
            }
            AgentEvent::KvCacheInvalidated { reason } => write!(f, "KvCacheInvalidated({reason})"),
            AgentEvent::PreCompact { trigger } => write!(f, "PreCompact({trigger:?})"),
            AgentEvent::PostCompact { trigger, .. } => write!(f, "PostCompact({trigger:?})"),
            AgentEvent::ToolPolicyDenied { tool_name } => {
                write!(f, "ToolPolicyDenied({tool_name})")
            }
            AgentEvent::ToolTimedOut {
                tool_name,
                timeout_ms,
            } => write!(f, "ToolTimedOut({tool_name}, timeout_ms={timeout_ms})"),
            AgentEvent::VisibleAssistantText { text } => {
                write!(f, "VisibleAssistantText({} chars)", text.len())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_error_display() {
        let e = AgentError::RepairFailed {
            name: "web_seach".into(),
            reason: "below threshold".into(),
        };
        assert!(e.to_string().contains("web_seach"));
    }

    #[test]
    fn agent_error_roundtrip() {
        let e = AgentError::GuardrailExceeded {
            kind: "max_turns".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: AgentError = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, AgentError::GuardrailExceeded { kind } if kind == "max_turns"));
    }

    #[test]
    fn compaction_event_roundtrip() {
        let e = CompactionEvent {
            tier: CompactionTier::Micro,
            tokens_before: 1000,
            tokens_after: 750,
            trigger: CompactionTrigger::MidTurn,
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: CompactionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tokens_after, 750);
        assert!(matches!(back.tier, CompactionTier::Micro));
    }

    #[test]
    fn agent_event_turn_started_roundtrip() {
        let ev = AgentEvent::TurnStarted {
            turn_index: 3,
            allow_tool_call: false,
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            AgentEvent::TurnStarted {
                turn_index: 3,
                allow_tool_call: false
            }
        ));
    }

    #[test]
    fn agent_event_kv_cache_invalidated_roundtrip() {
        let ev = AgentEvent::KvCacheInvalidated {
            reason: "Compacted: micro".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, AgentEvent::KvCacheInvalidated { reason } if reason.contains("micro"))
        );
    }

    #[test]
    fn all_agent_error_variants_serialize() {
        let variants: Vec<AgentError> = vec![
            AgentError::UnsupportedTool { name: "x".into() },
            AgentError::RepairFailed {
                name: "x".into(),
                reason: "y".into(),
            },
            AgentError::GuardrailExceeded {
                kind: "max_turns".into(),
            },
            AgentError::CompactionFailed {
                tier: "fifo".into(),
                reason: "empty".into(),
            },
            AgentError::InvalidTransition {
                from: "idle".into(),
                to: "done".into(),
            },
            AgentError::Internal("oops".into()),
        ];
        for v in &variants {
            let json = serde_json::to_string(v).unwrap();
            assert!(!json.is_empty());
        }
    }

    #[test]
    fn compaction_tier_display() {
        assert_eq!(
            CompactionTier::ArtifactSquash.to_string(),
            "artifact_squash"
        );
        assert_eq!(CompactionTier::Summary.to_string(), "summary");
    }
}
