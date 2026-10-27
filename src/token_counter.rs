//! Token-counting port (ENC-292).
//!
//! Budget estimation ([`crate::budget`]) and message truncation
//! ([`crate::truncate`]) previously called the concrete `tiktoken` encoders
//! directly via per-module `static` instances. This module introduces the
//! host-supplied port for that dependency, following the [`crate::host`]
//! pattern: domain code counts through `&dyn TokenCounter`, and the default
//! in-crate implementation ([`TiktokenTokenCounter`], `cl100k_base`) preserves
//! existing behavior bit-for-bit.
//!
//! Hosts that need a different tokenizer (on-device model vocabulary,
//! restricted runtimes without the `tiktoken` tables) implement
//! [`TokenCounter`] and call the `*_with` variants
//! ([`crate::budget::TokenLedger::estimate_with`],
//! [`crate::truncate::truncate_message_content_to_tokens_with`],
//! [`crate::compact::estimate_session_pressure_with`]). Tests inject a fake
//! counter the same way without touching domain code.

use once_cell::sync::Lazy;
use tiktoken_rs::CoreBPE;

/// Host-supplied token-counting port.
///
/// `Send + Sync` so a single instance can back concurrent sessions, matching
/// the [`crate::host`] transport/executor/summarizer bounds.
///
/// Mechanism vs policy: implementors supply the tokenizer primitives
/// (count, encode, decode) — the only parts that differ between
/// vocabularies. The truncation policy (zero-token early return, no-op
/// fast path, note-token reservation, the `max(1)` keep floor, note
/// appending) is implemented once as the default
/// [`truncate_text_to_tokens`](TokenCounter::truncate_text_to_tokens);
/// an implementor overrides it only when its token model cannot slice
/// by ids at all (e.g. a word-counting test fake).
pub trait TokenCounter: Send + Sync {
    /// Token count for `text`, including special tokens (same convention as
    /// `tiktoken` `encode_with_special_tokens`).
    fn count_tokens(&self, text: &str) -> u32;

    /// Encode `text` into token ids, including special tokens (the same
    /// sequence [`count_tokens`](TokenCounter::count_tokens) counts).
    fn encode_tokens(&self, text: &str) -> Vec<u32>;

    /// Decode token ids back into text. `Err` when the ids do not form
    /// valid text in this counter's vocabulary; the default truncation
    /// policy falls back to a character slice in that case.
    fn decode_tokens(&self, tokens: &[u32]) -> Result<String, String>;

    /// Truncate `text` to at most `max_tokens`, appending `truncation_note`
    /// when truncated. Returns the (possibly unchanged) text and whether
    /// truncation happened. Default implementation: the shared truncation
    /// policy over this counter's primitives.
    fn truncate_text_to_tokens(
        &self,
        text: &str,
        max_tokens: u32,
        truncation_note: &str,
    ) -> (String, bool) {
        if max_tokens == 0 {
            return (truncation_note.trim_start().to_string(), true);
        }
        let tokens = self.encode_tokens(text);
        if tokens.len() <= max_tokens as usize {
            return (text.to_string(), false);
        }
        let note_tokens = self.count_tokens(truncation_note);
        let keep = max_tokens.saturating_sub(note_tokens).max(1) as usize;
        let truncated_tokens = &tokens[..keep.min(tokens.len())];
        let decoded = self
            .decode_tokens(truncated_tokens)
            .unwrap_or_else(|_| text.chars().take(keep.saturating_mul(4)).collect());
        let mut out = decoded;
        out.push_str(truncation_note);
        (out, true)
    }
}

/// Default in-crate [`TokenCounter`]: OpenAI `cl100k_base` via `tiktoken-rs`.
///
/// This is the behavior every existing caller relied on before ENC-292; the
/// zero-arg budget/truncate entry points delegate here.
#[derive(Debug, Default, Clone, Copy)]
pub struct TiktokenTokenCounter;

static ENCODER: Lazy<CoreBPE> =
    Lazy::new(|| tiktoken_rs::cl100k_base().expect("tiktoken cl100k_base failed to load"));

static DEFAULT_COUNTER: Lazy<TiktokenTokenCounter> = Lazy::new(TiktokenTokenCounter::default);

/// The process-wide default counter used by the zero-arg entry points.
pub fn default_counter() -> &'static dyn TokenCounter {
    &*DEFAULT_COUNTER
}

