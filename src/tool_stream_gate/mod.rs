//! Streaming holdback for leaked tool syntax (TOOL-PARSE-002).

mod markers;
mod result;

pub use markers::{
    first_tool_marker_start, might_be_tool_marker_prefix, safe_emit_end, TOOL_MARKER_HOLDBACK_CHARS,
};
pub use result::{PushResult, StreamGateFinish};

#[derive(Debug, Default)]
pub struct ToolStreamGate {
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
        if !self.allow_tool_call {
            let emitted = chunk.to_string();
            self.emitted_length = self.buffer.len();
            return PushResult {
                emitted_prefix: emitted,
            };
        }

        let safe_end = safe_emit_end(&self.buffer);
        let mut emitted_prefix = String::new();
        if safe_end > self.emitted_length {
            emitted_prefix.push_str(&self.buffer[self.emitted_length..safe_end]);
            self.emitted_length = safe_end;
        }
        PushResult { emitted_prefix }
    }

    pub fn finish(&mut self, has_native_tool_calls: bool) -> StreamGateFinish {
        if !self.allow_tool_call {
            let trailing_flush = self.buffer[self.emitted_length..].to_string();
            self.emitted_length = self.buffer.len();
            return StreamGateFinish {
                trailing_flush,
                has_tool_marker: false,
            };
        }

        let marker_start = first_tool_marker_start(&self.buffer);
        if has_native_tool_calls {
            let trailing_flush = if marker_start.is_none() {
                self.buffer[self.emitted_length..].to_string()
            } else {
                String::new()
            };
            if marker_start.is_none() {
                self.emitted_length = self.buffer.len();
            }
            return StreamGateFinish {
                trailing_flush,
                has_tool_marker: marker_start.is_some(),
            };
        }

        let trailing_flush = if marker_start.is_some() {
            String::new()
        } else {
            self.buffer[self.emitted_length..].to_string()
        };
        if marker_start.is_none() {
            self.emitted_length = self.buffer.len();
        }
        StreamGateFinish {
            trailing_flush,
            has_tool_marker: marker_start.is_some(),
        }
    }

    pub fn buffered_text(&self) -> &str {
        &self.buffer
    }
}
