//! TDD Red — context policy resolution tests (S20, S21, S22, E8, E10).

use std::sync::Arc;

use agent_core::action::AgentAction;
use agent_core::compact::policy::build_compaction_plan;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::{AgentError, CompactionTier, CompactionTrigger};
use agent_core::host::{AgentToolExecutor, AgentTransport, NoOpSummarizer};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::policy::{resolve_context_policy, ContextPolicyTier};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{ModelRequest, ModelResponse};

struct PanicTransport;
impl AgentTransport for PanicTransport {
    fn complete_model_turn(&self, _: ModelRequest) -> Result<ModelResponse, AgentError> {
        panic!("unused");
    }
}
struct PanicExec;
impl AgentToolExecutor for PanicExec {
    fn execute_tools(&self, _: Vec<ToolCallRecord>) -> Result<Vec<ToolResultRecord>, AgentError> {
        panic!("unused");
    }
}

#[test]
fn s20_hosted_mutable_policy_enables_all_features() {
    let caps = SessionCapabilities {
        context_policy_tier: ContextPolicyTier::HostedMutable,
        summarization_allowed: true,
        kv_fixed: false,
        tool_schema_deferral_supported: true,
        file_backed_context_supported: true,
        prompt_cache_supported: true,
        ..Default::default()
    };
    let cfg = AgentConfig::default();
    let policy = resolve_context_policy(&cfg, &caps, "anthropic", 200_000);
    assert!(policy.summarization_allowed);
    assert!(policy.history_rewrite_allowed);
    assert!(policy.tool_schema_deferral_allowed);
    assert!(policy.file_backed_context_allowed);
    assert!(policy.prompt_cache_hinting_allowed);
}

#[test]
fn s21_fixed_kv_policy_disables_summary_and_history_rewrite() {
    let caps = SessionCapabilities {
        context_policy_tier: ContextPolicyTier::FixedKv,
        summarization_allowed: true,
        kv_fixed: true,
        history_rewrite_supported: false,
        ..Default::default()
    };
    let cfg = AgentConfig::default();
    let policy = resolve_context_policy(&cfg, &caps, "local", 4096);
    assert!(!policy.summarization_allowed);
    assert!(!policy.history_rewrite_allowed);
    assert!(policy.kv_rebuild_required_on_rewrite);
    assert!(!policy.fifo_allowed_by_default);
}

#[test]
fn s22_high_context_raises_thresholds() {
    let caps = SessionCapabilities {
        context_policy_tier: ContextPolicyTier::HighContext,
        prompt_cache_supported: true,
        ..Default::default()
    };
    let cfg = AgentConfig::default();
    let policy = resolve_context_policy(&cfg, &caps, "anthropic", 200_000);
    // High context relaxes preflight threshold to ~0.90 per spec S22.
    let plan = build_compaction_plan(&cfg, &caps, &policy, CompactionTrigger::Preflight, 200_000);
    assert!(
        (plan.threshold_ratio - 0.90).abs() < f32::EPSILON,
        "expected preflight threshold ratio 0.90 for high context, got {}",
        plan.threshold_ratio
    );

    // Spec S22 also requires that a prompt-cache-supported high-context
    // session emits exactly one cache hint covering the stable prefix.
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let stable_prefix = "stable ".repeat(2_500); // ~10k tokens by char/4 heuristic
    let handle = eng.create_session(
        vec![AgentMessage {
            id: "u0".into(),
            role: AgentRole::User,
            content: stable_prefix,
            metadata: serde_json::Value::Null,
        }],
        cfg.clone(),
        caps.clone(),
        serde_json::Value::Null,
        SessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tool_catalog_json: "[]".into(),
            tool_schema_registry_json: "[]".into(),
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        },
    );
    match eng.next_action(&handle) {
        AgentAction::CallModel(req) => {
            assert_eq!(
                req.cache_hints.len(),
                1,
                "expected exactly one cache hint for prompt-cache-supported high-context session, got {:?}",
                req.cache_hints
            );
        }
        other => panic!("expected CallModel, got {other:?}"),
    }
}

#[test]
fn e8_fixed_kv_rejects_history_rewrite_plan() {
    let caps = SessionCapabilities {
        context_policy_tier: ContextPolicyTier::FixedKv,
        kv_fixed: true,
        history_rewrite_supported: false,
        ..Default::default()
    };
    let cfg = AgentConfig::default();
    let policy = resolve_context_policy(&cfg, &caps, "local", 4096);
    let plan = build_compaction_plan(&cfg, &caps, &policy, CompactionTrigger::Preflight, 4096);
    assert!(
        !plan.ordered_tiers.contains(&CompactionTier::Summary),
        "fixed_kv must not include Summary tier"
    );
    assert!(
        !plan.ordered_tiers.contains(&CompactionTier::Fifo),
        "fixed_kv must not include Fifo tier"
    );
    assert!(!plan.allow_history_rewrite);
}

#[test]
fn e10_cache_hints_omitted_when_unsupported() {
    let caps = SessionCapabilities {
        prompt_cache_supported: false,
        ..Default::default()
    };
    let cfg = AgentConfig::default();
    let policy = resolve_context_policy(&cfg, &caps, "anthropic", 200_000);
    assert!(!policy.prompt_cache_hinting_allowed);
}
