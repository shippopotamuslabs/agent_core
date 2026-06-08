//! Full projected prompt budget and text-truncation integration tests.

use agent_core::budget::{
    effective_hard_prompt_ceiling_tokens, exceeds_hard_prompt_ceiling, PromptBudgetParts,
    TokenLedger,
};
use agent_core::compact::{
    estimate_session_pressure, policy::build_compaction_plan, text_truncate::TextTruncateCompactionTier,
    CompactionContext, CompactionOutcome, CompactionTier as CompactionTierTrait,
};
use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::{AgentError, CompactionTier, CompactionTrigger};
use agent_core::host::{AgentToolExecutor, AgentTransport, NoOpSummarizer};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::policy::{resolve_context_policy, ContextPolicyTier};
use agent_core::session::{AgentConfig, AgentSession, SessionCapabilities};
use agent_core::turn_io::{ModelRequest, ModelResponse};
use std::sync::Arc;

struct PanicTransport;
impl AgentTransport for PanicTransport {
    fn complete_model_turn(&self, _: ModelRequest) -> Result<ModelResponse, AgentError> {
        panic!("unexpected model call")
    }
}

struct PanicExec;
impl AgentToolExecutor for PanicExec {
    fn execute_tools(&self, _: Vec<ToolCallRecord>) -> Result<Vec<ToolResultRecord>, AgentError> {
        panic!("unexpected tool execution")
    }
}

fn msg(id: &str, role: AgentRole, content: &str) -> AgentMessage {
    AgentMessage {
        id: id.into(),
        role,
        content: content.into(),
        metadata: serde_json::Value::Null,
    }
}

#[test]
fn fixed_kv_plan_includes_text_truncate_after_fifo() {
    let caps = SessionCapabilities {
        context_policy_tier: ContextPolicyTier::FixedKv,
        summarization_allowed: false,
        kv_fixed: true,
        history_rewrite_supported: true,
        ..Default::default()
    };
    let cfg = AgentConfig::for_policy_tier(ContextPolicyTier::FixedKv);
    let policy = resolve_context_policy(&cfg, &caps, "LLAMA", 4096);
    let plan = build_compaction_plan(&cfg, &caps, &policy, CompactionTrigger::Preflight, 4096);
    assert!(plan.ordered_tiers.contains(&CompactionTier::Fifo));
    assert!(plan.ordered_tiers.contains(&CompactionTier::TextTruncate));
    assert_eq!(
        plan.max_prompt_tokens_after_plan,
        effective_hard_prompt_ceiling_tokens(&cfg, 4096)
    );
}

#[test]
fn kv_fixed_preflight_compacts_large_system_prompt_before_call_model() {
    let cfg = AgentConfig::for_policy_tier(ContextPolicyTier::FixedKv);
    let caps = SessionCapabilities {
        summarization_allowed: false,
        kv_fixed: true,
        context_policy_tier: ContextPolicyTier::FixedKv,
        history_rewrite_supported: true,
        ..Default::default()
    };
    let system = "# ABOUT\n".to_string() + &"detail ".repeat(1_500);
    let history = "history ".repeat(500);
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![
            msg("u1", AgentRole::User, &history),
            msg("a1", AgentRole::Assistant, &history),
            msg("u2", AgentRole::User, "latest question"),
        ],
        cfg,
        caps,
        serde_json::Value::Null,
        SessionIoParams {
            provider: "LLAMA".into(),
            context_window_tokens: 4096,
            tools_schema_json: "[]".into(),
            tool_catalog_json: "[]".into(),
            tool_schema_registry_json: "[]".into(),
            system_prompt: system,
        },
    );

    let before = eng.drain_events(&handle);
    let _ = before;

    match eng.next_action(&handle) {
        AgentAction::CallModel(req) => {
            let ctx = CompactionContext {
                provider: "LLAMA".into(),
                context_window_tokens: 4096,
                tools_schema_json: req.tools_schema_json.clone(),
                system_prompt: req.system_prompt.clone(),
                context_sidecars_estimate: String::new(),
                capabilities: SessionCapabilities {
                    summarization_allowed: false,
                    kv_fixed: true,
                    context_policy_tier: ContextPolicyTier::FixedKv,
                    history_rewrite_supported: true,
                    ..Default::default()
                },
                trigger: CompactionTrigger::Preflight,
            };
            let pressure = estimate_session_pressure(
                &AgentSession::new(req.session_messages.clone(), serde_json::Value::Null),
                &AgentConfig::for_policy_tier(ContextPolicyTier::FixedKv),
                &ctx,
            );
            assert!(
                !exceeds_hard_prompt_ceiling(&pressure),
                "projected prompt {} must fit within ceiling {}",
                pressure.total_tokens,
                pressure.usable_prompt_tokens
            );
        }
        other => panic!("expected CallModel after compaction, got {other:?}"),
    }

}

#[test]
fn text_truncate_shrinks_truncatable_messages_when_over_hard_ceiling() {
    let cfg = AgentConfig::for_policy_tier(ContextPolicyTier::FixedKv);
    let system = "sys ".repeat(900);
    let old = "history ".repeat(500);
    let truncatable = "block ".repeat(500);
    let session = AgentSession::new(
        vec![
            msg("u1", AgentRole::User, &old),
            msg("a1", AgentRole::Assistant, &old),
            msg("u2", AgentRole::User, &truncatable),
            msg("u3", AgentRole::User, "latest"),
        ],
        serde_json::Value::Null,
    );
    let ctx = CompactionContext {
        provider: "LLAMA".into(),
        context_window_tokens: 4096,
        tools_schema_json: "[]".into(),
        system_prompt: system,
        context_sidecars_estimate: String::new(),
        capabilities: SessionCapabilities {
            summarization_allowed: false,
            kv_fixed: true,
            context_policy_tier: ContextPolicyTier::FixedKv,
            ..Default::default()
        },
        trigger: CompactionTrigger::Preflight,
    };

    let before = estimate_session_pressure(&session, &cfg, &ctx);
    assert!(exceeds_hard_prompt_ceiling(&before));

    match TextTruncateCompactionTier.apply(session, &cfg, &ctx) {
        CompactionOutcome::Applied(after_session, ev) => {
            assert_eq!(ev.tier, CompactionTier::TextTruncate);
            assert!(ev.tokens_after < ev.tokens_before);
            let after = estimate_session_pressure(&after_session, &cfg, &ctx);
            assert!(after.total_tokens < before.total_tokens);
        }
        other => panic!("expected text truncate Applied, got {other:?}"),
    }
}

#[test]
fn hosted_plan_counts_system_prompt_in_pressure() {
    let cfg = AgentConfig::for_policy_tier(ContextPolicyTier::HostedMutable);
    let session = AgentSession::new(vec![msg("u1", AgentRole::User, "short")], serde_json::Value::Null);
    let messages_only_parts = PromptBudgetParts {
        provider: "anthropic",
        context_window_tokens: 200_000,
        tools_schema_json: "[]",
        system_prompt: "",
        context_sidecars: "",
    };
    let messages_only = TokenLedger::estimate(&session, &cfg, &messages_only_parts);
    let system_prompt = "policy ".repeat(5_000);
    let with_system_parts = PromptBudgetParts {
        provider: "anthropic",
        context_window_tokens: 200_000,
        tools_schema_json: "[]",
        system_prompt: &system_prompt,
        context_sidecars: "",
    };
    let with_system = TokenLedger::estimate(&session, &cfg, &with_system_parts);
    assert!(with_system.total_tokens > messages_only.total_tokens);
}