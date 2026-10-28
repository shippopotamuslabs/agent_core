//! TDD Red — tool context index tests (S13, S14, E6, M6).

use agent_core::action::AgentAction;
use agent_core::catalog::ToolSpec;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta};
use agent_core::policy::ContextPolicyTier;
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::tool_name::ToolName;
use agent_core::tool_semantics::{ToolCatalogConfig, ToolSemanticsConfig};

fn tn(name: &str) -> ToolName {
    ToolName::new(name).expect("test tool name must be valid")
}

const SCHEMAS_JSON: &str = r#"[
  {"name":"web_search","description":"Search the web for information","parameters":{}},
  {"name":"append_blocks","description":"Append blocks to an artifact","parameters":{}}
]"#;

#[test]
fn s13_tool_schemas_are_deferred_into_compact_tool_context_index() {
    let cfg = AgentConfig {
        enable_tool_schema_deferral: true,
        ..Default::default()
    };
    let caps = SessionCapabilities {
        tool_schema_deferral_supported: true,
        context_policy_tier: ContextPolicyTier::HostedMutable,
        ..Default::default()
    };
    let eng = AgentEngine::new();
    let handle = eng
        .create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "hi".into(),
                metadata: MessageMeta::new(),
            }],
            cfg,
            caps,
            serde_json::Value::Null,
            SessionIoParams::new(
                "anthropic".into(),
                200_000,
                SCHEMAS_JSON.into(),
                "sys".into(),
            )
            .expect("valid test io params"),
        )
        .expect("valid config creates session");

    match eng.next_action(&handle) {
        AgentAction::CallModel(req) => {
            let index = req
                .tool_context_index
                .as_ref()
                .expect("ModelRequest must carry tool_context_index when deferral is enabled");
            let names: Vec<&str> = index
                .advertised_tools
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            assert_eq!(names, vec!["web_search", "append_blocks"]);
            // ENC-296: no schema text is stashed anymore (host owns the
            // registry), so the index carries advertised metadata only.
            assert!(
                eng.search_context_references(&handle, "web_search".into(), 5)
                    .is_empty(),
                "no schema text may be stashed in the ContextStore"
            );
            // Crude token estimate of tools_schema_json must drop sharply when
            // deferral is in effect.
            let token_estimate = req.tools_schema_json().len() as u32 / 4;
            assert!(
                token_estimate < 128,
                "tools_schema_json token estimate must be < 128, got {}",
                token_estimate
            );
        }
        other => panic!("expected CallModel, got {other:?}"),
    }
}

#[test]
fn s14_deferred_tool_schema_metadata_is_advertised_without_stash() {
    // ENC-296 re-pin: the engine no longer stashes schema text (host owns
    // the registry; full schemas are served provider-side). Drive the index
    // build through the engine's `next_action` and assert the advertised
    // metadata is present while the store holds no schema text: search
    // finds nothing to load.
    let cfg = AgentConfig {
        enable_tool_schema_deferral: true,
        ..Default::default()
    };
    let caps = SessionCapabilities {
        tool_schema_deferral_supported: true,
        ..Default::default()
    };

    let eng = AgentEngine::new();
    let handle = eng
        .create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "hi".into(),
                metadata: MessageMeta::new(),
            }],
            cfg,
            caps,
            serde_json::Value::Null,
            SessionIoParams::new(
                "anthropic".into(),
                200_000,
                SCHEMAS_JSON.into(),
                "sys".into(),
            )
            .expect("valid test io params"),
        )
        .expect("valid config creates session");

    let summaries = match eng.next_action(&handle) {
        AgentAction::CallModel(req) => req
            .tool_context_index
            .as_ref()
            .expect("tool_context_index expected when deferral enabled")
            .advertised_tools
            .clone(),
        other => panic!("expected CallModel, got {other:?}"),
    };

    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].name, "web_search");
    assert!(summaries[0].description_preview.contains("Search the web"));
    assert_eq!(summaries[1].name, "append_blocks");

    let hits = eng.search_context_references(&handle, "web_search".into(), 5);
    assert!(
        hits.is_empty(),
        "no schema text may be stashed for search/read-back, got {hits:?}"
    );
}

#[test]
fn e6_tool_schema_deferral_disabled_falls_back_to_active_schema_json() {
    let cfg = AgentConfig {
        enable_tool_schema_deferral: true,
        ..Default::default()
    };
    let caps = SessionCapabilities {
        tool_schema_deferral_supported: false,
        ..Default::default()
    };
    let eng = AgentEngine::new();
    let handle = eng
        .create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "hi".into(),
                metadata: MessageMeta::new(),
            }],
            cfg,
            caps,
            serde_json::Value::Null,
            SessionIoParams::new(
                "anthropic".into(),
                200_000,
                r#"[{"name":"web_search"}]"#.into(),
                "sys".into(),
            )
            .expect("valid test io params"),
        )
        .expect("valid config creates session");
    match eng.next_action(&handle) {
        AgentAction::CallModel(req) => {
            assert!(req.tool_context_index.is_none());
            assert_eq!(req.tools_schema_json(), r#"[{"name":"web_search"}]"#);
        }
        other => panic!("expected CallModel, got {other:?}"),
    }
}

