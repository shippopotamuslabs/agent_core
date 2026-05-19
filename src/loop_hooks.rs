use crate::events::{AgentError, AgentEvent};
use crate::loop_metrics::LoopMetrics;
use crate::message::{AgentMessage, AgentRole};
use crate::session::AgentSession;
use crate::state_ledger::StateLedger;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookCheckpoint {
    BeforeModelRequest,
    AfterModelResponse,
    BeforeToolExecution,
    AfterToolResult,
    BeforeFinish,
}

impl HookCheckpoint {
    pub fn as_str(self) -> &'static str {
        match self {
            HookCheckpoint::BeforeModelRequest => "before_model_request",
            HookCheckpoint::AfterModelResponse => "after_model_response",
            HookCheckpoint::BeforeToolExecution => "before_tool_execution",
            HookCheckpoint::AfterToolResult => "after_tool_result",
            HookCheckpoint::BeforeFinish => "before_finish",
        }
    }
}

pub struct HookContext<'a> {
    pub checkpoint: HookCheckpoint,
    pub turn_index: u32,
    pub session: &'a AgentSession,
    pub state_ledger: &'a StateLedger,
    pub loop_metrics: &'a LoopMetrics,
}

#[derive(Debug, Clone, Default)]
pub struct HookEffect {
    pub injected_messages: Vec<AgentMessage>,
    pub emitted_events: Vec<AgentEvent>,
    pub finish_blocked: bool,
}

pub trait AgentLoopHook: Send + Sync {
    fn evaluate(&self, context: &HookContext<'_>) -> Result<HookEffect, AgentError>;
}

#[derive(Debug, Default)]
pub struct DoomLoopReminderHook;

impl AgentLoopHook for DoomLoopReminderHook {
    fn evaluate(&self, context: &HookContext<'_>) -> Result<HookEffect, AgentError> {
        if context.loop_metrics.detect_doom_loop(3).is_some() {
            return Ok(HookEffect {
                injected_messages: vec![AgentMessage {
                    id: "doom-loop-reminder".into(),
                    role: AgentRole::User,
                    content: "You are repeating the same tool pattern. Change approach before the next tool call.".into(),
                    metadata: serde_json::Value::Null,
                }],
                emitted_events: vec![],
                finish_blocked: false,
            });
        }
        Ok(HookEffect::default())
    }
}

#[derive(Debug, Default)]
pub struct PendingWorkReminderHook {
    pub last_injected_content: Option<String>,
}

impl AgentLoopHook for PendingWorkReminderHook {
    fn evaluate(&self, context: &HookContext<'_>) -> Result<HookEffect, AgentError> {
        if context.checkpoint != HookCheckpoint::BeforeFinish {
            return Ok(HookEffect::default());
        }
        if !context.loop_metrics.has_pending_work() {
            return Ok(HookEffect::default());
        }
        let items_text = context
            .loop_metrics
            .pending_work_items
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let content = format!("Pending work remains: {}", items_text);
        if self.last_injected_content.as_deref() == Some(&content) {
            return Ok(HookEffect {
                finish_blocked: true,
                injected_messages: vec![],
                emitted_events: vec![],
            });
        }
        Ok(HookEffect {
            injected_messages: vec![AgentMessage {
                id: "pending-work-reminder".into(),
                role: AgentRole::User,
                content,
                metadata: serde_json::Value::Null,
            }],
            emitted_events: vec![],
            finish_blocked: true,
        })
    }
}

#[derive(Debug, Default)]
pub struct HookRegistry;

impl HookRegistry {
    pub fn checkpoint_order_for_full_loop(&self) -> Vec<&'static str> {
        vec![
            "before_model_request",
            "after_model_response",
            "before_tool_execution",
            "after_tool_result",
            "before_finish",
        ]
    }
}
