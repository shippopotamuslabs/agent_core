//! Streaming holdback for leaked tool syntax (TOOL-PARSE-002).

mod gate_state;
mod markers;
mod result;

pub use gate_state::{has_tool_marker_at_finish, native_tool_flush_end, ActiveHold, HoldKind};
pub(crate) use markers::original_offset_for_normalized_offset;
pub use markers::{
    earliest_confirmed_marker_start, earliest_tentative_trigger,
    first_stream_holdback_marker_start, first_strong_tool_marker_start, first_tool_marker_start,
    might_be_tool_marker_prefix, normalize_spaced_dsml, resolve_tentative_hold, safe_emit_end,
    HoldResolution, TentativeKind, TOOL_MARKER_HOLDBACK_CHARS,
};
pub use result::{PushResult, StreamGateFinish};

#[derive(Debug, Default)]
pub struct ToolStreamGate {
    #[allow(dead_code)]
    allow_tool_call: bool,
    buffer: String,
    emitted_length: usize,
    hold: Option<ActiveHold>,
}

impl ToolStreamGate {
    pub fn new(allow_tool_call: bool) -> Self {
        Self {
            allow_tool_call,
            buffer: String::new(),
            emitted_length: 0,
            hold: None,
        }
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
        self.emitted_length = 0;
        self.hold = None;
    }

    pub fn push(&mut self, chunk: &str) -> PushResult {
        self.buffer.push_str(chunk);
        let reconciled =
            gate_state::reconcile_after_push(&self.buffer, self.emitted_length, self.hold);
        self.hold = reconciled.hold;

        let mut emitted_prefix = String::new();
        let emit_start = gate_state::char_boundary_truncate(&self.buffer, self.emitted_length);
        let emit_end =
            gate_state::char_boundary_truncate(&self.buffer, reconciled.emit_exclusive_end);
        if emit_end > emit_start {
            emitted_prefix.push_str(&self.buffer[emit_start..emit_end]);
            self.emitted_length = emit_end;
        }
        PushResult { emitted_prefix }
    }

    pub fn finish(&mut self, has_native_tool_calls: bool) -> StreamGateFinish {
        let has_tool_marker = has_tool_marker_at_finish(&self.buffer, self.hold);
        let trailing_flush = if has_native_tool_calls {
            let flush_end = native_tool_flush_end(&self.buffer, self.hold);
            self.flush_emitted_prefix(flush_end)
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

    pub fn has_tool_markup_at_finish(&self) -> bool {
        has_tool_marker_at_finish(&self.buffer, self.hold)
    }
}
