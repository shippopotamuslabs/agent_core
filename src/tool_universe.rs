//! The session tool world as a rich domain model (ENC-304).
//!
//! [`ToolUniverse`] owns every tool the engine can deterministically act
//! on for one session: the parsed schema entries ([`ToolSchemaSet`]), the
//! semantics bundle ([`ToolSemanticsConfig`]), the validated policy
//! catalog ([`ToolCatalog`]) derived from it, and the active (callable)
//! name set. It is built once at session construction from the strongly
//! typed boundary inputs; all construction validation lives in
//! [`ToolUniverse::build`], and the world grows only through
//! [`ToolUniverse::admit`] (new entries) and [`ToolUniverse::activate`]
//! (active-set membership). Fields are private: outside code reads through
//! the accessors and render methods, and a by-name lookup miss is the
//! typed [`ToolSemanticsError::UnknownToolReference`], never an
//! empty/default substitute.
//!
//! Single source of truth (ENC-304 fix pass, M3): the universe is the
//! session's LIVE tool world — the only live source of tool data. The
//! tool fields on the boundary inputs ([`AgentConfig::tool_semantics`],
//! [`AgentConfig::tool_catalog`], `SessionIoParams::schemas`) are
//! boundary-input shape only: `create_session` CONSUMES them into the
//! universe at construction and resets the stored copies (empty schema
//! set, agnostic semantics, default catalog). Live consumers read through
//! the universe's accessors, or through an effective config derived from
//! it (`InnerSession::effective_tool_config`) where an existing
//! `&AgentConfig` consumer (guardrails, compaction tiers) is fed at the
//! dispatch boundary.

use std::collections::{HashMap, HashSet};

use crate::catalog::{ToolCatalog, ToolSchemaHint, ToolSpec};
use crate::loop_metrics::LoopMetrics;
use crate::session::{AgentConfig, SessionCapabilities};
use crate::tool_name::ToolName;
use crate::tool_schema::{SchemaEntry, ToolSchemaSet};
use crate::tool_semantics::{
    result_policy_for, schema_hint_for_semantics, ToolResultPolicy, ToolSemanticsConfig,
    ToolSemanticsError,
};

/// The session's tool world: schema entries, semantics, catalog, and the
/// active name set, validated as a whole at construction (ENC-304 D1).
#[derive(Debug, Clone)]
pub struct ToolUniverse {
    schemas: ToolSchemaSet,
    semantics: ToolSemanticsConfig,
    catalog: ToolCatalog,
    /// Active (callable) tool names, first-seen order.
    active: Vec<ToolName>,
    /// By-name index into `schemas.active`.
    schema_index: HashMap<ToolName, usize>,
}

impl ToolUniverse {
    /// Build the tool world, owning ALL construction validation in today's
    /// order (ENC-304 D2): (1) [`ToolCatalog::from_semantics`] (structural
    /// validate + index), (2) the initial active names from the schema
    /// set, (3) referential validation of every semantics section against
    /// the catalog and the active schema names, (4) the by-name schema
    /// index. There is no other constructor; a failure builds no world.
    pub fn build(
        schemas: ToolSchemaSet,
        semantics: ToolSemanticsConfig,
    ) -> Result<Self, ToolSemanticsError> {
        let catalog = ToolCatalog::from_semantics(&semantics)?;
        let active = schemas.initial_active_names();
        semantics.validate_references(&catalog, &active)?;
        // ENC-304 fix pass (M2): the schema set itself is validated here —
        // duplicate canonical names and empty names among `schemas.active`
        // are rejected. This runs AFTER the semantics checks above on
        // purpose: a semantics error that fired first before this check
        // existed still fires first. The check is load-bearing because
        // [`ToolSchemaSet::from_entries`] (the FFI typed path) performs no
        // validation of its own and its raw-string dedupe lets canonical
        // duplicates through ("Web-Search" + "web_search"); entry names
        // arrive canonical, so plain `ToolName` comparison catches them.
        let mut seen: HashSet<&ToolName> = HashSet::new();
        for entry in &schemas.active {
            if entry.name.is_empty() {
                return Err(ToolSemanticsError::InvalidSchemaEntry {
                    message: "tool schema entry name must not be empty".to_string(),
                });
            }
            if !seen.insert(&entry.name) {
                return Err(ToolSemanticsError::DuplicateToolName {
                    normalized_name: entry.name.as_str().to_string(),
                });
            }
        }
        let schema_index = Self::schema_index_for(&schemas);
        Ok(Self {
            schemas,
            semantics,
            catalog,
            active,
            schema_index,
        })
    }

