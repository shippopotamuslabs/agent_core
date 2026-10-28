//! Tier 3 — artifact squash: collapse `append_blocks` call/result pairs into one concise
//! [`AgentRole::System`] line so long artifact loops fit the context window.
//!
//! Spec string (ADR / tickets):
//! `Artifact generation in progress. [N] blocks written so far. Last block: \`snippet\`. Continue appending.`
//!
//! ENC-302 Option A: the squash summary and the ledger anchor are derived
//! caches of engine-processed append results, not ground truth. The rendered
//! count is the newest observed value (`unknown` when unobserved) and may be
//! stale; the host artifact is the source of truth. Never branch on it.

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
    observed_total_blocks: Option<u32>,
    last_block: Option<&str>,
) -> String {
    let snippet = last_block
        .map(|t| tail_chars(t, semantics.snippet_max_chars))
        .unwrap_or_default();
    // ENC-302: the cached count is display-only; an unobserved count renders
    // as the literal `unknown`. The template's continuation text is untouched.
    let observed = observed_total_blocks
        .map(|t| t.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    semantics
        .summary_template
        .replace("{total}", &observed)
        .replace("{snippet}", &snippet)
}

pub(crate) fn metadata_has_tool(msg: &AgentMessage, tool_name: &str) -> bool {
    msg.metadata
        .raw_tool_calls()
        .iter()
        .any(|call| call.name == tool_name)
}

/// ENC-293/ENC-302: the one message-level append classification shared by
/// the squash engagement scan, the anchor extraction, and the spill
/// exemption. One parse per message — never a field-name substring sniff —
/// so large semantic results still reach the squash tier.
///
/// Engagement rule (BASE-identical for the metadata-less path):
/// - Assistant messages are never classified here (assistant append calls
///   engage the drop scan via `metadata_has_tool` only; they carry no
///   observed count).
/// - Tool/User results scoped to the configured append tool via `tool_name`
///   metadata (e.g. a `start_artifact` success result is never an append
///   result) engage on success alone; a missing total field yields an
///   unknown count.
/// - Metadata-less Tool/User results keep the BASE total-field-presence
///   guard: success alone does NOT engage — the total field must be present
///   (any value, including non-integer/`null`/`0`), with the observed count
///   parsed-or-`None`.
/// - Any other tool's metadata never engages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObservedAppend {
    NotAppend,
    Success { observed_total_blocks: Option<u32> },
}

/// ENC-302: observed-count parse over an already-parsed result value.
/// `Some(v)` when the total field is present, an integer (or an
/// integer-valued float such as `3.0`), and `>= 0` (a host-reported `0`
/// after success is contradictory but recorded verbatim);
/// absent/non-integer/negative/unrepresentable values map to `None` instead
/// of a `0` stand-in.
fn parse_observed_total_blocks(v: &Value, semantics: &ArtifactSquashSemantics) -> Option<u32> {
    match v.get(&semantics.total_blocks_result_field)? {
        Value::Number(n) => n
            .as_u64()
            .and_then(|u| u32::try_from(u).ok())
            .or_else(|| n.as_i64().and_then(|i| u32::try_from(i).ok()))
            .or_else(|| {
                n.as_f64().and_then(|f| {
                    (f.fract() == 0.0 && f >= 0.0 && f <= u32::MAX as f64).then_some(f as u32)
                })
            }),
        Value::String(s) => s.parse::<i64>().ok().and_then(|i| u32::try_from(i).ok()),
        _ => None,
    }
}

/// The single shared predicate: classifies one Tool/User result message.
/// Assistant and non-Tool/User roles are never append results here.
pub(crate) fn classify_append_message(
    msg: &AgentMessage,
    semantics: &ArtifactSquashSemantics,
) -> ObservedAppend {
    if !matches!(msg.role, AgentRole::Tool | AgentRole::User) {
        return ObservedAppend::NotAppend;
    }
    // ENC-288: single ingress parse; typed accessors only.
    let parsed = ParsedToolResult::parse(&msg.content);
    let v = parsed.value();
    if v.get(&semantics.status_field).and_then(|s| s.as_str())
        != Some(semantics.success_status_value.as_str())
    {
        return ObservedAppend::NotAppend;
    }
    match msg.metadata.tool_name() {
        Some(name) if name == semantics.append_tool_name.as_str() => ObservedAppend::Success {
            observed_total_blocks: parse_observed_total_blocks(v, semantics),
        },
        Some(_) => ObservedAppend::NotAppend,
        // Metadata-less: BASE guard — the total field must be present (any
        // value); success alone does not engage.
        None if v.get(&semantics.total_blocks_result_field).is_none() => ObservedAppend::NotAppend,
        None => ObservedAppend::Success {
            observed_total_blocks: parse_observed_total_blocks(v, semantics),
        },
    }
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
        return metadata_has_tool(msg, semantics.append_tool_name.as_str());
    }
    matches!(
        classify_append_message(msg, semantics),
        ObservedAppend::Success { .. }
    )
}

