//! TDD Red — sidecar budget scenarios LALR-05, LALR-06, LALR-10, LALR-13, LALR-18.

use agent_core::context_store::ContextStore;
use agent_core::lalr_harness::{
    collect_three_call_model_snapshots, default_long_loop_io, noop_transport_engine,
};
use agent_core::long_loop_policy::LongLoopPolicy;
use agent_core::sidecar_builder::SidecarBuilder;
use agent_core::state_ledger::StateLedger;
use agent_core::tool_context::ToolContextIndex;

fn oversized_ledger_json(byte_len: usize) -> StateLedger {
    let padding = "x".repeat(byte_len.saturating_sub(64));
    StateLedger {
        objective: Some(format!("objective-{padding}")),
        ..Default::default()
    }
}

#[test]
fn lalr_05_ledger_truncates_at_512_bytes() {
    let policy = LongLoopPolicy {
        sidecar_byte_budget_per_call: 512,
        ..LongLoopPolicy::hosted_mutable_defaults()
    };
    let ledger = oversized_ledger_json(4096);
    let mut builder = SidecarBuilder::new();
    let mut store = ContextStore::default();
    let delta = builder
        .build_ledger_delta(&mut store, &ledger, policy.sidecar_byte_budget_per_call)
        .expect("ledger delta");

    assert!(delta.contains("\"truncated\":true"));
    assert!(delta.contains("\"refs\":"));
    assert!(delta.len() <= 512);
}

#[test]
fn lalr_06_second_turn_ledger_delta_is_minimal() {
    let mut builder = SidecarBuilder::new();
    let mut ledger = StateLedger::default();
    ledger.objective = Some("build feature".into());
    let mut store = ContextStore::default();

    let turn1 = builder
        .build_ledger_delta(&mut store, &ledger, 8192)
        .expect("turn 1 full snapshot");
    assert!(turn1.contains("\"objective\":\"build feature\""));

    ledger.decisions.push("picked approach A".into());
    let turn2 = builder
        .build_ledger_delta(&mut store, &ledger, 8192)
        .expect("turn 2 delta");
    assert!(turn2.contains("\"decisions\":[\"picked approach A\"]"));
    assert!(!turn2.contains("\"objective\":\"build feature\""));
}

#[test]
fn lalr_10_sidecar_generation_increments_per_call_model() {
    let engine = noop_transport_engine();
    let handle = engine.create_session(
        vec![],
        agent_core::session::AgentConfig::default(),
        agent_core::session::SessionCapabilities::default(),
        serde_json::Value::Null,
        default_long_loop_io(),
    );

    let snapshots = collect_three_call_model_snapshots(&engine, &handle);
    assert_eq!(snapshots.len(), 3);
    assert_eq!(snapshots[0].sidecar_generation, 0);
    assert_eq!(snapshots[1].sidecar_generation, 1);
    assert_eq!(snapshots[2].sidecar_generation, 2);
}

#[test]
fn lalr_13_drops_oldest_generation_over_total_budget() {
    let policy = LongLoopPolicy {
        sidecar_total_byte_budget: 1024,
        ..LongLoopPolicy::hosted_mutable_defaults()
    };
    let mut builder = SidecarBuilder::new();
    builder.record_rolling_cache_entry(0, 700);
    builder.enforce_total_budget(&policy);

    builder.record_rolling_cache_entry(1, 500);
    builder.enforce_total_budget(&policy);

    assert!(builder.rolling_cache_total_bytes() <= 1024);
    assert!(!builder.rolling_cache_has_generation(0));
}

#[test]
fn lalr_18_full_schema_ref_omitted_in_delta() {
    let policy = LongLoopPolicy {
        sidecar_byte_budget_per_call: 256,
        ..LongLoopPolicy::hosted_mutable_defaults()
    };
    let mut index = ToolContextIndex {
        full_schema_ref: String::new(),
        advertised_tools: vec![],
    };
    index.full_schema_ref = "x".repeat(128);
    let mut builder = SidecarBuilder::new();
    let mut store = ContextStore::default();
    let delta = builder
        .build_tool_index_payload(&mut store, &index, policy.sidecar_byte_budget_per_call)
        .expect("tool index delta");
    assert!(delta.contains(r#""full_schema_ref":"<omitted:128 chars>""#));
}
