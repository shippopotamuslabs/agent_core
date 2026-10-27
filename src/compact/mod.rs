pub mod artifact_snapshot;
pub mod artifact_squash;
pub mod fifo;
pub mod history_snapshot;
pub mod micro;
pub mod pipeline;
pub mod policy;
pub mod summary;
pub mod text_truncate;
pub mod tool_spill;

pub use artifact_squash::ArtifactSquashCompactionTier;
pub use fifo::FifoCompactionTier;
pub use history_snapshot::HistorySnapshotCompactionTier;
pub use micro::MicroCompactionTier;
pub use pipeline::CompactionPipeline;
pub use summary::{apply_summary_result, SummaryCompactionTier, SUMMARY_INSTRUCTION};
pub use text_truncate::TextTruncateCompactionTier;
pub use tool_spill::ToolSpillCompactionTier;

use crate::budget::{PromptBudgetParts, TokenLedger};

use crate::events::{
    AgentError, CompactionEvent, CompactionTier as CompactionTierKind, CompactionTrigger,
};
use crate::session::{AgentConfig, AgentSession, SessionCapabilities};
use crate::turn_io::SummaryRequest;

/// Context passed to every compaction tier on each pipeline run.
#[derive(Clone)]
pub struct CompactionContext {
    /// Provider name for `AgentConfig.token_correction` lookup (e.g. `"anthropic"`).
    pub provider: String,
    /// Model's total context-window size in tokens.
    pub context_window_tokens: u32,
    /// Raw tools-schema JSON from `ModelRequest`; included in prompt budget.
    pub tools_schema_json: String,
    /// Stable system prompt shipped separately from `session.messages`.
    pub system_prompt: String,
    /// Projected Kotlin context sidecars (empty uses conservative budget estimate).
    pub context_sidecars_estimate: String,
    /// Host/session capabilities (e.g. whether LLM summarization is allowed).
    pub capabilities: SessionCapabilities,
    /// Which phase of the turn loop triggered this compaction pass.
    pub trigger: CompactionTrigger,
    /// When true, the host declined summarization for this pass (e.g. utility ONNX unavailable).
    pub suppress_summary_tier: bool,
}

/// Full projected prompt pressure for compaction tiers and engine preflight checks.
pub fn estimate_session_pressure(
    session: &AgentSession,
    config: &AgentConfig,
    ctx: &CompactionContext,
) -> crate::session::Pressure {
    estimate_session_pressure_with(
        crate::token_counter::default_counter(),
        session,
        config,
        ctx,
    )
}

/// [`estimate_session_pressure`] counted with an explicit [`TokenCounter`]
/// instead of the process default (ENC-292 port). Lives here, next to
/// [`CompactionContext`], rather than in the token-counting port: the port
/// stays a leaf depending only on its tokenizer, and the high-level types
/// (session, config, budget parts) stay on the domain side of it.
pub fn estimate_session_pressure_with(
    counter: &dyn crate::token_counter::TokenCounter,
    session: &AgentSession,
    config: &AgentConfig,
    ctx: &CompactionContext,
) -> crate::session::Pressure {
    let parts = PromptBudgetParts {
        provider: &ctx.provider,
        context_window_tokens: ctx.context_window_tokens,
        tools_schema_json: &ctx.tools_schema_json,
        system_prompt: &ctx.system_prompt,
        context_sidecars: if ctx.context_sidecars_estimate.is_empty() {
            ""
        } else {
            &ctx.context_sidecars_estimate
        },
    };
    TokenLedger::estimate_with(counter, session, config, &parts)
}

/// Result of a single compaction tier (or whole pipeline) run.
// large_enum_variant: variants carry owned sessions by value so the engine can
// continue without cloning; boxing would ripple through every tier signature.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum CompactionOutcome {
    /// Tier ran successfully and session was compacted.
    Applied(AgentSession, CompactionEvent),

    /// Summary tier needs a host round-trip. Carries the session as it stands
    /// *after* any sync tiers that already ran — the engine stores this session
    /// and applies the summary result via `submit_summary`.
    NeedsHostCall(SummaryRequest, AgentSession),

    /// Tier cannot help (e.g. no reproducible results to drop, nothing to squash).
    NotApplicable,

    /// Tier attempted to run but encountered a precondition violation or
    /// internal error. The pipeline surfaces this as a typed error to the
    /// engine so the host sees a structured failure rather than a panic.
    Failed(AgentError),
}

/// Implemented by each compaction strategy.
/// Sync tiers (Micro, FIFO, ArtifactSquash) return `Applied` or `NotApplicable`.
/// The Summary tier returns `NeedsHostCall`.
pub trait CompactionTier: Send + Sync {
    /// Attempt to compact `session`.
    ///
    /// - Must not mutate `session` and return `NotApplicable` in the same call.
    /// - `Applied` session must have `CompactionEvent.tokens_before / tokens_after`
    ///   set (tier computes counts using [`crate::budget::TokenLedger`]).
    fn apply(
        &self,
        session: AgentSession,
        config: &AgentConfig,
        ctx: &CompactionContext,
    ) -> CompactionOutcome;
}

/// Locked tier order: Micro → ArtifactSquash → Summary → FIFO.
pub fn default_compaction_pipeline() -> CompactionPipeline {
    CompactionPipeline::new(vec![
        Box::new(MicroCompactionTier),
        Box::new(ArtifactSquashCompactionTier),
        Box::new(SummaryCompactionTier),
        Box::new(FifoCompactionTier),
    ])
}

pub fn compaction_pipeline_for_plan(plan: &policy::CompactionPlan) -> CompactionPipeline {
    let tiers = plan
        .ordered_tiers
        .iter()
        .map(|tier| match tier {
            CompactionTierKind::Micro => Box::new(MicroCompactionTier) as Box<dyn CompactionTier>,
            CompactionTierKind::ArtifactSquash => {
                Box::new(ArtifactSquashCompactionTier) as Box<dyn CompactionTier>
            }
            CompactionTierKind::Summary => {
                Box::new(SummaryCompactionTier) as Box<dyn CompactionTier>
            }
            CompactionTierKind::Fifo => Box::new(FifoCompactionTier) as Box<dyn CompactionTier>,
            CompactionTierKind::ToolSpill => {
                Box::new(ToolSpillCompactionTier) as Box<dyn CompactionTier>
            }
            CompactionTierKind::HistorySnapshot => {
                Box::new(HistorySnapshotCompactionTier) as Box<dyn CompactionTier>
            }
            CompactionTierKind::TextTruncate => {
                Box::new(TextTruncateCompactionTier) as Box<dyn CompactionTier>
            }
        })
        .collect();
    CompactionPipeline::new(tiers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_context_fields() {
        let ctx = CompactionContext {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tools_schema_json: "[]".into(),
            system_prompt: String::new(),
            context_sidecars_estimate: String::new(),
            capabilities: SessionCapabilities {
                summarization_allowed: true,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            trigger: CompactionTrigger::Preflight,
            suppress_summary_tier: false,
        };
        assert_eq!(ctx.provider, "anthropic");
        assert_eq!(ctx.context_window_tokens, 200_000);
    }
}
