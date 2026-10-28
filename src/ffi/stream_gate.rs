//! UniFFI surface for `tool_stream_gate` (TOOL-PARSE-002).

use std::sync::Mutex;

use crate::tool_stream_gate::{PushResult, StreamGateFinish, ToolStreamGate};

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiStreamGatePushResult {
    pub emitted_prefix: String,
}

impl From<PushResult> for FfiStreamGatePushResult {
    fn from(p: PushResult) -> Self {
        Self {
            emitted_prefix: p.emitted_prefix,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiStreamGateFinish {
    pub trailing_flush: String,
    pub has_tool_marker: bool,
}

impl From<StreamGateFinish> for FfiStreamGateFinish {
    fn from(f: StreamGateFinish) -> Self {
        Self {
            trailing_flush: f.trailing_flush,
            has_tool_marker: f.has_tool_marker,
        }
    }
}

#[derive(uniffi::Object)]
pub struct FfiToolStreamGate {
    inner: Mutex<ToolStreamGate>,
}

fn lock_tool_stream_gate(
    inner: &Mutex<ToolStreamGate>,
) -> std::sync::MutexGuard<'_, ToolStreamGate> {
    inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[uniffi::export]
pub fn create_tool_stream_gate(
    allow_tool_call: bool,
    tool_call_format: super::types::FfiToolCallFormat,
) -> std::sync::Arc<FfiToolStreamGate> {
    // ENC-300 D5: the host declares the stream's format per gate.
    let format = crate::tool_call_format::ToolCallFormat::from(tool_call_format);
    std::sync::Arc::new(FfiToolStreamGate {
        inner: Mutex::new(ToolStreamGate::new(allow_tool_call, format)),
    })
}

#[uniffi::export]
impl FfiToolStreamGate {
    pub fn reset(&self) {
        lock_tool_stream_gate(&self.inner).reset();
    }

    pub fn push(&self, chunk: String) -> FfiStreamGatePushResult {
        lock_tool_stream_gate(&self.inner).push(&chunk).into()
    }

    pub fn finish(&self, has_native_tool_calls: bool) -> FfiStreamGateFinish {
        lock_tool_stream_gate(&self.inner)
            .finish(has_native_tool_calls)
            .into()
    }

    pub fn buffered_text(&self) -> String {
        lock_tool_stream_gate(&self.inner)
            .buffered_text()
            .to_string()
    }

    pub fn has_tool_markup(&self) -> bool {
        lock_tool_stream_gate(&self.inner).has_tool_markup_at_finish()
    }
}
