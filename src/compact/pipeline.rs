use super::{estimate_session_pressure, CompactionContext, CompactionOutcome, CompactionTier};
use crate::budget::{effective_threshold_tokens, should_compact_at_token_threshold};
use crate::compact::policy::{threshold_ratio_for_trigger, CompactionPlan};
use crate::events::CompactionEvent;
use crate::session::{AgentConfig, AgentSession};

/// Runs registered tiers in order (Micro→FIFO→ArtifactSquash→Summary).
///
/// Stop policy (locked per ADR):
/// - First tier that returns `Applied` AND brings pressure below `threshold_ratio` ends the pass.
/// - If `Applied` but still over threshold, continue to next tier.
/// - `NeedsHostCall` short-circuits immediately (session state at that point is returned).
/// - All tiers `NotApplicable` → return `NotApplicable`.
pub struct CompactionPipeline {
    tiers: Vec<Box<dyn CompactionTier>>,
}

impl CompactionPipeline {
    pub fn new(tiers: Vec<Box<dyn CompactionTier>>) -> Self {
        Self { tiers }
    }

    pub fn run(
        &self,
        mut session: AgentSession,
        ctx: &CompactionContext,
        config: &AgentConfig,
    ) -> CompactionOutcome {
        let pressure = estimate_session_pressure(&session, config, ctx);

        let threshold_ratio = threshold_ratio_for_trigger(config, ctx.trigger);
        let threshold_tokens =
            effective_threshold_tokens(config, ctx.context_window_tokens, threshold_ratio, None);
        if !should_compact_at_token_threshold(&pressure, threshold_tokens) {
            return CompactionOutcome::NotApplicable;
        }

        let mut last_event: Option<CompactionEvent> = None;

        for tier in &self.tiers {
            match tier.apply(session.clone(), config, ctx) {
                CompactionOutcome::NeedsHostCall(req, s) => {
                    return CompactionOutcome::NeedsHostCall(req, s);
                }
                CompactionOutcome::NotApplicable => continue,
                CompactionOutcome::Failed(err) => {
                    return CompactionOutcome::Failed(err);
                }
                CompactionOutcome::Applied(new_session, event) => {
                    session = new_session;
                    session.push_compaction_event(event.clone());

                    let new_pressure = estimate_session_pressure(&session, config, ctx);

                    let under = !should_compact_at_token_threshold(&new_pressure, threshold_tokens);
                    last_event = Some(event);

                    if under {
                        break;
                    }
                }
            }
        }

        match last_event {
            Some(event) => CompactionOutcome::Applied(session, event),
            None => CompactionOutcome::NotApplicable,
        }
    }

