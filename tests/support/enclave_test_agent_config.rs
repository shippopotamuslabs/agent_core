use agent_core::session::AgentConfig;
use agent_core::tool_call_format::ToolCallFormat;

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

/// ENC-300: engine tests that feed envelope text declare the matching format
/// (the default config is [`ToolCallFormat::Structured`], which never mines
/// body text).
#[allow(dead_code)]
pub fn enclave_test_agent_config_for(format: ToolCallFormat) -> AgentConfig {
    AgentConfig {
        tool_semantics: enclave_test_semantics(),
        tool_call_format: format,
        ..Default::default()
    }
}