    /// Derive the by-name index from the schema set's active entries —
    /// the single construction of the map from the vec (first occurrence
    /// wins; `build` has already rejected empty and duplicate names).
    fn schema_index_for(schemas: &ToolSchemaSet) -> HashMap<ToolName, usize> {
        let mut index = HashMap::new();
        for (position, entry) in schemas.active.iter().enumerate() {
            if !entry.name.is_empty() {
                index.entry(entry.name.clone()).or_insert(position);
            }
        }
        index
    }

    /// The single mutation point for entries + index: append the entry
    /// and index it atomically, so the vec and the map cannot desync.
    /// Admission commits through here and nowhere else.
    fn commit_schema_entry(&mut self, entry: SchemaEntry) {
        let index = self.schemas.active.len();
        let name = entry.name.clone();
        self.schemas.push_entry(entry);
        self.schema_index.insert(name, index);
    }

    /// The semantics bundle the world was built from.
    pub fn semantics(&self) -> &ToolSemanticsConfig {
        &self.semantics
    }

    /// The validated policy catalog derived from the semantics.
    pub fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }

    /// The parsed schema-entry set.
    pub fn schemas(&self) -> &ToolSchemaSet {
        &self.schemas
    }

    /// Active (callable) tool names, first-seen order.
    pub fn active_names(&self) -> &[ToolName] {
        &self.active
    }

    /// The schema entry for `name`, if the world carries one. Tools
    /// activated mid-session may have no entry (the host wires their full
    /// schemas provider-side, ENC-296); that is a normal `None`, not an
    /// error — use [`ToolUniverse::require_entry`] where a miss must fail.
    pub fn entry(&self, name: &ToolName) -> Option<&SchemaEntry> {
        self.schema_index
            .get(name)
            .map(|&index| &self.schemas.active[index])
    }

    /// The schema entry for `name`, or the typed lookup error on a miss
    /// (the ENC-305 seam).
    pub fn require_entry(&self, name: &ToolName) -> Result<&SchemaEntry, ToolSemanticsError> {
        self.entry(name)
            .ok_or_else(|| ToolSemanticsError::UnknownToolReference {
                section: "lookup".to_string(),
                name: name.as_str().to_string(),
            })
    }

    /// The catalog spec for `name` (canonical name or declared alias).
    pub fn spec(&self, name: &ToolName) -> Option<&ToolSpec> {
        self.catalog.resolve(name)
    }

    /// The result-handling policy for `name`, if one is configured.
    pub fn result_policy(&self, name: &ToolName) -> Option<&ToolResultPolicy> {
        result_policy_for(&self.semantics, name)
    }

    /// The schema hint for `name`, if its spec carries one.
    ///
    /// Canonical names only (ENC-304 fix pass, m5): hints are keyed by
    /// spec name in the semantics bundle, so an alias does NOT resolve
    /// here — resolve aliases through [`ToolUniverse::spec`] / the
    /// catalog first. This is the preserved behavior of the underlying
    /// `schema_hint_for_semantics`, not a restriction added here.
    pub fn schema_hint(&self, name: &ToolName) -> Option<ToolSchemaHint> {
        schema_hint_for_semantics(name, &self.semantics)
    }

    /// Session-level raw text of the active schema channel
    /// (pressure/budget paths), byte-identical to the schema set's.
    pub fn active_json(&self) -> &str {
        self.schemas.active_json()
    }

    /// Render the active schemas in active-set order, stubbing tools the
    /// active channel does not carry — byte-identical to
    /// [`ToolSchemaSet::render_active_schemas`] over the active names.
    pub fn render_active_schemas(&self) -> Vec<SchemaEntry> {
        self.active
            .iter()
            .map(|name| {
                self.entry(name)
                    .cloned()
                    .unwrap_or_else(|| SchemaEntry::stub(name))
            })
            .collect()
    }

    /// Render the runtime tool view over the parsed active channel,
    /// mirroring [`ToolSchemaSet::render_runtime_view`]'s signature
    /// against the world's own schema set.
    pub fn render_runtime_view(
        &self,
        config: &AgentConfig,
        capabilities: &SessionCapabilities,
        metrics: &LoopMetrics,
    ) -> Vec<SchemaEntry> {
        self.schemas
            .render_runtime_view(config, capabilities, metrics)
    }

    /// Merge names into the active set in first-seen order, deduped
    /// (ENC-304 D5). NO membership requirement: the host owns the registry
    /// (ENC-296), so activated names may be catalog-only or agnostic-mode
    /// names with no entry or spec in the world.
    pub fn activate(&mut self, names: impl IntoIterator<Item = ToolName>) {
        merge_tool_names_deduped(&mut self.active, names);
    }

    /// Replace the active set wholesale with `active`, deduped in
    /// first-seen order through the same merge helper [`activate`] uses
    /// (starting from an empty set). Snapshot-restore ONLY (ENC-305):
    /// [`crate::session_snapshot::SessionSnapshot`] import restores the
    /// live active set captured at export, which is authoritative over
    /// the schema set's initial active names the world was built with.
    /// Like `activate`, this imposes no membership rule — snapshot data
    /// came from a live universe, whose active set may carry
    /// host-activated entry-less names.
    pub(crate) fn restore_active(&mut self, active: Vec<ToolName>) {
        let mut restored = Vec::new();
        merge_tool_names_deduped(&mut restored, active);
        self.active = restored;
    }

    /// Admit a new tool into the world (ENC-304 D6): full validation of
    /// the candidate against the CURRENT world runs before any mutation,
    /// so a failure leaves the world unchanged. Admission does NOT
    /// activate — the admitted tool joins `active_names` only through a
    /// later [`ToolUniverse::activate`].
    pub fn admit(
        &mut self,
        entry: SchemaEntry,
        spec: Option<ToolSpec>,
    ) -> Result<(), ToolSemanticsError> {
        // Shape gate first (ENC-304 fix pass, M1): an empty name, a
        // non-object value, or a value whose `"name"` does not
        // canonicalize to the entry name is rejected before any
        // collision check, with the world unchanged.
        validate_schema_entry_shape(&entry)?;
        let entry_name = entry.name.clone();
        // The entry name must not collide with an existing entry or spec
        // name (DuplicateToolName) — the same variant and field
        // conventions `ToolSemanticsConfig::validate` applies to a
        // catalog.
        if self.schema_index.contains_key(&entry_name)
            || self
                .catalog
                .all()
                .iter()
                .any(|tool| tool.name == entry_name)
        {
            return Err(ToolSemanticsError::DuplicateToolName {
                normalized_name: entry_name.as_str().to_string(),
            });
        }
        // Entry name vs an existing alias (ENC-304 fix pass, m1): entries
        // are not semantics, so this check stays explicit — but the owner
        // is resolved through the CURRENT catalog: a resolution to a
        // DIFFERENT canonical tool means `entry_name` is that tool's
        // alias, and `first_tool` names the owning canonical tool.
        if let Some(owner) = self.catalog.resolve(&entry_name) {
            if owner.name != entry_name {
                return Err(ToolSemanticsError::AliasCollision {
                    alias: entry_name.as_str().to_string(),
                    first_tool: owner.name.as_str().to_string(),
                    second_tool: entry_name.as_str().to_string(),
                });
            }
        }
        if let Some(spec) = &spec {
            // A spec naming a different tool than the entry is a
            // malformed candidate (ENC-304 fix pass, m2), not a dangling
            // reference.
            if spec.name != entry_name {
                return Err(ToolSemanticsError::AdmissionNameMismatch {
                    entry: entry_name.as_str().to_string(),
                    spec: spec.name.as_str().to_string(),
                });
            }
            for alias in &spec.aliases {
                // An alias equal to its OWN canonical name is harmless
                // and stays allowed (mirrors `validate`).
                if alias == &spec.name {
                    continue;
                }
                // Alias vs an existing entry name, or vs an active name
                // other than the candidate's own (ENC-304 fix pass, m3):
                // a spec alias may not claim a host-activated entry-less
                // active name. (Admitting an ENTRY named exactly such an
                // active name stays legal — it upgrades a stub to a
                // schema; only the alias claim is rejected.) Alias vs
                // canonical spec names and alias vs alias are left to the
                // validation-by-construction below, whose canonical rule
                // set reports them.
                if self.schema_index.contains_key(alias)
                    || self.active.iter().any(|active| active == alias)
                {
                    return Err(ToolSemanticsError::AliasCollision {
                        alias: alias.as_str().to_string(),
                        first_tool: alias.as_str().to_string(),
                        second_tool: entry_name.as_str().to_string(),
                    });
                }
            }
        }
        // Validation by construction (ENC-304 fix pass, m1): the
        // candidate spec joins a CLONE of the semantics and the canonical
        // rule set (`ToolCatalog::from_semantics` -> `validate`) checks
        // the whole bundle — duplicate names, alias-vs-canonical and
        // alias-vs-alias collisions — so admission rules cannot drift
        // from catalog rules. Nothing has mutated yet, so a failure here
        // is atomic by construction.
        let mut candidate_semantics = self.semantics.clone();
        if let Some(spec) = &spec {
            candidate_semantics.catalog.tools.push(spec.clone());
        }
        let candidate_catalog = ToolCatalog::from_semantics(&candidate_semantics)?;
        // All checks passed: commit entry + semantics + catalog together.
        // The entry push resyncs the schema set's raw text via the
        // `entries_json` path.
        self.commit_schema_entry(entry);
        self.semantics = candidate_semantics;
        self.catalog = candidate_catalog;
        Ok(())
    }
}