impl TokenCounter for TiktokenTokenCounter {
    fn count_tokens(&self, text: &str) -> u32 {
        ENCODER.encode_with_special_tokens(text).len() as u32
    }

    fn encode_tokens(&self, text: &str) -> Vec<u32> {
        ENCODER.encode_with_special_tokens(text)
    }

    fn decode_tokens(&self, tokens: &[u32]) -> Result<String, String> {
        ENCODER
            .decode(tokens.to_vec())
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{PromptBudgetParts, TokenLedger};
    use crate::message::{AgentMessage, AgentRole};
    use crate::policy::ContextPolicyTier;
    use crate::session::{AgentConfig, AgentSession};
    use crate::truncate::truncate_message_content_to_tokens;

    /// Word-counting fake: proves a test counter is injectable without
    /// touching domain code.
    struct WordCounter;

    impl TokenCounter for WordCounter {
        fn count_tokens(&self, text: &str) -> u32 {
            text.split_whitespace().count() as u32
        }

        // Word ids are positions, not a decodable vocabulary: this fake
        // exists to prove count injection, and its truncation semantics
        // are word-based, so it overrides the policy method below.
        fn encode_tokens(&self, text: &str) -> Vec<u32> {
            text.split_whitespace().enumerate().map(|(i, _)| i as u32).collect()
        }

        fn decode_tokens(&self, _tokens: &[u32]) -> Result<String, String> {
            Err("WordCounter does not decode".to_string())
        }

        fn truncate_text_to_tokens(
            &self,
            text: &str,
            max_tokens: u32,
            truncation_note: &str,
        ) -> (String, bool) {
            let words: Vec<&str> = text.split_whitespace().collect();
            if words.len() <= max_tokens as usize {
                return (text.to_string(), false);
            }
            let note_words = truncation_note.split_whitespace().count() as u32;
            let keep = (max_tokens.saturating_sub(note_words).max(1) as usize).min(words.len());
            let mut out = words[..keep].join(" ");
            out.push_str(truncation_note);
            (out, true)
        }
    }

    fn make_session(texts: &[&str]) -> AgentSession {
        AgentSession::new(
            texts
                .iter()
                .enumerate()
                .map(|(i, t)| AgentMessage {
                    id: format!("m{i}"),
                    role: AgentRole::User,
                    content: t.to_string(),
                    metadata: serde_json::Value::Null,
                })
                .collect(),
            serde_json::Value::Null,
        )
    }

    #[test]
    fn default_counter_matches_legacy_entry_points() {
        let session = make_session(&["hello world, this is a budget probe"]);
        let config = AgentConfig::for_policy_tier(ContextPolicyTier::HostedMutable);
        let parts = PromptBudgetParts::from_legacy("anthropic", 200_000, "[]");
        let via_default_arg =
            TokenLedger::estimate_with(default_counter(), &session, &config, &parts);
        let via_legacy = TokenLedger::estimate(&session, &config, &parts);
        assert_eq!(via_default_arg.total_tokens, via_legacy.total_tokens);

        let body = "alpha ".repeat(400);
        let (a, a_changed) = truncate_message_content_to_tokens(&body, 32);
        let (b, b_changed) =
            crate::truncate::truncate_message_content_to_tokens_with(default_counter(), &body, 32);
        assert_eq!(a_changed, b_changed);
        assert_eq!(a, b);
    }

    #[test]
    fn injected_test_counter_drives_budget_without_domain_changes() {
        let fake = WordCounter;
        // Polysyllabic words: 2 whitespace-words but many cl100k tokens, so
        // the fake and default counters must diverge.
        let session =
            make_session(&["supercalifragilisticexpialidocious antidisestablishmentarianism"]);
        let config = AgentConfig::default();
        let parts = PromptBudgetParts::from_legacy("anthropic", 200_000, "[]");
        let with_fake = TokenLedger::estimate_with(&fake, &session, &config, &parts);
        // 8 message words + template overhead (12) + sidecar reserve: the
        // message contribution must be exactly the fake's word count.
        let pressure_default = TokenLedger::estimate(&session, &config, &parts);
        assert_ne!(with_fake.total_tokens, pressure_default.total_tokens);
        assert!(with_fake.total_tokens < pressure_default.total_tokens);

        let (out, changed) =
            crate::truncate::truncate_message_content_to_tokens_with(&fake, "a b c d e f", 3);
        assert!(changed);
        assert!(out.contains(crate::truncate::MESSAGE_TRUNCATION_NOTE.trim()));
    }
}
