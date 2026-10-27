#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();

#[cfg(feature = "ffi")]
pub mod ffi;

#[cfg(feature = "ffi")]
pub use ffi::{
    coerce_provider_arguments, create_agent_engine, create_tool_stream_gate,
    has_local_tool_contract, parse_tool_calls, repair_tool, result_json_looks_soft_failure,
    should_allow_tool, truncate_result, AgentException, FfiAgentAction, FfiAgentConfig,
    FfiAgentEngine, FfiAgentEvent, FfiAgentMessage, FfiAgentRole, FfiCompactionBoundary,
    FfiCompactionEvent, FfiCompactionTier, FfiCompactionTrigger, FfiContextPolicyTier,
    FfiContextReference, FfiContextReferenceKind, FfiFinishReason, FfiManualCompactRequest,
    FfiManualCompactResult, FfiModelRequest, FfiModelResponse, FfiProviderContextSnapshot,
    FfiRawToolCall, FfiSession, FfiSessionCapabilities, FfiSessionIoParams, FfiStreamGateFinish,
    FfiStreamGatePushResult, FfiSummaryRequest, FfiSummaryResult, FfiToolCallRecord,
    FfiToolParseBatch, FfiToolParseFailure, FfiToolParseFailureReason, FfiToolParseField,
    FfiToolResultRecord, FfiToolSchemaEntry, FfiToolStreamGate, RepairResult,
};

pub mod action;
pub mod agent_tool_budget;
pub mod artifact_finalize;
pub mod budget;
pub mod canonicalize;
pub mod catalog;
pub mod compact;
pub mod context_store;
pub mod engine;
pub mod events;
pub mod file_context;
pub mod forgecode_gap_compaction;
pub mod guardrails;
pub mod json_repair;
#[cfg(feature = "lalr-harness")]
#[doc(hidden)]
pub mod lalr_harness;
pub mod long_loop_policy;
pub mod loop_hooks;
pub mod loop_metrics;
pub mod manual_compact;
pub mod message;
pub mod model_api_retry;
pub mod parsed_tool_args;
pub mod policy;
pub mod repair;
pub mod request_pipeline;
pub mod schema_coerce;
pub mod session;
pub mod sidecar_builder;
pub mod state_ledger;
pub mod tool_context;
pub mod tool_name_match;
pub mod tool_parse;
pub mod tool_policy;
pub mod tool_schema;
pub mod tool_schema_activation;
pub mod tool_semantics;
pub mod tool_stream_gate;
pub mod transcript_revision;
pub mod truncate;
pub mod turn_io;

pub use engine::{AgentEngine, SessionHandle, SessionIoParams};
