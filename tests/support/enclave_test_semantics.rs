use agent_core::catalog::{ToolCatalog, ToolSpec};
use agent_core::tool_parse::{parse_tool_calls_with_semantics, ToolParseBatch};
use agent_core::tool_semantics::{ToolCatalogConfig, ToolSemanticsConfig};

/// Enclave-like semantics for integration tests only (not part of the SDK runtime).
pub fn enclave_test_semantics() -> ToolSemanticsConfig {
    let mut semantics: ToolSemanticsConfig = serde_json::from_str(include_str!(
        "enclave_tool_semantics.json"
    ))
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

pub fn enclave_test_catalog() -> ToolCatalog {
    ToolCatalog::from_semantics(&enclave_test_semantics()).expect("enclave test catalog")
}

pub fn parse_enclave_tool_calls(raw: &str) -> ToolParseBatch {
    let semantics = enclave_test_semantics();
    let catalog = enclave_test_catalog();
    parse_tool_calls_with_semantics(raw, &catalog, &semantics)
}
