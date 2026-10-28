//! Fuzzy tool-name resolution via [`strsim::normalized_levenshtein`].
//! Auto-accept threshold: **0.85** (ADR-locked).
//!
//! Leaf module (ENC-291): depends only on [`crate::catalog`], so
//! [`crate::canonicalize`] and [`crate::repair`] can both use it without a
//! module cycle.
//!
//! ENC-299: the case-fold here is similarity scoring, not identity — the
//! identity (b)-fold lives in [`crate::tool_name::ToolName`]. The returned
//! name is the already-canonical [`ToolName`] straight from the spec, so
//! callers adopt it without re-folding.

use crate::catalog::ToolCatalog;
use crate::tool_name::ToolName;

pub const FUZZY_AUTO_ACCEPT: f64 = 0.85;

/// Best catalog tool name by normalized Levenshtein similarity.
pub fn best_catalog_match<'a>(
    needle: &str,
    catalog: &'a ToolCatalog,
) -> Option<(&'a ToolName, f64)> {
    let needle_l = needle.to_lowercase();
    let mut best: Option<(&ToolName, f64)> = None;
    for spec in catalog.all() {
        let score = strsim::normalized_levenshtein(&needle_l, &spec.name.as_str().to_lowercase());
        if best.map_or(true, |(_, s)| score > s) {
            best = Some((&spec.name, score));
        }
        for alias in &spec.aliases {
            let score = strsim::normalized_levenshtein(&needle_l, &alias.as_str().to_lowercase());
            if best.map_or(true, |(_, s)| score > s) {
                best = Some((&spec.name, score));
            }
        }
    }
    best
}
