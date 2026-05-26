//! TDD Red — transcript revision scenarios LALR-02, LALR-04, LALR-17.

use agent_core::events::CompactionTier;
use agent_core::lalr_harness::{config_with_long_loop, run_destructive_micro_preflight_pass};
use agent_core::long_loop_policy::LongLoopPolicy;
use agent_core::message::{AgentMessage, AgentRole};
use agent_core::session::{AgentSession, SessionCapabilities};
use agent_core::transcript_revision::{
    assert_append_only_transcript_invariant, replace_message_content, transcript_revision_of,
};
use serde_json::json;

fn tool_message(id: &str, content: &str) -> AgentMessage {
    AgentMessage {
        id: id.into(),
        role: AgentRole::Tool,
        content: content.into(),
        metadata: json!({}),
    }
}

#[test]
fn lalr_02_revision_bumps_once_per_destructive_pass() {
    let config = config_with_long_loop(LongLoopPolicy::hosted_mutable_defaults());
    let caps = SessionCapabilities {
        summarization_allowed: true,
        ..Default::default()
    };
    let mut session = AgentSession::new(
        vec![tool_message("tool-msg-1", r#"{"result":"x"}"#)],
        serde_json::Value::Null,
    );
    assert_eq!(transcript_revision_of(&session), 0);

    let (revision, events) = run_destructive_micro_preflight_pass(&mut session, &config, &caps);
    assert_eq!(revision, 1);
    assert_eq!(transcript_revision_of(&session), 1);

    let boundaries: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            agent_core::events::AgentEvent::CompactionBoundary(b) => Some(b),
            _ => None,
        })
        .collect();
    assert_eq!(boundaries.len(), 1);
    assert_eq!(boundaries[0].revision_before, 0);
    assert_eq!(boundaries[0].revision_after, 1);
}

#[test]
fn lalr_04_micro_rewrite_creates_new_message_id() {
    let mut session = AgentSession::new(
        vec![tool_message("tool-msg-100", r#"{"result":"x"}"#)],
        serde_json::Value::Null,
    );

    let replacement = replace_message_content(
        &mut session,
        "tool-msg-100",
        r#"{"result":"cleared"}"#.into(),
        CompactionTier::Micro,
    )
    .unwrap();

    let original = session
        .messages
        .iter()
        .find(|m| m.id == "tool-msg-100")
        .expect("original tool-msg-100");
    assert_eq!(original.metadata["superseded"], json!(true));

    assert_ne!(replacement.id, "tool-msg-100");
    assert_eq!(replacement.metadata["rewrite_of"], json!("tool-msg-100"));
    assert_eq!(replacement.metadata["compaction_tier"], json!("micro"));
}

#[test]
fn lalr_17_same_id_new_content_without_rewrite_of_fails() {
    let mut session = AgentSession::new(
        vec![tool_message("tool-msg-200", "AAAA")],
        serde_json::Value::Null,
    );
    let prior_snapshot = session.messages.clone();
    if let Some(msg) = session.messages.iter_mut().find(|m| m.id == "tool-msg-200") {
        msg.content = "BBBB".into();
    }

    let result = assert_append_only_transcript_invariant(&session, &prior_snapshot);
    assert!(!result.valid);
    assert_eq!(
        result.violation_message.as_deref(),
        Some("append_only_violation:tool-msg-200")
    );
}
