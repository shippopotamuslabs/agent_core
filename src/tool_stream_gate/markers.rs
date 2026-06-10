//! Tool-marker detection and tentative-hold disambiguation.

use once_cell::sync::Lazy;
use regex::Regex;

pub const TOOL_MARKER_HOLDBACK_CHARS: usize = 64;

/// Strong sentinels — confirmed tool syntax (turn-end truncation + confirmed hold).
static STRONG_TOOL_MARKER_REGEXES: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?is)<\s*(?:[|｜]+\s*)?DSML\s*(?:[|｜]+\s*)?tool_calls").unwrap(),
        Regex::new(r"(?is)<\s*(?:[|｜]+\s*)?DSML\s*(?:[|｜]+\s*)?invoke\s+name\s*=").unwrap(),
        Regex::new(r"(?is)<\s*tool_call\b").unwrap(),
        Regex::new(r"(?is)<!\[CDATA\[<tool").unwrap(),
        Regex::new(r"(?is)\[TOOL_CALLS]").unwrap(),
        Regex::new(r"(?is)\bcall\s*:\s*[a-zA-Z_][a-zA-Z0-9_]*\s*\{").unwrap(),
    ]
});

static PARTIAL_TOOL_MARKER_REGEXES: Lazy<Vec<(&'static str, Regex)>> = Lazy::new(|| {
    vec![
        (
            "dsml_pipe",
            Regex::new(r"(?is)<\s*[|｜]+").unwrap(),
        ),
        (
            "dsml_partial",
            Regex::new(r"(?is)<\s*(?:[|｜]+\s*)?DSM").unwrap(),
        ),
        (
            "tool_partial",
            Regex::new(r"(?is)<\s*tool").unwrap(),
        ),
        (
            "tool_underscore",
            Regex::new(r"(?is)<\s*tool_").unwrap(),
        ),
        (
            "call_colon",
            Regex::new(r"(?is)\bcall\s*:").unwrap(),
        ),
        (
            "tool_calls_bracket",
            Regex::new(r"(?is)\[TOOL_CALLS\]").unwrap(),
        ),
    ]
});

static CALL_COLON_PREFIX: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?is)^call\s*:").unwrap());

static TOOL_PARTIAL_STILL: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?is)^<\s*tool").unwrap());

static DSML_PARTIAL_STILL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)^<\s*(?:[|｜]+|(?:[|｜]+\s*)?DSM)").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TentativeKind {
    AngleBracket,
    CallColon,
    DsmlPartial,
    ToolCallPartial,
    ToolCallsBracket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldResolution {
    StillTentative,
    Confirmed,
    Disproved,
}

/// Strong sentinels only — used for turn-end visible-text truncation.
pub fn first_strong_tool_marker_start(text: &str) -> Option<usize> {
    earliest_confirmed_marker_start(text, 0)
}

/// Earliest confirmed (strong) marker at or after `search_from`.
pub fn earliest_confirmed_marker_start(text: &str, search_from: usize) -> Option<usize> {
    if search_from >= text.len() {
        return None;
    }
    let search_from = super::gate_state::char_boundary_truncate(text, search_from);
    let slice = &text[search_from..];
    let normalized = normalize_spaced_dsml(slice);
    let mut best: Option<usize> = None;
    for regex in STRONG_TOOL_MARKER_REGEXES.iter() {
        if let Some(found) = regex.find(slice) {
            consider_marker_start(text, search_from + found.start(), &mut best);
        } else if let Some(found) = regex.find(normalized.as_str()) {
            if let Some(orig_off) = original_offset_for_normalized_offset(slice, found.start()) {
                consider_marker_start(text, search_from + orig_off, &mut best);
            }
        }
    }
    best
}

fn consider_marker_start(text: &str, abs: usize, best: &mut Option<usize>) {
    let abs = super::gate_state::char_boundary_truncate(text, abs);
    if best.map(|b| abs < b).unwrap_or(true) {
        *best = Some(abs);
    }
}

