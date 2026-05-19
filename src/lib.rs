uniffi::setup_scaffolding!();

pub mod ffi;

pub use ffi::{
    coerce_provider_arguments, create_agent_engine, create_tool_stream_gate,
    has_local_tool_contract, parse_tool_calls, repair_tool, result_json_looks_soft_failure,
    should_allow_tool, truncate_result, AgentException, FfiAgentAction, FfiAgentConfig,
    FfiAgentEngine, FfiAgentEvent, FfiAgentMessage, FfiAgentRole, FfiCompactionEvent,
    FfiCompactionTier, FfiCompactionTrigger, FfiContextPolicyTier, FfiContextReference,
    FfiContextReferenceKind, FfiFinishReason, FfiManualCompactRequest, FfiManualCompactResult,
    FfiModelRequest, FfiModelResponse, FfiRawToolCall, FfiSession, FfiSessionCapabilities,
    FfiSessionIoParams, FfiStreamGateFinish, FfiStreamGatePushResult, FfiSummaryRequest,
    FfiSummaryResult, FfiToolCallRecord, FfiToolParseBatch, FfiToolParseFailure,
    FfiToolParseFailureReason, FfiToolParseField, FfiToolResultRecord, FfiToolStreamGate,
    RepairResult,
};

/// Kotlin / host transport — UniFFI callback (crate root required for UniFFI metadata).
#[uniffi::export(callback_interface)]
pub trait AgentTransportCallback: Send + Sync {
    fn complete_model_turn(&self, request: FfiModelRequest) -> FfiModelResponse;
}

#[uniffi::export(callback_interface)]
pub trait AgentToolExecutorCallback: Send + Sync {
    fn execute_tools(&self, calls: Vec<FfiToolCallRecord>) -> Vec<FfiToolResultRecord>;
}

#[uniffi::export(callback_interface)]
pub trait AgentSummarizerCallback: Send + Sync {
    fn summarize(&self, request: FfiSummaryRequest) -> FfiSummaryResult;
}

pub mod action;
pub mod artifact_finalize;
pub mod budget;
pub mod catalog;
pub mod compact;
pub mod context_store;
pub mod engine;
pub mod events;
pub mod file_context;
pub mod forgecode_gap_compaction;
pub mod guardrails;
pub mod host;
pub mod loop_hooks;
pub mod loop_metrics;
pub mod manual_compact;
pub mod message;
pub mod policy;
pub mod repair;
pub mod request_pipeline;
pub mod session;
pub mod state_ledger;
pub mod tool_context;
pub mod tool_parse;
pub mod tool_stream_gate;
pub mod tool_policy;
pub mod truncate;
pub mod turn_io;

pub use engine::{AgentEngine, SessionHandle, SessionIoParams};
