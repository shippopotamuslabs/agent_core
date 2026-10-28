//! Fuzzy tool-name resolution via [`strsim::normalized_levenshtein`].
//! Auto-accept threshold: **0.85** (ADR-locked).
//!
//! Leaf module (ENC-291): depends only on [`crate::catalog`], so
//! [`crate::canonicalize`] and [`crate::repair`] can both use it without a
//! module cycle.

use crate::catalog::ToolCatalog;

pub const FUZZY_AUTO_ACCEPT: f64 = 0.85;

/// Best catalog tool name by normalized Levenshtein similarity.
pub fn best_catalog_match<'a>(needle: &str, catalog: &'a ToolCatalog) -> Option<(&'a str, f64)> {
    let needle_l = needle.to_lowercase();
    let mut best: Option<(&str, f64)> = None;
    for spec in catalog.all() {
        let score = strsim::normalized_levenshtein(&needle_l, &spec.name.to_lowercase());
        if best.map_or(true, |(_, s)| score > s) {
            best = Some((spec.name.as_str(), score));
        }
        for alias in &spec.aliases {
            let score = strsim::normalized_levenshtein(&needle_l, &alias.to_lowercase());
            if best.map_or(true, |(_, s)| score > s) {
                best = Some((spec.name.as_str(), score));
            }
        }
    }
    best
}
