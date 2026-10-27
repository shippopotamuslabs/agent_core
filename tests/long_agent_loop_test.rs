//! TDD Red — `FEAT-long-agent-loop-robustness` integration scenarios LALR-01, LALR-11, LALR-20.

use agent_core::action::AgentAction;
use agent_core::lalr_harness::{
    config_with_long_loop, default_long_loop_io, inject_semantic_fact_enclave,
    test_engine, run_twenty_five_tool_turn_loop,
};
use agent_core::long_loop_policy::LongLoopPolicy;
use agent_core::session::SessionCapabilities;
use agent_core::state_ledger::StateLedger;

#[test]
fn lalr_01_twenty_five_tool_turns_within_sidecar_budget() {
    let mut long_loop = LongLoopPolicy::hosted_mutable_defaults();
    long_loop.sidecar_byte_budget_per_call = 8192;
    let config = config_with_long_loop(long_loop);
    let engine = test_engine();
    let handle = engine.create_session(
        vec![],
        config.clone(),
        SessionCapabilities {
            summarization_allowed: true,
            ..Default::default()
        },
        serde_json::Value::Null,
        default_long_loop_io(),
    );

    let outcome = run_twenty_five_tool_turn_loop(&engine, &handle, &config, "noop");

    for req in &outcome.model_requests {
        assert!(
            req.provider_context.sidecar_byte_len <= 8192,
            "sidecar_byte_len must be <= 8192 per CallModel"
        );
    }
    assert!(matches!(outcome.final_action, AgentAction::Finished));
}

#[test]
fn lalr_11_codename_survives_twenty_five_tool_turns() {
    let config = config_with_long_loop(LongLoopPolicy::hosted_mutable_defaults());
    let mut ledger = StateLedger::default();
    inject_semantic_fact_enclave(&mut ledger);
    let engine = test_engine();
    let handle = engine.create_session(
        vec![],
        config.clone(),
        SessionCapabilities {
            summarization_allowed: true,
            ..Default::default()
        },
        serde_json::Value::Null,
        default_long_loop_io(),
    );

    let _ledger = ledger;
    let outcome = run_twenty_five_tool_turn_loop(&engine, &handle, &config, "noop");
    assert!(matches!(outcome.final_action, AgentAction::Finished));
    assert!(
        outcome.semantic_fact_preserved,
        "PROJECT_CODENAME=ENCLAVE must survive in ledger or transcript"
    );
}

#[test]
fn lalr_20_message_count_cap_after_twenty_five_turns() {
    let config = config_with_long_loop(LongLoopPolicy::hosted_mutable_defaults());
    let engine = test_engine();
    let handle = engine.create_session(
        vec![],
        config.clone(),
        SessionCapabilities {
            summarization_allowed: true,
            ..Default::default()
        },
        serde_json::Value::Null,
        default_long_loop_io(),
    );

    let outcome = run_twenty_five_tool_turn_loop(&engine, &handle, &config, "noop");
    assert!(matches!(outcome.final_action, AgentAction::Finished));
    assert!(
        outcome.session_message_count <= 40,
        "session.messages.len() must be <= 40 after 25 tool rounds with compaction"
    );
}
