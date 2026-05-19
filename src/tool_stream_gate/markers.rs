//! Tool-marker detection for streaming holdback (ported from `AgentTurnTextGate.kt`).

use once_cell::sync::Lazy;
use regex::Regex;

pub const TOOL_MARKER_HOLDBACK_CHARS: usize = 64;

/// Strong sentinels used for streaming holdback (excludes weak JSON-in-prose marker).
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

static PARTIAL_TOOL_MARKER_REGEXES: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?is)<\s*[|｜]+").unwrap(),
        Regex::new(r"(?is)<\s*(?:[|｜]+\s*)?DSM").unwrap(),
        Regex::new(r"(?is)<\s*tool").unwrap(),
        Regex::new(r"(?is)<\s*tool_").unwrap(),
        Regex::new(r"(?is)\bcall\s*:").unwrap(),
    ]
});

pub fn first_strong_tool_marker_start(text: &str) -> Option<usize> {
    STRONG_TOOL_MARKER_REGEXES
        .iter()
        .chain(PARTIAL_TOOL_MARKER_REGEXES.iter())
        .filter_map(|regex| regex.find(text).map(|m| m.start()))
        .min()
}

/// All markers including parse-only weak JSON (not used by stream gate).
pub fn first_tool_marker_start(text: &str) -> Option<usize> {
    first_strong_tool_marker_start(text)
}

/// Collapse spaced DSML spellings (`< | DSML |` → `<|DSML|`) before marker detect / parse.
pub fn normalize_spaced_dsml(text: &str) -> String {
    static SPACED_DSML: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?is)(</?)\s*\|\s*DSML\s*\|\s*").unwrap());
    SPACED_DSML
        .replace_all(text, |caps: &regex::Captures| format!("{}|DSML|", &caps[1]))
        .into_owned()
}

pub fn safe_emit_end(buffered: &str) -> usize {
    if let Some(marker_start) = first_strong_tool_marker_start(buffered) {
        return marker_start;
    }
    if let Some(angle) = last_ambiguous_angle_bracket(buffered) {
        return angle;
    }
    // JSON-in-prose (e.g. `{"name":"demo"}`) is not a strong sentinel; stream through in full.
    if buffered.contains('{') && first_strong_tool_marker_start(buffered).is_none() {
        return buffered.len();
    }
    let mut end = buffered.len().saturating_sub(TOOL_MARKER_HOLDBACK_CHARS);
    while end > 0 && !buffered.is_char_boundary(end) {
        end -= 1;
    }
    end
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
