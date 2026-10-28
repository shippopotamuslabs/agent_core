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

use std::collections::HashMap;

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
        let mut schema_index = HashMap::new();
        for (index, entry) in schemas.active.iter().enumerate() {
            if !entry.name.is_empty() {
                schema_index.entry(entry.name.clone()).or_insert(index);
            }
        }
        Ok(Self {
            schemas,
            semantics,
            catalog,
            active,
            schema_index,
        })
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
        self.entry(name).ok_or_else(|| ToolSemanticsError::UnknownToolReference {
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
        self.schemas.render_runtime_view(config, capabilities, metrics)
    }

    /// Merge names into the active set in first-seen order, deduped
    /// (ENC-304 D5). NO membership requirement: the host owns the registry
    /// (ENC-296), so activated names may be catalog-only or agnostic-mode
    /// names with no entry or spec in the world.
    pub fn activate(&mut self, names: impl IntoIterator<Item = ToolName>) {
        for name in names {
            if !self.active.iter().any(|existing| existing == &name) {
                self.active.push(name);
            }
        }
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
        let entry_name = entry.name.clone();
        // The entry name must not collide with an existing entry or spec
        // name (DuplicateToolName) or with an existing alias
        // (AliasCollision) — the same variants and field conventions
        // `ToolSemanticsConfig::validate` applies to a catalog.
        if self.schema_index.contains_key(&entry_name)
            || self.catalog.all().iter().any(|tool| tool.name == entry_name)
        {
            return Err(ToolSemanticsError::DuplicateToolName {
                normalized_name: entry_name.as_str().to_string(),
            });
        }
        if let Some(owner) = self.alias_owner(&entry_name) {
            return Err(ToolSemanticsError::AliasCollision {
                alias: entry_name.as_str().to_string(),
                first_tool: owner.as_str().to_string(),
                second_tool: entry_name.as_str().to_string(),
            });
        }
        if let Some(spec) = &spec {
            if spec.name != entry_name {
                return Err(ToolSemanticsError::UnknownToolReference {
                    section: "admit".to_string(),
                    name: spec.name.as_str().to_string(),
                });
            }
            for alias in &spec.aliases {
                // An alias equal to its OWN canonical name is harmless
                // and stays allowed (mirrors `validate`).
                if alias == &spec.name {
                    continue;
                }
                // Alias vs an existing entry or canonical spec name.
                if self.schema_index.contains_key(alias)
                    || self.catalog.all().iter().any(|tool| &tool.name == alias)
                {
                    return Err(ToolSemanticsError::AliasCollision {
                        alias: alias.as_str().to_string(),
                        first_tool: alias.as_str().to_string(),
                        second_tool: spec.name.as_str().to_string(),
                    });
                }
                // Alias vs an alias already owned by another tool.
                if let Some(owner) = self.alias_owner(alias) {
                    return Err(ToolSemanticsError::AliasCollision {
                        alias: alias.as_str().to_string(),
                        first_tool: owner.as_str().to_string(),
                        second_tool: spec.name.as_str().to_string(),
                    });
                }
            }
        }
        // All checks passed: mutate. The entry push resyncs the schema
        // set's raw text via the `entries_json` path.
        let index = self.schemas.active.len();
        let admitted_name = entry.name.clone();
        self.schemas.push_entry(entry);
        if !admitted_name.is_empty() {
            self.schema_index.insert(admitted_name, index);
        }
        if let Some(spec) = spec {
            self.semantics.catalog.tools.push(spec);
            self.catalog = ToolCatalog::from_specs(self.semantics.catalog.tools.clone());
        }
        Ok(())
    }

    /// The canonical name of the tool that owns `alias`, if any spec
    /// declares it.
    fn alias_owner(&self, alias: &ToolName) -> Option<&ToolName> {
        self.catalog
            .all()
            .iter()
            .find(|tool| tool.aliases.iter().any(|candidate| candidate == alias))
            .map(|tool| &tool.name)
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
        assert_eq!(universe.active_names(), &[tn("web_search"), tn("read_file")]);
        assert_eq!(
            universe.entry(&tn("web_search")).expect("entry").description(),
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
        assert_eq!(universe.spec(&tn("web_search")).expect("spec").name, tn("web_search"));
        assert_eq!(universe.spec(&tn("websearch")).expect("alias spec").name, tn("web_search"));
        assert!(universe
            .result_policy(&tn("web_search"))
            .expect("policy")
            .spill_stored_copy_only);
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
        let universe = ToolUniverse::build(
            schemas(&["web_search"]),
            ToolSemanticsConfig::agnostic(),
        )
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
        let mut universe = ToolUniverse::build(
            schemas(&["a_tool"]),
            ToolSemanticsConfig::agnostic(),
        )
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
            .admit(entry("fetch_page"), Some(spec("fetch_page", &["fetchpage"])))
            .expect("admission succeeds");
        assert_eq!(
            universe.entry(&tn("fetch_page")).expect("entry").name,
            tn("fetch_page")
        );
        assert_eq!(universe.spec(&tn("fetchpage")).expect("spec").name, tn("fetch_page"));
        assert!(universe.active_json().contains("fetch_page"));
        // Admission != activation.
        assert_eq!(universe.active_names(), &[tn("web_search")]);
        universe.activate([tn("fetch_page")]);
        assert_eq!(universe.active_names(), &[tn("web_search"), tn("fetch_page")]);
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
            .admit(entry("fetch_page"), Some(spec("fetch_page", &["web_search"])))
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
        let mut universe = ToolUniverse::build(
            schemas(&["web_search"]),
            ToolSemanticsConfig::agnostic(),
        )
        .expect("world builds");
        assert!(universe
            .admit(entry("fetch_page"), Some(spec("other_tool", &[])))
            .is_err());
        assert!(universe.entry(&tn("fetch_page")).is_none());
        assert!(universe.catalog().is_empty());
    }
}
