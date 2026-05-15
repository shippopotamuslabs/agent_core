pub mod pipeline;

use crate::events::{CompactionEvent, CompactionTrigger};
use crate::session::{AgentConfig, AgentSession};
use crate::turn_io::SummaryRequest;

/// Context passed to every compaction tier on each pipeline run.
pub struct CompactionContext {
    /// Provider name for `AgentConfig.token_correction` lookup (e.g. `"anthropic"`).
    pub provider: String,
    /// Model's total context-window size in tokens.
    pub context_window_tokens: u32,
    /// Raw tools-schema JSON from `ModelRequest`; used for token reservation.
    pub tools_schema_json: String,
    /// Which phase of the turn loop triggered this compaction pass.
    pub trigger: CompactionTrigger,
}

/// Result of a single compaction tier (or whole pipeline) run.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_context_fields() {
        let ctx = CompactionContext {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tools_schema_json: "[]".into(),
            trigger: CompactionTrigger::Preflight,
        };
        assert_eq!(ctx.provider, "anthropic");
        assert_eq!(ctx.context_window_tokens, 200_000);
    }
}
