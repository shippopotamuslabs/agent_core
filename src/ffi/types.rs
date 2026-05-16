//! UniFFI-friendly DTOs (no `serde_json::Value` in the FFI boundary).

use crate::message::{AgentMessage, AgentRole, RepairRecord, ToolCallRecord};
use crate::session::{AgentConfig, SessionCapabilities};
use crate::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall, SummaryRequest, SummaryResult};

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiAgentRole {
    System,
    User,
    Assistant,
    Tool,
    CompactSummary,
}

impl From<AgentRole> for FfiAgentRole {
    fn from(r: AgentRole) -> Self {
        match r {
            AgentRole::System => FfiAgentRole::System,
            AgentRole::User => FfiAgentRole::User,
            AgentRole::Assistant => FfiAgentRole::Assistant,
            AgentRole::Tool => FfiAgentRole::Tool,
            AgentRole::CompactSummary => FfiAgentRole::CompactSummary,
        }
    }
}

impl From<FfiAgentRole> for AgentRole {
    fn from(r: FfiAgentRole) -> Self {
        match r {
            FfiAgentRole::System => AgentRole::System,
            FfiAgentRole::User => AgentRole::User,
            FfiAgentRole::Assistant => AgentRole::Assistant,
            FfiAgentRole::Tool => AgentRole::Tool,
            FfiAgentRole::CompactSummary => AgentRole::CompactSummary,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAgentMessage {
    pub id: String,
    pub role: FfiAgentRole,
    pub content: String,
    /// JSON object or array as a string (e.g. `"{}"` or `"null"`).
    pub metadata_json: String,
}

impl From<FfiAgentMessage> for AgentMessage {
    fn from(m: FfiAgentMessage) -> Self {
        let metadata = serde_json::from_str(&m.metadata_json).unwrap_or(serde_json::Value::Null);
        Self {
            id: m.id,
            role: m.role.into(),
            content: m.content,
            metadata,
        }
    }
}

impl From<AgentMessage> for FfiAgentMessage {
    fn from(m: AgentMessage) -> Self {
        Self {
            id: m.id,
            role: m.role.into(),
            content: m.content,
            metadata_json: serde_json::to_string(&m.metadata).unwrap_or_else(|_| "null".into()),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiModelRequest {
    pub session_messages: Vec<FfiAgentMessage>,
    pub system_prompt: String,
    pub tools_schema_json: String,
    pub allow_tool_call: bool,
}

impl From<FfiModelRequest> for ModelRequest {
    fn from(m: FfiModelRequest) -> Self {
        Self {
            session_messages: m.session_messages.into_iter().map(Into::into).collect(),
            system_prompt: m.system_prompt,
            tools_schema_json: m.tools_schema_json,
            allow_tool_call: m.allow_tool_call,
        }
    }
}

impl From<ModelRequest> for FfiModelRequest {
    fn from(m: ModelRequest) -> Self {
        Self {
            session_messages: m.session_messages.into_iter().map(Into::into).collect(),
            system_prompt: m.system_prompt,
            tools_schema_json: m.tools_schema_json,
            allow_tool_call: m.allow_tool_call,
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiFinishReason {
    ToolUse,
    Stop,
    MaxTokens,
    Other { reason: String },
}

impl From<FinishReason> for FfiFinishReason {
    fn from(r: FinishReason) -> Self {
        match r {
            FinishReason::ToolUse => FfiFinishReason::ToolUse,
            FinishReason::Stop => FfiFinishReason::Stop,
            FinishReason::MaxTokens => FfiFinishReason::MaxTokens,
            FinishReason::Other(s) => FfiFinishReason::Other { reason: s },
        }
    }
}

impl From<FfiFinishReason> for FinishReason {
    fn from(r: FfiFinishReason) -> Self {
        match r {
            FfiFinishReason::ToolUse => FinishReason::ToolUse,
            FfiFinishReason::Stop => FinishReason::Stop,
            FfiFinishReason::MaxTokens => FinishReason::MaxTokens,
            FfiFinishReason::Other { reason } => FinishReason::Other(reason),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiRawToolCall {
    pub id: Option<String>,
    pub name: String,
    pub arguments_json: String,
}

impl From<FfiRawToolCall> for RawToolCall {
    fn from(t: FfiRawToolCall) -> Self {
        Self {
            id: t.id,
            name: t.name,
            arguments_json: t.arguments_json,
        }
    }
}

impl From<RawToolCall> for FfiRawToolCall {
    fn from(t: RawToolCall) -> Self {
        Self {
            id: t.id,
            name: t.name,
            arguments_json: t.arguments_json,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiModelResponse {
    pub assistant_text: Option<String>,
    pub tool_calls: Vec<FfiRawToolCall>,
    pub finish_reason: FfiFinishReason,
}

impl From<FfiModelResponse> for ModelResponse {
    fn from(m: FfiModelResponse) -> Self {
        Self {
            assistant_text: m.assistant_text,
            tool_calls: m.tool_calls.into_iter().map(Into::into).collect(),
            finish_reason: m.finish_reason.into(),
        }
    }
}

impl From<ModelResponse> for FfiModelResponse {
    fn from(m: ModelResponse) -> Self {
        Self {
            assistant_text: m.assistant_text,
            tool_calls: m.tool_calls.into_iter().map(Into::into).collect(),
            finish_reason: m.finish_reason.into(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiRepairRecord {
    pub original_name: String,
    pub applied_aliases: Vec<String>,
    pub confidence: f32,
}

impl From<RepairRecord> for FfiRepairRecord {
    fn from(r: RepairRecord) -> Self {
        Self {
            original_name: r.original_name,
            applied_aliases: r.applied_aliases,
            confidence: r.confidence,
        }
    }
}

impl From<FfiRepairRecord> for RepairRecord {
    fn from(r: FfiRepairRecord) -> Self {
        Self {
            original_name: r.original_name,
            applied_aliases: r.applied_aliases,
            confidence: r.confidence,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolCallRecord {
    pub call_id: String,
    pub name: String,
    pub arguments_json: String,
    pub repair_metadata: Option<FfiRepairRecord>,
}

impl From<FfiToolCallRecord> for ToolCallRecord {
    fn from(t: FfiToolCallRecord) -> Self {
        Self {
            call_id: t.call_id,
            name: t.name,
            arguments_json: t.arguments_json,
            repair_metadata: t.repair_metadata.map(Into::into),
        }
    }
}

impl From<ToolCallRecord> for FfiToolCallRecord {
    fn from(t: ToolCallRecord) -> Self {
        Self {
            call_id: t.call_id,
            name: t.name,
            arguments_json: t.arguments_json,
            repair_metadata: t.repair_metadata.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolResultRecord {
    pub call_id: String,
    pub result_json: String,
    pub truncated: bool,
    pub soft_failure: bool,
}

impl From<FfiToolResultRecord> for crate::message::ToolResultRecord {
    fn from(t: FfiToolResultRecord) -> Self {
        Self {
            call_id: t.call_id,
            result_json: t.result_json,
            truncated: t.truncated,
            soft_failure: t.soft_failure,
        }
    }
}

impl From<crate::message::ToolResultRecord> for FfiToolResultRecord {
    fn from(t: crate::message::ToolResultRecord) -> Self {
        Self {
            call_id: t.call_id,
            result_json: t.result_json,
            truncated: t.truncated,
            soft_failure: t.soft_failure,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSummaryRequest {
    pub messages_to_summarize: Vec<FfiAgentMessage>,
    pub instruction: String,
    pub last_message_id: String,
}

impl From<FfiSummaryRequest> for SummaryRequest {
    fn from(s: FfiSummaryRequest) -> Self {
        Self {
            messages_to_summarize: s.messages_to_summarize.into_iter().map(Into::into).collect(),
            instruction: s.instruction,
            last_message_id: s.last_message_id,
        }
    }
}

impl From<SummaryRequest> for FfiSummaryRequest {
    fn from(s: SummaryRequest) -> Self {
        Self {
            messages_to_summarize: s.messages_to_summarize.into_iter().map(Into::into).collect(),
            instruction: s.instruction,
            last_message_id: s.last_message_id,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSummaryResult {
    pub summary_text: String,
    pub last_message_id: String,
}

impl From<FfiSummaryResult> for SummaryResult {
    fn from(s: FfiSummaryResult) -> Self {
        Self {
            summary_text: s.summary_text,
            last_message_id: s.last_message_id,
        }
    }
}

impl From<SummaryResult> for FfiSummaryResult {
    fn from(s: SummaryResult) -> Self {
        Self {
            summary_text: s.summary_text,
            last_message_id: s.last_message_id,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSessionIoParams {
    pub provider: String,
    pub context_window_tokens: u32,
    pub tools_schema_json: String,
    pub system_prompt: String,
}

impl From<FfiSessionIoParams> for crate::engine::SessionIoParams {
    fn from(p: FfiSessionIoParams) -> Self {
        Self {
            provider: p.provider,
            context_window_tokens: p.context_window_tokens,
            tools_schema_json: p.tools_schema_json,
            system_prompt: p.system_prompt,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAgentConfig {
    pub max_turns: u32,
    pub threshold_ratio: f32,
    pub micro_keep_recent: u32,
    pub tool_retry_limit: u32,
    pub greedy_append_blocks: bool,
    pub tool_result_budget_cloud_bytes: u64,
}

impl Default for FfiAgentConfig {
    fn default() -> Self {
        let c = AgentConfig::default();
        Self {
            max_turns: c.max_turns,
            threshold_ratio: c.threshold_ratio,
            micro_keep_recent: c.micro_keep_recent,
            tool_retry_limit: c.tool_retry_limit,
            greedy_append_blocks: c.greedy_append_blocks,
            tool_result_budget_cloud_bytes: c.tool_result_budget_cloud_bytes as u64,
        }
    }
}

impl From<FfiAgentConfig> for AgentConfig {
    fn from(f: FfiAgentConfig) -> Self {
        Self {
            max_turns: f.max_turns,
            threshold_ratio: f.threshold_ratio,
            micro_keep_recent: f.micro_keep_recent,
            tool_retry_limit: f.tool_retry_limit,
            greedy_append_blocks: f.greedy_append_blocks,
            tool_result_budget_cloud_bytes: f.tool_result_budget_cloud_bytes as usize,
            token_correction: std::collections::HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSessionCapabilities {
    pub summarization_allowed: bool,
    pub kv_fixed: bool,
    pub tool_result_budget_tokens: Option<u32>,
}

impl From<FfiSessionCapabilities> for SessionCapabilities {
    fn from(f: FfiSessionCapabilities) -> Self {
        Self {
            summarization_allowed: f.summarization_allowed,
            kv_fixed: f.kv_fixed,
            tool_result_budget_tokens: f.tool_result_budget_tokens,
        }
    }
}

// ── Actions / events / errors (FFI) ─────────────────────────────────────────

use crate::action::AgentAction;
use crate::events::{AgentError, AgentEvent, CompactionEvent, CompactionTier, CompactionTrigger};

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiCompactionTrigger {
    Preflight,
    MidTurn,
    PostToolResult,
}

impl From<CompactionTrigger> for FfiCompactionTrigger {
    fn from(t: CompactionTrigger) -> Self {
        match t {
            CompactionTrigger::Preflight => FfiCompactionTrigger::Preflight,
            CompactionTrigger::MidTurn => FfiCompactionTrigger::MidTurn,
            CompactionTrigger::PostToolResult => FfiCompactionTrigger::PostToolResult,
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiCompactionTier {
    Micro,
    Fifo,
    ArtifactSquash,
    Summary,
}

impl From<CompactionTier> for FfiCompactionTier {
    fn from(t: CompactionTier) -> Self {
        match t {
            CompactionTier::Micro => FfiCompactionTier::Micro,
            CompactionTier::Fifo => FfiCompactionTier::Fifo,
            CompactionTier::ArtifactSquash => FfiCompactionTier::ArtifactSquash,
            CompactionTier::Summary => FfiCompactionTier::Summary,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiCompactionEvent {
    pub tier: FfiCompactionTier,
    pub tokens_before: u32,
    pub tokens_after: u32,
    pub trigger: FfiCompactionTrigger,
}

impl From<CompactionEvent> for FfiCompactionEvent {
    fn from(e: CompactionEvent) -> Self {
        Self {
            tier: e.tier.into(),
            tokens_before: e.tokens_before,
            tokens_after: e.tokens_after,
            trigger: e.trigger.into(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiAgentEvent {
    TurnStarted {
        turn_index: u32,
        allow_tool_call: bool,
    },
    TurnCompleted {
        turn_index: u32,
        tool_call_count: u32,
        terminal: bool,
    },
    CompactionApplied {
        event: FfiCompactionEvent,
    },
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
    KvCacheInvalidated {
        reason: String,
    },
}

impl From<AgentEvent> for FfiAgentEvent {
    fn from(ev: AgentEvent) -> Self {
        match ev {
            AgentEvent::TurnStarted {
                turn_index,
                allow_tool_call,
            } => FfiAgentEvent::TurnStarted {
                turn_index,
                allow_tool_call,
            },
            AgentEvent::TurnCompleted {
                turn_index,
                tool_call_count,
                terminal,
            } => FfiAgentEvent::TurnCompleted {
                turn_index,
                tool_call_count,
                terminal,
            },
            AgentEvent::CompactionApplied(e) => FfiAgentEvent::CompactionApplied {
                event: e.into(),
            },
            AgentEvent::ContextPressure { ratio } => FfiAgentEvent::ContextPressure { ratio },
            AgentEvent::ToolRepaired {
                original_name,
                resolved_name,
                confidence,
            } => FfiAgentEvent::ToolRepaired {
                original_name,
                resolved_name,
                confidence,
            },
            AgentEvent::ToolSoftFailure {
                tool_name,
                turn_index,
            } => FfiAgentEvent::ToolSoftFailure {
                tool_name,
                turn_index,
            },
            AgentEvent::KvCacheInvalidated { reason } => FfiAgentEvent::KvCacheInvalidated { reason },
        }
    }
}

#[derive(Debug, Clone, thiserror::Error, uniffi::Error)]
pub enum AgentException {
    #[error("repair failed")]
    RepairFailed { name: String, reason: String },
    #[error("guardrail")]
    GuardrailExceeded { kind: String },
    #[error("compaction")]
    CompactionFailed { tier: String, reason: String },
    #[error("invalid transition")]
    InvalidTransition { from: String, to: String },
    #[error("internal")]
    Internal { detail: String },
}

impl From<AgentError> for AgentException {
    fn from(e: AgentError) -> Self {
        match e {
            AgentError::UnsupportedTool { name } => AgentException::Internal {
                detail: format!("Unsupported tool: {name}"),
            },
            AgentError::RepairFailed { name, reason } => AgentException::RepairFailed { name, reason },
            AgentError::GuardrailExceeded { kind } => AgentException::GuardrailExceeded { kind },
            AgentError::CompactionFailed { tier, reason } => AgentException::CompactionFailed { tier, reason },
            AgentError::InvalidTransition { from, to } => AgentException::InvalidTransition { from, to },
            AgentError::Internal(m) => AgentException::Internal { detail: m },
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiAgentAction {
    CallModel { request: FfiModelRequest },
    ExecuteTools { calls: Vec<FfiToolCallRecord> },
    RequestSummary { request: FfiSummaryRequest },
    Finished,
    Failed { error: AgentException },
}

impl From<AgentAction> for FfiAgentAction {
    fn from(a: AgentAction) -> Self {
        match a {
            AgentAction::CallModel(r) => FfiAgentAction::CallModel {
                request: r.into(),
            },
            AgentAction::ExecuteTools(calls) => FfiAgentAction::ExecuteTools {
                calls: calls.into_iter().map(Into::into).collect(),
            },
            AgentAction::RequestSummary(req) => FfiAgentAction::RequestSummary {
                request: req.into(),
            },
            AgentAction::Finished => FfiAgentAction::Finished,
            AgentAction::Failed(e) => FfiAgentAction::Failed { error: e.into() },
        }
    }
}
