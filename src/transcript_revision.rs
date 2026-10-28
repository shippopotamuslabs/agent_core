//! Append-only transcript revision and compaction boundaries.

use serde::{Deserialize, Serialize};

use crate::events::{AgentError, CompactionTier, CompactionTrigger};
use crate::message::{AgentMessage, MessageMeta};
use crate::session::AgentSession;

fn next_rewrite_message_id() -> String {
    format!("msg-{}", uuid::Uuid::new_v4())
}

/// Monotonic destructive-rewrite counter for a session transcript.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptRevision(pub u64);

/// Emitted once per compaction pass that applied at least one destructive tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionBoundary {
    pub boundary_id: String,
    pub revision_before: u64,
    pub revision_after: u64,
    pub trigger: CompactionTrigger,
    pub tiers_applied: Vec<CompactionTier>,
    pub message_count_before: u32,
    pub message_count_after: u32,
    pub destructive: bool,
}

impl CompactionBoundary {
    /// Literal format required by tests: `cb-{revision_after}-{trigger}`.
    pub fn format_boundary_id(revision_after: u64, trigger: CompactionTrigger) -> String {
        let trigger_snake = match trigger {
            CompactionTrigger::Preflight => "preflight",
            CompactionTrigger::MidTurn => "mid_turn",
            CompactionTrigger::PostToolResult => "post_tool_result",
            CompactionTrigger::Manual => "manual",
        };
        format!("cb-{revision_after}-{trigger_snake}")
    }
}

/// Result of scanning the transcript for in-place content edits without rewrite metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendOnlyInvariantResult {
    pub valid: bool,
    pub violation_message: Option<String>,
}

/// Mutation-defense harness: fails when any message id's content changed without
/// `metadata.superseded` on the prior row and a replacement with `metadata.rewrite_of`.
pub fn assert_append_only_transcript_invariant(
    session: &AgentSession,
    prior_snapshot: &[AgentMessage],
) -> AppendOnlyInvariantResult {
    let prior_by_id: std::collections::HashMap<&str, &str> = prior_snapshot
        .iter()
        .map(|m| (m.id.as_str(), m.content.as_str()))
        .collect();

    for message in &session.messages {
        let Some(prior_content) = prior_by_id.get(message.id.as_str()) else {
            continue;
        };
        if prior_content == &message.content {
            continue;
        }
        if message.metadata.is_superseded() {
            continue;
        }
        let has_replacement = session
            .messages
            .iter()
            .any(|m| m.metadata.rewrite_of() == Some(message.id.as_str()));
        if has_replacement {
            continue;
        }
        return AppendOnlyInvariantResult {
            valid: false,
            violation_message: Some(format!("append_only_violation:{}", message.id)),
        };
    }

    AppendOnlyInvariantResult {
        valid: true,
        violation_message: None,
    }
}

/// Append-only rewrite helper for destructive compaction tiers.
pub fn replace_message_content(
    session: &mut AgentSession,
    message_id: &str,
    new_content: String,
    tier: CompactionTier,
) -> Result<AgentMessage, AgentError> {
    let tier_label = tier.to_string();
    let position = session
        .messages
        .iter()
        .position(|m| m.id == message_id)
        .ok_or_else(|| AgentError::CompactionFailed {
            tier: tier_label.clone(),
            reason: format!("replace_message_content: message id {message_id} not found"),
        })?;

    if let Some(original) = session.messages.get_mut(position) {
        original.metadata.mark_superseded();
    }

    let replacement = AgentMessage {
        id: next_rewrite_message_id(),
        role: session.messages[position].role.clone(),
        content: new_content,
        metadata: MessageMeta::new()
            .with_rewrite_of(message_id)
            .with_compaction_tier(tier_label),
    };

    session.messages.insert(position + 1, replacement.clone());
    Ok(replacement)
}

/// Records one destructive compaction pass: bumps revision once and builds boundary metadata.
pub fn record_destructive_compaction_boundary(
    session: &mut AgentSession,
    trigger: CompactionTrigger,
    tiers_applied: Vec<CompactionTier>,
    message_count_before: u32,
) -> CompactionBoundary {
    let revision_before = session.transcript_revision;
    let revision_after = bump_transcript_revision(session);
    CompactionBoundary {
        boundary_id: CompactionBoundary::format_boundary_id(revision_after, trigger),
        revision_before,
        revision_after,
        trigger,
        tiers_applied,
        message_count_before,
        message_count_after: session.messages.len() as u32,
        destructive: true,
    }
}

pub fn transcript_revision_of(session: &AgentSession) -> u64 {
    session.transcript_revision
}

pub fn bump_transcript_revision(session: &mut AgentSession) -> u64 {
    session.transcript_revision = session.transcript_revision.saturating_add(1);
    session.transcript_revision
}
