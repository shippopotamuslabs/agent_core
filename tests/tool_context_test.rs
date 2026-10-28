//! TDD Red — tool context index tests (S13, S14, E6, M6).

use agent_core::action::AgentAction;
use agent_core::context_store::ContextReferenceKind;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta};
use agent_core::policy::ContextPolicyTier;
use agent_core::session::{AgentConfig, SessionCapabilities};

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
    let handle = eng.create_session(
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
            "[]".into(),
            "[]".into(),
            SCHEMAS_JSON.into(),
            "sys".into(),
        )
        .expect("valid test io params"),
    );

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
            assert!(
                index.full_schema_ref.starts_with("tool-schema-"),
                "full_schema_ref must start with 'tool-schema-', got {}",
                index.full_schema_ref
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
fn s14_deferred_tool_schema_metadata_can_be_searched_and_loaded() {
    // Drive the index build through the engine's `next_action`, which the
    // spec says is the entry point that registers tool schemas into the
    // engine-owned ContextStore when deferral is enabled. Then search and
    // read back through the engine APIs so both ends share the same store.
    let cfg = AgentConfig {
        enable_tool_schema_deferral: true,
        ..Default::default()
    };
    let caps = SessionCapabilities {
        tool_schema_deferral_supported: true,
        ..Default::default()
    };

    let eng = AgentEngine::new();
    let handle = eng.create_session(
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
            "[]".into(),
            "[]".into(),
            SCHEMAS_JSON.into(),
            "sys".into(),
        )
        .expect("valid test io params"),
    );

    let schema_ref = match eng.next_action(&handle) {
        AgentAction::CallModel(req) => req
            .tool_context_index
            .as_ref()
            .expect("tool_context_index expected when deferral enabled")
            .full_schema_ref
            .clone(),
        other => panic!("expected CallModel, got {other:?}"),
    };

    let hits = eng.search_context_references(&handle, "web_search".into(), 5);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, ContextReferenceKind::ToolSchema);

    let raw = eng
        .read_context_reference(&handle, schema_ref)
        .expect("schema reference should be loadable through the engine");
    assert!(raw.contains("web_search"));
}

#[test]
fn e6_tool_schema_deferral_disabled_falls_back_to_full_schema_json() {
    let cfg = AgentConfig {
        enable_tool_schema_deferral: true,
        ..Default::default()
    };
    let caps = SessionCapabilities {
        tool_schema_deferral_supported: false,
        ..Default::default()
    };
    let eng = AgentEngine::new();
    let handle = eng.create_session(
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
            "[]".into(),
            "[]".into(),
            r#"[{"name":"web_search"}]"#.into(),
            "sys".into(),
        )
        .expect("valid test io params"),
    );
    match eng.next_action(&handle) {
        AgentAction::CallModel(req) => {
            assert!(req.tool_context_index.is_none());
            assert_eq!(req.tools_schema_json(), r#"[{"name":"web_search"}]"#);
        }
        other => panic!("expected CallModel, got {other:?}"),
    }
}

#[test]
fn m6_tool_deferral_cannot_drop_retrievability_for_omitted_schemas() {
    // Drive everything through the engine so the store driving the index
    // build and the store backing reads are the same instance.
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
    let handle = eng.create_session(
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
            "[]".into(),
            "[]".into(),
            schemas.into(),
            "sys".into(),
        )
        .expect("valid test io params"),
    );

    // Force the engine to register schemas in its owned store.
    let _ = match eng.next_action(&handle) {
        AgentAction::CallModel(req) => req,
        other => panic!("expected CallModel, got {other:?}"),
    };

    let hits = eng.search_context_references(&handle, "rare_tool".into(), 5);
    assert_eq!(hits.len(), 1);
    let id = hits[0].id.clone();

    let raw = eng
        .read_context_reference(&handle, id)
        .expect("schema should be readable by ref id");
    assert!(raw.contains("rare_tool"));
}
