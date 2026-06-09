//! Tool-loop guardrails: max turn count, soft-failure streak, configurable greedy extension.

use serde_json::Value;

use crate::agent_tool_budget::AgentToolBudgetMeta;
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
    /// Consecutive envelope parse failures (TOOL-PARSE-002).
    pub consecutive_envelope_parse_failures: u32,
    /// Consecutive transient model API transport failures for the in-flight model turn.
    pub consecutive_model_api_failures: u32,
}

impl GuardrailState {
    pub fn effective_max_tool_calls(
        &self,
        config: &AgentConfig,
        last_model_response_tool_names: &[String],
    ) -> u32 {
        let mut cap = config.max_turns;
        for last in last_model_response_tool_names {
            for greedy in &config.tool_semantics.greedy_extension_tools {
                if greedy.tool_name == *last {
                    cap = cap.max(self.tool_calls_executed.saturating_add(greedy.extra_turns));
                }
            }
        }
        cap
    }

    pub fn allow_tool_call(
        &self,
        config: &AgentConfig,
        last_model_response_tool_names: &[String],
    ) -> bool {
        self.tool_calls_executed
            < self.effective_max_tool_calls(config, last_model_response_tool_names)
            && self.consecutive_soft_failure_turns < config.tool_retry_limit
            && self.consecutive_envelope_parse_failures < config.tool_retry_limit
    }

    pub fn reset_envelope_parse_failures(&mut self) {
        self.consecutive_envelope_parse_failures = 0;
    }

    pub fn record_envelope_parse_failure(&mut self) {
        self.consecutive_envelope_parse_failures =
            self.consecutive_envelope_parse_failures.saturating_add(1);
    }

    pub fn reset_model_api_failures(&mut self) {
        self.consecutive_model_api_failures = 0;
    }

    pub fn record_model_api_failure(&mut self) {
        self.consecutive_model_api_failures =
            self.consecutive_model_api_failures.saturating_add(1);
    }

    pub fn count_tool_executed(&mut self, n: u32) {
        self.tool_calls_executed = self.tool_calls_executed.saturating_add(n);
    }

    pub fn tool_budget_snapshot(
        &self,
        config: &AgentConfig,
        last_model_response_tool_names: &[String],
    ) -> AgentToolBudgetMeta {
        let limit = self.effective_max_tool_calls(config, last_model_response_tool_names);
        let used = self.tool_calls_executed;
        let allow = self.allow_tool_call(config, last_model_response_tool_names);
        let reason = if allow {
            None
        } else {
            Some(crate::agent_tool_budget::resolve_shutdown_reason(
                self,
                config,
                last_model_response_tool_names,
            ))
        };
        AgentToolBudgetMeta {
            tool_calls_used: used,
            tool_calls_limit: limit,
            tool_calls_remaining: limit.saturating_sub(used),
            allow_tool_call: allow,
            reason,
        }
    }

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
    use crate::tool_semantics::ToolGreedyExtensionSemantics;

    fn cfg() -> AgentConfig {
        AgentConfig::default()
    }

    fn greedy_cfg() -> AgentConfig {
        let mut config = AgentConfig::default();
        config.tool_semantics.greedy_extension_tools =
            vec![ToolGreedyExtensionSemantics {
                tool_name: "append_blocks".into(),
                extra_turns: 20,
            }];
        config
    }

    #[test]
    fn allow_true_under_limit_false_at_limit() {
        let mut g = GuardrailState::default();
        let c = cfg();
        assert!(g.allow_tool_call(&c, &[]));
        g.count_tool_executed(c.max_turns);
        assert!(!g.allow_tool_call(&c, &[]));
    }

    #[test]
    fn three_turn_all_soft_failure_disables_tools() {
        let mut g = GuardrailState::default();
        let c = cfg();
        assert!(g.allow_tool_call(&c, &[]));

        g.count_tool_executed(2);
        g.on_tool_turn_finished(&c, 2, true, false, false, "t");

        g.count_tool_executed(2);
        g.on_tool_turn_finished(&c, 2, true, false, false, "t");

        assert!(g.allow_tool_call(&c, &[]));

        g.count_tool_executed(2);
        let r = g.on_tool_turn_finished(&c, 2, true, false, false, "t");
        assert!(r.validation_retry_exhaustion_logged);
        assert!(!g.allow_tool_call(&c, &[]));
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
        g.on_tool_turn_finished(&c, 1, true, true, false, "t");

        assert_eq!(g.consecutive_soft_failure_turns, 0);
        assert!(g.allow_tool_call(&c, &[]));
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
        assert!(g.allow_tool_call(&c, &[]));
    }

    #[test]
    fn greedy_extension_extends_cap() {
        let mut g = GuardrailState::default();
        g.count_tool_executed(15);
        let mut c = greedy_cfg();
        c.max_turns = 20;
        assert!(g.allow_tool_call(&c, &["append_blocks".into()]));
        g.count_tool_executed(5);
        assert!(!g.allow_tool_call(&c, &[]));
        assert!(g.allow_tool_call(&c, &["append_blocks".into()]));
    }

    #[test]
    fn multi_tool_calls_increment_total() {
        let mut g = GuardrailState::default();
        let c = cfg();
        g.count_tool_executed(3);
        assert_eq!(g.tool_calls_executed, 3);
        assert!(g.allow_tool_call(&c, &[]));
    }

    #[test]
    fn recursion_semantics_stop_at_effective_cap() {
        let mut g = GuardrailState::default();
        let c = cfg();
        g.count_tool_executed(c.max_turns);
        assert!(!g.allow_tool_call(&c, &[]));
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

    #[test]
    fn tb_01_tool_budget_snapshot_mid_loop() {
        let g = GuardrailState {
            tool_calls_executed: 17,
            ..GuardrailState::default()
        };
        let c = cfg();
        let snapshot = g.tool_budget_snapshot(&c, &[]);
        assert_eq!(snapshot.tool_calls_used, 17);
        assert_eq!(snapshot.tool_calls_limit, 20);
        assert_eq!(snapshot.tool_calls_remaining, 3);
        assert!(snapshot.allow_tool_call);
        assert!(snapshot.reason.is_none());
    }

    #[test]
    fn tb_03_tool_budget_snapshot_first_tool() {
        let g = GuardrailState {
            tool_calls_executed: 1,
            ..GuardrailState::default()
        };
        let c = cfg();
        let snapshot = g.tool_budget_snapshot(&c, &[]);
        assert_eq!(snapshot.tool_calls_used, 1);
        assert_eq!(snapshot.tool_calls_limit, 20);
        assert_eq!(snapshot.tool_calls_remaining, 19);
        assert!(snapshot.allow_tool_call);
    }

    #[test]
    fn tb_09_tool_budget_snapshot_greedy_extension() {
        let g = GuardrailState {
            tool_calls_executed: 21,
            ..GuardrailState::default()
        };
        let c = greedy_cfg();
        let snapshot = g.tool_budget_snapshot(&c, &["append_blocks".into()]);
        assert_eq!(snapshot.tool_calls_limit, 41);
        assert_eq!(snapshot.tool_calls_used, 21);
        assert_eq!(snapshot.tool_calls_remaining, 20);
        assert!(snapshot.allow_tool_call);
    }

    #[test]
    fn envelope_parse_streak_disables_allow_tool_call_at_limit() {
        let mut g = GuardrailState::default();
        let c = cfg();
        assert!(g.allow_tool_call(&c, &[]));
        g.consecutive_envelope_parse_failures = c.tool_retry_limit;
        assert!(!g.allow_tool_call(&c, &[]));
    }
}
