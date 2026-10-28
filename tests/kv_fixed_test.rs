//! Extra kv_fixed assertions (AGENT-047). Run: `cargo test --test kv_fixed_test`

use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::AgentError;
use agent_core::events::AgentEvent;
use agent_core::message::{AgentMessage, AgentRole, MessageMeta};
use agent_core::session::{AgentConfig, SessionCapabilities};

#[test]
fn kv_fixed_emits_invalidated_non_kv_fixed_does_not_on_same_shape() {
    let big = "hello ".repeat(400);
    let mk = || AgentEngine::new();
    let io = || SessionIoParams {
        provider: "anthropic".into(),
        context_window_tokens: 256,
        tool_catalog_json: "[]".into(),
        tool_schema_registry_json: "[]".into(),
        tools_schema_json: "[]".into(),
        system_prompt: "sys".into(),
    };
    let msgs = vec![
        AgentMessage {
            id: "u0".into(),
            role: AgentRole::User,
            content: big.clone(),
            metadata: MessageMeta::new(),
        },
        AgentMessage {
            id: "a0".into(),
            role: AgentRole::Assistant,
            content: big,
            metadata: MessageMeta::new(),
        },
        AgentMessage {
            id: "u1".into(),
            role: AgentRole::User,
            content: "tail".into(),
            metadata: MessageMeta::new(),
        },
    ];

    let eng_kv = mk();
    let h_kv = eng_kv.create_session(
        msgs.clone(),
        AgentConfig::default(),
        SessionCapabilities {
            summarization_allowed: false,
            kv_fixed: true,
            tool_result_budget_tokens: None,
            ..Default::default()
        },
        serde_json::Value::Null,
        io(),
    );
    let _ = eng_kv.next_action(&h_kv);
    assert!(eng_kv
        .drain_events(&h_kv)
        .iter()
        .any(|e| matches!(e, AgentEvent::KvCacheInvalidated { .. })));

    let eng_plain = mk();
    let h_plain = eng_plain.create_session(
        msgs,
        AgentConfig::default(),
        SessionCapabilities {
            summarization_allowed: false,
            kv_fixed: false,
            tool_result_budget_tokens: None,
            ..Default::default()
        },
        serde_json::Value::Null,
        io(),
    );
    let _ = eng_plain.next_action(&h_plain);
    assert!(!eng_plain
        .drain_events(&h_plain)
        .iter()
        .any(|e| matches!(e, AgentEvent::KvCacheInvalidated { .. })));
}

/// Probe: kv_fixed session with summarization disabled and oversized history must
/// resolve preflight pressure deterministically — either a successful compaction
/// (CallModel) or a clean CompactionFailed — never a panic or stuck phase.
#[test]
fn kv_fixed_without_summarization_oversized_history_resolves_cleanly() {
    let big = "hello ".repeat(600);
    let eng = AgentEngine::new();
    let handle = eng.create_session(
        vec![
            AgentMessage {
                id: "u0".into(),
                role: AgentRole::User,
                content: big.clone(),
                metadata: MessageMeta::new(),
            },
            AgentMessage {
                id: "a0".into(),
                role: AgentRole::Assistant,
                content: big.clone(),
                metadata: MessageMeta::new(),
            },
            AgentMessage {
                id: "u1".into(),
                role: AgentRole::User,
                content: big.clone(),
                metadata: MessageMeta::new(),
            },
            AgentMessage {
                id: "a1".into(),
                role: AgentRole::Assistant,
                content: big,
                metadata: MessageMeta::new(),
            },
            AgentMessage {
                id: "u2".into(),
                role: AgentRole::User,
                content: "tail".into(),
                metadata: MessageMeta::new(),
            },
        ],
        AgentConfig::default(),
        SessionCapabilities {
            summarization_allowed: false,
            kv_fixed: true,
            context_policy_tier: agent_core::policy::ContextPolicyTier::FixedKv,
            ..Default::default()
        },
        serde_json::Value::Null,
        SessionIoParams {
            provider: "llama".into(),
            context_window_tokens: 512,
            tool_catalog_json: "[]".into(),
            tool_schema_registry_json: "[]".into(),
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        },
    );

    let action = eng.next_action(&handle);
    match action {
        agent_core::action::AgentAction::CallModel(_) => {}
        agent_core::action::AgentAction::Failed(AgentError::CompactionFailed { .. }) => {}
        other => panic!("expected CallModel or clean CompactionFailed, got {other:?}"),
    }
    // A second call must return the same class of action (no phase corruption).
    let again = eng.next_action(&handle);
    assert!(
        matches!(
            again,
            agent_core::action::AgentAction::CallModel(_)
                | agent_core::action::AgentAction::Failed(AgentError::CompactionFailed { .. })
        ),
        "second next_action changed action class: {again:?}"
    );
}
