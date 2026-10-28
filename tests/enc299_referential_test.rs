//! ENC-299 D4/D5 acceptance: construction-time referential validation.
//!
//! Every tool name referenced by a configured semantics section must resolve
//! to a catalog tool (canonical name or declared alias) or to an active
//! schema name, checked in `create_session` where the catalog and the parsed
//! schema set meet. Tests with dangling references fail construction; the fix
//! is to add the missing ToolSpec/schema, never to weaken validation.

#[path = "support/enclave_test_semantics.rs"]
mod enclave;

#[path = "support/tool_semantics_support.rs"]
mod semantics;

use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::tool_name::ToolName;
use agent_core::tool_semantics::{ToolArgumentRepairRule, ToolSemanticsConfig, ToolSemanticsError};
use serde_json::Value;

fn tn(name: &str) -> ToolName {
    ToolName::new(name).expect("test tool name must be valid")
}

fn try_session_err(semantics: ToolSemanticsConfig, schemas: &str) -> ToolSemanticsError {
    match try_session(semantics, schemas) {
        Err(error) => error,
        Ok(_) => panic!("expected session creation to fail"),
    }
}

fn try_session(
    semantics: ToolSemanticsConfig,
    schemas: &str,
) -> Result<agent_core::SessionHandle, ToolSemanticsError> {
    let eng = AgentEngine::new();
    eng.create_session(
        vec![AgentMessage {
            id: "u1".to_string(),
            role: AgentRole::User,
            content: "hi".to_string(),
            metadata: MessageMeta::new(),
        }],
        AgentConfig {
            tool_semantics: semantics,
            ..AgentConfig::default()
        },
        SessionCapabilities::default(),
        Value::Null,
        SessionIoParams::new(
            "anthropic".to_string(),
            200_000,
            schemas.to_string(),
            "sys".to_string(),
        )
        .expect("valid test io params"),
    )
}

#[test]
fn enc299_typo_in_argument_repairs_rejects_session() {
    // FRG-36 regression pin: a repair rule naming a tool that is in neither
    // the catalog nor the active schemas fails construction, naming the
    // section and the dangling name.
    let mut config = semantics::custom_semantics();
    config.argument_repairs[0].tool_name = tn("write_docc");

    assert_eq!(
        try_session_err(config, "[]"),
        ToolSemanticsError::UnknownToolReference {
            section: "argument_repairs.tool_name".to_string(),
            name: "write_docc".to_string(),
        }
    );
}

#[test]
fn enc299_rule_referencing_declared_alias_constructs() {
    // References resolve through declared aliases, not just canonical names.
    let mut doc = semantics::tool("doc");
    doc.aliases = vec![tn("document")];
    let config = ToolSemanticsConfig {
        catalog: agent_core::tool_semantics::ToolCatalogConfig { tools: vec![doc] },
        argument_repairs: vec![ToolArgumentRepairRule {
            tool_name: tn("document"),
            aliases: Vec::new(),
            special_values: Vec::new(),
        }],
        ..ToolSemanticsConfig::agnostic()
    };

    try_session(config, "[]").expect("alias reference must construct");
}

