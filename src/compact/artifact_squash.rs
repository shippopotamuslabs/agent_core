//! Tier 3 — artifact squash: collapse `append_blocks` call/result pairs into one concise
//! [`AgentRole::System`] line so long artifact loops fit the context window.
//!
//! Spec string (ADR / tickets):  
//! `Artifact generation in progress. [N] blocks written so far. Last block: \`snippet\`. Continue appending.`

use regex::Regex;

use super::estimate_session_pressure;
use crate::events::{CompactionEvent, CompactionTier as TierEnum};
use crate::message::{AgentMessage, AgentRole, MessageMeta};
use crate::parsed_tool_args::{ParsedToolArgs, ParsedToolResult};
use crate::session::{AgentConfig, AgentSession};
use crate::state_ledger::ArtifactStateAnchor;
use crate::tool_semantics::ArtifactSquashSemantics;
use serde_json::Value;

use super::{CompactionContext, CompactionOutcome, CompactionTier};

#[cfg(test)]
const SNIPPET_MAX_CHARS: usize = 200;

fn tail_chars(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if n <= max_chars {
        return chars.into_iter().collect();
    }
    chars[n - max_chars..].iter().collect()
}

fn build_semantic_summary_line(
    semantics: &ArtifactSquashSemantics,
    total_blocks: i64,
    last_block: Option<&str>,
) -> String {
    let snippet = last_block
        .map(|t| tail_chars(t, semantics.snippet_max_chars))
        .unwrap_or_default();
    semantics
        .summary_template
        .replace("{total}", &total_blocks.to_string())
        .replace("{snippet}", &snippet)
}

pub(crate) fn metadata_has_tool(msg: &AgentMessage, tool_name: &str) -> bool {
    msg.metadata
        .raw_tool_calls()
        .iter()
        .any(|call| call.name == tool_name)
}

/// ENC-293: shared typed probe for squash-claimed tool payloads. Both the
/// squash tier and the spill exemption use this parse — never a field-name
/// substring sniff — so large semantic results still reach the squash tier.
pub(crate) fn is_semantic_success_payload(
    content: &str,
    semantics: &ArtifactSquashSemantics,
) -> bool {
    semantic_success_total_blocks(content, semantics).is_some()
}

/// ENC-288: single ingress parse of a result payload; typed accessors only.
/// Returns the total-blocks value (`0` when the field is present but not an
/// integer, matching the previous `unwrap_or(0)` behavior).
fn semantic_success_total_blocks(
    content: &str,
    semantics: &ArtifactSquashSemantics,
) -> Option<i64> {
    let parsed = ParsedToolResult::parse(content);
    let v = parsed.value();
    if v.get(&semantics.status_field).and_then(|s| s.as_str())
        != Some(semantics.success_status_value.as_str())
        || v.get(&semantics.total_blocks_result_field).is_none()
    {
        return None;
    }
    Some(parse_semantic_total_blocks(v, semantics).unwrap_or(0))
}

fn parse_semantic_total_blocks(v: &Value, semantics: &ArtifactSquashSemantics) -> Option<i64> {
    v.get(&semantics.total_blocks_result_field).and_then(|b| {
        b.as_i64()
            .or_else(|| b.as_u64().map(|u| u as i64))
            .or_else(|| b.as_str().and_then(|s| s.parse::<i64>().ok()))
    })
}