    pub fn run_plan(
        &self,
        mut session: AgentSession,
        ctx: &CompactionContext,
        config: &AgentConfig,
        plan: &CompactionPlan,
    ) -> CompactionOutcome {
        let pressure = estimate_session_pressure(&session, config, ctx);

        if !should_compact_at_token_threshold(&pressure, plan.threshold_tokens) {
            return CompactionOutcome::NotApplicable;
        }

        let mut last_event: Option<CompactionEvent> = None;
        for tier in &self.tiers {
            match tier.apply(session.clone(), config, ctx) {
                CompactionOutcome::NeedsHostCall(req, s) => {
                    return CompactionOutcome::NeedsHostCall(req, s);
                }
                CompactionOutcome::NotApplicable => continue,
                CompactionOutcome::Failed(err) => return CompactionOutcome::Failed(err),
                CompactionOutcome::Applied(new_session, event) => {
                    session = new_session;
                    session.push_compaction_event(event.clone());

                    let new_pressure = estimate_session_pressure(&session, config, ctx);
                    let under =
                        !should_compact_at_token_threshold(&new_pressure, plan.threshold_tokens);
                    last_event = Some(event);
                    if under {
                        break;
                    }
                }
            }
        }

        match last_event {
            Some(event) => CompactionOutcome::Applied(session, event),
            None => CompactionOutcome::NotApplicable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact::CompactionContext;
    use crate::events::{CompactionEvent, CompactionTier as CTier, CompactionTrigger};
    use crate::message::{AgentMessage, AgentRole};
    use crate::session::AgentSession;
    use crate::turn_io::SummaryRequest;

    fn ctx(context_window_tokens: u32) -> CompactionContext {
        CompactionContext {
            provider: "anthropic".into(),
            context_window_tokens,
            tools_schema_json: "[]".into(),
            system_prompt: String::new(),
            context_sidecars_estimate: String::new(),
            capabilities: crate::session::SessionCapabilities {
                summarization_allowed: true,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            trigger: CompactionTrigger::Preflight,
        }
    }

    fn config() -> AgentConfig {
        AgentConfig::default() // threshold_ratio = 0.80
    }

    /// Build a session whose content tokenizes to roughly `n` tokens by
    /// repeating a known 1-token word `n` times.
    fn session_with_tokens(approx_tokens: usize) -> AgentSession {
        let content = "hello ".repeat(approx_tokens);
        AgentSession::new(
            vec![AgentMessage {
                id: "m0".into(),
                role: AgentRole::User,
                content,
                metadata: serde_json::Value::Null,
            }],
            serde_json::Value::Null,
        )
    }

    fn empty_session() -> AgentSession {
        AgentSession::new(vec![], serde_json::Value::Null)
    }

    fn make_event(tier: CTier, before: u32, after: u32) -> CompactionEvent {
        CompactionEvent {
            tier,
            tokens_before: before,
            tokens_after: after,
            trigger: CompactionTrigger::Preflight,
        }
    }

    // ── Mock tiers ─────────────────────────────────────────────────────────────

    struct AlwaysApplied {
        /// Session returned after compaction (the "compacted" session).
        result_session: AgentSession,
        event: CompactionEvent,
    }

    impl CompactionTier for AlwaysApplied {
        fn apply(
            &self,
            _: AgentSession,
            _: &AgentConfig,
            _: &CompactionContext,
        ) -> CompactionOutcome {
            CompactionOutcome::Applied(self.result_session.clone(), self.event.clone())
        }
    }

    struct AlwaysNotApplicable;

    impl CompactionTier for AlwaysNotApplicable {
        fn apply(
            &self,
            _: AgentSession,
            _: &AgentConfig,
            _: &CompactionContext,
        ) -> CompactionOutcome {
            CompactionOutcome::NotApplicable
        }
    }

    struct AlwaysNeedsHostCall;

    impl CompactionTier for AlwaysNeedsHostCall {
        fn apply(
            &self,
            session: AgentSession,
            _: &AgentConfig,
            _: &CompactionContext,
        ) -> CompactionOutcome {
            CompactionOutcome::NeedsHostCall(
                SummaryRequest {
                    messages_to_summarize: session.messages.clone(),
                    instruction: "Summarize.".into(),
                    last_message_id: "m0".into(),
                    trigger: crate::events::CompactionTrigger::Preflight,
                    focus_instruction: None,
                    history_ref: None,
                    state_ledger_snapshot: None,
                },
                session,
            )
        }
    }

    struct PanicIfCalled;

    impl CompactionTier for PanicIfCalled {
        fn apply(
            &self,
            _: AgentSession,
            _: &AgentConfig,
            _: &CompactionContext,
        ) -> CompactionOutcome {
            panic!("this tier must not be called");
        }
    }

    // ── Tests ──────────────────────────────────────────────────────────────────

    #[test]
    fn already_under_budget_returns_not_applicable() {
        // empty session → ratio = 0.0 → under any threshold
        let pipeline = CompactionPipeline::new(vec![Box::new(AlwaysApplied {
            result_session: empty_session(),
            event: make_event(CTier::Micro, 0, 0),
        })]);
        let outcome = pipeline.run(empty_session(), &ctx(200_000), &config());
        assert!(matches!(outcome, CompactionOutcome::NotApplicable));
    }

    #[test]
    fn over_budget_tier_applied_returns_applied() {
        // session with 100 tokens, context_window = 100 → ratio = 1.0 → over budget (>0.80)
        // After compaction: empty session → ratio = 0.0 → under budget → stop
        let over = session_with_tokens(100);
        let pipeline = CompactionPipeline::new(vec![Box::new(AlwaysApplied {
            result_session: empty_session(),
            event: make_event(CTier::Micro, 100, 0),
        })]);
        let outcome = pipeline.run(over, &ctx(100), &config());
        assert!(matches!(outcome, CompactionOutcome::Applied(_, _)));
    }

    #[test]
    fn over_budget_applied_records_event_in_session() {
        let over = session_with_tokens(100);
        let pipeline = CompactionPipeline::new(vec![Box::new(AlwaysApplied {
            result_session: empty_session(),
            event: make_event(CTier::Micro, 100, 0),
        })]);
        match pipeline.run(over, &ctx(100), &config()) {
            CompactionOutcome::Applied(session, _) => {
                assert_eq!(session.compaction_log.len(), 1);
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn all_tiers_not_applicable_returns_not_applicable() {
        let over = session_with_tokens(100);
        let pipeline = CompactionPipeline::new(vec![
            Box::new(AlwaysNotApplicable),
            Box::new(AlwaysNotApplicable),
            Box::new(AlwaysNotApplicable),
        ]);
        let outcome = pipeline.run(over, &ctx(100), &config());
        assert!(matches!(outcome, CompactionOutcome::NotApplicable));
    }

    #[test]
    fn needs_host_call_short_circuits() {
        // NeedsHostCall at position 2 → tier at position 3 (PanicIfCalled) must not run
        let over = session_with_tokens(100);
        let pipeline = CompactionPipeline::new(vec![
            Box::new(AlwaysNotApplicable),
            Box::new(AlwaysNeedsHostCall),
            Box::new(PanicIfCalled),
        ]);
        let outcome = pipeline.run(over, &ctx(100), &config());
        assert!(matches!(outcome, CompactionOutcome::NeedsHostCall(_, _)));
    }

    #[test]
    fn needs_host_call_carries_session_state() {
        // Even after a prior Applied tier, NeedsHostCall returns updated session
        let over = session_with_tokens(100);
        // First tier: Applied but still over budget (returns 100-token session)
        let still_over = session_with_tokens(100);
        let pipeline = CompactionPipeline::new(vec![
            Box::new(AlwaysApplied {
                result_session: still_over,
                event: make_event(CTier::Micro, 100, 90),
            }),
            Box::new(AlwaysNeedsHostCall),
        ]);
        match pipeline.run(over, &ctx(100), &config()) {
            CompactionOutcome::NeedsHostCall(_, session) => {
                // session compaction_log has 1 event from the first Applied tier
                assert_eq!(session.compaction_log.len(), 1);
            }
            other => panic!("expected NeedsHostCall, got {other:?}"),
        }
    }

    /// Mock tier that returns the input session unchanged (still over budget after it "runs").
    struct NoopApplied {
        event: CompactionEvent,
    }
    impl CompactionTier for NoopApplied {
        fn apply(
            &self,
            session: AgentSession,
            _: &AgentConfig,
            _: &CompactionContext,
        ) -> CompactionOutcome {
            CompactionOutcome::Applied(session, self.event.clone())
        }
    }

    /// Mock tier that clears all messages from the input session (brings it under budget).
    struct ClearMessages {
        event: CompactionEvent,
    }
    impl CompactionTier for ClearMessages {
        fn apply(
            &self,
            mut session: AgentSession,
            _: &AgentConfig,
            _: &CompactionContext,
        ) -> CompactionOutcome {
            session.messages.clear();
            CompactionOutcome::Applied(session, self.event.clone())
        }
    }

    #[test]
    fn applied_but_still_over_continues_to_next_tier() {
        // Tier 1 (Micro): Applied but session unchanged → still over budget
        // Tier 2 (FIFO): Clears messages → under budget
        // Both events must appear in the final session's compaction_log.
        let over = session_with_tokens(100);
        let pipeline = CompactionPipeline::new(vec![
            Box::new(NoopApplied {
                event: make_event(CTier::Micro, 100, 100),
            }),
            Box::new(ClearMessages {
                event: make_event(CTier::Fifo, 100, 0),
            }),
        ]);
        match pipeline.run(over, &ctx(100), &config()) {
            CompactionOutcome::Applied(session, event) => {
                assert!(matches!(event.tier, CTier::Fifo));
                assert_eq!(session.compaction_log.len(), 2);
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn pipeline_with_no_tiers_returns_not_applicable() {
        let over = session_with_tokens(100);
        let pipeline = CompactionPipeline::new(vec![]);
        let outcome = pipeline.run(over, &ctx(100), &config());
        assert!(matches!(outcome, CompactionOutcome::NotApplicable));
    }
}
