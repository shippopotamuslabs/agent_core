//! Canonical tool-name newtype (ENC-299).
//!
//! Every tool identity in the engine (catalog specs + aliases, semantics
//! `tool_name` fields, schema entries, the active tool set) is a
//! [`ToolName`]. The canonical form is `trim + lowercase + replace('-',' '
//! -> '_')`; blank input is rejected.
//!
//! The (b)-fold that used to live as per-site copies
//! (`catalog::normalize`, `tool_semantics::normalize_tool_name`, and the
//! lookup-site `to_lowercase + replace` pairs) now lives in exactly one
//! place: [`ToolName::new`]. Lookup sites construct a `ToolName` and compare
//! for equality instead of re-folding strings.
//!
//! What stays OUT of this type, deliberately:
//! - Fuzzy scoring ([`crate::tool_name_match`]) keeps its own case-fold:
//!   that is similarity scoring, not identity.
//!
//! Raw model output enters through [`ToolName::from_model_output`]: the
//! repair pipeline (`functions.`/`tools.` prefix stripping + camelCase /
//! punctuation snake-splitting, then the identity fold). Host-declared
//! boundary names (catalog specs, schema channels, FFI records) use
//! [`ToolName::new`] directly: host names are already identifiers and must
//! NOT be snake-split.
//!
//! Serde goes through [`ToolName::new`]: a blank name is a deserialization
//! error, never a silent empty identity. The single exception is the
//! crate-internal [`ToolName::from_raw_lenient`], kept ONLY for the legacy
//! [`crate::tool_schema::SchemaEntry`] `From<Value>` `""` sentinel (nameless
//! entries must still construct so the strict channel parser can reject them
//! with entry context). No other unchecked construction exists.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Canonical tool identity. Construction validates (see [`ToolName::new`]);
/// equality, hash, and order all operate on the canonical form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToolName(String);

/// Why a [`ToolName`] could not be constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolNameError {
    /// The input was empty or whitespace-only (nothing left after trim).
    Empty,
}

impl fmt::Display for ToolNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "tool name must not be blank"),
        }
    }
}

impl std::error::Error for ToolNameError {}

/// The single (b)-fold: trim, lowercase, `-`/space to `_`.
fn canonicalize(raw: &str) -> String {
    raw.trim().to_lowercase().replace(['-', ' '], "_")
}

/// Strip provider/model namespace prefixes from raw model output.
/// Shared with the repair pipeline ([`crate::canonicalize`]).
pub(crate) fn strip_tool_prefixes(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    loop {
        if let Some(rest) = s.strip_prefix("functions.") {
            s = rest.to_string();
        } else if let Some(rest) = s.strip_prefix("tools.") {
            s = rest.to_string();
        } else {
            return s;
        }
    }
}

/// Snake-split raw model output: camelCase boundaries become `_`,
/// `-`/space become `_`, other punctuation is dropped.
/// Shared with the repair pipeline ([`crate::canonicalize`]).
pub(crate) fn normalize_to_snake_case(input: &str) -> String {
    let mut result = String::new();
    let chars: Vec<char> = input.trim().chars().collect();
    for (index, &ch) in chars.iter().enumerate() {
        if ch == '-' || ch == ' ' {
            if !result.ends_with('_') && !result.is_empty() {
                result.push('_');
            }
            continue;
        }
        if ch.is_uppercase() {
            let prev_lower = index > 0 && chars[index - 1].is_lowercase();
            let next_lower = index + 1 < chars.len() && chars[index + 1].is_lowercase();
            if (prev_lower || (index > 0 && chars[index - 1].is_ascii_uppercase() && next_lower))
                && !result.ends_with('_')
                && !result.is_empty()
            {
                result.push('_');
            }
            for lower in ch.to_lowercase() {
                result.push(lower);
            }
        } else if ch.is_alphanumeric() || ch == '_' {
            result.push(ch);
        }
    }
    while result.contains("__") {
        result = result.replace("__", "_");
    }
    result.trim_matches('_').to_string()
}

impl ToolName {
    /// Build the canonical identity for `raw`, rejecting blank input.
    pub fn new(raw: &str) -> Result<Self, ToolNameError> {
        let canonical = canonicalize(raw);
        if canonical.is_empty() {
            return Err(ToolNameError::Empty);
        }
        Ok(Self(canonical))
    }