/// Map a byte offset in `normalize_spaced_dsml(original)` to a char boundary in `original`.
pub(crate) fn original_offset_for_normalized_offset(
    original: &str,
    normalized_offset: usize,
) -> Option<usize> {
    if normalized_offset == 0 {
        return Some(0);
    }
    let normalized = normalize_spaced_dsml(original);
    if normalized_offset > normalized.len() {
        return None;
    }
    for (idx, _) in original.char_indices().skip(1) {
        if normalize_spaced_dsml(&original[..idx]).len() >= normalized_offset {
            return Some(super::gate_state::char_boundary_truncate(original, idx));
        }
    }
    if normalize_spaced_dsml(original).len() >= normalized_offset {
        Some(super::gate_state::char_boundary_truncate(
            original,
            original.len(),
        ))
    } else {
        None
    }
}

/// Strong + partial sentinels — legacy helper for tests and external callers.
pub fn first_stream_holdback_marker_start(text: &str) -> Option<usize> {
    earliest_tentative_trigger(text, 0).map(|(start, _)| start).or_else(|| {
        earliest_confirmed_marker_start(text, 0)
    })
}

/// All stream-gate markers (strong + partial).
pub fn first_tool_marker_start(text: &str) -> Option<usize> {
    first_stream_holdback_marker_start(text)
}

/// Earliest tentative trigger at or after `search_from`.
pub fn earliest_tentative_trigger(text: &str, search_from: usize) -> Option<(usize, TentativeKind)> {
    if search_from >= text.len() {
        return None;
    }
    // `search_from` may come from raw byte arithmetic (e.g. the len-minus-holdback
    // tail window) and land inside a multi-byte char; slicing there panics.
    let search_from = super::gate_state::char_boundary_truncate(text, search_from);
    let slice = &text[search_from..];
    let mut best: Option<(usize, TentativeKind)> = None;

    for (name, regex) in PARTIAL_TOOL_MARKER_REGEXES.iter() {
        if let Some(found) = regex.find(slice) {
            let abs = search_from + found.start();
            let kind = match *name {
                "call_colon" => TentativeKind::CallColon,
                "tool_calls_bracket" => TentativeKind::ToolCallsBracket,
                "dsml_pipe" | "dsml_partial" => TentativeKind::DsmlPartial,
                "tool_partial" | "tool_underscore" => TentativeKind::ToolCallPartial,
                _ => TentativeKind::ToolCallPartial,
            };
            if best.map(|(b, _)| abs < b).unwrap_or(true) {
                best = Some((abs, kind));
            }
        }
    }

    if let Some(angle) = last_ambiguous_angle_bracket(text) {
        if angle >= search_from {
            if best.map(|(b, _)| angle < b).unwrap_or(true) {
                best = Some((angle, TentativeKind::AngleBracket));
            }
        }
    }

    best
}

/// Resolve whether a tentative hold region is confirmed tool syntax, disproved prose, or pending.
pub fn resolve_tentative_hold(suffix: &str, kind: TentativeKind) -> HoldResolution {
    if earliest_confirmed_marker_start(suffix, 0).is_some() {
        return HoldResolution::Confirmed;
    }

    match kind {
        TentativeKind::CallColon => resolve_call_colon(suffix),
        TentativeKind::AngleBracket => resolve_angle_bracket(suffix),
        TentativeKind::ToolCallPartial => resolve_tool_partial(suffix),
        TentativeKind::DsmlPartial => resolve_dsml_partial(suffix),
        TentativeKind::ToolCallsBracket => {
            if suffix.contains("[TOOL_CALLS]") {
                HoldResolution::Confirmed
            } else {
                HoldResolution::StillTentative
            }
        }
    }
}

fn resolve_call_colon(suffix: &str) -> HoldResolution {
    let Some(prefix) = CALL_COLON_PREFIX.find(suffix) else {
        return HoldResolution::StillTentative;
    };
    let rest = suffix[prefix.end()..].trim_start();
    if rest.is_empty() {
        return HoldResolution::StillTentative;
    }

    let ident_len = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .count();
    if ident_len == 0 {
        return HoldResolution::Disproved;
    }

    let after_ident = &rest[ident_len..];
    if after_ident.starts_with('{') {
        return HoldResolution::Confirmed;
    }
    if !after_ident.is_empty() {
        return HoldResolution::Disproved;
    }
    HoldResolution::StillTentative
}

