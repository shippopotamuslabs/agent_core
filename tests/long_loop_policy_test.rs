//! TDD Red — long-loop policy scenarios LALR-12.

use agent_core::action::AgentAction;
use agent_core::lalr_harness::{
    config_with_long_loop, default_long_loop_io, guardrail_kind, next_action_at_max_tool_turns,
    test_engine,
};
use agent_core::long_loop_policy::LongLoopPolicy;
use agent_core::session::SessionCapabilities;

#[test]
fn lalr_12_stops_at_five_tool_rounds() {
    let mut long_loop = LongLoopPolicy::hosted_mutable_defaults();
    long_loop.max_tool_turns = 5;
    let config = config_with_long_loop(long_loop);
    let engine = test_engine();
    let handle = engine
        .create_session(
            vec![],
            config,
            SessionCapabilities::default(),
            serde_json::Value::Null,
            default_long_loop_io(),
        )
        .expect("valid config creates session");

    let action = next_action_at_max_tool_turns(&engine, &handle, 5);
    assert!(matches!(action, AgentAction::Failed(_)));
    if let AgentAction::Failed(err) = action {
        assert_eq!(guardrail_kind(&err), Some("max_tool_turns"));
    }
}
