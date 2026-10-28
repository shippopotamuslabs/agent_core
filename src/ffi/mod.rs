//! UniFFI exports for Android / Kotlin (AGENT-021, AGENT-022).
//!
//! ENC-285: hosts drive the engine through the polling surface only; there are
//! no host callback parameters left on this bridge.

mod policy_bridge;
mod semantics;
mod stream_gate;
mod types;

pub use policy_bridge::{
    coerce_provider_arguments, has_local_tool_contract, parse_tool_calls, repair_tool,
    result_json_looks_soft_failure, should_allow_tool, truncate_result, RepairResult,
};
pub use semantics::{
    FfiArtifactSquashSemantics, FfiArtifactToolSemantics, FfiDefaultValue, FfiFieldHint,
    FfiParameterAlias, FfiSpecialValueRepair, FfiToolArgumentRepairRule, FfiToolCanonicalShape,
    FfiToolCompactionSemantics, FfiToolDiscoverySemantics, FfiToolGreedyExtensionSemantics,
    FfiToolResultPolicy, FfiToolSchemaHint, FfiToolSemantics, FfiToolSpec,
};
pub use stream_gate::{
    create_tool_stream_gate, FfiStreamGateFinish, FfiStreamGatePushResult, FfiToolStreamGate,
};
pub use types::{
    AgentException, FfiAgentAction, FfiAgentConfig, FfiAgentEvent, FfiAgentMessage, FfiAgentRole,
    FfiCompactionBoundary, FfiCompactionEvent, FfiCompactionTier, FfiCompactionTrigger,
    FfiContextPolicyTier, FfiContextReference, FfiContextReferenceKind, FfiFinishReason,
    FfiManualCompactRequest, FfiManualCompactResult, FfiModelRequest, FfiModelResponse,
    FfiProviderContextSnapshot, FfiRawToolCall, FfiSessionCapabilities, FfiSessionIoParams,
    FfiSummaryRequest, FfiSummaryResult, FfiToolCallFormat, FfiToolCallRecord, FfiToolParseBatch,
    FfiToolParseFailure, FfiToolParseFailureReason, FfiToolParseField, FfiToolResultRecord,
    FfiToolSchemaEntry,
};

use std::sync::Arc;

use crate::engine::{AgentEngine, SessionHandle};

pub type FfiEngineInner = AgentEngine;

/// Construct an [`AgentEngine`].
///
/// ENC-285: construction takes no host callbacks. The engine is driven exclusively
/// through the polling surface (`create_session` / `next_action` / `submit_*`);
/// the former `transport` / `executor` / `summarizer` callback parameters were
/// decorative DI — stored but never invoked — and were removed with the
/// `AgentEngine<T, E, S>` generics.
#[uniffi::export]
pub fn create_agent_engine() -> Arc<FfiAgentEngine> {
    Arc::new(FfiAgentEngine {
        inner: Arc::new(AgentEngine::new()),
    })
}

#[derive(uniffi::Object)]
pub struct FfiAgentEngine {
    inner: Arc<FfiEngineInner>,
}

#[derive(uniffi::Object)]
pub struct FfiSession {
    pub(crate) handle: SessionHandle,
}

#[uniffi::export]
impl FfiAgentEngine {
    pub fn create_session(
        &self,
        initial_messages: Vec<FfiAgentMessage>,
        config: FfiAgentConfig,
        capabilities: FfiSessionCapabilities,
        session_context_json: String,
        io: FfiSessionIoParams,
    ) -> Result<Arc<FfiSession>, AgentException> {
        let ctx: serde_json::Value =
            serde_json::from_str(&session_context_json).unwrap_or(serde_json::Value::Null);
        let msgs: Vec<crate::message::AgentMessage> =
            initial_messages.into_iter().map(Into::into).collect();
        let io = types::session_io_params(io)?;
        // ENC-297 D2: fallible typed conversion — invalid semantics/catalog
        // is an AgentException, never an eprintln + agnostic fallback.
        let config = types::agent_config(config)?;
        // ENC-297 J1: engine validation failure surfaces as a typed
        // AgentException — never a live session with a stale catalog.
        let handle = self
            .inner
            .create_session(msgs, config, capabilities.into(), ctx, io)
            .map_err(|error| AgentException::Internal {
                detail: format!("invalid tool semantics: {error:?}"),
            })?;
        Ok(Arc::new(FfiSession { handle }))
    }

    pub fn next_action(&self, session: &FfiSession) -> FfiAgentAction {
        self.inner.next_action(&session.handle).into()
    }

    pub fn submit_model_response(&self, session: &FfiSession, response: FfiModelResponse) {
        self.inner
            .submit_model_response(&session.handle, response.into());
    }

    pub fn submit_model_turn_failure(
        &self,
        session: &FfiSession,
        report: types::FfiModelTurnFailureReport,
    ) {
        self.inner
            .submit_model_turn_failure(&session.handle, report.into());
    }

    pub fn submit_tool_results(&self, session: &FfiSession, results: Vec<FfiToolResultRecord>) {
        self.inner.submit_tool_results(
            &session.handle,
            results.into_iter().map(Into::into).collect(),
        );
    }

    pub fn submit_summary(&self, session: &FfiSession, summary: FfiSummaryResult) {
        self.inner.submit_summary(&session.handle, summary.into());
    }

    pub fn cancel(&self, session: &FfiSession) {
        self.inner.cancel(&session.handle);
    }

    pub fn drain_events(&self, session: &FfiSession) -> Vec<FfiAgentEvent> {
        self.inner
            .drain_events(&session.handle)
            .into_iter()
            .map(Into::into)
            .collect()
    }

    pub fn request_manual_compact(
        &self,
        session: &FfiSession,
        request: FfiManualCompactRequest,
    ) -> FfiAgentAction {
        self.inner
            .request_manual_compact(&session.handle, request.into())
            .into()
    }

    pub fn read_context_reference(
        &self,
        session: &FfiSession,
        reference_id: String,
    ) -> Result<String, AgentException> {
        self.inner
            .read_context_reference(&session.handle, reference_id)
            .map_err(Into::into)
    }

    pub fn search_context_references(
        &self,
        session: &FfiSession,
        query: String,
        limit: u32,
    ) -> Vec<FfiContextReference> {
        self.inner
            .search_context_references(&session.handle, query, limit as usize)
            .into_iter()
            .map(Into::into)
            .collect()
    }

    pub fn insert_terminal_output(
        &self,
        session: &FfiSession,
        id: String,
        title: String,
        content: String,
    ) -> FfiContextReference {
        self.inner
            .insert_terminal_output(&session.handle, id, title, content)
            .into()
    }
}
