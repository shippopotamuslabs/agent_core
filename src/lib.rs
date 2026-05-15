uniffi::setup_scaffolding!();

/// Stub error for initial bindgen smoke test.
/// Full error types added in AGENT-006.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AgentError {
    #[error("Internal error: {0}")]
    Internal(String),
}
