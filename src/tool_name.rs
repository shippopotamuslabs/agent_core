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
//! - Raw model-output repair (the full snake pipeline, camelCase splitting
//!   and `functions.`/`tools.` prefix stripping) stays in
//!   [`crate::canonicalize`]: it maps noisy model text INTO a `ToolName`,
//!   it is not identity comparison.
//! - Fuzzy scoring ([`crate::tool_name_match`]) keeps its own case-fold:
//!   that is similarity scoring, not identity.
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

impl ToolName {
    /// Build the canonical identity for `raw`, rejecting blank input.
    pub fn new(raw: &str) -> Result<Self, ToolNameError> {
        let canonical = canonicalize(raw);
        if canonical.is_empty() {
            return Err(ToolNameError::Empty);
        }
        Ok(Self(canonical))
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
}
