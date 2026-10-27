//! TDD Red — state ledger tests (S11, S12, S15, M4, M5).

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::message::{AgentMessage, AgentRole, MessageMeta};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::state_ledger::{ArtifactStateAnchor, StateLedger};
use agent_core::turn_io::SummaryResult;

fn user_msg(content: &str) -> AgentMessage {
    AgentMessage {
        id: "u-test".into(),
        role: AgentRole::User,
        content: content.into(),
        metadata: MessageMeta::new(),
    }
}

fn assistant_msg(content: &str) -> AgentMessage {
    AgentMessage {
        id: "a-test".into(),
        role: AgentRole::Assistant,
        content: content.into(),
        metadata: MessageMeta::new(),
    }
}

#[test]
fn s11_state_ledger_records_objective_constraints_decisions_open_questions() {
    let mut ledger = StateLedger::default();
    let msg = user_msg(
        "Objective: build context manager. Constraint: do not edit contracts. \
         Decision: put ArtifactSquash before FIFO. Open question: retention policy?",
    );
    ledger.observe_user_message(&msg);
    assert_eq!(ledger.objective.as_deref(), Some("build context manager"));
    assert_eq!(
        ledger.constraints,
        vec!["do not edit contracts".to_string()]
    );
    assert_eq!(
        ledger.decisions,
        vec!["put ArtifactSquash before FIFO".to_string()]
    );
    assert_eq!(ledger.open_questions, vec!["retention policy?".to_string()]);
}

#[test]
fn s12_model_request_includes_state_ledger_anchors_after_compaction() {
    let cfg = AgentConfig {
        enable_state_ledger: true,
        ..Default::default()
    };
    let eng = AgentEngine::new();
    let big = "hello ".repeat(400);
    let handle = eng.create_session(
        vec![
            AgentMessage {
                id: "u0".into(),
                role: AgentRole::User,
                content: format!("Objective: preserve semantic continuity. {big}"),
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
        ],
        cfg,
        SessionCapabilities {
            summarization_allowed: true,
            kv_fixed: false,
            ..Default::default()
        },
        serde_json::Value::Null,
        SessionIoParams::new(
            "anthropic".into(),
            256,
            "[]".into(),
            "[]".into(),
            "[]".into(),
            "sys".into(),
        ),
    );
    match eng.next_action(&handle) {
        AgentAction::RequestSummary(req) => {
            assert_eq!(req.last_message_id, "a0");
            eng.submit_summary(
                &handle,
                SummaryResult {
                    summary_text: "Objective: preserve semantic continuity".into(),
                    last_message_id: "a0".into(),
                },
            );
        }
        other => panic!("expected RequestSummary, got {other:?}"),
    }

    match eng.next_action(&handle) {
        AgentAction::CallModel(req) => {
            let ledger = req
                .state_ledger
                .as_ref()
                .expect("ModelRequest must carry state_ledger after compaction");
            assert_eq!(
                ledger.objective.as_deref(),
                Some("preserve semantic continuity")
            );
            assert!(
                req.session_messages
                    .iter()
                    .any(|m| matches!(m.role, AgentRole::CompactSummary)),
                "expected at least one compact_summary role in session messages"
            );
        }
        other => panic!("expected CallModel, got {other:?}"),
    }
}

#[test]
fn s15_artifact_squash_persists_artifact_state_into_ledger() {
    let mut ledger = StateLedger::default();
    let anchor = ArtifactStateAnchor {
        artifact_id: "art-1".into(),
        total_blocks: 3,
        last_block_preview: "gamma".into(),
        continuation_instruction: "Continue appending.".into(),
    };
    ledger.observe_artifact_anchor(anchor);
    assert_eq!(ledger.artifact_states[0].artifact_id, "art-1");
    assert_eq!(ledger.artifact_states[0].total_blocks, 3);
    assert_eq!(ledger.artifact_states[0].last_block_preview, "gamma");
    assert_eq!(
        ledger.artifact_states[0].continuation_instruction,
        "Continue appending."
    );
}

#[test]
fn m4_state_ledger_cannot_be_a_static_placeholder() {
    let mut ledger = StateLedger::default();
    ledger.observe_user_message(&user_msg("Objective: build A. Decision: choose B."));
    ledger.observe_assistant_message(&assistant_msg("Decision: revise to C."));
    assert_eq!(ledger.objective.as_deref(), Some("build A"));
    assert_eq!(
        ledger.decisions,
        vec!["choose B".to_string(), "revise to C".to_string()]
    );
}

#[test]
fn m5_artifact_continuation_cannot_rely_only_on_synthetic_system_text() {
    // Build a session that already has the synthetic system message injected
    // by an earlier ArtifactSquash pass. Then register an anchor in the
    // ledger, run a summary merge, and finally delete the synthetic system
    // message (modelling what a follow-up Summary compaction would do).
    // The anchor must survive that deletion — proving the ledger, not the
    // synthetic message, is the source of truth for artifact continuation.
    let mut session_messages = vec![
        AgentMessage {
            id: "u0".into(),
            role: AgentRole::User,
            content: "Start artifact art-2".into(),
            metadata: MessageMeta::new(),
        },
        AgentMessage {
            id: "sys-squash".into(),
            role: AgentRole::System,
            content:
                "Artifact generation in progress. 1 blocks written so far. Last block: `omega`. Continue appending."
                    .into(),
            metadata: MessageMeta::new(),
        },
    ];

    let mut ledger = StateLedger::default();
    ledger.observe_artifact_anchor(ArtifactStateAnchor {
        artifact_id: "art-2".into(),
        total_blocks: 1,
        last_block_preview: "omega".into(),
        continuation_instruction: "Continue appending.".into(),
    });
    // Simulate summary compaction merging while not touching artifact_states.
    ledger.merge_summary("summary text", Some("summary-1"));

    // Remove the synthetic system message, as a follow-up summary
    // compaction would do once it rewrites the protected range.
    session_messages.retain(|m| m.id != "sys-squash");
    assert!(
        !session_messages.iter().any(|m| m.id == "sys-squash"),
        "synthetic system message must be gone for this defence to be meaningful"
    );

    // Anchor must still be intact in the ledger.
    assert_eq!(ledger.artifact_states[0].artifact_id, "art-2");
    assert_eq!(ledger.artifact_states[0].last_block_preview, "omega");
}
