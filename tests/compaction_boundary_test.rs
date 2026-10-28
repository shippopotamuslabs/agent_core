//! Compaction boundary scenarios LALR-03, LALR-09, LALR-14, LALR-15, LALR-16, LALR-19.
//! (ENC-309: the three red tests were stale specs from the global-catalog
//! era, not pending features — the harness now declares its tool
//! host-style. See src/lalr_harness.rs `scenario_tool_semantics`.)

use agent_core::events::CompactionTrigger;
use agent_core::lalr_harness::{
    compaction_boundary_events, config_with_long_loop, post_compact_revision,
    run_history_snapshot_only_prelude, run_kv_fixed_destructive_preflight,
    run_summary_compaction_with_objective, run_two_destructive_compaction_passes,
};
use agent_core::long_loop_policy::LongLoopPolicy;
use agent_core::session::{AgentSession, SessionCapabilities};
use agent_core::state_ledger::StateLedger;
use agent_core::transcript_revision::transcript_revision_of;
use agent_core::{FfiCompactionBoundary, FfiCompactionTier, FfiCompactionTrigger};

#[test]
fn lalr_03_boundary_id_literal_format() {
    let config = config_with_long_loop(LongLoopPolicy::hosted_mutable_defaults());
    let caps = SessionCapabilities {
        summarization_allowed: true,
        ..Default::default()
    };
    let mut session = AgentSession::new(vec![], serde_json::Value::Null);
    assert_eq!(transcript_revision_of(&session), 0);

    let events = agent_core::lalr_harness::run_destructive_micro_preflight_pass(
        &mut session,
        &config,
        &caps,
    )
    .1;
    let boundaries = compaction_boundary_events(&events);
    assert_eq!(boundaries.len(), 1);
    assert_eq!(boundaries[0].boundary_id, "cb-1-preflight");
    assert!(boundaries[0].destructive);
}

#[test]
fn lalr_09_post_compact_revision_matches_session() {
    let mut session = AgentSession::new(vec![], serde_json::Value::Null);
    assert_eq!(transcript_revision_of(&session), 0);

    let (revision, events) = run_two_destructive_compaction_passes(&mut session);
    assert_eq!(revision, 2);
    assert_eq!(transcript_revision_of(&session), 2);

    let post_revision = post_compact_revision(&events, CompactionTrigger::PostToolResult)
        .or_else(|| post_compact_revision(&events, CompactionTrigger::Preflight))
        .or_else(|| post_compact_revision(&events, CompactionTrigger::MidTurn))
        .expect("PostCompact with transcript_revision_after");
    assert_eq!(post_revision, 2);
}

#[test]
fn lalr_14_objective_after_summary_tier() {
    let mut ledger = StateLedger {
        objective: Some("ship long-loop v1".into()),
        ..StateLedger::default()
    };
    let mut session = AgentSession::new(vec![], serde_json::Value::Null);

    run_summary_compaction_with_objective(&mut ledger, &mut session);
    assert_eq!(ledger.objective.as_deref(), Some("ship long-loop v1"));
}

#[test]
fn lalr_15_kv_fixed_emits_boundary_without_summary_tier() {
    let config = config_with_long_loop(LongLoopPolicy::fixed_kv_defaults());
    let mut session = AgentSession::new(vec![], serde_json::Value::Null);
    let events = run_kv_fixed_destructive_preflight(&mut session, &config);
    let boundaries = compaction_boundary_events(&events);
    assert_eq!(boundaries.len(), 1);
    let tiers: Vec<String> = boundaries[0]
        .tiers_applied
        .iter()
        .map(|t| t.to_string())
        .collect();
    assert!(tiers.iter().any(|t| t == "micro"));
    assert!(!tiers.iter().any(|t| t == "summary"));
}

#[test]
fn lalr_16_ffi_boundary_json_roundtrip() {
    let boundary = FfiCompactionBoundary {
        boundary_id: "cb-1-mid_turn".into(),
        revision_before: 0,
        revision_after: 1,
        trigger: FfiCompactionTrigger::MidTurn,
        tiers_applied: vec![FfiCompactionTier::Micro],
        message_count_before: 10,
        message_count_after: 8,
        destructive: true,
    };
    let json = serde_json::to_string(&boundary).expect("serialize");
    let back: FfiCompactionBoundary = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.boundary_id, "cb-1-mid_turn");
    assert_eq!(back.revision_after, 1);
}

#[test]
fn lalr_19_history_snapshot_only_leaves_revision_at_zero() {
    let mut session = AgentSession::new(vec![], serde_json::Value::Null);
    let (revision, events) = run_history_snapshot_only_prelude(&mut session);
    assert_eq!(revision, 0);
    assert_eq!(transcript_revision_of(&session), 0);
    assert!(compaction_boundary_events(&events).is_empty());
}
