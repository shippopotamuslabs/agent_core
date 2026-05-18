use crate::events::CompactionEvent;
use crate::message::{AgentMessage, ToolCallRecord};
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
    /// When true: extend max_turns by +20 if last turn contained an `append_blocks` call.
    #[serde(default = "default_true")]
    pub greedy_append_blocks: bool,
    /// Per-result byte budget for cloud providers.
    #[serde(default = "default_tool_result_budget_cloud_bytes")]
    pub tool_result_budget_cloud_bytes: usize,
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
    #[serde(default = "default_prompt_cache_anchor_threshold_tokens")]
    pub prompt_cache_anchor_threshold_tokens: u32,
}

fn default_threshold_ratio() -> f32 {
    0.80
}
fn default_micro_keep_recent() -> u32 {
    2
}
fn default_tool_retry_limit() -> u32 {
    3
}
fn default_true() -> bool {
    true
}
fn default_tool_result_budget_cloud_bytes() -> usize {
    4096
}
fn default_mid_turn_threshold_ratio() -> f32 {
    0.90
}
fn default_post_tool_result_threshold_ratio() -> f32 {
    0.85
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
    4096
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 20,
            threshold_ratio: default_threshold_ratio(),
            micro_keep_recent: default_micro_keep_recent(),
            tool_retry_limit: default_tool_retry_limit(),
            greedy_append_blocks: default_true(),
            tool_result_budget_cloud_bytes: default_tool_result_budget_cloud_bytes(),
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
}

impl AgentSession {
    pub fn new(initial_messages: Vec<AgentMessage>, session_context: serde_json::Value) -> Self {
        Self {
            messages: initial_messages,
            tool_trace: Vec::new(),
            compaction_log: VecDeque::new(),
            ledger_snapshot: None,
            session_context,
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
        assert_eq!(cfg.micro_keep_recent, 2);
        assert_eq!(cfg.tool_retry_limit, 3);
        assert!(cfg.greedy_append_blocks);
        assert_eq!(cfg.tool_result_budget_cloud_bytes, 4096);
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
