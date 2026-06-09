//! Tentative-hold state machine for streaming tool-syntax disambiguation.

use super::markers::{
    earliest_confirmed_marker_start, earliest_tentative_trigger, last_ambiguous_angle_bracket,
    resolve_tentative_hold, HoldResolution, TentativeKind, TOOL_MARKER_HOLDBACK_CHARS,
};

/// Active hold region in the streaming buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveHold {
    pub start: usize,
    pub kind: HoldKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldKind {
    Tentative(TentativeKind),
    Confirmed,
}

/// Result of reconciling the buffer after a push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileResult {
    pub hold: Option<ActiveHold>,
    pub emit_exclusive_end: usize,
}

/// Reconcile hold state and compute how far it is safe to emit through `buffer`.
pub fn reconcile_after_push(
    buffer: &str,
    emitted_length: usize,
    hold: Option<ActiveHold>,
) -> ReconcileResult {
    let mut hold = hold;
    let mut disproved_flush = false;

    if let Some(active) = hold {
        match active.kind {
            HoldKind::Confirmed => {}
            HoldKind::Tentative(kind) => {
                match resolve_tentative_hold(&buffer[active.start..], kind) {
                    HoldResolution::Confirmed => {
                        hold = Some(ActiveHold {
                            start: active.start,
                            kind: HoldKind::Confirmed,
                        });
                    }
                    HoldResolution::Disproved => {
                        hold = None;
                        disproved_flush = true;
                    }
                    HoldResolution::StillTentative => {}
                }
            }
        }
    }

    if hold.is_none() && !disproved_flush {
        hold = detect_hold(buffer, emitted_length);
        if let Some(ActiveHold {
            start,
            kind: HoldKind::Tentative(kind),
        }) = hold
        {
            match resolve_tentative_hold(&buffer[start..], kind) {
                HoldResolution::Confirmed => {
                    hold = Some(ActiveHold {
                        start,
                        kind: HoldKind::Confirmed,
                    });
                }
                HoldResolution::Disproved => {
                    hold = None;
                    disproved_flush = true;
                }
                HoldResolution::StillTentative => {}
            }
        }
    }

    let emit_exclusive_end = if disproved_flush {
        char_boundary_truncate(buffer, buffer.len())
    } else {
        emit_boundary(buffer, emitted_length, hold)
    };

    ReconcileResult {
        hold,
        emit_exclusive_end,
    }
}

fn detect_hold(buffer: &str, search_from: usize) -> Option<ActiveHold> {
    let confirmed_at = earliest_confirmed_marker_start(buffer, search_from);
    let tentative = earliest_tentative_trigger(buffer, search_from);

    match (confirmed_at, tentative) {
        (Some(confirmed), Some((start, _kind))) if confirmed <= start => Some(ActiveHold {
            start: confirmed,
            kind: HoldKind::Confirmed,
        }),
        (Some(_confirmed), Some((start, kind))) => Some(ActiveHold {
            start,
            kind: HoldKind::Tentative(kind),
        }),
        (Some(confirmed), None) => Some(ActiveHold {
            start: confirmed,
            kind: HoldKind::Confirmed,
        }),
        (None, Some((start, kind))) => Some(ActiveHold {
            start,
            kind: HoldKind::Tentative(kind),
        }),
        (None, None) => None,
    }
}

fn emit_boundary(buffer: &str, emitted_length: usize, hold: Option<ActiveHold>) -> usize {
    let emitted_length = char_boundary_truncate(buffer, emitted_length);
    if let Some(active) = hold {
        return char_boundary_truncate(buffer, active.start);
    }
    emit_without_active_hold(buffer, emitted_length)
}

fn emit_without_active_hold(buffer: &str, emitted_length: usize) -> usize {
    if let Some(angle) = last_ambiguous_angle_bracket(buffer) {
        if angle >= emitted_length {
            return angle;
        }
    }
    let tail_start = buffer.len().saturating_sub(TOOL_MARKER_HOLDBACK_CHARS);
    let search_from = emitted_length.max(tail_start);
    if let Some((start, _)) = earliest_tentative_trigger(buffer, search_from) {
        return start;
    }
    char_boundary_truncate(buffer, buffer.len())
}

pub(crate) fn char_boundary_truncate(buffer: &str, end: usize) -> usize {
    let mut end = end.min(buffer.len());
    while end > 0 && !buffer.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Whether the gate should report a tool marker at finish (fail-closed for unresolved holds).
pub fn has_tool_marker_at_finish(buffer: &str, hold: Option<ActiveHold>) -> bool {
    if earliest_confirmed_marker_start(buffer, 0).is_some() {
        return true;
    }
    match hold {
        Some(ActiveHold {
            kind: HoldKind::Confirmed,
            ..
        }) => true,
        Some(ActiveHold {
            start,
            kind: HoldKind::Tentative(kind),
        }) => {
            !matches!(
                resolve_tentative_hold(&buffer[start..], kind),
                HoldResolution::Disproved
            )
        }
        None => false,
    }
}

/// Exclusive end index for trailing flush when native structured tool calls are present.
pub fn native_tool_flush_end(buffer: &str, hold: Option<ActiveHold>) -> Option<usize> {
    hold.map(|h| h.start)
        .or_else(|| earliest_confirmed_marker_start(buffer, 0))
        .or_else(|| {
            earliest_tentative_trigger(buffer, 0).map(|(start, _)| start)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_disproved_call_colon_flushes_through_end() {
        let buffer = "To call: the API";
        let result = reconcile_after_push(buffer, 0, None);
        assert_eq!(result.emit_exclusive_end, buffer.len());
        assert!(result.hold.is_none());
    }
}