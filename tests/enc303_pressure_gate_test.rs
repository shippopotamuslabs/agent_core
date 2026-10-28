//! ENC-303 Part 2 — pressure-class rule table + confirmation gate pins.

use agent_core::budget::effective_hard_prompt_ceiling_tokens;
use agent_core::compact::policy::{
    apply_confirmation_gate, build_compaction_plan, tier_pressure_class, CompactionPlan,
    PressureClass,
};
use agent_core::events::{CompactionTier, CompactionTrigger};
use agent_core::policy::{resolve_context_policy, ContextPolicyTier};
use agent_core::session::{AgentConfig, Pressure, SessionCapabilities};

fn below_ceiling() -> Pressure {
    Pressure {
        total_tokens: 1_000,
        threshold_tokens: 3_000,
        usable_prompt_tokens: 3_072,
        ratio: 0.25,
    }
}

fn over_ceiling() -> Pressure {
    Pressure {
        total_tokens: 4_000,
        threshold_tokens: 3_000,
        usable_prompt_tokens: 3_072,
        ratio: 1.1,
    }
}

fn fixed_kv_plan(trigger: CompactionTrigger) -> (AgentConfig, CompactionPlan) {
    let cfg = AgentConfig::for_policy_tier(ContextPolicyTier::FixedKv);
    let caps = SessionCapabilities {
        summarization_allowed: false,
        kv_fixed: true,
        context_policy_tier: ContextPolicyTier::FixedKv,
        history_rewrite_supported: true,
        ..Default::default()
    };
    let policy = resolve_context_policy(&cfg, &caps, "LLAMA", 4096);
    let plan = build_compaction_plan(&cfg, &caps, &policy, trigger, 4096, false);
    (cfg, plan)
}

#[test]
fn enc303_tier_pressure_class_table() {
    // Q3(c): every tier maps to exactly one pressure class.
    for tier in [
        CompactionTier::ToolSpill,
        CompactionTier::ArtifactSquash,
        CompactionTier::HistorySnapshot,
    ] {
        assert_eq!(
            tier_pressure_class(&tier),
            PressureClass::Reversible,
            "{tier:?} must be Reversible"
        );
    }
    for tier in [CompactionTier::Micro, CompactionTier::Summary] {
        assert_eq!(
            tier_pressure_class(&tier),
            PressureClass::EstimateAllowed,
            "{tier:?} must be EstimateAllowed"
        );
    }
    for tier in [CompactionTier::Fifo, CompactionTier::TextTruncate] {
        assert_eq!(
            tier_pressure_class(&tier),
            PressureClass::ConfirmationGated,
            "{tier:?} must be ConfirmationGated"
        );
    }
}

#[test]
fn enc303_ratio_plan_without_snapshot_satisfaction_excludes_gated_tiers() {
    // Q3(a): without the snapshot-first proxy (no HistorySnapshot in the
    // plan and the snapshot requirement on), a ratio-triggered plan loses
    // both ConfirmationGated tiers below the hard ceiling.
    let mut plan = CompactionPlan {
        trigger: CompactionTrigger::Preflight,
        threshold_ratio: 0.80,
        threshold_tokens: 3_000,
        ordered_tiers: vec![
            CompactionTier::ToolSpill,
            CompactionTier::Micro,
            CompactionTier::ArtifactSquash,
            CompactionTier::Summary,
            CompactionTier::Fifo,
            CompactionTier::TextTruncate,
        ],
        allow_history_rewrite: true,
        require_snapshot_before_destructive_drop: true,
        max_prompt_tokens_after_plan: 3_072,
    };
    apply_confirmation_gate(&mut plan, &below_ceiling());
    assert!(!plan.ordered_tiers.contains(&CompactionTier::Fifo));
    assert!(!plan.ordered_tiers.contains(&CompactionTier::TextTruncate));
    assert_eq!(plan.ordered_tiers.len(), 4);

    // The snapshot requirement is Fifo-only: with the requirement off,
    // Fifo stays while TextTruncate still drops below the ceiling.
    let mut relaxed = CompactionPlan {
        require_snapshot_before_destructive_drop: false,
        ..plan.clone()
    };
    relaxed.ordered_tiers = vec![
        CompactionTier::ToolSpill,
        CompactionTier::Micro,
        CompactionTier::ArtifactSquash,
        CompactionTier::Summary,
        CompactionTier::Fifo,
        CompactionTier::TextTruncate,
    ];
    apply_confirmation_gate(&mut relaxed, &below_ceiling());
    assert!(relaxed.ordered_tiers.contains(&CompactionTier::Fifo));
    assert!(!relaxed
        .ordered_tiers
        .contains(&CompactionTier::TextTruncate));
}

#[test]
fn enc303_built_ratio_plan_drops_text_truncate_below_ceiling() {
    // Built ratio plans always run HistorySnapshot first, so Fifo's
    // snapshot proxy is satisfied — but TextTruncate has no proxy and
    // drops below the hard ceiling. This is the one deliberate behavior
    // change of the Q2 gate (re-pin of the raw-plan expectation).
    let (_cfg, mut plan) = fixed_kv_plan(CompactionTrigger::Preflight);
    assert!(plan.ordered_tiers.contains(&CompactionTier::Fifo));
    assert!(plan.ordered_tiers.contains(&CompactionTier::TextTruncate));
    apply_confirmation_gate(&mut plan, &below_ceiling());
    assert!(
        plan.ordered_tiers.contains(&CompactionTier::Fifo),
        "plan-built HistorySnapshot satisfies Fifo, got {:?}",
        plan.ordered_tiers
    );
    assert!(
        !plan.ordered_tiers.contains(&CompactionTier::TextTruncate),
        "TextTruncate needs the hard ceiling on a ratio trigger, got {:?}",
        plan.ordered_tiers
    );
    assert_eq!(
        plan.max_prompt_tokens_after_plan,
        effective_hard_prompt_ceiling_tokens(
            &AgentConfig::for_policy_tier(ContextPolicyTier::FixedKv),
            4096
        )
    );
}

#[test]
fn enc303_hard_ceiling_keeps_gated_tiers_in_ratio_plan() {
    // Q3(b): over the hard ceiling the ratio plan keeps both gated tiers.
    let (_cfg, mut plan) = fixed_kv_plan(CompactionTrigger::Preflight);
    apply_confirmation_gate(&mut plan, &over_ceiling());
    assert!(plan.ordered_tiers.contains(&CompactionTier::Fifo));
    assert!(plan.ordered_tiers.contains(&CompactionTier::TextTruncate));
}

#[test]
fn enc303_manual_plan_keeps_gated_tiers_below_ceiling() {
    // Q3(b): a manual trigger is the host's explicit confirmation — the
    // gate leaves the plan untouched even below the hard ceiling.
    let (_cfg, mut plan) = fixed_kv_plan(CompactionTrigger::Manual);
    let before = plan.ordered_tiers.clone();
    assert!(before.contains(&CompactionTier::Fifo));
    assert!(before.contains(&CompactionTier::TextTruncate));
    apply_confirmation_gate(&mut plan, &below_ceiling());
    assert_eq!(plan.ordered_tiers, before);
}
