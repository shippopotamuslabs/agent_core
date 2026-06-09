use crate::events::CompactionEvent;
use crate::long_loop_policy::LongLoopPolicy;
use crate::message::{AgentMessage, ToolCallRecord};
use crate::policy::ContextPolicyTier;
use crate::tool_semantics::ToolSemanticsConfig;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

const COMPACTION_LOG_CAPACITY: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Maximum number of tool calls (turns) before guardrails stop tools.
    pub max_turns: u32,
    /// Context pressure threshold — compact when ratio >= this value.
    /// Locked at 0.80 (replaces legacy 0.75/0.85 per ADR).
    #[serde(default = "default_threshold_ratio")]
    pub threshold_ratio: f32,
    /// Number of recent reproducible tool results to keep per tool during micro-compact.
    #[serde(default = "default_micro_keep_recent")]
    pub micro_keep_recent: u32,
    /// Consecutive all-soft-failure turns before `allow_tool_call` is set false.
    #[serde(default = "default_tool_retry_limit")]
    pub tool_retry_limit: u32,
    /// Maximum transient model API retries per in-flight model turn (not counting first attempt).
    #[serde(default = "default_model_api_retry_limit")]
    pub model_api_retry_limit: u32,
    /// Base backoff for model API retries when the host does not supply `retry_after_ms`.
    #[serde(default = "default_model_api_retry_base_delay_ms")]
    pub model_api_retry_base_delay_ms: u64,
    /// Per-result byte budget for cloud providers when context pressure is high.
    #[serde(default = "default_tool_result_budget_cloud_bytes")]
    pub tool_result_budget_cloud_bytes: usize,
    /// Per-result byte budget while context pressure is below
    /// [`tool_result_truncation_pressure_ratio`].
    #[serde(default = "default_tool_result_budget_relaxed_bytes")]
    pub tool_result_budget_relaxed_bytes: usize,
    /// Apply the strict [`tool_result_budget_cloud_bytes`] only when session
    /// pressure ratio is at or above this value.
    #[serde(default = "default_tool_result_truncation_pressure_ratio")]
    pub tool_result_truncation_pressure_ratio: f32,
    /// Per-provider token-count correction multipliers applied on top of tiktoken estimate.
    /// Key: provider name (e.g. "anthropic", "google"). Value: multiplier (e.g. 1.1).
    #[serde(default)]
    pub token_correction: HashMap<String, f32>,
    // ── Context management (AGENT-context-mgmt) ───────────────────────────
    #[serde(default = "default_mid_turn_threshold_ratio")]
    pub mid_turn_threshold_ratio: f32,
    #[serde(default = "default_post_tool_result_threshold_ratio")]
    pub post_tool_result_threshold_ratio: f32,
    #[serde(default = "default_manual_compact_threshold_ratio")]
    pub manual_compact_threshold_ratio: f32,
    #[serde(default = "default_allow_fifo_on_preflight")]
    pub allow_fifo_on_preflight: bool,
    #[serde(default = "default_allow_fifo_on_mid_turn")]
    pub allow_fifo_on_mid_turn: bool,
    #[serde(default = "default_allow_fifo_on_post_tool_result")]
    pub allow_fifo_on_post_tool_result: bool,
    #[serde(default = "default_true")]
    pub require_snapshot_before_fifo: bool,
    #[serde(default = "default_true")]
    pub enable_context_store: bool,
    #[serde(default = "default_true")]
    pub enable_state_ledger: bool,
    #[serde(default = "default_true")]
    pub enable_tool_schema_deferral: bool,
    #[serde(default = "default_true")]
    pub enable_file_condensation: bool,
    #[serde(default = "default_true")]
    pub enable_manual_compact: bool,
    #[serde(default = "default_tool_spill_threshold_bytes")]
    pub tool_spill_threshold_bytes: usize,
    #[serde(default = "default_terminal_spill_threshold_bytes")]
    pub terminal_spill_threshold_bytes: usize,
    #[serde(default = "default_history_snapshot_threshold_tokens")]
    pub history_snapshot_threshold_tokens: u32,
    #[serde(default = "default_file_outline_threshold_tokens")]
    pub file_outline_threshold_tokens: u32,
    /// Opportunistic prompt-cache anchor control.
    ///
    /// Semantics are intentionally inverted from a normal threshold:
    /// - `u32::MAX` — cache anchoring **disabled** (used for small-window / FixedKv tiers
    ///   where rewriting history invalidates the KV cache).
    /// - Any other value — cache anchoring **enabled** (0 is the standard on-by-default value).
    ///
    /// The name retains the original "threshold" form for FFI/serialization compatibility.
    /// Use `== u32::MAX` to test for disabled state.
    #[serde(default = "default_prompt_cache_anchor_threshold_tokens")]
    pub prompt_cache_anchor_threshold_tokens: u32,
    #[serde(default = "default_output_reserve_ratio")]
    pub output_reserve_ratio: f32,
    #[serde(default = "default_output_reserve_floor_tokens")]
    pub output_reserve_floor_tokens: u32,
    #[serde(default = "default_output_reserve_max_tokens")]
    pub output_reserve_max_tokens: u32,
    #[serde(default)]
    pub preflight_token_threshold: Option<u32>,
    #[serde(default)]
    pub mid_turn_token_threshold: Option<u32>,
    #[serde(default)]
    pub post_tool_result_token_threshold: Option<u32>,
    #[serde(default)]
    pub turn_threshold: Option<u32>,
    #[serde(default)]
    pub message_threshold: Option<u32>,
    #[serde(default)]
    pub compact_on_turn_end: bool,
    #[serde(default)]
    pub retention_window_messages: usize,
    #[serde(default)]
    pub retention_window_tokens: u32,
    #[serde(default)]
    pub eviction_window_ratio: f32,
    #[serde(default)]
    pub enable_loop_hooks: bool,
    #[serde(default)]
    pub enable_doom_loop_guard: bool,
    #[serde(default)]
    pub enable_pending_work_guard: bool,
    #[serde(default)]
    pub require_read_before_edit: bool,
    #[serde(default)]
    pub default_tool_timeout_ms: Option<u64>,
    #[serde(default = "default_max_model_requests_per_turn")]
    pub max_model_requests_per_turn: u32,
    /// Long agent-loop policy (tool caps, sidecar budgets, turn-count compaction).
    #[serde(default)]
    pub long_loop: LongLoopPolicy,
    #[serde(default)]
    pub tool_semantics: ToolSemanticsConfig,
}

