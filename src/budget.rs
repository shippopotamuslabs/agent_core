use once_cell::sync::Lazy;
use tiktoken_rs::CoreBPE;

use crate::session::{AgentConfig, AgentSession, Pressure, SessionCapabilities};

/// Reserved for model output tokens.
const OUTPUT_RESERVE_TOKENS: u32 = 1024;
/// Reserved for CoT / reasoning tokens.
const REASONING_RESERVE_TOKENS: u32 = 256;
/// Approximate bytes per token for local budget conversion.
const AVG_BYTES_PER_TOKEN: usize = 4;

static ENCODER: Lazy<CoreBPE> =
    Lazy::new(|| tiktoken_rs::cl100k_base().expect("tiktoken cl100k_base failed to load"));

fn count_tokens(text: &str) -> u32 {
    ENCODER.encode_with_special_tokens(text).len() as u32
}

pub(crate) fn apply_correction(raw_tokens: u32, provider: &str, config: &AgentConfig) -> u32 {
    let factor = config
        .token_correction
        .get(provider)
        .copied()
        .unwrap_or(1.0);
    (raw_tokens as f32 * factor).round() as u32
}

fn count_session_tokens(session: &AgentSession) -> u32 {
    session
        .messages
        .iter()
        .map(|m| count_tokens(&m.content))
        .sum()
}

pub struct TokenLedger;

impl TokenLedger {
    /// Compute context pressure for the current session.
    ///
    /// - `provider`: key into `config.token_correction` (e.g. `"anthropic"`).
    /// - `context_window_tokens`: model's total context limit.
    /// - `tools_schema_json`: raw JSON blob from `ModelRequest`; token cost estimated here.
    pub fn estimate(
        session: &AgentSession,
        config: &AgentConfig,
        provider: &str,
        context_window_tokens: u32,
        tools_schema_json: &str,
    ) -> Pressure {
        let raw = count_session_tokens(session);
        let total_tokens = apply_correction(raw, provider, config);

        let threshold_tokens =
            (context_window_tokens as f32 * config.threshold_ratio).round() as u32;

        let schema_tokens = count_tokens(tools_schema_json);
        let usable_prompt_tokens = context_window_tokens
            .saturating_sub(OUTPUT_RESERVE_TOKENS)
            .saturating_sub(REASONING_RESERVE_TOKENS)
            .saturating_sub(schema_tokens);

        let ratio = if context_window_tokens == 0 {
            0.0
        } else {
            total_tokens as f32 / context_window_tokens as f32
        };

        Pressure {
            total_tokens,
            threshold_tokens,
            usable_prompt_tokens,
            ratio,
        }
    }
}

/// Returns true when compaction must run before the next model call.
///
/// `config` carries the default preflight `threshold_ratio`. For trigger-aware
/// thresholds use [`crate::compact::policy::threshold_ratio_for_trigger`] with
/// [`should_compact_at`].
pub fn should_compact(pressure: &Pressure, config: &AgentConfig) -> bool {
    pressure.ratio >= config.threshold_ratio
}

/// Trigger-aware variant of [`should_compact`].
pub fn should_compact_at(pressure: &Pressure, threshold_ratio: f32) -> bool {
    pressure.ratio >= threshold_ratio
}

/// Strict per-tool-result byte budget used when context pressure is high.
///
/// - Local model with `tool_result_budget_tokens`: `tokens × AVG_BYTES_PER_TOKEN`.
/// - Cloud (or no override): `config.tool_result_budget_cloud_bytes`.
pub fn truncate_budget_per_result(caps: &SessionCapabilities, config: &AgentConfig) -> usize {
    caps.tool_result_budget_tokens
        .map(|t| t as usize * AVG_BYTES_PER_TOKEN)
        .unwrap_or(config.tool_result_budget_cloud_bytes)
}