/// The one home for the first-seen-order, deduped `ToolName` merge
/// (ENC-304 fix pass, s1): [`ToolUniverse::activate`] merges into the
/// session's active set through here, and the spill-reference activation
/// path merges into a `ToolResultRecord`'s `activated_tool_names`
/// through here, so the two sides cannot drift.
pub(crate) fn merge_tool_names_deduped(
    target: &mut Vec<ToolName>,
    names: impl IntoIterator<Item = ToolName>,
) {
    for name in names {
        if !target.iter().any(|existing| existing == &name) {
            target.push(name);
        }
    }
}

/// Single-entry shape gate for admission (ENC-304 fix pass, M1).
///
/// Mirrors the per-entry rules behind `ToolSchemaSet::validate_entries`
/// (object value; `"name"` present, a string, non-blank; canonical name
/// unique within the channel — the uniqueness half is `admit`'s
/// DuplicateToolName check). It is mirrored rather than shared because
/// `validate_entries` is channel-shaped: it consumes raw `Value`s and
/// reports `ToolSchemaError` with channel/index context, while admission
/// validates an already-built [`SchemaEntry`] and reports this family's
/// [`ToolSemanticsError`]. The rules themselves are the same: the value
/// `"name"` must canonicalize (via [`ToolName::new`], the same fold
/// `validate_entries` applies) to exactly the entry's name.
fn validate_schema_entry_shape(entry: &SchemaEntry) -> Result<(), ToolSemanticsError> {
    if entry.name.is_empty() {
        return Err(ToolSemanticsError::InvalidSchemaEntry {
            message: "tool schema entry name must not be empty".to_string(),
        });
    }
    if !entry.value.is_object() {
        return Err(ToolSemanticsError::InvalidSchemaEntry {
            message: format!(
                "tool schema entry '{}' value must be a schema object",
                entry.name
            ),
        });
    }
    let value_name = entry
        .value
        .get("name")
        .and_then(|name| name.as_str())
        .ok_or_else(|| ToolSemanticsError::InvalidSchemaEntry {
            message: format!(
                "tool schema entry '{}' value has no string \"name\"",
                entry.name
            ),
        })?;
    match ToolName::new(value_name) {
        Ok(name) if name == entry.name => Ok(()),
        _ => Err(ToolSemanticsError::InvalidSchemaEntry {
            message: format!(
                "tool schema entry name '{}' does not match its value \"name\" '{value_name}'",
                entry.name
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ToolSchemaHint;
    use crate::tool_semantics::{ToolCatalogConfig, ToolResultPolicy};
    use std::collections::HashMap as StdHashMap;

    fn tn(name: &str) -> ToolName {
        ToolName::new(name).expect("valid test name")
    }

    fn entry(name: &str) -> SchemaEntry {
        SchemaEntry::from(serde_json::json!({"name": name, "description": "d"}))
    }

    fn spec(name: &str, aliases: &[&str]) -> ToolSpec {
        ToolSpec {
            name: tn(name),
            aliases: aliases.iter().map(|alias| tn(alias)).collect(),
            reproducible: true,
            schema_hint: None,
        }
    }

    fn schemas(names: &[&str]) -> ToolSchemaSet {
        ToolSchemaSet::from_entries(names.iter().map(|name| entry(name)).collect())
    }

    fn semantics_with(tools: Vec<ToolSpec>) -> ToolSemanticsConfig {
        ToolSemanticsConfig {
            catalog: ToolCatalogConfig { tools },
            ..ToolSemanticsConfig::agnostic()
        }
    }

    #[test]
    fn build_constructs_and_lookups_resolve() {
        let mut hint_spec = spec("web_search", &["websearch"]);
        hint_spec.schema_hint = Some(ToolSchemaHint {
            required_fields: Vec::new(),
            defaults: StdHashMap::new(),
        });
        let mut semantics = semantics_with(vec![hint_spec]);
        semantics.result_policies = vec![ToolResultPolicy {
            tool_name: tn("web_search"),
            spill_stored_copy_only: true,
            skip_result_truncation: false,
            preserve_in_micro_compaction: false,
        }];
        let universe = ToolUniverse::build(schemas(&["web_search", "read_file"]), semantics)
            .expect("valid world builds");
        assert_eq!(
            universe.active_names(),
            &[tn("web_search"), tn("read_file")]
        );
        assert_eq!(
            universe
                .entry(&tn("web_search"))
                .expect("entry")
                .description(),
            "d"
        );
        assert!(universe.entry(&tn("ghost")).is_none());
        assert_eq!(
            universe
                .require_entry(&tn("read_file"))
                .expect("entry")
                .name,
            tn("read_file")
        );
        // Spec lookup resolves canonical names and aliases alike.
        assert_eq!(
            universe.spec(&tn("web_search")).expect("spec").name,
            tn("web_search")
        );
        assert_eq!(
            universe.spec(&tn("websearch")).expect("alias spec").name,
            tn("web_search")
        );
        assert!(
            universe
                .result_policy(&tn("web_search"))
                .expect("policy")
                .spill_stored_copy_only
        );
        assert!(universe.result_policy(&tn("read_file")).is_none());
        assert!(universe.schema_hint(&tn("web_search")).is_some());
        assert!(universe.schema_hint(&tn("read_file")).is_none());
        assert_eq!(universe.catalog().len(), 1);
        assert!(universe.active_json().contains("web_search"));
        // Render: active order, full entries for carried tools.
        let rendered = universe.render_active_schemas();
        assert_eq!(rendered.len(), 2);
        assert_eq!(rendered[0].name, tn("web_search"));
    }

    #[test]
    fn build_rejects_unknown_semantics_reference() {
        let mut semantics = semantics_with(vec![spec("web_search", &[])]);
        semantics.result_policies = vec![ToolResultPolicy {
            tool_name: tn("ghost_tool"),
            spill_stored_copy_only: false,
            skip_result_truncation: false,
            preserve_in_micro_compaction: false,
        }];
        let error = ToolUniverse::build(schemas(&["web_search"]), semantics)
            .expect_err("dangling reference must fail build");
        assert!(matches!(
            error,
            ToolSemanticsError::UnknownToolReference { ref name, .. } if name == "ghost_tool"
        ));
    }

    #[test]
    fn build_rejects_duplicate_spec_names() {
        let semantics = semantics_with(vec![spec("web_search", &[]), spec("web-search", &[])]);
        let error = ToolUniverse::build(schemas(&["web_search"]), semantics)
            .expect_err("duplicate spec names must fail build");
        assert!(matches!(
            error,
            ToolSemanticsError::DuplicateToolName { .. }
        ));
    }

    #[test]
    fn require_entry_miss_is_unknown_tool_reference() {
        let universe =
            ToolUniverse::build(schemas(&["web_search"]), ToolSemanticsConfig::agnostic())
                .expect("agnostic world builds");
        let error = universe
            .require_entry(&tn("ghost_tool"))
            .expect_err("missing entry must error");
        assert_eq!(
            error,
            ToolSemanticsError::UnknownToolReference {
                section: "lookup".to_string(),
                name: "ghost_tool".to_string(),
            }
        );
    }

    #[test]
    fn activate_dedupes_and_keeps_first_seen_order() {
        let mut universe =
            ToolUniverse::build(schemas(&["a_tool"]), ToolSemanticsConfig::agnostic())
                .expect("agnostic world builds");
        // Host-owns-registry: names with no entry/spec still merge.
        universe.activate([tn("b_tool"), tn("a_tool"), tn("c_tool")]);
        universe.activate([tn("b_tool"), tn("d_tool")]);
        assert_eq!(
            universe.active_names(),
            &[tn("a_tool"), tn("b_tool"), tn("c_tool"), tn("d_tool")]
        );
    }

    #[test]
    fn admit_joins_world_without_activating() {
        let mut universe = ToolUniverse::build(
            schemas(&["web_search"]),
            semantics_with(vec![spec("web_search", &[])]),
        )
        .expect("world builds");
        universe
            .admit(
                entry("fetch_page"),
                Some(spec("fetch_page", &["fetchpage"])),
            )
            .expect("admission succeeds");
        assert_eq!(
            universe.entry(&tn("fetch_page")).expect("entry").name,
            tn("fetch_page")
        );
        assert_eq!(
            universe.spec(&tn("fetchpage")).expect("spec").name,
            tn("fetch_page")
        );
        assert!(universe.active_json().contains("fetch_page"));
        // Admission != activation.
        assert_eq!(universe.active_names(), &[tn("web_search")]);
        universe.activate([tn("fetch_page")]);
        assert_eq!(
            universe.active_names(),
            &[tn("web_search"), tn("fetch_page")]
        );
        // The admitted entry renders as a full entry, not a stub.
        let rendered = universe.render_active_schemas();
        assert_eq!(rendered[1].description(), "d");
    }

    #[test]
    fn admit_duplicate_name_rejected_and_world_unchanged() {
        let mut universe = ToolUniverse::build(
            schemas(&["web_search"]),
            semantics_with(vec![spec("web_search", &[])]),
        )
        .expect("world builds");
        let error = universe
            .admit(entry("web_search"), Some(spec("web_search", &[])))
            .expect_err("duplicate admission must fail");
        assert!(matches!(
            error,
            ToolSemanticsError::DuplicateToolName { .. }
        ));
        assert_eq!(universe.schemas().active.len(), 1);
        assert_eq!(universe.catalog().len(), 1);
    }

    #[test]
    fn admit_entry_name_colliding_with_alias_rejected() {
        let mut universe = ToolUniverse::build(
            schemas(&["web_search"]),
            semantics_with(vec![spec("web_search", &["websearch"])]),
        )
        .expect("world builds");
        let error = universe
            .admit(entry("websearch"), None)
            .expect_err("entry name shadowing an alias must fail");
        assert!(matches!(
            error,
            ToolSemanticsError::AliasCollision { ref alias, .. } if alias == "websearch"
        ));
        assert_eq!(universe.schemas().active.len(), 1);
    }

    #[test]
    fn admit_alias_colliding_with_canonical_name_rejected() {
        let mut universe = ToolUniverse::build(
            schemas(&["web_search"]),
            semantics_with(vec![spec("web_search", &[])]),
        )
        .expect("world builds");
        let error = universe
            .admit(
                entry("fetch_page"),
                Some(spec("fetch_page", &["web_search"])),
            )
            .expect_err("alias shadowing a canonical name must fail");
        assert!(matches!(
            error,
            ToolSemanticsError::AliasCollision { ref alias, ref first_tool, ref second_tool }
            if alias == "web_search" && first_tool == "web_search" && second_tool == "fetch_page"
        ));
        // World unchanged on failure.
        assert!(universe.entry(&tn("fetch_page")).is_none());
        assert_eq!(universe.catalog().len(), 1);
    }

    #[test]
    fn admit_spec_naming_a_different_tool_rejected() {
        let mut universe =
            ToolUniverse::build(schemas(&["web_search"]), ToolSemanticsConfig::agnostic())
                .expect("world builds");
        // m2: a spec/entry mismatch is the dedicated variant carrying
        // both names, NOT UnknownToolReference.
        let error = universe
            .admit(entry("fetch_page"), Some(spec("other_tool", &[])))
            .expect_err("spec/entry mismatch must fail");
        assert_eq!(
            error,
            ToolSemanticsError::AdmissionNameMismatch {
                entry: "fetch_page".to_string(),
                spec: "other_tool".to_string(),
            }
        );
        assert!(universe.entry(&tn("fetch_page")).is_none());
        assert!(universe.catalog().is_empty());
    }

    #[test]
    fn build_rejects_canonical_duplicate_schema_entries() {
        // M2: `from_entries` (the FFI typed path) performs no validation,
        // so "Web-Search" + "web_search" arrive as two entries that
        // canonicalize identically; build must reject them.
        let dupes = ToolSchemaSet::from_entries(vec![entry("Web-Search"), entry("web_search")]);
        let error = ToolUniverse::build(dupes, ToolSemanticsConfig::agnostic())
            .expect_err("canonical-duplicate entries must fail build");
        assert_eq!(
            error,
            ToolSemanticsError::DuplicateToolName {
                normalized_name: "web_search".to_string(),
            }
        );
    }

    #[test]
    fn build_rejects_empty_name_schema_entry() {
        // M2: a nameless value builds an empty-sentinel entry via the
        // lenient `From<Value>` path; build must reject it.
        let nameless = ToolSchemaSet::from_entries(vec![SchemaEntry::from(
            serde_json::json!({"description": "no name"}),
        )]);
        let error = ToolUniverse::build(nameless, ToolSemanticsConfig::agnostic())
            .expect_err("empty-name entry must fail build");
        assert!(matches!(
            error,
            ToolSemanticsError::InvalidSchemaEntry { .. }
        ));
    }

    #[test]
    fn build_schema_check_runs_after_semantics_validation() {
        // M2 ordering: a semantics error that fired first before the
        // schema check existed (here a dangling reference, caught by
        // validate_references) must still fire first — the schema check
        // must not preempt it.
        let mut semantics = semantics_with(vec![spec("web_search", &[])]);
        semantics.result_policies = vec![ToolResultPolicy {
            tool_name: tn("ghost_tool"),
            spill_stored_copy_only: false,
            skip_result_truncation: false,
            preserve_in_micro_compaction: false,
        }];
        let dupes = ToolSchemaSet::from_entries(vec![entry("Web-Search"), entry("web_search")]);
        let error = ToolUniverse::build(dupes, semantics)
            .expect_err("dangling reference must fail build first");
        assert!(matches!(
            error,
            ToolSemanticsError::UnknownToolReference { ref name, .. } if name == "ghost_tool"
        ));
    }

    #[test]
    fn admit_empty_name_rejected_and_world_unchanged() {
        // M1: the empty-sentinel entry (nameless value) is rejected at
        // the shape gate.
        let mut universe =
            ToolUniverse::build(schemas(&["web_search"]), ToolSemanticsConfig::agnostic())
                .expect("world builds");
        let active_json_before = universe.active_json().to_string();
        let error = universe
            .admit(
                SchemaEntry::from(serde_json::json!({"description": "no name"})),
                None,
            )
            .expect_err("empty-name admission must fail");
        assert!(matches!(
            error,
            ToolSemanticsError::InvalidSchemaEntry { .. }
        ));
        assert_eq!(universe.schemas().active.len(), 1);
        assert_eq!(universe.active_json(), active_json_before);
    }

    #[test]
    fn admit_non_object_value_rejected() {
        // M1: a manually built entry whose value is not a schema object.
        let mut universe =
            ToolUniverse::build(schemas(&["web_search"]), ToolSemanticsConfig::agnostic())
                .expect("world builds");
        let bad = SchemaEntry {
            name: tn("fetch_page"),
            value: serde_json::json!(["not", "an", "object"]),
        };
        let error = universe
            .admit(bad, None)
            .expect_err("non-object admission must fail");
        assert!(matches!(
            error,
            ToolSemanticsError::InvalidSchemaEntry { .. }
        ));
        assert!(universe.entry(&tn("fetch_page")).is_none());
    }

    #[test]
    fn admit_name_value_divergence_rejected() {
        // M1: entry.name and value["name"] disagree (only reachable via
        // direct struct construction — `From<Value>` keeps them in sync).
        let mut universe =
            ToolUniverse::build(schemas(&["web_search"]), ToolSemanticsConfig::agnostic())
                .expect("world builds");
        let active_json_before = universe.active_json().to_string();
        let divergent = SchemaEntry {
            name: tn("fetch_page"),
            value: serde_json::json!({"name": "other_tool", "description": "d"}),
        };
        let error = universe
            .admit(divergent, None)
            .expect_err("name/value divergence must fail");
        assert!(matches!(
            error,
            ToolSemanticsError::InvalidSchemaEntry { .. }
        ));
        assert!(universe.entry(&tn("fetch_page")).is_none());
        assert_eq!(universe.active_json(), active_json_before);
    }

    #[test]
    fn admit_alias_colliding_with_existing_alias_rejected_via_from_semantics() {
        // Disposition: alias-vs-alias rejection through the
        // validation-by-construction path — "websearch" is web_search's
        // alias, not an entry name and not active, so only the cloned
        // semantics validation (`ToolCatalog::from_semantics`) can catch
        // the candidate claiming it.
        let mut universe = ToolUniverse::build(
            schemas(&["web_search"]),
            semantics_with(vec![spec("web_search", &["websearch"])]),
        )
        .expect("world builds");
        let active_json_before = universe.active_json().to_string();
        let error = universe
            .admit(
                entry("fetch_page"),
                Some(spec("fetch_page", &["websearch"])),
            )
            .expect_err("alias-vs-alias admission must fail");
        assert_eq!(
            error,
            ToolSemanticsError::AliasCollision {
                alias: "websearch".to_string(),
                first_tool: "web_search".to_string(),
                second_tool: "fetch_page".to_string(),
            }
        );
        assert!(universe.entry(&tn("fetch_page")).is_none());
        assert_eq!(universe.catalog().len(), 1);
        assert_eq!(universe.active_json(), active_json_before);
    }

    #[test]
    fn admit_alias_claiming_active_entry_less_name_rejected() {
        // m3: a spec alias may not claim a host-activated entry-less
        // active name.
        let mut universe =
            ToolUniverse::build(schemas(&["a_tool"]), ToolSemanticsConfig::agnostic())
                .expect("world builds");
        universe.activate([tn("host_only_tool")]);
        let error = universe
            .admit(
                entry("fetch_page"),
                Some(spec("fetch_page", &["host_only_tool"])),
            )
            .expect_err("alias claiming an active entry-less name must fail");
        assert_eq!(
            error,
            ToolSemanticsError::AliasCollision {
                alias: "host_only_tool".to_string(),
                first_tool: "host_only_tool".to_string(),
                second_tool: "fetch_page".to_string(),
            }
        );
        assert!(universe.entry(&tn("fetch_page")).is_none());
        assert!(universe.catalog().is_empty());
    }

    #[test]
    fn admit_entry_for_active_entry_less_name_is_legal() {
        // (f): admitting an ENTRY named exactly an active entry-less name
        // stays legal — it upgrades the host stub to a real schema.
        let mut universe =
            ToolUniverse::build(schemas(&["a_tool"]), ToolSemanticsConfig::agnostic())
                .expect("world builds");
        universe.activate([tn("host_only_tool")]);
        universe
            .admit(entry("host_only_tool"), None)
            .expect("entry admission for an active entry-less name must succeed");
        assert_eq!(
            universe
                .entry(&tn("host_only_tool"))
                .expect("entry")
                .description(),
            "d"
        );
        // Admission did not change the active set (it was already there).
        assert_eq!(
            universe.active_names(),
            &[tn("a_tool"), tn("host_only_tool")]
        );
        // And it now renders its full entry instead of a stub.
        let rendered = universe.render_active_schemas();
        assert_eq!(rendered[1].description(), "d");
    }
}