#[test]
fn m6_tool_deferral_advertises_active_tools_without_stashing_schemas() {
    // ENC-296 re-pin: retrievability of full schemas moved host-side with
    // the registry. The engine-side guarantee is now that deferral keeps
    // advertising the active tools' metadata while stashing nothing: the
    // full schemas stay out of the model payload (no catalog here) and out
    // of the store.
    let cfg = AgentConfig {
        enable_tool_schema_deferral: true,
        ..Default::default()
    };
    let caps = SessionCapabilities {
        tool_schema_deferral_supported: true,
        ..Default::default()
    };
    let schemas = r#"[{"name":"rare_tool","description":"A rare tool"}]"#;

    let eng = AgentEngine::new();
    let handle = eng
        .create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "hi".into(),
                metadata: MessageMeta::new(),
            }],
            cfg,
            caps,
            serde_json::Value::Null,
            SessionIoParams::new("anthropic".into(), 200_000, schemas.into(), "sys".into())
                .expect("valid test io params"),
        )
        .expect("valid config creates session");

    // Drive the index build through the engine.
    let req = match eng.next_action(&handle) {
        AgentAction::CallModel(req) => req,
        other => panic!("expected CallModel, got {other:?}"),
    };

    let names: Vec<&str> = req
        .tool_context_index
        .as_ref()
        .expect("tool_context_index expected when deferral enabled")
        .advertised_tools
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, vec!["rare_tool"]);
    // No catalog: the full schemas stay out of the model payload.
    assert_eq!(req.tools_schema_json(), "[]");

    // And no schema text is stashed in the engine-owned store.
    let hits = eng.search_context_references(&handle, "rare_tool".into(), 5);
    assert!(
        hits.is_empty(),
        "no schema text may be stashed in the ContextStore, got {hits:?}"
    );
}

// ENC-297 D5 delta pin: legacy catalog-channel text (including the
// semantics-file '{}' object form, which previously counted as catalog
// presence via raw text) no longer counts. Catalog presence is the typed
// ToolCatalog only: an empty catalog + active tools still defers (index
// built from active, schemas withheld), built here through the fallible
// constructor to also cover that surface.
#[test]
fn enc_297_empty_typed_catalog_defers_active_schemas() {
    let semantics = ToolSemanticsConfig {
        catalog: ToolCatalogConfig { tools: Vec::new() },
        ..ToolSemanticsConfig::agnostic()
    };
    let mut cfg = AgentConfig::with_tool_semantics(semantics).expect("agnostic semantics valid");
    assert!(cfg.tool_catalog.is_empty());
    cfg.enable_tool_schema_deferral = true;
    let caps = SessionCapabilities {
        tool_schema_deferral_supported: true,
        context_policy_tier: ContextPolicyTier::HostedMutable,
        ..SessionCapabilities::default()
    };
    let eng = AgentEngine::new();
    let handle = eng
        .create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "hi".into(),
                metadata: MessageMeta::new(),
            }],
            cfg,
            caps,
            serde_json::Value::Null,
            SessionIoParams::new(
                "anthropic".into(),
                200_000,
                SCHEMAS_JSON.into(),
                "sys".into(),
            )
            .expect("valid test io params"),
        )
        .expect("valid config creates session");
    match eng.next_action(&handle) {
        AgentAction::CallModel(req) => {
            let names: Vec<&str> = req
                .tool_context_index
                .as_ref()
                .expect("index built from active tools despite empty catalog")
                .advertised_tools
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            assert_eq!(names, vec!["web_search", "append_blocks"]);
            assert_eq!(req.tools_schema_json(), "[]");
        }
        other => panic!("expected CallModel, got {other:?}"),
    }
}

// ENC-297 D5: a non-empty typed ToolCatalog counts as catalog presence, so
// the deferred view is lifted — the index is still advertised, but the full
// active schemas travel with the request.
#[test]
fn enc_297_nonempty_typed_catalog_shows_active_schemas() {
    let semantics = ToolSemanticsConfig {
        catalog: ToolCatalogConfig {
            tools: vec![
                ToolSpec {
                    name: tn("web_search"),
                    aliases: Vec::new(),
                    reproducible: true,
                    schema_hint: None,
                },
                ToolSpec {
                    name: tn("append_blocks"),
                    aliases: Vec::new(),
                    reproducible: false,
                    schema_hint: None,
                },
            ],
        },
        ..ToolSemanticsConfig::agnostic()
    };
    let mut cfg = AgentConfig::with_tool_semantics(semantics).expect("valid semantics");
    assert_eq!(cfg.tool_catalog.len(), 2);
    cfg.enable_tool_schema_deferral = true;
    let caps = SessionCapabilities {
        tool_schema_deferral_supported: true,
        context_policy_tier: ContextPolicyTier::HostedMutable,
        ..SessionCapabilities::default()
    };
    let eng = AgentEngine::new();
    let handle = eng
        .create_session(
            vec![AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: "hi".into(),
                metadata: MessageMeta::new(),
            }],
            cfg,
            caps,
            serde_json::Value::Null,
            SessionIoParams::new(
                "anthropic".into(),
                200_000,
                SCHEMAS_JSON.into(),
                "sys".into(),
            )
            .expect("valid test io params"),
        )
        .expect("valid config creates session");
    match eng.next_action(&handle) {
        AgentAction::CallModel(req) => {
            assert!(
                req.tool_context_index.is_some(),
                "index still advertised when catalog present"
            );
            let schemas: serde_json::Value =
                serde_json::from_str(&req.tools_schema_json()).expect("valid schemas json");
            let names: Vec<&str> = schemas
                .as_array()
                .expect("array")
                .iter()
                .map(|entry| entry.get("name").and_then(|name| name.as_str()).unwrap())
                .collect();
            assert_eq!(names, vec!["web_search", "append_blocks"]);
        }
        other => panic!("expected CallModel, got {other:?}"),
    }
}
