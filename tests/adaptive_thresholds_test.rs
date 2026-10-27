#![cfg(feature = "lalr-harness")]

use agent_core::budget::{
    effective_micro_keep_recent, effective_output_reserve_tokens,
    effective_retention_window_tokens, effective_tool_spill_threshold_bytes,
};
use agent_core::compact::{CompactionContext, CompactionOutcome, CompactionTier};
use agent_core::events::CompactionTrigger;
use agent_core::message::{AgentMessage, AgentRole, MessageMeta};
use agent_core::policy::ContextPolicyTier;
use agent_core::session::{AgentConfig, AgentSession, SessionCapabilities};

#[test]
fn at_4k_window_output_reserve_clamps_to_quarter_window() {
    let config = AgentConfig::for_policy_tier(ContextPolicyTier::SmallContext);

    assert_eq!(effective_output_reserve_tokens(&config, 4096), 1024);
}

#[test]
fn at_200k_window_retention_is_16k_not_50k() {
    let config = AgentConfig::for_policy_tier(ContextPolicyTier::HostedMutable);

    assert_eq!(effective_retention_window_tokens(&config, 200_000), 16_384);
}

#[test]
fn at_4k_window_summary_tier_requests_host_when_allowed() {
    let config = AgentConfig::for_policy_tier(ContextPolicyTier::SmallContext);
    let session = AgentSession::new(
        vec![
            message("u1", AgentRole::User, &"older user ".repeat(80)),
            message("a1", AgentRole::Assistant, &"older assistant ".repeat(80)),
            message("u2", AgentRole::User, "current user"),
        ],
        serde_json::Value::Null,
    );
    let ctx = CompactionContext {
        provider: "local".into(),
        context_window_tokens: 4096,
        tools_schema_json: "[]".into(),
        system_prompt: String::new(),
        context_sidecars_estimate: String::new(),
        capabilities: SessionCapabilities {
            summarization_allowed: true,
            kv_fixed: true,
            context_policy_tier: ContextPolicyTier::SmallContext,
            history_rewrite_supported: true,
            ..Default::default()
        },
        trigger: CompactionTrigger::Preflight,
        suppress_summary_tier: false,
    };

    let outcome = agent_core::compact::summary::SummaryCompactionTier.apply(session, &config, &ctx);

    assert!(matches!(outcome, CompactionOutcome::NeedsHostCall(_, _)));
}

#[test]
fn at_4k_window_micro_keep_recent_is_1() {
    let config = AgentConfig::for_policy_tier(ContextPolicyTier::SmallContext);

    assert_eq!(effective_micro_keep_recent(&config, 4096), 1);
}

#[test]
fn at_4k_window_tool_spill_triggers_below_8k_bytes() {
    let config = AgentConfig::for_policy_tier(ContextPolicyTier::HostedMutable);

    assert_eq!(effective_tool_spill_threshold_bytes(&config, 4096), 1024);
}

fn message(id: &str, role: AgentRole, content: &str) -> AgentMessage {
    AgentMessage {
        id: id.into(),
        role,
        content: content.into(),
        metadata: MessageMeta::new(),
    }
}