fn artifact_id_from_assistant_metadata(
    msg: &AgentMessage,
    semantics: &ArtifactSquashSemantics,
) -> Option<String> {
    msg.metadata.raw_tool_calls().iter().find_map(|call| {
        if call.name.as_str() != semantics.append_tool_name.as_str() {
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
/// index, its observed total-block count (`None` when the result carried no
/// integer total), and the preceding assistant's last-block text, if any.
/// The one canonical definition of "most recent successful append" shared by
/// the anchor extraction and the squash scan.
///
/// ENC-288 fusion, kept inside the FRG-43 structure: the reverse scan calls
/// the single-parse `classify_append_message` once per message, so the
/// success check and the observed-count extraction share one parse — the
/// found payload is never parsed a second time.
///
/// ENC-302: existence is the newest observed successful append result alone;
/// there is no total<=0 kill. The cached count may be stale (out-of-band
/// host growth is invisible until the next observed result); the host
/// artifact is the source of truth, never the cache.
fn last_append_lookup(
    messages: &[AgentMessage],
    semantics: &ArtifactSquashSemantics,
) -> Option<(usize, Option<u32>, Option<String>)> {
    let (result_idx, observed) = messages.iter().enumerate().rev().find_map(|(i, msg)| {
        match classify_append_message(msg, semantics) {
            ObservedAppend::Success {
                observed_total_blocks,
            } => Some((i, observed_total_blocks)),
            ObservedAppend::NotAppend => None,
        }
    })?;
    Some((
        result_idx,
        observed,
        last_block_text_before(messages, result_idx, semantics),
    ))
}

pub fn extract_artifact_anchor(
    messages: &[AgentMessage],
    semantics: &ArtifactSquashSemantics,
) -> Option<ArtifactStateAnchor> {
    let (result_idx, observed_total_blocks, last_block_text) =
        last_append_lookup(messages, semantics)?;

    Some(ArtifactStateAnchor {
        artifact_id: anchor_artifact_id(messages, result_idx, semantics)
            .unwrap_or_else(|| "unknown-artifact".to_string()),
        observed_total_blocks,
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

/// Reverse scan for the most recent successful append result: its observed
/// total-block count plus the assistant's last-block text, if any.
fn find_last_append_block(
    messages: &[AgentMessage],
    semantics: &ArtifactSquashSemantics,
) -> Option<(Option<u32>, Option<String>)> {
    let (_, observed_total_blocks, last_block_text) = last_append_lookup(messages, semantics)?;
    Some((observed_total_blocks, last_block_text))
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

    let Some((observed_total_blocks, last_block_text)) =
        find_last_append_block(&session.messages, semantics)
    else {
        return CompactionOutcome::NotApplicable;
    };

    let summary_text =
        build_semantic_summary_line(semantics, observed_total_blocks, last_block_text.as_deref());
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
                    // ENC-293: assistant append detection reads metadata only.
                    metadata: MessageMeta::new().with_raw_tool_calls(vec![RawToolCall {
                        id: None,
                        name: "append_blocks".into(),
                        arguments_json: "{}".into(),
                    }]),
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

    /// ENC-302: an append-tool-metadata success result with no total field
    /// still engages the squash scan; the summary renders `unknown` and the
    /// continuation instruction is emitted verbatim. (A metadata-less
    /// success without a total field does NOT engage — BASE guard; see the
    /// regression test below.)
    #[test]
    fn enc302_squash_summary_renders_unknown_when_total_unobserved() {
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
                    metadata: MessageMeta::new().with_raw_tool_calls(vec![RawToolCall {
                        id: None,
                        name: "append_blocks".into(),
                        arguments_json: r#"{"artifactId":"123"}"#.into(),
                    }]),
                },
                AgentMessage {
                    id: "m2".into(),
                    role: AgentRole::Tool,
                    content: r#"{"status":"success"}"#.into(),
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
                assert_eq!(s.messages[1].role, AgentRole::System);
                assert!(
                    s.messages[1].content.contains("unknown blocks"),
                    "unobserved count must render literally: {}",
                    s.messages[1].content
                );
                assert!(
                    s.messages[1].content.contains("Part 1"),
                    "snippet must survive: {}",
                    s.messages[1].content
                );
                assert!(
                    s.messages[1].content.contains("Continue appending."),
                    "continuation must be present regardless of count: {}",
                    s.messages[1].content
                );
            }
            o => panic!("expected Applied, got {o:?}"),
        }

        let anchor_messages = vec![
            AgentMessage {
                id: "m1".into(),
                role: AgentRole::Assistant,
                content: r#"call:append_blocks{"artifactId":"123","blocks":[{"type":"paragraph","text":"Part 1"}]}"#
                    .into(),
                metadata: MessageMeta::new().with_raw_tool_calls(vec![RawToolCall {
                    id: None,
                    name: "append_blocks".into(),
                    arguments_json: r#"{"artifactId":"123"}"#.into(),
                }]),
            },
            AgentMessage {
                id: "m2".into(),
                role: AgentRole::Tool,
                content: r#"{"status":"success"}"#.into(),
                metadata: MessageMeta::new().with_tool_name("append_blocks"),
            },
        ];
        let squash = cfg
            .tool_semantics
            .compaction
            .artifact_squash
            .as_ref()
            .expect("fixture must configure artifact_squash");
        let anchor = extract_artifact_anchor(&anchor_messages, squash).expect("anchor must exist");
        assert_eq!(anchor.observed_total_blocks, None);
    }

    /// ENC-302 J1 regression: a metadata-less `{"status":"success"}` with
    /// no total field is NOT squash-engaged (BASE total-field-presence
    /// guard) — nothing is dropped and the tier does not apply.
    #[test]
    fn enc302_metadata_less_success_without_total_not_engaged() {
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
                    role: AgentRole::Tool,
                    content: r#"{"status":"success"}"#.into(),
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
            CompactionOutcome::NotApplicable => {}
            o => panic!("expected NotApplicable, got {o:?}"),
        }

        let squash = cfg
            .tool_semantics
            .compaction
            .artifact_squash
            .as_ref()
            .expect("fixture must configure artifact_squash");
        let lone = AgentMessage {
            id: "m1".into(),
            role: AgentRole::Tool,
            content: r#"{"status":"success"}"#.into(),
            metadata: MessageMeta::new(),
        };
        assert_eq!(
            classify_append_message(&lone, squash),
            ObservedAppend::NotAppend
        );
        assert!(extract_artifact_anchor(std::slice::from_ref(&lone), squash).is_none());
    }

    /// ENC-302 m1: integer-valued float totals (`3.0`) observe as integers;
    /// genuinely fractional totals (`3.5`) engage (field present) but observe
    /// as unknown.
    #[test]
    fn enc302_integer_valued_float_total_observed() {
        let cfg = AgentConfig {
            tool_semantics: crate::tool_semantics::test_fixture_enclave_semantics(),
            ..Default::default()
        };
        let squash = cfg
            .tool_semantics
            .compaction
            .artifact_squash
            .as_ref()
            .expect("fixture must configure artifact_squash");
        let result = |payload: &str| AgentMessage {
            id: "r".into(),
            role: AgentRole::Tool,
            content: payload.into(),
            metadata: MessageMeta::new().with_tool_name("append_blocks"),
        };
        assert_eq!(
            classify_append_message(&result(r#"{"status":"success","totalBlocks":3.0}"#), squash),
            ObservedAppend::Success {
                observed_total_blocks: Some(3)
            }
        );
        assert_eq!(
            classify_append_message(&result(r#"{"status":"success","totalBlocks":3.5}"#), squash),
            ObservedAppend::Success {
                observed_total_blocks: None
            }
        );

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
                    metadata: MessageMeta::new().with_raw_tool_calls(vec![RawToolCall {
                        id: None,
                        name: "append_blocks".into(),
                        arguments_json: r#"{"artifactId":"123"}"#.into(),
                    }]),
                },
                result(r#"{"status":"success","totalBlocks":3.0}"#),
            ],
            serde_json::Value::Null,
        );
        match ArtifactSquashCompactionTier.apply(session, &cfg, &ctx()) {
            CompactionOutcome::Applied(s, _) => {
                assert!(
                    s.messages[1].content.contains("3 blocks"),
                    "{}",
                    s.messages[1].content
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