fn default_threshold_ratio() -> f32 {
    0.80
}
fn default_micro_keep_recent() -> u32 {
    4
}
fn default_tool_retry_limit() -> u32 {
    3
}
fn default_model_api_retry_limit() -> u32 {
    3
}
fn default_model_api_retry_base_delay_ms() -> u64 {
    1_000
}
fn default_true() -> bool {
    true
}
fn default_tool_result_budget_cloud_bytes() -> usize {
    16_384
}
fn default_tool_result_budget_relaxed_bytes() -> usize {
    131_072
}
fn default_tool_result_truncation_pressure_ratio() -> f32 {
    0.85
}
fn default_mid_turn_threshold_ratio() -> f32 {
    0.93
}
fn default_post_tool_result_threshold_ratio() -> f32 {
    0.92
}
fn default_manual_compact_threshold_ratio() -> f32 {
    0.80
}
fn default_allow_fifo_on_preflight() -> bool {
    true
}
fn default_allow_fifo_on_mid_turn() -> bool {
    false
}
fn default_allow_fifo_on_post_tool_result() -> bool {
    false
}
fn default_tool_spill_threshold_bytes() -> usize {
    8192
}
fn default_terminal_spill_threshold_bytes() -> usize {
    8192
}
fn default_history_snapshot_threshold_tokens() -> u32 {
    1024
}
fn default_file_outline_threshold_tokens() -> u32 {
    2048
}
fn default_prompt_cache_anchor_threshold_tokens() -> u32 {
    0
}
fn default_output_reserve_ratio() -> f32 {
    0.05
}
fn default_output_reserve_floor_tokens() -> u32 {
    4096
}
fn default_output_reserve_max_tokens() -> u32 {
    16_384
}
fn default_max_model_requests_per_turn() -> u32 {
    1
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 20,
            threshold_ratio: default_threshold_ratio(),
            micro_keep_recent: default_micro_keep_recent(),
            tool_retry_limit: default_tool_retry_limit(),
            model_api_retry_limit: default_model_api_retry_limit(),
            model_api_retry_base_delay_ms: default_model_api_retry_base_delay_ms(),
            tool_result_budget_cloud_bytes: default_tool_result_budget_cloud_bytes(),
            tool_result_budget_relaxed_bytes: default_tool_result_budget_relaxed_bytes(),
            tool_result_truncation_pressure_ratio: default_tool_result_truncation_pressure_ratio(),
            token_correction: HashMap::new(),
            mid_turn_threshold_ratio: default_mid_turn_threshold_ratio(),
            post_tool_result_threshold_ratio: default_post_tool_result_threshold_ratio(),
            manual_compact_threshold_ratio: default_manual_compact_threshold_ratio(),
            allow_fifo_on_preflight: default_allow_fifo_on_preflight(),
            allow_fifo_on_mid_turn: default_allow_fifo_on_mid_turn(),
            allow_fifo_on_post_tool_result: default_allow_fifo_on_post_tool_result(),
            require_snapshot_before_fifo: default_true(),
            enable_context_store: default_true(),
            enable_state_ledger: default_true(),
            enable_tool_schema_deferral: default_true(),
            enable_file_condensation: default_true(),
            enable_manual_compact: default_true(),
            tool_spill_threshold_bytes: default_tool_spill_threshold_bytes(),
            terminal_spill_threshold_bytes: default_terminal_spill_threshold_bytes(),
            history_snapshot_threshold_tokens: default_history_snapshot_threshold_tokens(),
            file_outline_threshold_tokens: default_file_outline_threshold_tokens(),
            prompt_cache_anchor_threshold_tokens: default_prompt_cache_anchor_threshold_tokens(),
            output_reserve_ratio: default_output_reserve_ratio(),
            output_reserve_floor_tokens: default_output_reserve_floor_tokens(),
            output_reserve_max_tokens: default_output_reserve_max_tokens(),
            preflight_token_threshold: None,
            mid_turn_token_threshold: None,
            post_tool_result_token_threshold: None,
            turn_threshold: None,
            message_threshold: None,
            compact_on_turn_end: false,
            retention_window_messages: 0,
            retention_window_tokens: 0,
            eviction_window_ratio: 0.0,
            enable_loop_hooks: false,
            enable_doom_loop_guard: false,
            enable_pending_work_guard: false,
            require_read_before_edit: false,
            default_tool_timeout_ms: None,
            max_model_requests_per_turn: default_max_model_requests_per_turn(),
            long_loop: LongLoopPolicy::default(),
            tool_semantics: ToolSemanticsConfig::agnostic(),
        }
    }
}

