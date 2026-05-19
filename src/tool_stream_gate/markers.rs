//! Tool-marker detection for streaming holdback (ported from `AgentTurnTextGate.kt`).

use once_cell::sync::Lazy;
use regex::Regex;

pub const TOOL_MARKER_HOLDBACK_CHARS: usize = 64;

static TOOL_MARKER_REGEXES: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?is)<\s*(?:[|｜]+\s*)?DSML\s*(?:[|｜]+\s*)?tool_calls").unwrap(),
        Regex::new(r"(?is)<\s*(?:[|｜]+\s*)?DSML\s*(?:[|｜]+\s*)?invoke\s+name\s*=").unwrap(),
        Regex::new(r"(?is)<\s*tool_call\b").unwrap(),
        Regex::new(r"(?is)<!\[CDATA\[<tool").unwrap(),
        Regex::new(r"(?is)\[TOOL_CALLS]").unwrap(),
        Regex::new(r#"(?is)\{\s*["']name["']\s*:"#).unwrap(),
    ]
});

static PARTIAL_TOOL_MARKER_REGEXES: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?is)<\s*[|｜]+").unwrap(),
        Regex::new(r"(?is)<\s*(?:[|｜]+\s*)?DSM").unwrap(),
        Regex::new(r"(?is)<\s*tool_").unwrap(),
    ]
});

pub fn first_tool_marker_start(text: &str) -> Option<usize> {
    TOOL_MARKER_REGEXES
        .iter()
        .chain(PARTIAL_TOOL_MARKER_REGEXES.iter())
        .filter_map(|regex| regex.find(text).map(|m| m.start()))
        .min()
}

pub fn safe_emit_end(buffered: &str) -> usize {
    if let Some(marker_start) = first_tool_marker_start(buffered) {
        return marker_start;
    }
    if let Some(angle) = last_ambiguous_angle_bracket(buffered) {
        return angle;
    }
    buffered.len().saturating_sub(TOOL_MARKER_HOLDBACK_CHARS)
}

fn last_ambiguous_angle_bracket(text: &str) -> Option<usize> {
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
    const PREFIXES: &[&str] = &[
        "DSML",
        "tool_call",
        "tool_calls",
        "![CDATA[<tool",
        "tool>",
    ];
    PREFIXES.iter().any(|prefix| {
        prefix.len() >= tail.len() && prefix[..tail.len()].eq_ignore_ascii_case(tail)
            || tail.len() >= prefix.len() && tail[..prefix.len()].eq_ignore_ascii_case(prefix)
    })
}