#[test]
fn enc299_rule_referencing_schema_only_tool_constructs() {
    // Known names are the union of catalog tools and active schema names:
    // the enclave fixture relies on this (test schema sets are subsets).
    let mut config = semantics::custom_semantics();
    config.argument_repairs.push(ToolArgumentRepairRule {
        tool_name: tn("schema_only_tool"),
        aliases: Vec::new(),
        special_values: Vec::new(),
    });

    try_session(config.clone(), r#"[{"name":"schema_only_tool"}]"#)
        .expect("schema-union reference must construct");
    // ...but without the schema (or a catalog entry) it is dangling.
    assert_eq!(
        try_session_err(config, "[]"),
        ToolSemanticsError::UnknownToolReference {
            section: "argument_repairs.tool_name".to_string(),
            name: "schema_only_tool".to_string(),
        }
    );
}

#[test]
fn enc299_dangling_reference_rejected_in_every_section() {
    // m5: parametrized pin across ALL validated sections — a ghost name in
    // any one section (all others valid) fails construction naming that
    // section. Sections: discovery, companions, artifact start/append,
    // repairs, shapes, policies, squash, greedy.
    type Mutator = Box<dyn Fn(&mut ToolSemanticsConfig)>;
    let cases: Vec<(&str, Mutator)> = vec![
        (
            "discovery.tool_name",
            Box::new(|config| {
                config
                    .discovery
                    .as_mut()
                    .expect("custom has discovery")
                    .tool_name = tn("ghost_tool");
            }),
        ),
        (
            "discovery.companion_callable_tools",
            Box::new(|config| {
                config
                    .discovery
                    .as_mut()
                    .expect("custom has discovery")
                    .companion_callable_tools = vec![tn("ghost_tool")];
            }),
        ),
        (
            "artifact.start_tool_name",
            Box::new(|config| {
                config
                    .artifact
                    .as_mut()
                    .expect("custom has artifact")
                    .start_tool_name = tn("ghost_tool");
            }),
        ),
        (
            "artifact.append_tool_name",
            Box::new(|config| {
                config
                    .artifact
                    .as_mut()
                    .expect("custom has artifact")
                    .append_tool_name = tn("ghost_tool");
            }),
        ),
        (
            "argument_repairs.tool_name",
            Box::new(|config| {
                config.argument_repairs[0].tool_name = tn("ghost_tool");
            }),
        ),
        (
            "canonical_shapes.tool_name",
            Box::new(|config| {
                config.canonical_shapes[0].tool_name = tn("ghost_tool");
            }),
        ),
        (
            "result_policies.tool_name",
            Box::new(|config| {
                config.result_policies[0].tool_name = tn("ghost_tool");
            }),
        ),
        (
            "compaction.artifact_squash.append_tool_name",
            Box::new(|config| {
                config
                    .compaction
                    .artifact_squash
                    .as_mut()
                    .expect("custom has squash")
                    .append_tool_name = tn("ghost_tool");
            }),
        ),
        (
            "greedy_extension_tools.tool_name",
            Box::new(|config| {
                config.greedy_extension_tools[0].tool_name = tn("ghost_tool");
            }),
        ),
    ];
    assert_eq!(cases.len(), 9, "one case per validated section");
    for (section, mutate) in cases {
        let mut config = semantics::custom_semantics();
        mutate(&mut config);
        assert_eq!(
            try_session_err(config, "[]"),
            ToolSemanticsError::UnknownToolReference {
                section: section.to_string(),
                name: "ghost_tool".to_string(),
            },
            "dangling name in {section} must name that section"
        );
    }
}

#[test]
fn enc299_unknown_companion_callable_rejects_session() {
    let mut config = semantics::custom_semantics();
    config
        .discovery
        .as_mut()
        .expect("custom semantics has discovery")
        .companion_callable_tools = vec![tn("ghost_tool")];

    assert_eq!(
        try_session_err(config, "[]"),
        ToolSemanticsError::UnknownToolReference {
            section: "discovery.companion_callable_tools".to_string(),
            name: "ghost_tool".to_string(),
        }
    );
}

#[test]
fn enc299_alias_shadowing_canonical_is_collision() {
    // D5: an alias colliding with another tool's canonical name is an
    // AliasCollision — otherwise the first-alias-wins catalog index would
    // silently shadow the canonical owner post-validation.
    let mut shadowed = semantics::tool("doc");
    shadowed.aliases = vec![tn("document")];
    let canonical = semantics::tool("document");
    let config = ToolSemanticsConfig {
        catalog: agent_core::tool_semantics::ToolCatalogConfig {
            tools: vec![canonical, shadowed],
        },
        ..ToolSemanticsConfig::agnostic()
    };

    assert_eq!(
        config.validate().expect_err("shadow must collide"),
        ToolSemanticsError::AliasCollision {
            alias: "document".to_string(),
            first_tool: "document".to_string(),
            second_tool: "doc".to_string(),
        }
    );
    // ...and session creation fails the same way (structural validation runs
    // before referential validation).
    assert_eq!(
        try_session_err(config, "[]"),
        ToolSemanticsError::AliasCollision {
            alias: "document".to_string(),
            first_tool: "document".to_string(),
            second_tool: "doc".to_string(),
        }
    );
}

#[test]
fn enc299_own_canonical_name_as_alias_is_allowed() {
    // An alias equal to its OWN canonical name is harmless and stays allowed.
    let mut doc = semantics::tool("doc");
    doc.aliases = vec![tn("doc")];
    let config = ToolSemanticsConfig {
        catalog: agent_core::tool_semantics::ToolCatalogConfig { tools: vec![doc] },
        ..ToolSemanticsConfig::agnostic()
    };

    try_session(config, "[]").expect("self-alias must construct");
}

#[test]
fn enc299_enclave_fixture_set_constructs_unchanged() {
    // The enclave fixture's references (8 names) all live in the shipped
    // 20-spec catalog, so the full set constructs with empty and populated
    // schema channels alike.
    let config = enclave::enclave_test_semantics();
    try_session(config.clone(), "[]").expect("enclave set with no schemas");
    try_session(
        config,
        r#"[{"name":"web_search"},{"name":"append_blocks"}]"#,
    )
    .expect("enclave set with schemas");
}

#[test]
fn enc299_agnostic_config_passes() {
    // Absent sections are skipped: the agnostic config always constructs.
    try_session(ToolSemanticsConfig::agnostic(), "[]").expect("agnostic constructs");
}
