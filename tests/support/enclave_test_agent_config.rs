use agent_core::session::AgentConfig;

use super::enclave_test_semantics::enclave_test_semantics;

/// Integration-test [`AgentConfig`] with Enclave-like tool semantics (not in SDK runtime).
pub fn enclave_test_agent_config() -> AgentConfig {
    let mut config = AgentConfig::default();
    config.tool_semantics = enclave_test_semantics();
    config
}
