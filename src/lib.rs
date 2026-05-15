uniffi::setup_scaffolding!();

pub mod events;
pub mod message;
pub mod session;

/// Stub top-level error for initial bindgen smoke test.
/// Full error enum added in AGENT-006.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AgentError {
    #[error("Internal error: {0}")]
    Internal(String),
}
