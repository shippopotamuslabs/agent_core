//! Model API transport failure classification and retry backoff policy.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelApiErrorClass {
    Transient,
    Permanent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTurnFailureReport {
    pub error_class: ModelApiErrorClass,
    pub http_status: Option<u16>,
    pub provider_code: Option<String>,
    pub message: String,
    pub had_partial_output: bool,
    pub partial_output_chars: u32,
    pub retry_after_ms: Option<u64>,
}

/// Computes host backoff before re-issuing the same [`crate::turn_io::ModelRequest`].
///
/// `attempt` is the 1-based consecutive transient failure count for this model turn.
pub fn model_api_retry_delay_ms(
    attempt: u32,
    base_delay_ms: u64,
    report_retry_after_ms: Option<u64>,
) -> u64 {
    if let Some(retry_after) = report_retry_after_ms.filter(|value| *value > 0) {
        return retry_after.min(60_000);
    }
    let exponent = attempt.saturating_sub(1).min(15);
    let multiplier = 1u64.checked_shl(exponent).unwrap_or(u64::MAX);
    base_delay_ms.saturating_mul(multiplier).min(30_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honors_retry_after_header() {
        assert_eq!(
            model_api_retry_delay_ms(2, 1_000, Some(5_000)),
            5_000
        );
    }

    #[test]
    fn exponential_backoff_caps_at_thirty_seconds() {
        assert_eq!(model_api_retry_delay_ms(1, 1_000, None), 1_000);
        assert_eq!(model_api_retry_delay_ms(2, 1_000, None), 2_000);
        assert_eq!(model_api_retry_delay_ms(5, 1_000, None), 16_000);
        assert_eq!(model_api_retry_delay_ms(10, 1_000, None), 30_000);
    }
}