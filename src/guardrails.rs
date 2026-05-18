//! Tool-loop guardrails: max turn count, soft-failure streak, greedy `append_blocks` cap
//! (AGENT-015). Mirrors `LlmToolingOrchestrator` bookkeeping.

use serde_json::Value;

use crate::session::AgentConfig;
use log::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnBookkeepingResult {
    /// True when parallel soft-failure exhaustion log was emitted (Kotlin: `validation retry exhausted`).
    pub validation_retry_exhaustion_logged: bool,
}

/// Kotlin-compatible soft failure: JSON object with top-level `error` key,
/// `status: "error"`, or `data: null` when no `results` array is present.
pub fn is_soft_failure_result_json(result_json: &str) -> bool {
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(result_json) else {
        return false;
    };

    if obj.contains_key("error") {
        return true;
    }

    if obj
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("error"))
    {
        return true;
    }

    obj.get("data").is_some_and(Value::is_null) && !obj.contains_key("results")
}

#[derive(Debug, Clone, Default)]
pub struct GuardrailState {
    /// Total tool executions in the current session loop (Kotlin `toolCallCount`).
    pub tool_calls_executed: u32,
    /// Consecutive tool turns where every completed result was soft failure (no hard failure, no non-soft outcome).
    pub consecutive_soft_failure_turns: u32,
}

impl GuardrailState {
    /// Effective recursion cap, including greedy expansion for `append_blocks` turns.
    pub fn effective_max_tool_calls(
        &self,
        config: &AgentConfig,
        last_model_response_had_append_blocks: bool,
    ) -> u32 {
        let mut cap = config.max_turns;
        if config.greedy_append_blocks && last_model_response_had_append_blocks {
            cap = cap.max(self.tool_calls_executed.saturating_add(20));
        }
        cap
    }

    /// `allow_tool_call` bit passed into [`crate::turn_io::ModelRequest`] (Kotlin: `allowToolCall`).
    pub fn allow_tool_call(
        &self,
        config: &AgentConfig,
        last_model_response_had_append_blocks: bool,
    ) -> bool {
        self.tool_calls_executed
            < self.effective_max_tool_calls(config, last_model_response_had_append_blocks)
            && self.consecutive_soft_failure_turns < config.tool_retry_limit
    }

    /// Increment after each tool execution attempt (success, soft error payload, or synthetic limit error).
    pub fn count_tool_executed(&mut self, n: u32) {
        self.tool_calls_executed = self.tool_calls_executed.saturating_add(n);
    }

    /// End-of-turn accounting after all parallel tools in the model response are handled.
    ///
    /// `parallel_tool_calls` is the number of tools requested in the single model response.
    pub fn on_tool_turn_finished(
        &mut self,
        config: &AgentConfig,
        parallel_tool_calls: usize,
        turn_had_tool_calls: bool,
        turn_had_hard_failure: bool,
        turn_had_non_soft_outcome: bool,
        log_tag: &str,
    ) -> TurnBookkeepingResult {
        let mut validation_retry_exhaustion_logged = false;
        if turn_had_hard_failure || turn_had_non_soft_outcome {
            self.consecutive_soft_failure_turns = 0;
        } else if turn_had_tool_calls {
            self.consecutive_soft_failure_turns =
                self.consecutive_soft_failure_turns.saturating_add(1);
            if parallel_tool_calls >= 2
                && self.consecutive_soft_failure_turns >= config.tool_retry_limit
            {
                warn!(
                    target: "agent_core",
                    "validation retry exhausted tag={log_tag} (repeated parallel tool calls returned only soft errors)",
                );
                validation_retry_exhaustion_logged = true;
            }
        }
        TurnBookkeepingResult {
            validation_retry_exhaustion_logged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::AgentConfig;

    fn cfg() -> AgentConfig {
        AgentConfig::default()
    }

    #[test]
    fn allow_true_under_limit_false_at_limit() {
        let mut g = GuardrailState::default();
        let c = cfg();
        assert!(g.allow_tool_call(&c, false));
        g.count_tool_executed(c.max_turns);
        assert!(!g.allow_tool_call(&c, false));
    }

    #[test]
    fn three_turn_all_soft_failure_disables_tools() {
        let mut g = GuardrailState::default();
        let c = cfg();
        assert!(g.allow_tool_call(&c, false));

        // Turn 1 — two parallel tools, both soft failure
        g.count_tool_executed(2);
        g.on_tool_turn_finished(&c, 2, true, false, false, "t");

        g.count_tool_executed(2);
        g.on_tool_turn_finished(&c, 2, true, false, false, "t");

        assert!(g.allow_tool_call(&c, false));

        g.count_tool_executed(2);
        let r = g.on_tool_turn_finished(&c, 2, true, false, false, "t");
        assert!(r.validation_retry_exhaustion_logged);
        assert!(!g.allow_tool_call(&c, false));
    }

    #[test]
    fn hard_failure_resets_soft_failure_streak() {
        let mut g = GuardrailState::default();
        let c = cfg();

        g.count_tool_executed(1);
        g.on_tool_turn_finished(&c, 1, true, false, false, "t");
        g.count_tool_executed(1);
        g.on_tool_turn_finished(&c, 1, true, false, false, "t");

        g.count_tool_executed(1);
        g.on_tool_turn_finished(&c, 1, true, true, false, "t"); // hard failure

        assert_eq!(g.consecutive_soft_failure_turns, 0);
        assert!(g.allow_tool_call(&c, false));
    }

    #[test]
    fn non_soft_result_resets_streak() {
        let mut g = GuardrailState::default();
        let c = cfg();
        g.count_tool_executed(1);
        g.on_tool_turn_finished(&c, 1, true, false, false, "t");
        g.count_tool_executed(1);
        g.on_tool_turn_finished(&c, 1, true, false, true, "t");
        assert_eq!(g.consecutive_soft_failure_turns, 0);
        assert!(g.allow_tool_call(&c, false));
    }

    #[test]
    fn greedy_append_blocks_extends_cap() {
        let mut g = GuardrailState::default();
        g.count_tool_executed(15);
        let mut c = cfg();
        c.max_turns = 20;
        // 15 < 20 → allow; effective cap after append = max(20, 15+20)=35
        assert!(g.allow_tool_call(&c, true));
        // consume up to default cap using synthetic count
        g.count_tool_executed(5); // total 20 — at base limit
        assert!(!g.allow_tool_call(&c, false));
        assert!(g.allow_tool_call(&c, true)); // greedy reopen window until 35
    }

    #[test]
    fn multi_tool_calls_increment_total() {
        let mut g = GuardrailState::default();
        let c = cfg();
        g.count_tool_executed(3);
        assert_eq!(g.tool_calls_executed, 3);
        assert!(g.allow_tool_call(&c, false));
    }

    #[test]
    fn recursion_semantics_stop_at_effective_cap() {
        let mut g = GuardrailState::default();
        let c = cfg();
        g.count_tool_executed(c.max_turns);
        assert!(!g.allow_tool_call(&c, false));
    }

    #[test]
    fn soft_failure_detection_matches_legacy_kotlin_shapes() {
        assert!(is_soft_failure_result_json(r#"{"error":"x"}"#));
        assert!(is_soft_failure_result_json(r#"{"status":"error"}"#));
        assert!(is_soft_failure_result_json(r#"{"status":"ERROR"}"#));
        assert!(is_soft_failure_result_json(r#"{"data":null}"#));
        assert!(!is_soft_failure_result_json(r#"{"results":[]}"#));
        assert!(!is_soft_failure_result_json(
            r#"{"data":null,"results":[]}"#
        ));
    }
}