/// Byte budget for an incoming tool result given current session pressure.
///
/// While pressure is below [`AgentConfig::tool_result_truncation_pressure_ratio`], search
/// and other tool payloads are kept intact (up to the relaxed cap) so the model can
/// use them on the next turn. Strict truncation applies only when the window is
/// actually filling up.
pub fn effective_tool_result_budget(
    pressure: &Pressure,
    caps: &SessionCapabilities,
    config: &AgentConfig,
) -> usize {
    if caps.tool_result_budget_tokens.is_some() {
        return truncate_budget_per_result(caps, config);
    }
    if should_compact_at(pressure, config.tool_result_truncation_pressure_ratio) {
        truncate_budget_per_result(caps, config)
    } else {
        config.tool_result_budget_relaxed_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{AgentMessage, AgentRole};
    use crate::session::{AgentConfig, AgentSession, SessionCapabilities};

    fn make_session(texts: &[&str]) -> AgentSession {
        let messages = texts
            .iter()
            .enumerate()
            .map(|(i, t)| AgentMessage {
                id: format!("m{i}"),
                role: AgentRole::User,
                content: t.to_string(),
                metadata: serde_json::Value::Null,
            })
            .collect();
        AgentSession::new(messages, serde_json::Value::Null)
    }

    fn make_pressure(ratio: f32) -> Pressure {
        let total = (10_000.0 * ratio) as u32;
        Pressure {
            total_tokens: total,
            threshold_tokens: 8_000,
            usable_prompt_tokens: 7_000,
            ratio,
        }
    }

    // ── should_compact ────────────────────────────────────────────────────────

    #[test]
    fn should_compact_exactly_at_threshold() {
        let config = AgentConfig::default();
        assert!(should_compact(&make_pressure(0.80), &config));
    }

    #[test]
    fn should_compact_above_threshold() {
        let config = AgentConfig::default();
        assert!(should_compact(&make_pressure(0.95), &config));
    }

    #[test]
    fn should_not_compact_below_threshold() {
        let config = AgentConfig::default();
        assert!(!should_compact(&make_pressure(0.79), &config));
    }

    #[test]
    fn should_not_compact_empty_session() {
        let config = AgentConfig::default();
        assert!(!should_compact(&make_pressure(0.0), &config));
    }

    // ── apply_correction ─────────────────────────────────────────────────────

    #[test]
    fn correction_multiplier_1_1_at_100_tokens_gives_110() {
        let mut config = AgentConfig::default();
        config.token_correction.insert("anthropic".into(), 1.1);
        assert_eq!(apply_correction(100, "anthropic", &config), 110);
    }

    #[test]
    fn correction_missing_provider_is_identity() {
        let config = AgentConfig::default();
        assert_eq!(apply_correction(200, "unknown", &config), 200);
    }

    #[test]
    fn correction_rounds_fractional_result() {
        let mut config = AgentConfig::default();
        config.token_correction.insert("google".into(), 1.05);
        // 100 * 1.05 = 105.0 → 105
        assert_eq!(apply_correction(100, "google", &config), 105);
    }

    // ── TokenLedger::estimate ─────────────────────────────────────────────────

    #[test]
    fn estimate_empty_session_has_zero_total() {
        let session = make_session(&[]);
        let config = AgentConfig::default();
        let p = TokenLedger::estimate(&session, &config, "anthropic", 200_000, "[]");
        assert_eq!(p.total_tokens, 0);
    }

    #[test]
    fn estimate_threshold_tokens_uses_locked_ratio() {
        let session = make_session(&[]);
        let config = AgentConfig::default(); // threshold_ratio = 0.80
        let p = TokenLedger::estimate(&session, &config, "anthropic", 200_000, "[]");
        assert_eq!(p.threshold_tokens, 160_000); // 200_000 * 0.80
    }

    #[test]
    fn estimate_usable_prompt_excludes_reserves() {
        let session = make_session(&[]);
        let config = AgentConfig::default();
        // tools_schema_json = "[]" → 1 token
        let p = TokenLedger::estimate(&session, &config, "anthropic", 10_000, "[]");
        // 10_000 - 1024 (output) - 256 (reasoning) - schema_tokens
        let schema_tokens = count_tokens("[]");
        let expected = 10_000u32
            .saturating_sub(OUTPUT_RESERVE_TOKENS)
            .saturating_sub(REASONING_RESERVE_TOKENS)
            .saturating_sub(schema_tokens);
        assert_eq!(p.usable_prompt_tokens, expected);
    }

    #[test]
    fn estimate_nonempty_session_counts_content() {
        // "hello world" is 2 tokens in cl100k
        let session = make_session(&["hello world"]);
        let config = AgentConfig::default();
        let p = TokenLedger::estimate(&session, &config, "anthropic", 200_000, "[]");
        assert!(p.total_tokens > 0);
        assert!(p.ratio > 0.0);
    }

    #[test]
    fn estimate_correction_applied_to_total() {
        let session = make_session(&["hello world"]);
        let mut config = AgentConfig::default();
        config.token_correction.insert("anthropic".into(), 2.0);
        let base = TokenLedger::estimate(
            &session,
            &AgentConfig::default(),
            "anthropic",
            200_000,
            "[]",
        );
        let doubled = TokenLedger::estimate(&session, &config, "anthropic", 200_000, "[]");
        assert_eq!(doubled.total_tokens, base.total_tokens * 2);
    }

    #[test]
    fn estimate_zero_context_window_ratio_is_zero() {
        let session = make_session(&[]);
        let config = AgentConfig::default();
        let p = TokenLedger::estimate(&session, &config, "anthropic", 0, "[]");
        assert_eq!(p.ratio, 0.0);
    }

    // ── effective_tool_result_budget ───────────────────────────────────────

    #[test]
    fn relaxed_budget_when_pressure_low() {
        let config = AgentConfig::default();
        let caps = SessionCapabilities::default();
        let pressure = make_pressure(0.50);
        let budget = effective_tool_result_budget(&pressure, &caps, &config);
        assert_eq!(budget, config.tool_result_budget_relaxed_bytes);
    }

    #[test]
    fn strict_budget_when_pressure_high() {
        let config = AgentConfig::default();
        let caps = SessionCapabilities::default();
        let pressure = make_pressure(0.90);
        let budget = effective_tool_result_budget(&pressure, &caps, &config);
        assert_eq!(budget, config.tool_result_budget_cloud_bytes);
    }

    // ── truncate_budget_per_result ─────────────────────────────────────────

    #[test]
    fn cloud_budget_uses_config_default() {
        let caps = SessionCapabilities {
            summarization_allowed: true,
            kv_fixed: false,
            tool_result_budget_tokens: None,
            ..Default::default()
        };
        let config = AgentConfig::default();
        assert_eq!(
            truncate_budget_per_result(&caps, &config),
            config.tool_result_budget_cloud_bytes
        );
    }

    #[test]
    fn local_budget_uses_tokens_times_avg_bytes() {
        let caps = SessionCapabilities {
            summarization_allowed: false,
            kv_fixed: true,
            tool_result_budget_tokens: Some(512),
            ..Default::default()
        };
        let config = AgentConfig::default();
        // 512 tokens × 4 bytes/token = 2048 bytes
        assert_eq!(truncate_budget_per_result(&caps, &config), 2048);
    }

    #[test]
    fn local_budget_zero_tokens_gives_zero() {
        let caps = SessionCapabilities {
            summarization_allowed: false,
            kv_fixed: true,
            tool_result_budget_tokens: Some(0),
            ..Default::default()
        };
        let config = AgentConfig::default();
        assert_eq!(truncate_budget_per_result(&caps, &config), 0);
    }
}