impl AgentConfig {
    /// Applies host IO payloads to [`tool_semantics`] (catalog JSON merge).
    pub fn prepare_tool_semantics(&mut self, io: &crate::engine::SessionIoParams) {
        if let Err(error) = self.tool_semantics.merge_catalog_json(&io.tool_catalog_json) {
            eprintln!("agent_core: tool catalog merge failed: {error:?}");
        }
    }

    pub fn for_policy_tier(tier: ContextPolicyTier) -> Self {
        let mut config = Self::default();
        config.long_loop = LongLoopPolicy::for_policy_tier(tier);
        match tier {
            ContextPolicyTier::HostedMutable | ContextPolicyTier::HighContext => {
                config.threshold_ratio = 0.80;
                config.preflight_token_threshold = Some(150_000);
                config.mid_turn_token_threshold = Some(180_000);
                config.output_reserve_ratio = 0.05;
                config.output_reserve_floor_tokens = 4096;
                config.output_reserve_max_tokens = 16_384;
                config.retention_window_tokens = 16_384;
                config.message_threshold = Some(200);
                config.tool_spill_threshold_bytes = 8192;
                config.terminal_spill_threshold_bytes = 8192;
                config.tool_result_budget_cloud_bytes = 16_384;
                config.tool_result_budget_relaxed_bytes = 131_072;
                config.prompt_cache_anchor_threshold_tokens = 0;
            }
            ContextPolicyTier::HostedProxy => {
                config.threshold_ratio = 0.80;
                config.preflight_token_threshold = Some(120_000);
                config.mid_turn_token_threshold = None;
                config.output_reserve_ratio = 0.10;
                config.output_reserve_floor_tokens = 4096;
                config.output_reserve_max_tokens = 12_288;
                config.retention_window_tokens = 12_288;
                config.message_threshold = Some(150);
                config.tool_spill_threshold_bytes = 8192;
                config.terminal_spill_threshold_bytes = 8192;
                config.tool_result_budget_cloud_bytes = 16_384;
                config.tool_result_budget_relaxed_bytes = 131_072;
                config.prompt_cache_anchor_threshold_tokens = 0;
            }
            ContextPolicyTier::LocalMutable => {
                config.threshold_ratio = 0.78;
                config.preflight_token_threshold = None;
                config.mid_turn_token_threshold = None;
                config.output_reserve_ratio = 0.15;
                config.output_reserve_floor_tokens = 2048;
                config.output_reserve_max_tokens = 8192;
                config.retention_window_tokens = 8192;
                config.message_threshold = Some(100);
                config.tool_spill_threshold_bytes = 4096;
                config.terminal_spill_threshold_bytes = 4096;
                config.tool_result_budget_cloud_bytes = 8192;
                config.tool_result_budget_relaxed_bytes = 65_536;
                config.prompt_cache_anchor_threshold_tokens = 0;
            }
            ContextPolicyTier::SmallContext => {
                config.threshold_ratio = 0.72;
                config.preflight_token_threshold = None;
                config.mid_turn_token_threshold = None;
                config.output_reserve_ratio = 0.20;
                config.output_reserve_floor_tokens = 1024;
                config.output_reserve_max_tokens = 4096;
                config.retention_window_tokens = 4096;
                config.message_threshold = Some(80);
                config.tool_spill_threshold_bytes = 2048;
                config.terminal_spill_threshold_bytes = 2048;
                config.tool_result_budget_cloud_bytes = 4096;
                config.tool_result_budget_relaxed_bytes = 32_768;
                config.enable_context_store = true;
                config.prompt_cache_anchor_threshold_tokens = u32::MAX;
            }
            ContextPolicyTier::FixedKv => {
                config.threshold_ratio = 0.60;
                config.preflight_token_threshold = None;
                config.mid_turn_token_threshold = None;
                config.output_reserve_ratio = 0.20;
                config.output_reserve_floor_tokens = 512;
                config.output_reserve_max_tokens = 2048;
                config.retention_window_tokens = 2048;
                config.message_threshold = Some(60);
                config.tool_spill_threshold_bytes = 1024;
                config.terminal_spill_threshold_bytes = 1024;
                config.tool_result_budget_cloud_bytes = 2048;
                config.tool_result_budget_relaxed_bytes = 16_384;
                config.enable_context_store = true;
                config.prompt_cache_anchor_threshold_tokens = u32::MAX;
            }
        }
        config
    }

