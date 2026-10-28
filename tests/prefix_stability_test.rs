#![cfg(feature = "lalr-harness")]

//! Wire-prefix stability across consecutive `CallModel` actions in a multi-turn
//! tool loop. Catches regressions where engine mutates byte 0..N of the
//! `ModelRequest.session_messages` between turns — the exact pattern that
//! breaks xAI/OpenAI prompt-cache prefix.

use agent_core::action::AgentAction;
use agent_core::lalr_harness::default_long_loop_io;
use agent_core::message::{AgentMessage, MessageMeta, ToolResultRecord};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall};
use agent_core::{AgentEngine, SessionHandle};

/// Drive N tool rounds and capture the `ModelRequest` from each `CallModel`.
fn capture_call_model_requests(rounds: u32) -> Vec<ModelRequest> {
    let engine = AgentEngine::new();
    let handle = engine.create_session(
        vec![AgentMessage {
            id: "u0".into(),
            role: agent_core::message::AgentRole::User,
            content: "Objective: ship long-loop v1. Decision: keep transcript stable.".into(),
            metadata: MessageMeta::new(),
        }],
        cache_friendly_config(),
        SessionCapabilities {
            summarization_allowed: true,
            prompt_cache_supported: true,
            ..Default::default()
        },
        serde_json::Value::Null,
        default_long_loop_io(),
    );

    let mut requests = Vec::new();
    let tool_response = ModelResponse {
        assistant_text: None,
        tool_calls: vec![RawToolCall {
            id: Some("call-1".into()),
            name: "web_search".into(),
            arguments_json: r#"{"query":"prefix-stability"}"#.into(),
        }],
        finish_reason: FinishReason::ToolUse,
        provider_reasoning_content: None,
    };

    while (requests.len() as u32) < rounds {
        match engine.next_action(&handle) {
            AgentAction::CallModel(req) => {
                requests.push(req.clone());
                if requests.len() as u32 >= rounds {
                    break;
                }
                engine.submit_model_response(&handle, tool_response.clone());
            }
            AgentAction::ExecuteTools(calls) => {
                let results: Vec<ToolResultRecord> = calls
                    .into_iter()
                    .map(|c| ToolResultRecord {
                        call_id: c.call_id,
                        result_json: r#"{"status":"ok"}"#.into(),
                        truncated: false,
                        soft_failure: false,
                        activated_tool_names: Vec::new(),
                        result_metadata: None,
                    })
                    .collect();
                engine.submit_tool_results(&handle, results);
            }
            AgentAction::Finished | AgentAction::Failed(_) => break,
            other => panic!("unexpected action while capturing requests: {other:?}"),
        }
    }
    let _: &SessionHandle = &handle;
    requests
}

/// Avoid early pressure-triggered compaction; we want pure append turns.
fn cache_friendly_config() -> AgentConfig {
    let mut config = AgentConfig::default();
    config.enable_state_ledger = true;
    config.enable_loop_hooks = false;
    config.enable_pending_work_guard = false;
    config.long_loop.compact_every_n_tool_turns = 0;
    config.long_loop.summarize_after_tool_turns = 0;
    config.threshold_ratio = 0.99;
    config.preflight_token_threshold = None;
    config.mid_turn_token_threshold = None;
    config
}

#[test]
fn session_messages_prefix_is_byte_stable_across_consecutive_call_models() {
    let requests = capture_call_model_requests(4);
    assert!(
        requests.len() >= 2,
        "expected at least 2 CallModel actions, got {}",
        requests.len()
    );

    for window in requests.windows(2) {
        let (prior, next) = (&window[0], &window[1]);
        assert!(
            next.session_messages.len() >= prior.session_messages.len(),
            "next turn's session_messages must not be shorter than prior turn",
        );
        for (index, prior_message) in prior.session_messages.iter().enumerate() {
            let next_message = &next.session_messages[index];
            assert_eq!(
                prior_message.id, next_message.id,
                "prefix break at index {index}: id changed from {} to {}",
                prior_message.id, next_message.id,
            );
            assert_eq!(
                prior_message.content, next_message.content,
                "prefix break at index {index} (id {}): content mutated between turns. \
                 Provider prompt cache will miss everything after this position.",
                prior_message.id,
            );
            assert_eq!(
                prior_message.role, next_message.role,
                "prefix break at index {index}: role changed between turns",
            );
        }
    }
}

#[test]
fn session_messages_prefix_holds_under_state_ledger_growth() {
    // Earlier behavior injected a `state-ledger-anchor` at position 0 whose content
    // grew every turn (new decisions appended). Verify the engine no longer mutates
    // position 0 across turns when state_ledger is enabled.
    let requests = capture_call_model_requests(3);
    assert!(requests.len() >= 2, "need at least 2 turns to compare");

    let first_message_turn1 = requests[0]
        .session_messages
        .first()
        .expect("turn 1 must have at least one message");
    let first_message_turn2 = requests[1]
        .session_messages
        .first()
        .expect("turn 2 must have at least one message");

    assert_eq!(
        first_message_turn1.id, first_message_turn2.id,
        "position 0 message id changed between turn 1 and turn 2",
    );
    assert_eq!(
        first_message_turn1.content, first_message_turn2.content,
        "position 0 message content changed between turn 1 and turn 2 \
         (state-ledger anchor regression?)",
    );
    assert_ne!(
        first_message_turn1.id, "state-ledger-anchor",
        "engine must not inject a synthetic state-ledger anchor at position 0; \
         ledger info belongs in the tail-volatile sidecar",
    );
}

#[test]
fn tool_result_content_unchanged_after_budget_inject_across_call_models() {
    let requests = capture_call_model_requests(3);
    assert!(
        requests.len() >= 2,
        "expected at least 2 CallModel actions after tool batch, got {}",
        requests.len()
    );

    let tool_message_id = requests[1]
        .session_messages
        .iter()
        .find(|m| m.content.contains(r#""status":"ok""#))
        .map(|m| m.id.clone())
        .expect("tool result message must exist after first tool batch");

    for (index, request) in requests.iter().enumerate().skip(1) {
        let tool_message = request
            .session_messages
            .iter()
            .find(|m| m.id == tool_message_id)
            .unwrap_or_else(|| panic!("request {index} missing tool message {tool_message_id}"));
        assert_eq!(
            tool_message.content, r#"{"status":"ok"}"#,
            "tool result content mutated at request {index} after budget inject \
             (breaks provider prompt-cache prefix)",
        );
    }
}