fn extract_last_block_text_with_field(content: &str, field: &str) -> Option<String> {
    let pattern = format!(r#"(?s)"{}"\s*:\s*(\[.*?\])"#, regex::escape(field));
    let regex = Regex::new(&pattern).ok()?;
    let caps = regex.captures(content)?;
    let blocks_json = caps.get(1)?.as_str();
    let arr = serde_json::from_str::<Vec<Value>>(blocks_json).ok()?;
    let last = arr.last()?;
    let obj = last.as_object()?;
    if let Some(t) = obj.get("text").and_then(|x| x.as_str()) {
        return Some(t.to_string());
    }
    if let Some(items) = obj.get("items").and_then(|x| x.as_array()) {
        return items.last().and_then(|x| x.as_str()).map(|s| s.to_string());
    }
    if let Some(t) = obj.get("latex").and_then(|x| x.as_str()) {
        return Some(t.to_string());
    }
    None
}

/// ENC-293: assistant-side append detection reads typed `raw_tool_calls`
/// metadata only; the content-substring arm was deleted (`metadata_has_tool`
/// already covered it, and substring matching misfires on prose mentions).
fn is_semantic_append_message(msg: &AgentMessage, semantics: &ArtifactSquashSemantics) -> bool {
    if msg.role == AgentRole::Assistant {
        return metadata_has_tool(msg, &semantics.append_tool_name);
    }
    if matches!(msg.role, AgentRole::Tool | AgentRole::User) {
        return semantic_success_total_blocks(&msg.content, semantics).is_some();
    }
    false
}

fn artifact_id_from_assistant_metadata(
    msg: &AgentMessage,
    semantics: &ArtifactSquashSemantics,
) -> Option<String> {
    msg.metadata.raw_tool_calls().iter().find_map(|call| {
        if call.name != semantics.append_tool_name {
            return None;
        }
        // ENC-288: single ingress parse; typed accessor instead of re-parsing.
        ParsedToolArgs::parse(&call.arguments_json).get_str(&semantics.id_field)
    })
}

/// The assistant message immediately preceding `result_idx`, if it exists
/// and is an assistant message. The one definition of "the message before
/// the result" shared by the last-block and artifact-id lookups.
fn prev_assistant_before(messages: &[AgentMessage], result_idx: usize) -> Option<&AgentMessage> {
    if result_idx == 0 {
        return None;
    }
    let prev = &messages[result_idx - 1];
    (prev.role == AgentRole::Assistant).then_some(prev)
}

/// Last-block text comes from the assistant message immediately preceding the
/// append result, when present.
fn last_block_text_before(
    messages: &[AgentMessage],
    result_idx: usize,
    semantics: &ArtifactSquashSemantics,
) -> Option<String> {
    let prev = prev_assistant_before(messages, result_idx)?;
    extract_last_block_text_with_field(&prev.content, &semantics.blocks_field)
        .or_else(|| Some(prev.content.clone()).filter(|s| !s.is_empty()))
}

/// Newest assistant tool call at or after the result wins; otherwise the
/// assistant message just before the result. Mirrors the pre-refactor
/// reverse latch order.
fn anchor_artifact_id(
    messages: &[AgentMessage],
    result_idx: usize,
    semantics: &ArtifactSquashSemantics,
) -> Option<String> {
    if let Some(id) = messages[result_idx..]
        .iter()
        .rev()
        .filter(|msg| msg.role == AgentRole::Assistant)
        .find_map(|msg| artifact_id_from_assistant_metadata(msg, semantics))
    {
        return Some(id);
    }
    let prev = prev_assistant_before(messages, result_idx)?;
    artifact_id_from_assistant_metadata(prev, semantics)
}

/// The most recent successful append, resolved once: the result message
/// index, its total-block count, and the preceding assistant's last-block
/// text, if any. The one canonical definition of "most recent successful
/// append" shared by the anchor extraction and the squash scan.
///
/// ENC-288 fusion, kept inside the FRG-43 structure: the reverse scan calls
/// the single-parse `semantic_success_total_blocks` once per message, so the
/// success check and the total-block extraction share one parse — the found
/// payload is never parsed a second time.
fn last_append_lookup(
    messages: &[AgentMessage],
    semantics: &ArtifactSquashSemantics,
) -> Option<(usize, i64, Option<String>)> {
    let (result_idx, total_blocks) = messages.iter().enumerate().rev().find_map(|(i, msg)| {
        semantic_success_total_blocks(&msg.content, semantics).map(|t| (i, t))
    })?;
    Some((
        result_idx,
        total_blocks,
        last_block_text_before(messages, result_idx, semantics),
    ))
}

pub fn extract_artifact_anchor(
    messages: &[AgentMessage],
    semantics: &ArtifactSquashSemantics,
) -> Option<ArtifactStateAnchor> {
    let (result_idx, total_blocks, last_block_text) = last_append_lookup(messages, semantics)?;

    if total_blocks <= 0 {
        return None;
    }

    Some(ArtifactStateAnchor {
        artifact_id: anchor_artifact_id(messages, result_idx, semantics)
            .unwrap_or_else(|| "unknown-artifact".to_string()),
        total_blocks: total_blocks as u32,
        last_block_preview: last_block_text
            .map(|text| tail_chars(&text, semantics.snippet_max_chars))
            .unwrap_or_default(),
        continuation_instruction: semantics.continuation_instruction.clone(),
    })
}

pub struct ArtifactSquashCompactionTier;

impl Default for ArtifactSquashCompactionTier {
    fn default() -> Self {
        Self
    }
}

impl CompactionTier for ArtifactSquashCompactionTier {
    fn apply(
        &self,
        session: AgentSession,
        config: &AgentConfig,
        ctx: &CompactionContext,
    ) -> CompactionOutcome {
        let tokens_before = estimate_session_pressure(&session, config, ctx).total_tokens;

        let Some(semantics) = config.tool_semantics.compaction.artifact_squash.as_ref() else {
            return CompactionOutcome::NotApplicable;
        };
        apply_semantic_artifact_squash(session, config, ctx, tokens_before, semantics)
    }
}

/// Reverse scan for the most recent successful append result: its total-block
/// count plus the assistant's last-block text, if any.
fn find_last_append_block(
    messages: &[AgentMessage],
    semantics: &ArtifactSquashSemantics,
) -> Option<(i64, Option<String>)> {
    let (_, total_blocks, last_block_text) = last_append_lookup(messages, semantics)?;
    Some((total_blocks, last_block_text))
}

/// Second pass: drop every append-loop message, emitting one summary line
/// where each loop ends — including at the tail edge.
fn squash_appends(
    messages: Vec<AgentMessage>,
    semantics: &ArtifactSquashSemantics,
    summary_text: String,
) -> Vec<AgentMessage> {
    let mut out: Vec<AgentMessage> = Vec::with_capacity(messages.len());
    let mut in_append_loop = false;

    for msg in messages {
        let is_append = is_semantic_append_message(&msg, semantics);
        if is_append {
            in_append_loop = true;
            continue;
        }
        if in_append_loop {
            out.push(AgentMessage {
                id: format!("artifact-squash-{}", out.len()),
                role: AgentRole::System,
                content: summary_text.clone(),
                metadata: MessageMeta::new(),
            });
            in_append_loop = false;
        }
        out.push(msg);
    }
    if in_append_loop {
        out.push(AgentMessage {
            id: format!("artifact-squash-tail-{}", out.len()),
            role: AgentRole::System,
            content: summary_text.clone(),
            metadata: MessageMeta::new(),
        });
    }

    out
}

fn apply_semantic_artifact_squash(
    mut session: AgentSession,
    config: &AgentConfig,
    ctx: &CompactionContext,
    tokens_before: u32,
    semantics: &ArtifactSquashSemantics,
) -> CompactionOutcome {
    if !session
        .messages
        .iter()
        .any(|msg| is_semantic_append_message(msg, semantics))
    {
        return CompactionOutcome::NotApplicable;
    }

    let Some((total_blocks, last_block_text)) =
        find_last_append_block(&session.messages, semantics)
    else {
        return CompactionOutcome::NotApplicable;
    };

    let summary_text =
        build_semantic_summary_line(semantics, total_blocks, last_block_text.as_deref());
    let out = squash_appends(
        std::mem::take(&mut session.messages),
        semantics,
        summary_text,
    );

    session.messages = out;
    let tokens_after = estimate_session_pressure(&session, config, ctx).total_tokens;

    CompactionOutcome::Applied(
        session,
        CompactionEvent {
            tier: TierEnum::ArtifactSquash,
            tokens_before,
            tokens_after,
            trigger: ctx.trigger,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::CompactionTrigger;
    use crate::turn_io::RawToolCall;

    fn ctx() -> CompactionContext {
        CompactionContext {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tools_schema_json: "[]".into(),
            system_prompt: String::new(),
            context_sidecars_estimate: String::new(),
            capabilities: crate::session::SessionCapabilities {
                summarization_allowed: true,
                kv_fixed: false,
                tool_result_budget_tokens: None,
                ..Default::default()
            },
            trigger: CompactionTrigger::PostToolResult,
            suppress_summary_tier: false,
        }
    }

    /// Mirrors `ArtifactContextCompressorTest` history (Kotlin `ChatMessage` → Rust roles).
    #[test]
    fn squashes_append_blocks_retains_start_artifact_and_last_block_snippet() {
        let session = AgentSession::new(
            vec![
                AgentMessage {
                    id: "m0".into(),
                    role: AgentRole::User,
                    content: "Start a report".into(),
                    metadata: MessageMeta::new(),
                },
                AgentMessage {
                    id: "m1".into(),
                    role: AgentRole::Assistant,
                    content: r#"call:start_artifact{"title":"Report"}"#.into(),
                    metadata: MessageMeta::new(),
                },
                AgentMessage {
                    id: "m2".into(),
                    role: AgentRole::Tool,
                    content: r#"{"status":"success","artifactId":"123"}"#.into(),
                    metadata: MessageMeta::new().with_tool_name("start_artifact"),
                },
                AgentMessage {
                    id: "m3".into(),
                    role: AgentRole::Assistant,
                    content: r#"call:append_blocks{"artifactId":"123","blocks":[{"type":"paragraph","text":"Part 1"}]}"#
                        .into(),
                    // ENC-293: assistant append detection reads metadata only.
                    metadata: MessageMeta::new().with_raw_tool_calls(vec![RawToolCall {
                        id: None,
                        name: "append_blocks".into(),
                        arguments_json: "{}".into(),
                    }]),
                },
                AgentMessage {
                    id: "m4".into(),
                    role: AgentRole::Tool,
                    content: r#"{"status":"success","totalBlocks":1}"#.into(),
                    metadata: MessageMeta::new().with_tool_name("append_blocks"),
                },
                AgentMessage {
                    id: "m5".into(),
                    role: AgentRole::Assistant,
                    content: r#"call:append_blocks{"artifactId":"123","blocks":[{"type":"paragraph","text":"Part 2"}]}"#
                        .into(),
                    // ENC-293: assistant append detection reads metadata only.
                    metadata: MessageMeta::new().with_raw_tool_calls(vec![RawToolCall {
                        id: None,
                        name: "append_blocks".into(),
                        arguments_json: "{}".into(),
                    }]),
                },
                AgentMessage {
                    id: "m6".into(),
                    role: AgentRole::Tool,
                    content: r#"{"status":"success","totalBlocks":2}"#.into(),
                    metadata: MessageMeta::new().with_tool_name("append_blocks"),
                },
                AgentMessage {
                    id: "m7".into(),
                    role: AgentRole::Assistant,
                    content: "Now I will continue...".into(),
                    metadata: MessageMeta::new(),
                },
            ],
            serde_json::Value::Null,
        );

        let tier = ArtifactSquashCompactionTier;
        let cfg = AgentConfig {
            tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
            ..Default::default()
        };
        match tier.apply(session, &cfg, &ctx()) {
            CompactionOutcome::Applied(s, ev) => {
                assert_eq!(ev.tier, TierEnum::ArtifactSquash);
                assert_eq!(s.messages.len(), 5);
                assert_eq!(s.messages[0].content, "Start a report");
                assert!(s.messages[1].content.contains("start_artifact"));
                assert_eq!(s.messages[2].role, AgentRole::Tool);
                assert_eq!(s.messages[3].role, AgentRole::System);
                assert!(s.messages[3].content.contains("2 blocks"));
                assert!(s.messages[3].content.contains("Part 2"));
                assert_eq!(s.messages[4].content, "Now I will continue...");
            }
            o => panic!("expected Applied, got {o:?}"),
        }
    }

    #[test]
    fn not_applicable_without_append_blocks() {
        let session = AgentSession::new(
            vec![
                AgentMessage {
                    id: "a".into(),
                    role: AgentRole::User,
                    content: "Hello".into(),
                    metadata: MessageMeta::new(),
                },
                AgentMessage {
                    id: "b".into(),
                    role: AgentRole::Assistant,
                    content: "Hi".into(),
                    metadata: MessageMeta::new(),
                },
            ],
            serde_json::Value::Null,
        );

        assert!(matches!(
            ArtifactSquashCompactionTier.apply(session, &AgentConfig::default(), &ctx()),
            CompactionOutcome::NotApplicable
        ));
    }

    /// Characterization (FRG-43): when the append loop runs to the very end of
    /// history, the summary must be appended at the tail (id prefix
    /// `artifact-squash-tail-`), not dropped.
    #[test]
    fn squash_appends_summary_at_tail_when_loop_runs_to_end() {
        let session = AgentSession::new(
            vec![
                AgentMessage {
                    id: "m0".into(),
                    role: AgentRole::User,
                    content: "Start a report".into(),
                    metadata: MessageMeta::new(),
                },
                AgentMessage {
                    id: "m1".into(),
                    role: AgentRole::Assistant,
                    content: r#"call:append_blocks{"artifactId":"123","blocks":[{"type":"paragraph","text":"Part 1"}]}"#
                        .into(),
                    metadata: MessageMeta::new(),
                },
                AgentMessage {
                    id: "m2".into(),
                    role: AgentRole::Tool,
                    content: r#"{"status":"success","totalBlocks":1}"#.into(),
                    metadata: MessageMeta::new().with_tool_name("append_blocks"),
                },
            ],
            serde_json::Value::Null,
        );

        let tier = ArtifactSquashCompactionTier;
        let cfg = AgentConfig {
            tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
            ..Default::default()
        };
        match tier.apply(session, &cfg, &ctx()) {
            CompactionOutcome::Applied(s, _) => {
                assert_eq!(s.messages.len(), 2);
                assert_eq!(s.messages[0].content, "Start a report");
                assert_eq!(s.messages[1].role, AgentRole::System);
                assert!(s.messages[1].content.contains("1 blocks"));
                assert!(s.messages[1].content.contains("Part 1"));
                assert!(
                    s.messages[1].id.starts_with("artifact-squash-tail-"),
                    "tail summary id, got {}",
                    s.messages[1].id
                );
            }
            o => panic!("expected Applied, got {o:?}"),
        }
    }

    #[test]
    fn snippet_truncates_to_200_chars() {
        let long = "Z".repeat(250);
        let t = tail_chars(&long, SNIPPET_MAX_CHARS);
        assert_eq!(t.chars().count(), SNIPPET_MAX_CHARS);
        assert!(t.chars().all(|c| c == 'Z'));
    }
}
