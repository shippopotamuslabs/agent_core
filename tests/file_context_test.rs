//! TDD Red -- file context condensation tests (S17, E7, M7).

use agent_core::action::AgentAction;
use agent_core::context_store::ContextStore;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::file_context::{condense_file_context, FileCondensationLevel};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta};
use agent_core::session::{AgentConfig, SessionCapabilities};

#[test]
fn s17_large_parseable_file_becomes_outline_and_chunk_refs() {
    let cfg = AgentConfig {
        enable_file_condensation: true,
        file_outline_threshold_tokens: 100,
        ..Default::default()
    };
    let caps = SessionCapabilities {
        file_backed_context_supported: true,
        ..Default::default()
    };
    let mut store = ContextStore::default();
    let files = serde_json::json!([
        {
            "path": "src/large.rs",
            "token_estimate": 500,
            "content": "// license header\npub fn alpha() {}\nstruct Beta {}\nfn gamma() {}"
        }
    ]);

    let index = condense_file_context(&files.to_string(), &mut store, &cfg, &caps, 0)
        .expect("file context index");

    assert_eq!(index.included_paths.len(), 1);
    let summary = &index.included_paths[0];
    assert_eq!(summary.path, "src/large.rs");
    assert_eq!(summary.condensation_level, FileCondensationLevel::Outline);
    let outline_ref = summary.outline_ref.as_ref().expect("outline ref");
    assert_eq!(summary.chunk_refs.len(), 1);
    let outline = store.get(outline_ref).expect("stored outline");
    assert!(outline.content.contains("pub fn alpha"));
    let chunk = store.get(&summary.chunk_refs[0]).expect("stored chunk");
    assert!(chunk.content.contains("license header"));
    assert!(!outline.content.contains("license header"));
}

#[test]
fn e7_unparseable_file_context_becomes_filename_only() {
    let cfg = AgentConfig {
        enable_file_condensation: true,
        file_outline_threshold_tokens: 100,
        ..Default::default()
    };
    let caps = SessionCapabilities {
        file_backed_context_supported: true,
        ..Default::default()
    };
    let mut store = ContextStore::default();
    let files = serde_json::json!([
        {
            "path": "blob.bin",
            "token_estimate": 500,
            "content": "plain bytes without source structure"
        }
    ]);

    let index = condense_file_context(&files.to_string(), &mut store, &cfg, &caps, 0)
        .expect("file context index");

    assert_eq!(index.included_paths.len(), 1);
    let summary = &index.included_paths[0];
    assert_eq!(summary.path, "blob.bin");
    assert_eq!(
        summary.condensation_level,
        FileCondensationLevel::FilenameOnly
    );
    assert!(summary.outline_ref.is_none());
    assert!(summary.chunk_refs.is_empty());
}

#[test]
fn m7_file_context_does_not_inline_full_body_into_model_messages() {
    let body = format!("pub fn alpha() {{}}\n{}", "SENTINEL_FULL_BODY".repeat(512));
    let cfg = AgentConfig {
        enable_file_condensation: true,
        file_outline_threshold_tokens: 100,
        ..Default::default()
    };
    let caps = SessionCapabilities {
        file_backed_context_supported: true,
        ..Default::default()
    };
    let session_context = serde_json::json!({
        "included_files": [
            {
                "path": "src/large.rs",
                "token_estimate": 500,
                "content": body
            }
        ]
    });
    let eng = AgentEngine::new();
    let handle = eng.create_session(
        vec![AgentMessage {
            id: "u1".into(),
            role: AgentRole::User,
            content: "Use attached source context".into(),
            metadata: MessageMeta::new(),
        }],
        cfg,
        caps,
        session_context,
        SessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 8_000,
            tool_catalog_json: "[]".into(),
            tool_schema_registry_json: "[]".into(),
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        },
    );

    let request = match eng.next_action(&handle) {
        AgentAction::CallModel(request) => request,
        other => panic!("expected CallModel, got {other:?}"),
    };

    assert!(request.file_context_index.is_some());
    assert!(request
        .session_messages
        .iter()
        .all(|message| !message.content.contains("SENTINEL_FULL_BODY")));
}
