//! UniFFI exports for Android / Kotlin (AGENT-021, AGENT-022).
//!
//! Callback traits are defined at the crate root (`lib.rs`) so UniFFI registers
//! `FfiConverter` for `Arc<dyn …Callback>` correctly.

mod callbacks;
mod policy_bridge;
mod types;

pub use callbacks::{UniFfiExecutor, UniFfiSummarizer, UniFfiTransport};
pub use policy_bridge::{
    repair_tool, result_json_looks_soft_failure, should_allow_tool, truncate_result, RepairResult,
};
pub use types::{
    AgentException, FfiAgentAction, FfiAgentConfig, FfiAgentEvent, FfiAgentMessage, FfiAgentRole,
    FfiCompactionEvent, FfiCompactionTier, FfiCompactionTrigger, FfiFinishReason, FfiModelRequest,
    FfiModelResponse, FfiRawToolCall, FfiSessionCapabilities, FfiSessionIoParams, FfiSummaryRequest,
    FfiSummaryResult, FfiToolCallRecord, FfiToolResultRecord,
};

use std::sync::Arc;

use crate::engine::{AgentEngine, SessionHandle};
use crate::{
    AgentSummarizerCallback, AgentToolExecutorCallback, AgentTransportCallback,
};

pub type FfiEngineInner = AgentEngine<UniFfiTransport, UniFfiExecutor, UniFfiSummarizer>;

/// Construct an [`AgentEngine`] wired to Kotlin callback implementations.
///
/// Callbacks are invoked on the **caller thread** (often a JNI worker thread). Kotlin must
/// dispatch heavy work to `Dispatchers.IO` and never block the Android main thread.
#[uniffi::export]
pub fn create_agent_engine(
    transport: Box<dyn AgentTransportCallback>,
    executor: Box<dyn AgentToolExecutorCallback>,
    summarizer: Option<Box<dyn AgentSummarizerCallback>>,
) -> Arc<FfiAgentEngine> {
    let sum: UniFfiSummarizer = match summarizer {
        Some(s) => UniFfiSummarizer::Callback(s),
        None => UniFfiSummarizer::NoOp,
    };
    Arc::new(FfiAgentEngine {
        inner: Arc::new(AgentEngine::new(
            Arc::new(UniFfiTransport(transport)),
            Arc::new(UniFfiExecutor(executor)),
            Arc::new(sum),
        )),
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
    ) -> Arc<FfiSession> {
        let ctx: serde_json::Value =
            serde_json::from_str(&session_context_json).unwrap_or(serde_json::Value::Null);
        let msgs: Vec<crate::message::AgentMessage> =
            initial_messages.into_iter().map(Into::into).collect();
        let handle = self.inner.create_session(
            msgs,
            config.into(),
            capabilities.into(),
            ctx,
            io.into(),
        );
        Arc::new(FfiSession { handle })
    }

    pub fn next_action(&self, session: &FfiSession) -> FfiAgentAction {
        self.inner.next_action(&session.handle).into()
    }

    pub fn submit_model_response(&self, session: &FfiSession, response: FfiModelResponse) {
        self.inner
            .submit_model_response(&session.handle, response.into());
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
}