fn resolve_angle_bracket(suffix: &str) -> HoldResolution {
    if !suffix.starts_with('<') {
        return HoldResolution::Disproved;
    }
    if !might_be_tool_marker_prefix(suffix) {
        return HoldResolution::Disproved;
    }
    HoldResolution::StillTentative
}

fn resolve_tool_partial(suffix: &str) -> HoldResolution {
    if !TOOL_PARTIAL_STILL.is_match(suffix) {
        return HoldResolution::Disproved;
    }
    HoldResolution::StillTentative
}

fn resolve_dsml_partial(suffix: &str) -> HoldResolution {
    let normalized = normalize_spaced_dsml(suffix);
    if earliest_confirmed_marker_start(suffix, 0).is_some()
        || earliest_confirmed_marker_start(&normalized, 0).is_some()
    {
        return HoldResolution::Confirmed;
    }
    if !DSML_PARTIAL_STILL.is_match(suffix) && !suffix.starts_with('<') {
        return HoldResolution::Disproved;
    }
    HoldResolution::StillTentative
}

/// Collapse spaced DSML spellings (`< | DSML |` → `<|DSML|`) before marker detect / parse.
pub fn normalize_spaced_dsml(text: &str) -> String {
    static SPACED_DSML: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?is)(</?)\s*(?:\|\s*)+DSML(?:\s*\|)+\s*").unwrap());
    SPACED_DSML
        .replace_all(text, |caps: &regex::Captures| format!("{}|DSML|", &caps[1]))
        .into_owned()
}

/// Legacy helper retained for gradual migration; prefer [gate_state::reconcile_after_push].
pub fn safe_emit_end(buffered: &str) -> usize {
    let result = super::gate_state::reconcile_after_push(buffered, 0, None);
    result.emit_exclusive_end
}

pub fn last_ambiguous_angle_bracket(text: &str) -> Option<usize> {
    let search_start = text.len().saturating_sub(TOOL_MARKER_HOLDBACK_CHARS);
    for (byte_index, _) in text.char_indices().rev() {
        if byte_index < search_start {
            break;
        }
        if text.as_bytes().get(byte_index) == Some(&b'<')
            && might_be_tool_marker_prefix(&text[byte_index..])
        {
            return Some(byte_index);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_emit_end_aligns_to_char_boundary_with_emoji() {
        let prefix = "Your research document is complete and ready to export! Here's what's inside:\n\n---\n\n## ";
        let suffix =
            "📄 **Title** some more text to make this buffer long enough to exceed 64 bytes tail";
        let buffered = format!("{}{}", prefix, suffix);
        let end = safe_emit_end(&buffered);
        assert!(
            buffered.is_char_boundary(end),
            "end={end} is not a char boundary"
        );
        let _ = &buffered[..end];
    }

    #[test]
    fn call_colon_prose_disproved() {
        assert_eq!(
            resolve_tentative_hold("call: the API", TentativeKind::CallColon),
            HoldResolution::Disproved
        );
    }

    #[test]
    fn call_colon_ident_without_brace_stays_tentative() {
        assert_eq!(
            resolve_tentative_hold("call: append_blocks", TentativeKind::CallColon),
            HoldResolution::StillTentative
        );
    }
}

pub fn might_be_tool_marker_prefix(suffix: &str) -> bool {
    if !suffix.starts_with('<') {
        return false;
    }
    let tail = &suffix[1..];
    if tail.is_empty() {
        return true;
    }
    if tail
        .chars()
        .all(|c| c == '|' || c == '｜' || c.is_whitespace())
    {
        return true;
    }
    const PREFIXES: &[&str] = &["DSML", "tool_call", "tool_calls", "![CDATA[<tool", "tool>"];
    PREFIXES.iter().any(|prefix| {
        prefix.len() >= tail.len() && prefix[..tail.len()].eq_ignore_ascii_case(tail)
            || tail.len() >= prefix.len() && tail[..prefix.len()].eq_ignore_ascii_case(prefix)
    })
}