    /// Build the canonical identity for raw model output (J1).
    ///
    /// The single model-output constructor: strip provider/model namespace
    /// prefixes, snake-split camelCase/punctuation, then enter the canonical
    /// identity space via [`ToolName::new`]. A blank result (raw was
    /// punctuation/whitespace only) is not a name at all and is rejected —
    /// it must never resolve to an empty identity.
    ///
    /// Use this for every lookup keyed by a model-emitted name (intake
    /// gating, greedy-name capture, policy lookups over call records).
    /// Host-declared boundary names use [`ToolName::new`] instead.
    pub fn from_model_output(raw: &str) -> Result<Self, ToolNameError> {
        Self::new(&normalize_to_snake_case(&strip_tool_prefixes(raw)))
    }

    /// Leniency escape hatch, crate-internal ONLY.
    ///
    /// Still canonicalizes, but permits the empty sentinel. Reserved for the
    /// [`crate::tool_schema::SchemaEntry`] `From<Value>` `""` sentinel, where
    /// nameless entries must construct so the strict channel parser can
    /// reject them with entry context. Every other construction path uses
    /// [`ToolName::new`].
    pub(crate) fn from_raw_lenient(raw: &str) -> Self {
        Self(canonicalize(raw))
    }

    /// The canonical name (`" Web-Search "` -> `"web_search"`).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True only for the [`ToolName::from_raw_lenient`] empty sentinel;
    /// [`ToolName::new`] can never produce this.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ToolName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ToolName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        ToolName::new(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_form_trims_lowercases_and_folds_separators() {
        assert_eq!(
            ToolName::new(" Web-Search ").unwrap().as_str(),
            "web_search"
        );
        assert_eq!(ToolName::new("WEB-SEARCH").unwrap().as_str(), "web_search");
        assert_eq!(ToolName::new("web_search").unwrap().as_str(), "web_search");
    }

    #[test]
    fn blank_is_rejected() {
        assert_eq!(ToolName::new(""), Err(ToolNameError::Empty));
        assert_eq!(ToolName::new("   "), Err(ToolNameError::Empty));
    }

    #[test]
    fn serde_round_trip_goes_through_new() {
        let name: ToolName = serde_json::from_str(r#""Web-Search""#).expect("valid name");
        assert_eq!(name.as_str(), "web_search");
        assert_eq!(serde_json::to_string(&name).unwrap(), r#""web_search""#);
        assert!(serde_json::from_str::<ToolName>(r#""""#).is_err());
        assert!(serde_json::from_str::<ToolName>(r#""   ""#).is_err());
    }

    #[test]
    fn lenient_sentinel_still_canonicalizes() {
        let sentinel = ToolName::from_raw_lenient("");
        assert!(sentinel.is_empty());
        assert_eq!(
            ToolName::from_raw_lenient(" Web-Search ").as_str(),
            "web_search"
        );
    }

    #[test]
    fn from_model_output_repairs_noisy_model_names() {
        // J1: the gate constructor snake-splits camelCase, so a model
        // emitting `webSearch` resolves to the active `web_search`.
        assert_eq!(
            ToolName::from_model_output("webSearch").unwrap().as_str(),
            "web_search"
        );
        assert_eq!(
            ToolName::from_model_output("functions.tools.WebSearch")
                .unwrap()
                .as_str(),
            "web_search"
        );
        assert_eq!(
            ToolName::from_model_output("web-search").unwrap().as_str(),
            "web_search"
        );
        // Idempotent on already-canonical names.
        assert_eq!(
            ToolName::from_model_output("web_search").unwrap().as_str(),
            "web_search"
        );
    }

    #[test]
    fn from_model_output_rejects_punctuation_only() {
        // Punctuation/whitespace-only model output is not a name at all
        // (was Resolved("") pre-ENC-299; now Rejects).
        assert_eq!(ToolName::from_model_output(""), Err(ToolNameError::Empty));
        assert_eq!(
            ToolName::from_model_output("   "),
            Err(ToolNameError::Empty)
        );
        assert_eq!(
            ToolName::from_model_output("!!!"),
            Err(ToolNameError::Empty)
        );
    }

    #[test]
    fn new_does_not_snake_split_host_names() {
        // Host-declared boundary names keep ToolName::new: no camelCase
        // splitting — `webSearch` as a host name stays `websearch`.
        assert_eq!(ToolName::new("webSearch").unwrap().as_str(), "websearch");
    }
}
