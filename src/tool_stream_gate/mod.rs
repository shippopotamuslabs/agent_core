//! Streaming holdback for leaked tool syntax (TOOL-PARSE-002).

mod markers;
mod result;

pub use markers::{
    first_strong_tool_marker_start, first_tool_marker_start, might_be_tool_marker_prefix,
    normalize_spaced_dsml, safe_emit_end, TOOL_MARKER_HOLDBACK_CHARS,
};
pub use result::{PushResult, StreamGateFinish};

#[derive(Debug, Default)]
pub struct ToolStreamGate {
    #[allow(dead_code)]
    allow_tool_call: bool,
    buffer: String,
    emitted_length: usize,
}

impl ToolStreamGate {
    pub fn new(allow_tool_call: bool) -> Self {
        Self {
            allow_tool_call,
            buffer: String::new(),
            emitted_length: 0,
        }
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
        self.emitted_length = 0;
    }

    pub fn push(&mut self, chunk: &str) -> PushResult {
        self.buffer.push_str(chunk);
        // Hold back leaked tool syntax for UI streaming even when the engine has disabled
        // tool execution (`allow_tool_call = false`). Execution policy is enforced in
        // `engine::ingest_model_response_envelopes`; the gate only controls visible tokens.
        let safe_end = safe_emit_end(&self.buffer);
        let mut emitted_prefix = String::new();
        if safe_end > self.emitted_length {
            emitted_prefix.push_str(&self.buffer[self.emitted_length..safe_end]);
            self.emitted_length = safe_end;
        }
        PushResult { emitted_prefix }
    }

    pub fn finish(&mut self, has_native_tool_calls: bool) -> StreamGateFinish {
        let marker_start = first_strong_tool_marker_start(&self.buffer);
        let has_tool_marker = marker_start.is_some();
        let trailing_flush = if has_native_tool_calls {
            self.flush_emitted_prefix(marker_start)
        } else if has_tool_marker {
            String::new()
        } else {
            self.flush_emitted_prefix(None)
        };
        StreamGateFinish {
            trailing_flush,
            has_tool_marker,
        }
    }

    /// Emits `buffer[emitted_length..end)` and advances `emitted_length` to `end`.
    fn flush_emitted_prefix(&mut self, end_exclusive: Option<usize>) -> String {
        let end = end_exclusive
            .unwrap_or(self.buffer.len())
            .min(self.buffer.len());
        let flush = if end > self.emitted_length {
            self.buffer[self.emitted_length..end].to_string()
        } else {
            String::new()
        };
        self.emitted_length = end;
        flush
    }

    pub fn buffered_text(&self) -> &str {
        &self.buffer
    }
}
