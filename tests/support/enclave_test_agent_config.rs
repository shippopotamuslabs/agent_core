use agent_core::session::AgentConfig;

use super::enclave_test_semantics::enclave_test_semantics;

/// Integration-test [`AgentConfig`] with Enclave-like tool semantics (not in SDK runtime).
// Used by only some test binaries; each binary compiles this support module
// separately, so unused-in-this-binary helpers must not fail the build.
#[allow(dead_code)]
pub fn enclave_test_agent_config() -> AgentConfig {
    AgentConfig {
        tool_semantics: enclave_test_semantics(),
        ..Default::default()
    }
}
