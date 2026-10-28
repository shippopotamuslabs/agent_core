use agent_core::catalog::{ToolCatalog, ToolSpec};
use agent_core::tool_call_format::ToolCallFormat;
use agent_core::tool_parse::{parse_tool_calls_with_semantics, ToolParseBatch};
use agent_core::tool_semantics::{ToolCatalogConfig, ToolSemanticsConfig};

/// Enclave-like semantics for integration tests only (not part of the SDK runtime).
pub fn enclave_test_semantics() -> ToolSemanticsConfig {
    let mut semantics: ToolSemanticsConfig =
        serde_json::from_str(include_str!("enclave_tool_semantics.json"))
            .expect("enclave_tool_semantics.json");
    let raw = include_str!("../../resources/tool_catalog.json");
    #[derive(serde::Deserialize)]
    struct CatalogFile {
        tools: Vec<ToolSpec>,
    }
    let catalog = serde_json::from_str::<CatalogFile>(raw).expect("tool_catalog.json");
    semantics.catalog = ToolCatalogConfig {
        tools: catalog.tools,
    };
    semantics
}

// Used by only some test binaries; each binary compiles this support module
// separately, so unused-in-this-binary helpers must not fail the build.
#[allow(dead_code)]
pub fn enclave_test_catalog() -> ToolCatalog {
    ToolCatalog::from_semantics(&enclave_test_semantics()).expect("enclave test catalog")
}

// Used by only some test binaries; each binary compiles this support module
// separately, so unused-in-this-binary helpers must not fail the build.
//
// ENC-300: every format test passes its declared format — there is no
// sniffing fallback, so the helper cannot pick one.
#[allow(dead_code)]
pub fn parse_enclave_tool_calls(raw: &str, format: ToolCallFormat) -> ToolParseBatch {
    let semantics = enclave_test_semantics();
    let catalog = enclave_test_catalog();
    parse_tool_calls_with_semantics(format, raw, &catalog, &semantics)
}