    /// Apply tier-level defaults to fields that the host has not explicitly overridden.
    ///
    /// # Trade-off
    /// Field-by-field equality against `Self::default()` cannot distinguish "host explicitly
    /// set this field to its default value" from "host left this field unset". For example,
    /// a host that intentionally sets `retention_window_tokens = 0` on a `HostedMutable`
    /// session will see it overridden to `16384`.
    ///
    /// If per-field opt-out is needed in the future, fields should be migrated to
    /// `Option<T>` (a larger refactor deferred for now).
    pub fn apply_policy_tier_defaults(
        &mut self,
        tier: ContextPolicyTier,
        context_window_tokens: u32,
    ) {
        let base = Self::default();
        let tiered = Self::for_policy_tier(tier);

        if self.long_loop == base.long_loop {
            self.long_loop = tiered.long_loop;
        }
        if self.threshold_ratio == base.threshold_ratio {
            self.threshold_ratio = tiered.threshold_ratio;
        }
        if self.preflight_token_threshold == base.preflight_token_threshold
            && self.threshold_ratio == base.threshold_ratio
        {
            self.preflight_token_threshold = tiered.preflight_token_threshold;
        }
        if self.mid_turn_token_threshold == base.mid_turn_token_threshold
            && self.mid_turn_threshold_ratio == base.mid_turn_threshold_ratio
        {
            self.mid_turn_token_threshold = tiered.mid_turn_token_threshold;
        }
        if self.post_tool_result_token_threshold == base.post_tool_result_token_threshold
            && self.post_tool_result_threshold_ratio == base.post_tool_result_threshold_ratio
        {
            self.post_tool_result_token_threshold = tiered.post_tool_result_token_threshold;
        }
        if self.output_reserve_ratio == base.output_reserve_ratio {
            self.output_reserve_ratio = tiered.output_reserve_ratio;
        }
        if self.output_reserve_floor_tokens == base.output_reserve_floor_tokens {
            self.output_reserve_floor_tokens = tiered.output_reserve_floor_tokens;
        }
        if self.output_reserve_max_tokens == base.output_reserve_max_tokens {
            self.output_reserve_max_tokens = tiered.output_reserve_max_tokens;
        }
        if self.retention_window_tokens == base.retention_window_tokens {
            self.retention_window_tokens = tiered.retention_window_tokens;
        }
        if self.message_threshold == base.message_threshold {
            self.message_threshold = tiered.message_threshold;
        }
        if self.tool_spill_threshold_bytes == base.tool_spill_threshold_bytes {
            self.tool_spill_threshold_bytes = tiered.tool_spill_threshold_bytes;
        }
        if self.terminal_spill_threshold_bytes == base.terminal_spill_threshold_bytes {
            self.terminal_spill_threshold_bytes = tiered.terminal_spill_threshold_bytes;
        }
        if self.tool_result_budget_cloud_bytes == base.tool_result_budget_cloud_bytes {
            self.tool_result_budget_cloud_bytes = tiered.tool_result_budget_cloud_bytes;
        }
        if self.tool_result_budget_relaxed_bytes == base.tool_result_budget_relaxed_bytes {
            self.tool_result_budget_relaxed_bytes = tiered.tool_result_budget_relaxed_bytes;
        }
        if self.prompt_cache_anchor_threshold_tokens == base.prompt_cache_anchor_threshold_tokens {
            self.prompt_cache_anchor_threshold_tokens = tiered.prompt_cache_anchor_threshold_tokens;
        }
        if context_window_tokens < 8_000 {
            self.enable_context_store = true;
            self.prompt_cache_anchor_threshold_tokens = u32::MAX;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCapabilities {
    /// Whether the LLM summary compaction tier (Tier 4) is available.
    /// Set false for local models (Llama, LiteRT).
    pub summarization_allowed: bool,
    /// Whether the transport uses a fixed KV cache that invalidates on history rewrite.
    /// When true, engine emits `AgentEvent::KvCacheInvalidated` after any compaction.
    pub kv_fixed: bool,
    /// Per-result token budget for local models.
    /// When Some(n), overrides `AgentConfig.tool_result_budget_cloud_bytes`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result_budget_tokens: Option<u32>,
    #[serde(default)]
    pub context_policy_tier: crate::policy::ContextPolicyTier,
    #[serde(default)]
    pub tool_schema_deferral_supported: bool,
    #[serde(default)]
    pub file_backed_context_supported: bool,
    #[serde(default)]
    pub prompt_cache_supported: bool,
    #[serde(default = "default_true")]
    pub history_rewrite_supported: bool,
    #[serde(default)]
    pub request_transform_pipeline_supported: bool,
    #[serde(default)]
    pub lifecycle_hooks_supported: bool,
    #[serde(default)]
    pub delegation_tools_supported: bool,
    #[serde(default)]
    pub host_policy_callbacks_supported: bool,
}

impl Default for SessionCapabilities {
    fn default() -> Self {
        Self {
            summarization_allowed: false,
            kv_fixed: false,
            tool_result_budget_tokens: None,
            context_policy_tier: crate::policy::ContextPolicyTier::HostedMutable,
            tool_schema_deferral_supported: false,
            file_backed_context_supported: false,
            prompt_cache_supported: false,
            history_rewrite_supported: true,
            request_transform_pipeline_supported: false,
            lifecycle_hooks_supported: false,
            delegation_tools_supported: false,
            host_policy_callbacks_supported: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pressure {
    pub total_tokens: u32,
    pub threshold_tokens: u32,
    pub usable_prompt_tokens: u32,
    pub ratio: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub messages: Vec<AgentMessage>,
    pub tool_trace: Vec<ToolCallRecord>,
    /// Ring buffer capped at 32 entries.
    pub compaction_log: VecDeque<CompactionEvent>,
    pub ledger_snapshot: Option<Pressure>,
    /// Opaque host context (e.g. `{"chatId":"...","userMessageId":"...","assistantMessageId":"..."}`).
    /// Read by `AgentToolExecutorAdapter` to plumb IDs into `ToolCallRequest`.
    #[serde(default)]
    pub session_context: serde_json::Value,
    /// Monotonic counter bumped on destructive compaction rewrites.
    #[serde(default)]
    pub transcript_revision: u64,
}

impl AgentSession {
    pub fn new(initial_messages: Vec<AgentMessage>, session_context: serde_json::Value) -> Self {
        Self {
            messages: initial_messages,
            tool_trace: Vec::new(),
            compaction_log: VecDeque::new(),
            ledger_snapshot: None,
            session_context,
            transcript_revision: 0,
        }
    }

    pub fn push_compaction_event(&mut self, event: CompactionEvent) {
        if self.compaction_log.len() >= COMPACTION_LOG_CAPACITY {
            self.compaction_log.pop_front();
        }
        self.compaction_log.push_back(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{CompactionTier, CompactionTrigger};
    use crate::message::AgentRole;
    use serde_json::json;

    fn make_event(tier: CompactionTier) -> CompactionEvent {
        CompactionEvent {
            tier,
            tokens_before: 1000,
            tokens_after: 800,
            trigger: CompactionTrigger::MidTurn,
        }
    }

    fn make_message(id: &str) -> AgentMessage {
        crate::message::AgentMessage {
            id: id.into(),
            role: AgentRole::User,
            content: "test".into(),
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn agent_config_defaults() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.threshold_ratio, 0.80);
        assert_eq!(cfg.micro_keep_recent, 4);
        assert_eq!(cfg.tool_retry_limit, 3);
        assert!(cfg.tool_semantics.greedy_extension_tools.is_empty());
        assert_eq!(cfg.tool_result_budget_cloud_bytes, 16_384);
        assert_eq!(cfg.tool_result_budget_relaxed_bytes, 131_072);
        assert_eq!(cfg.tool_result_truncation_pressure_ratio, 0.85);
    }

    #[test]
    fn agent_config_roundtrip() {
        let cfg = AgentConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.threshold_ratio, cfg.threshold_ratio);
        assert_eq!(back.max_turns, cfg.max_turns);
    }

    #[test]
    fn session_capabilities_roundtrip() {
        let caps = SessionCapabilities {
            summarization_allowed: false,
            kv_fixed: true,
            tool_result_budget_tokens: Some(512),
            ..Default::default()
        };
        let json = serde_json::to_string(&caps).unwrap();
        let back: SessionCapabilities = serde_json::from_str(&json).unwrap();
        assert!(!back.summarization_allowed);
        assert!(back.kv_fixed);
        assert_eq!(back.tool_result_budget_tokens, Some(512));
    }

    #[test]
    fn session_capabilities_no_budget_omits_field() {
        let caps = SessionCapabilities {
            summarization_allowed: true,
            kv_fixed: false,
            tool_result_budget_tokens: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&caps).unwrap();
        assert!(!json.contains("tool_result_budget_tokens"));
    }

    #[test]
    fn session_context_opaque_value() {
        let ctx = json!({"chatId": "chat-1", "userMessageId": "msg-1"});
        let session = AgentSession::new(vec![], ctx.clone());
        let json = serde_json::to_string(&session).unwrap();
        let back: AgentSession = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_context["chatId"], "chat-1");
    }

    #[test]
    fn session_context_defaults_null_when_missing() {
        let session = AgentSession::new(vec![], serde_json::Value::Null);
        let s = serde_json::to_string(&session).unwrap();
        let back: AgentSession = serde_json::from_str(&s).unwrap();
        assert!(back.session_context.is_null());
    }

    #[test]
    fn compaction_log_bounded_at_32() {
        let mut session = AgentSession::new(vec![], serde_json::Value::Null);
        for _ in 0..33 {
            session.push_compaction_event(make_event(CompactionTier::Micro));
        }
        assert_eq!(session.compaction_log.len(), 32);
    }

    #[test]
    fn compaction_log_drops_oldest_first() {
        let mut session = AgentSession::new(vec![], serde_json::Value::Null);
        session.push_compaction_event(make_event(CompactionTier::Micro));
        for _ in 0..32 {
            session.push_compaction_event(make_event(CompactionTier::Fifo));
        }
        // oldest (Micro) should be gone
        assert_eq!(session.compaction_log.len(), 32);
        assert!(matches!(
            session.compaction_log[0].tier,
            CompactionTier::Fifo
        ));
    }

    #[test]
    fn session_new_preserves_initial_messages() {
        let msgs = vec![make_message("m1"), make_message("m2")];
        let session = AgentSession::new(msgs, serde_json::Value::Null);
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].id, "m1");
    }

    #[test]
    fn pressure_roundtrip() {
        let p = Pressure {
            total_tokens: 8000,
            threshold_tokens: 6400,
            usable_prompt_tokens: 5000,
            ratio: 0.82,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Pressure = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ratio, 0.82);
    }
}
