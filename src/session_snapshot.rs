//! Typed session snapshot (ENC-305): the ONLY state that crosses a
//! process boundary for session persistence.
//!
//! [`SessionSnapshot`] mirrors `InnerSession` (engine/state.rs) field for
//! field, with three structural transformations, all forced by how
//! `create_session` consumes its boundary inputs:
//! - The stored `SessionIoParams` is split into its restorable triple
//!   (`provider`, `context_window_tokens`, `system_prompt`); the stored
//!   `io.schemas` is always the empty set after construction (the live
//!   schemas live in the universe), so import rebuilds that empty set.
//! - The `ToolUniverse` is split into `schemas` + `active_tool_names`,
//!   with the semantics and catalog carried by `config`.
//! - `config` is NOT the stored config verbatim: it is the EFFECTIVE
//!   config at export (tool fields sourced from the universe, so the
//!   snapshot is self-describing). Import consumes it the way
//!   construction consumes its boundary config: the policy-tier
//!   defaults are applied, the semantics are taken into
//!   `ToolUniverse::build` — construction's full validation — and the
//!   captured active set then replaces the built initial set.
//!
//! Everything else is carried verbatim: import performs NO ledger
//! re-seeding and NO counter resets, so a restored session continues
//! exactly where the exported one left off.

use serde::{Deserialize, Serialize};

use crate::action::AgentAction;
use crate::context_store::ContextStore;
use crate::engine::{EnginePhase, PendingToolTurn};
use crate::events::AgentEvent;
use crate::guardrails::GuardrailState;
use crate::loop_metrics::LoopMetrics;
use crate::session::{AgentConfig, AgentSession, SessionCapabilities};
use crate::sidecar_builder::SidecarBuilder;
use crate::state_ledger::StateLedger;
use crate::tool_name::ToolName;
use crate::tool_schema::ToolSchemaSet;

/// The snapshot format version carried by every [`SessionSnapshot`]
/// (ENC-305 fix pass, m5). The format is born with this field — there
/// are no pre-version snapshots — so it is a REQUIRED serde field, and
/// import rejects any other version with
/// `ToolSemanticsError::InvalidSnapshot`. Bump when the wire shape
/// changes incompatibly.
pub const SNAPSHOT_VERSION: u32 = 1;

/// The full restorable state of one session (ENC-305 D4).
///
/// Produced by `AgentEngine::export_snapshot`, consumed by
/// `AgentEngine::import_snapshot`. At the FFI surface it crosses as the
/// serde JSON of this typed struct — never as ad-hoc JSON state fields.
///
/// No `Debug` derive (ENC-305 fix pass, m7): a snapshot embeds full
/// pending model requests, and making the whole graph printable invites
/// dumping live session content into logs. `Clone` stays.
#[derive(Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Snapshot format version; must equal [`SNAPSHOT_VERSION`].
    pub version: u32,
    pub session: AgentSession,
    /// Effective config at export (tool semantics/catalog sourced from
    /// the universe). `tool_catalog` is serde-skipped on `AgentConfig`
    /// and rebuilt by `ToolUniverse::build` at import.
    pub config: AgentConfig,
    pub capabilities: SessionCapabilities,
    /// IO triple (see module docs): provider name.
    pub provider: String,
    /// IO triple: the model's total context-window size in tokens.
    pub context_window_tokens: u32,
    /// IO triple: the session's stable system prompt.
    pub system_prompt: String,
    /// The universe's schema entries at export.
    pub schemas: ToolSchemaSet,
    /// The universe's live active set at export; authoritative over the
    /// schemas' initial active names at import.
    pub active_tool_names: Vec<ToolName>,
    pub context_store: ContextStore,
    pub state_ledger: StateLedger,
    pub guardrails: GuardrailState,
    pub loop_metrics: LoopMetrics,
    pub phase: EnginePhase,
    pub pending: PendingToolTurn,
    pub sidecar_builder: SidecarBuilder,
    pub model_requests_this_turn: u32,
    pub events: Vec<AgentEvent>,
    pub turn_index: u32,
    pub cancelled: bool,
    pub terminal_action: Option<AgentAction>,
    pub last_model_response_greedy_tool_names: Vec<ToolName>,
    pub tools_blocked_nudges_this_turn: u32,
    pub compaction_passes_this_turn: u32,
    pub destructive_tiers_this_turn: u32,
    pub suppress_summary_tier: bool,
}
