//! Tool-marker detection and tentative-hold disambiguation.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::tool_call_format::ToolCallFormat;

pub const TOOL_MARKER_HOLDBACK_CHARS: usize = 64;

/// Strong sentinels tagged by declared envelope format (ENC-300 D4/D5):
/// the stream gate and turn-end recovery consult only the declared format's
/// set, never the union.
static STRONG_TOOL_MARKERS: Lazy<Vec<(ToolCallFormat, Regex)>> = Lazy::new(|| {
    vec![
        (
            ToolCallFormat::Dsml,
            Regex::new(r"(?is)<\s*(?:[|｜]+\s*)?DSML\s*(?:[|｜]+\s*)?tool_calls").unwrap(),
        ),
        (
            ToolCallFormat::Dsml,
            Regex::new(r"(?is)<\s*(?:[|｜]+\s*)?DSML\s*(?:[|｜]+\s*)?invoke\s+name\s*=").unwrap(),
        ),
        (
            ToolCallFormat::XmlToolCall,
            Regex::new(r"(?is)<\s*tool_call\b").unwrap(),
        ),
        (
            ToolCallFormat::Cdata,
            Regex::new(r"(?is)<!\[CDATA\[<tool").unwrap(),
        ),
        (
            ToolCallFormat::BracketArray,
            Regex::new(r"(?is)\[TOOL_CALLS]").unwrap(),
        ),
        (
            ToolCallFormat::CallBrace,
            Regex::new(r"(?is)\bcall\s*:\s*[a-zA-Z_][a-zA-Z0-9_]*\s*\{").unwrap(),
        ),
        (
            ToolCallFormat::RawJson,
            Regex::new(r#"(?is)^\s*\{\s*"name"\s*:"#).unwrap(),
        ),
    ]
});

static PARTIAL_TOOL_MARKERS: Lazy<Vec<(ToolCallFormat, &'static str, Regex)>> = Lazy::new(|| {
    vec![
        (
            ToolCallFormat::Dsml,
            "dsml_pipe",
            Regex::new(r"(?is)<\s*[|｜]+").unwrap(),
        ),
        (
            ToolCallFormat::Dsml,
            "dsml_partial",
            Regex::new(r"(?is)<\s*(?:[|｜]+\s*)?DSM").unwrap(),
        ),
        (
            ToolCallFormat::XmlToolCall,
            "tool_partial",
            Regex::new(r"(?is)<\s*tool").unwrap(),
        ),
        (
            ToolCallFormat::XmlToolCall,
            "tool_underscore",
            Regex::new(r"(?is)<\s*tool_").unwrap(),
        ),
        (
            ToolCallFormat::CallBrace,
            "call_colon",
            Regex::new(r"(?is)\bcall\s*:").unwrap(),
        ),
        (
            ToolCallFormat::BracketArray,
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
    BareJsonTool,
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
    earliest_marker_start(text, search_from, |_| true)
}

/// Earliest strong marker of the DECLARED envelope format at or after
/// `search_from` (ENC-300 D4 shared marker fn). [`ToolCallFormat::Structured`]
/// has no markers: always `None`.
pub fn declared_marker_start(
    text: &str,
    search_from: usize,
    format: ToolCallFormat,
) -> Option<usize> {
    if format.is_structured() {
        return None;
    }
    earliest_marker_start(text, search_from, |candidate| candidate == format)
}

/// Earliest strong marker of any envelope format OTHER than `declared`
/// (ENC-300 D4 wrong-format detection). [`ToolCallFormat::Structured`] never
/// inspects text: always `None`.
pub fn other_format_marker_start(text: &str, declared: ToolCallFormat) -> Option<usize> {
    if declared.is_structured() {
        return None;
    }
    earliest_marker_start(text, 0, |candidate| candidate != declared)
}

/// Core strong-marker scan filtered by `allow`: the union behavior is
/// `allow = |_| true`; declared-format callers pass an equality filter.
fn earliest_marker_start(
    text: &str,
    search_from: usize,
    allow: impl Fn(ToolCallFormat) -> bool,
) -> Option<usize> {
    if search_from >= text.len() {
        return None;
    }
    let search_from = super::gate_state::char_boundary_truncate(text, search_from);
    let slice = &text[search_from..];
    let normalized = normalize_spaced_dsml(slice);
    let mut best: Option<usize> = None;
    for (format, regex) in STRONG_TOOL_MARKERS.iter() {
        if !allow(*format) {
            continue;
        }
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
    earliest_tentative_trigger(text, 0)
        .map(|(start, _)| start)
        .or_else(|| earliest_confirmed_marker_start(text, 0))
}

/// All stream-gate markers (strong + partial).
pub fn first_tool_marker_start(text: &str) -> Option<usize> {
    first_stream_holdback_marker_start(text)
}

/// Owning declared format of a tentative-hold kind (ENC-300 D5).
/// `None` for [`TentativeKind::AngleBracket`], which is shared by every
/// `<`-starting shape (Dsml, XmlToolCall, Cdata).
fn tentative_kind_format(kind: TentativeKind) -> Option<ToolCallFormat> {
    match kind {
        TentativeKind::CallColon => Some(ToolCallFormat::CallBrace),
        TentativeKind::BareJsonTool => Some(ToolCallFormat::RawJson),
        TentativeKind::AngleBracket => None,
        TentativeKind::DsmlPartial => Some(ToolCallFormat::Dsml),
        TentativeKind::ToolCallPartial => Some(ToolCallFormat::XmlToolCall),
        TentativeKind::ToolCallsBracket => Some(ToolCallFormat::BracketArray),
    }
}

/// True for formats whose markers start with `<`: only those ever hold on a
/// bare angle bracket.
fn is_angle_bracket_format(format: ToolCallFormat) -> bool {
    matches!(
        format,
        ToolCallFormat::Dsml | ToolCallFormat::XmlToolCall | ToolCallFormat::Cdata
    )
}

fn partial_kind(name: &str) -> TentativeKind {
    match name {
        "call_colon" => TentativeKind::CallColon,
        "tool_calls_bracket" => TentativeKind::ToolCallsBracket,
        "dsml_pipe" | "dsml_partial" => TentativeKind::DsmlPartial,
        "tool_partial" | "tool_underscore" => TentativeKind::ToolCallPartial,
        _ => TentativeKind::ToolCallPartial,
    }
}

/// Earliest tentative trigger at or after `search_from`.
pub fn earliest_tentative_trigger(
    text: &str,
    search_from: usize,
) -> Option<(usize, TentativeKind)> {
    earliest_tentative_trigger_inner(text, search_from, |_| true, AngleScope::Union)
}

/// Earliest tentative trigger of the DECLARED format's set at or after
/// `search_from` (ENC-300 D5). [`ToolCallFormat::Structured`] never holds:
/// always `None`.
pub fn earliest_tentative_trigger_for(
    text: &str,
    search_from: usize,
    format: ToolCallFormat,
) -> Option<(usize, TentativeKind)> {
    if format.is_structured() {
        return None;
    }
    earliest_tentative_trigger_inner(
        text,
        search_from,
        |kind| tentative_kind_format(kind).map_or(true, |owner| owner == format),
        AngleScope::Declared(format),
    )
}

/// Which ambiguous-angle-bracket scan the tentative trigger uses: the legacy
/// union prefixes, or the declared format's prefixes (none for non-`<`
/// shapes, which never hold on `<`).
#[derive(Debug, Clone, Copy)]
enum AngleScope {
    Union,
    Declared(ToolCallFormat),
}

fn earliest_tentative_trigger_inner(
    text: &str,
    search_from: usize,
    allow_kind: impl Fn(TentativeKind) -> bool,
    angle_scope: AngleScope,
) -> Option<(usize, TentativeKind)> {
    if search_from >= text.len() {
        return None;
    }
    // `search_from` may come from raw byte arithmetic (e.g. the len-minus-holdback
    // tail window) and land inside a multi-byte char; slicing there panics.
    let search_from = super::gate_state::char_boundary_truncate(text, search_from);
    let slice = &text[search_from..];
    let mut best: Option<(usize, TentativeKind)> = None;

    let object_start = text.len() - text.trim_start().len();
    if object_start >= search_from
        && text[object_start..].starts_with('{')
        && allow_kind(TentativeKind::BareJsonTool)
    {
        best = Some((object_start, TentativeKind::BareJsonTool));
    }

    for (_, name, regex) in PARTIAL_TOOL_MARKERS.iter() {
        let kind = partial_kind(name);
        if !allow_kind(kind) {
            continue;
        }
        if let Some(found) = regex.find(slice) {
            let abs = search_from + found.start();
            if best.map(|(b, _)| abs < b).unwrap_or(true) {
                best = Some((abs, kind));
            }
        }
    }

    let ambiguous_angle = match angle_scope {
        AngleScope::Union => last_ambiguous_angle_bracket(text),
        AngleScope::Declared(format) => last_ambiguous_angle_bracket_for(text, format),
    };
    let better_angle = ambiguous_angle
        .filter(|angle| *angle >= search_from)
        .filter(|angle| best.map(|(b, _)| *angle < b).unwrap_or(true));
    if let Some(angle) = better_angle {
        best = Some((angle, TentativeKind::AngleBracket));
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
        TentativeKind::BareJsonTool => resolve_bare_json_tool(suffix),
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

/// Resolve a tentative hold against the DECLARED format's marker set only
/// (ENC-300 D5): confirmation requires a declared-format strong marker, and
/// a hold kind owned by another format is disproved outright.
pub fn resolve_tentative_hold_for(
    suffix: &str,
    kind: TentativeKind,
    format: ToolCallFormat,
) -> HoldResolution {
    if declared_marker_start(suffix, 0, format).is_some() {
        return HoldResolution::Confirmed;
    }
    if let Some(owner) = tentative_kind_format(kind) {
        if owner != format {
            return HoldResolution::Disproved;
        }
    }

    match kind {
        TentativeKind::CallColon => resolve_call_colon(suffix),
        TentativeKind::BareJsonTool => resolve_bare_json_tool(suffix),
        TentativeKind::AngleBracket => resolve_angle_bracket_for(suffix, format),
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

fn resolve_bare_json_tool(suffix: &str) -> HoldResolution {
    let Some(rest) = suffix.strip_prefix('{') else {
        return HoldResolution::Disproved;
    };
    let rest = rest.trim_start();
    if rest.is_empty() {
        return HoldResolution::StillTentative;
    }

    const NAME_KEY: &str = "\"name\"";
    if NAME_KEY.starts_with(rest) {
        return HoldResolution::StillTentative;
    }
    let Some(after_name) = rest.strip_prefix(NAME_KEY) else {
        return HoldResolution::Disproved;
    };
    let after_name = after_name.trim_start();
    if after_name.is_empty() {
        return HoldResolution::StillTentative;
    }
    if after_name.starts_with(':') {
        return HoldResolution::Confirmed;
    }
    HoldResolution::Disproved
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

fn resolve_angle_bracket_for(suffix: &str, format: ToolCallFormat) -> HoldResolution {
    if !suffix.starts_with('<') {
        return HoldResolution::Disproved;
    }
    if !might_be_tool_marker_prefix_for(suffix, format) {
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

/// Declared-format version of [`last_ambiguous_angle_bracket`] (ENC-300 D5):
/// `None` for shapes whose markers never start with `<` — other-format text
/// streams through unheld.
pub fn last_ambiguous_angle_bracket_for(text: &str, format: ToolCallFormat) -> Option<usize> {
    if !is_angle_bracket_format(format) {
        return None;
    }
    let search_start = text.len().saturating_sub(TOOL_MARKER_HOLDBACK_CHARS);
    for (byte_index, _) in text.char_indices().rev() {
        if byte_index < search_start {
            break;
        }
        if text.as_bytes().get(byte_index) == Some(&b'<')
            && might_be_tool_marker_prefix_for(&text[byte_index..], format)
        {
            return Some(byte_index);
        }
    }
    None
}

/// Declared-format version of [`might_be_tool_marker_prefix`] (ENC-300 D5):
/// the tail is checked against the declared shape's opening prefix only, so
/// other-format text is never treated as a partial marker.
pub fn might_be_tool_marker_prefix_for(suffix: &str, format: ToolCallFormat) -> bool {
    if !suffix.starts_with('<') {
        return false;
    }
    // Only `<`-starting shapes hold on `<`; anything else streams through.
    let prefixes: &[&str] = match format {
        ToolCallFormat::Dsml => &["DSML"],
        ToolCallFormat::XmlToolCall => &["tool_call"],
        ToolCallFormat::Cdata => &["![CDATA[<tool"],
        _ => return false,
    };
    let tail = &suffix[1..];
    if tail.is_empty() {
        return true;
    }
    if tail
        .chars()
        .all(|c| c == '|' || c == '｜' || c.is_whitespace())
    {
        // Pipe-spaced partials are DSML-only.
        return format == ToolCallFormat::Dsml;
    }
    prefixes.iter().any(|prefix| {
        prefix.len() >= tail.len() && prefix[..tail.len()].eq_ignore_ascii_case(tail)
            || tail.len() >= prefix.len() && tail[..prefix.len()].eq_ignore_ascii_case(prefix)
    })
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
