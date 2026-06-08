//! Tier 3 — artifact squash: collapse `append_blocks` call/result pairs into one concise
//! [`AgentRole::System`] line so long artifact loops fit the context window.
//!
//! Spec string (ADR / tickets):  
//! `Artifact generation in progress. [N] blocks written so far. Last block: \`snippet\`. Continue appending.`

use regex::Regex;

use super::estimate_session_pressure;
use crate::events::{CompactionEvent, CompactionTier as TierEnum};
use crate::message::{AgentMessage, AgentRole};
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

fn metadata_has_tool(msg: &AgentMessage, tool_name: &str) -> bool {
    msg.metadata
        .get("raw_tool_calls")
        .and_then(|v| v.as_array())
        .map(|calls| {
            calls.iter().any(|call| {
                call.get("name")
                    .and_then(|v| v.as_str())
                    .map(|name| name == tool_name)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn is_semantic_success_payload(content: &str, semantics: &ArtifactSquashSemantics) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(content) else {
        return false;
    };
    v.get(&semantics.status_field).and_then(|s| s.as_str())
        == Some(semantics.success_status_value.as_str())
        && v.get(&semantics.total_blocks_result_field).is_some()
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

fn is_semantic_append_message(msg: &AgentMessage, semantics: &ArtifactSquashSemantics) -> bool {
    if msg.role == AgentRole::Assistant {
        return msg.content.contains(&semantics.append_tool_name)
            || metadata_has_tool(msg, &semantics.append_tool_name);
    }
    if matches!(msg.role, AgentRole::Tool | AgentRole::User) {
        return is_semantic_success_payload(&msg.content, semantics);
    }
    false
}

fn artifact_id_from_assistant_metadata(msg: &AgentMessage, semantics: &ArtifactSquashSemantics) -> Option<String> {
    msg.metadata
        .get("raw_tool_calls")
        .and_then(|v| v.as_array())
        .and_then(|calls| {
            calls.iter().find_map(|call| {
                let name = call.get("name").and_then(|v| v.as_str())?;
                if name != semantics.append_tool_name {
                    return None;
                }
                let args = call.get("arguments_json").and_then(|v| v.as_str())?;
                let parsed: Value = serde_json::from_str(args).ok()?;
                parsed
                    .get(&semantics.id_field)
                    .and_then(|v| v.as_str())
                    .filter(|id| !id.is_empty())
                    .map(ToString::to_string)
            })
        })
}

pub fn extract_artifact_anchor(
    messages: &[AgentMessage],
    semantics: &ArtifactSquashSemantics,
) -> Option<ArtifactStateAnchor> {
    let mut artifact_id: Option<String> = None;
    let mut total_blocks: i64 = 0;
    let mut last_block_text: Option<String> = None;

    for i in (0..messages.len()).rev() {
        let msg = &messages[i];
        if artifact_id.is_none() && msg.role == AgentRole::Assistant {
            artifact_id = artifact_id_from_assistant_metadata(msg, semantics);
        }
        if !is_semantic_success_payload(&msg.content, semantics) {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&msg.content) {
            total_blocks = parse_semantic_total_blocks(&v, semantics).unwrap_or(0);
        }
        if i > 0 {
            let prev = &messages[i - 1];
            if prev.role == AgentRole::Assistant {
                if artifact_id.is_none() {
                    artifact_id = artifact_id_from_assistant_metadata(prev, semantics);
                }
                last_block_text =
                    extract_last_block_text_with_field(&prev.content, &semantics.blocks_field)
                        .or_else(|| Some(prev.content.clone()).filter(|s| !s.is_empty()));
            }
        }
        break;
    }

    if total_blocks <= 0 {
        return None;
    }

    Some(ArtifactStateAnchor {
        artifact_id: artifact_id.unwrap_or_else(|| "unknown-artifact".to_string()),
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

    let mut append_found = false;
    let mut total_blocks: i64 = 0;
    let mut last_block_text: Option<String> = None;

    for i in (0..session.messages.len()).rev() {
        let msg = &session.messages[i];
        if !is_semantic_success_payload(&msg.content, semantics) {
            continue;
        }
        if append_found {
            continue;
        }
        append_found = true;
        if let Ok(v) = serde_json::from_str::<Value>(&msg.content) {
            total_blocks = parse_semantic_total_blocks(&v, semantics).unwrap_or(0);
        }
        if i > 0 {
            let prev = &session.messages[i - 1];
            if prev.role == AgentRole::Assistant {
                last_block_text =
                    extract_last_block_text_with_field(&prev.content, &semantics.blocks_field)
                        .or_else(|| Some(prev.content.clone()).filter(|s| !s.is_empty()));
            }
        }
    }

    if !append_found {
        return CompactionOutcome::NotApplicable;
    }

    let summary_text =
        build_semantic_summary_line(semantics, total_blocks, last_block_text.as_deref());
    let mut out: Vec<AgentMessage> = Vec::with_capacity(session.messages.len());
    let mut in_append_loop = false;

    for msg in session.messages.drain(..) {
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
                metadata: serde_json::Value::Null,
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
            metadata: serde_json::Value::Null,
        });
    }

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
    use serde_json::json;

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
                    metadata: serde_json::Value::Null,
                },
                AgentMessage {
                    id: "m1".into(),
                    role: AgentRole::Assistant,
                    content: r#"call:start_artifact{"title":"Report"}"#.into(),
                    metadata: serde_json::Value::Null,
                },
                AgentMessage {
                    id: "m2".into(),
                    role: AgentRole::Tool,
                    content: r#"{"status":"success","artifactId":"123"}"#.into(),
                    metadata: json!({ "tool_name": "start_artifact" }),
                },
                AgentMessage {
                    id: "m3".into(),
                    role: AgentRole::Assistant,
                    content: r#"call:append_blocks{"artifactId":"123","blocks":[{"type":"paragraph","text":"Part 1"}]}"#
                        .into(),
                    metadata: serde_json::Value::Null,
                },
                AgentMessage {
                    id: "m4".into(),
                    role: AgentRole::Tool,
                    content: r#"{"status":"success","totalBlocks":1}"#.into(),
                    metadata: json!({ "tool_name": "append_blocks" }),
                },
                AgentMessage {
                    id: "m5".into(),
                    role: AgentRole::Assistant,
                    content: r#"call:append_blocks{"artifactId":"123","blocks":[{"type":"paragraph","text":"Part 2"}]}"#
                        .into(),
                    metadata: serde_json::Value::Null,
                },
                AgentMessage {
                    id: "m6".into(),
                    role: AgentRole::Tool,
                    content: r#"{"status":"success","totalBlocks":2}"#.into(),
                    metadata: json!({ "tool_name": "append_blocks" }),
                },
                AgentMessage {
                    id: "m7".into(),
                    role: AgentRole::Assistant,
                    content: "Now I will continue...".into(),
                    metadata: serde_json::Value::Null,
                },
            ],
            serde_json::Value::Null,
        );

        let tier = ArtifactSquashCompactionTier;
        let mut cfg = AgentConfig::default();
        cfg.tool_semantics = crate::tool_semantics::test_fixture_enclave_semantics();
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
                    metadata: serde_json::Value::Null,
                },
                AgentMessage {
                    id: "b".into(),
                    role: AgentRole::Assistant,
                    content: "Hi".into(),
                    metadata: serde_json::Value::Null,
                },
            ],
            serde_json::Value::Null,
        );

        assert!(matches!(
            ArtifactSquashCompactionTier.apply(session, &AgentConfig::default(), &ctx()),
            CompactionOutcome::NotApplicable
        ));
    }

    #[test]
    fn snippet_truncates_to_200_chars() {
        let long = "Z".repeat(250);
        let t = tail_chars(&long, SNIPPET_MAX_CHARS);
        assert_eq!(t.chars().count(), SNIPPET_MAX_CHARS);
        assert!(t.chars().all(|c| c == 'Z'));
    }
}
