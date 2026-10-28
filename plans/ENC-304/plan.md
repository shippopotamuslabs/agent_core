# Plan: ENC-304 — ToolUniverse rich domain model (stacked on ENC-299/#31)

## Decisions
- D1 New module `src/tool_universe.rs`: `pub struct ToolUniverse` with PRIVATE fields:
  `schemas: ToolSchemaSet`, `semantics: ToolSemanticsConfig`, `catalog: ToolCatalog`,
  `active: Vec<ToolName>` (first-seen order), `schema_index: HashMap<ToolName, usize>`.
  Export from lib.rs next to tool_name/tool_schema. Derive Debug + Clone if the
  neighbors allow (ToolSchemaSet/ToolSemanticsConfig/ToolCatalog are Clone; follow suit).
- D2 Construction: `ToolUniverse::build(schemas: ToolSchemaSet, semantics:
  ToolSemanticsConfig) -> Result<Self, ToolSemanticsError>` owns ALL construction
  validation, in today's order: (1) `ToolCatalog::from_semantics(&semantics)?`
  (structural validate + index); (2) `active = schemas.initial_active_names()`;
  (3) `semantics.validate_references(&catalog, &active)?`; (4) build schema_index
  over schemas.active. No other constructor; fields never pub.
- D3 create_session rewiring (engine/mod.rs:141-220): after
  `apply_policy_tier_defaults`, consume the tool data into the universe:
  `schemas = std::mem::replace(&mut io.schemas, ToolSchemaSet::from_entries(Vec::new()))`,
  `semantics = std::mem::replace(&mut config.tool_semantics, ToolSemanticsConfig::agnostic())`,
  `config.tool_catalog = ToolCatalog::default()` (reset; build re-derives inside the
  universe), then `let universe = ToolUniverse::build(schemas, semantics)?;`
  The prepare_tool_semantics + validate_references calls at :153-165 DELETE (build
  owns them). InnerSession: `active_tool_names` field REPLACED by `universe:
  ToolUniverse` (state.rs). Stored config/io keep their (now inert) tool fields as
  the serde/boundary input shape — document that on the InnerSession field: the
  universe is the session's tool world; config.tool_semantics / io.schemas are
  consumed at construction. Error type of create_session unchanged
  (ToolSemanticsError).
- D4 Read surface on ToolUniverse (the ONLY ways engine code reads world data):
  `semantics() -> &ToolSemanticsConfig`, `catalog() -> &ToolCatalog`,
  `schemas() -> &ToolSchemaSet`, `active_names() -> &[ToolName]`,
  `entry(&ToolName) -> Option<&SchemaEntry>` (schema_index),
  `require_entry(&ToolName) -> Result<&SchemaEntry, ToolSemanticsError>`
  (UnknownToolReference { section: "lookup", name } on miss — the ENC-305 seam),
  `spec(&ToolName) -> Option<&ToolSpec>` (catalog.resolve),
  `result_policy(&ToolName) -> Option<&ToolResultPolicy>` and
  `schema_hint(&ToolName)` (delegate to the existing free fns in tool_semantics.rs
  against self.semantics — same behavior),
  `active_json() -> &str` (schemas), `render_active_schemas() -> String`
  (universe method: iterate active_names, entry via index else SchemaEntry::stub —
  byte-identical to ToolSchemaSet::render_active_schemas(active) today),
  `render_runtime_view(...)` mirroring ToolSchemaSet::render_runtime_view's
  signature against self.schemas.
  Engine rewire: every `g.config.tool_semantics` / `g.config.tool_catalog` /
  `g.io.schemas` / `g.active_tool_names` read inside src/engine/ goes through
  `g.universe` (accessors or methods). Outside-engine consumers keep taking
  &ToolSemanticsConfig/&ToolCatalog params, fed `universe.semantics()` /
  `universe.catalog()` at call sites. tool_context.rs string-API and
  ffi From<FfiModelRequest> are NOT touched (ENC-305 / separate string API).
- D5 Active-set updates on the model: `activate(&mut self, names: impl
  IntoIterator<Item = ToolName>)` — dedupe + first-seen push (today's
  merge_active_tool_names semantics exactly). NO membership requirement:
  host owns the registry (ENC-296); activated names may be catalog-only or
  agnostic-mode names (research.md). `merge_active_tool_names` (engine/mod.rs)
  DELETES; its 3 call paths rewire: submit_tool_results host activations ->
  `universe.activate(...)`; `activate_tools_for_spill_reference` takes
  `&mut ToolUniverse` for the session side (the ToolResultRecord side keeps
  merging into result.activated_tool_names as today); model_intake
  RepairTurnInputs.active_tool_names field becomes `&mut ToolUniverse`
  (companion auto-merge + callable gate read active via the model).
  finish.rs `&[ToolName]` reads become `universe.active_names()`.
  is_companion_callable_tool stays a free fn fed from universe.semantics().
- D6 Admission — the expand path: `admit(&mut self, entry: SchemaEntry, spec:
  Option<ToolSpec>) -> Result<(), ToolSemanticsError>`. Validation on the
  candidate against the CURRENT world, all before any mutation (failure leaves
  the world unchanged): entry name must not collide with an existing entry,
  spec name, or alias (DuplicateToolName / AliasCollision — reuse the existing
  ToolSemanticsError variants and their field conventions); if spec present,
  spec.name must equal entry.name and its aliases must not collide with any
  existing name/alias (same checks ToolSemanticsConfig::validate applies to a
  catalog — mirror them, do not invent a parallel rule set). On success:
  push entry (index + raw_active resync via existing entries_json path —
  ToolSchemaSet needs a crate-internal push that keeps raw_active in sync;
  add `pub(crate) fn push_entry(&mut self, entry: SchemaEntry)` on
  ToolSchemaSet if no such path exists), push spec into
  semantics.catalog.tools and rebuild catalog via ToolCatalog::from_specs.
  Admission does NOT activate (admission != activation, per design sketch).
  No production caller in this ticket — unit/integration tests exercise it.
- D7 Byte-identity guardrails: schema Value passthrough untouched; renders
  (render_active_schemas, active_json, runtime view) byte-identical;
  ToolName rules unchanged; NO bare-string restore; From<FfiModelRequest>
  untouched; tool_schema_activation.rs dead stub untouched.

## Acceptance scenarios (tests)
- tests/enc304_universe_test.rs (new): build with enclave fixture semantics +
  schema set constructs; universe lookups resolve entry/spec/policy by ToolName.
- require_entry on a missing name -> UnknownToolReference naming the tool.
- admit: new entry+spec joins and resolves; duplicate name rejected and world
  unchanged; alias colliding with an existing canonical name rejected;
  spec naming a different tool than the entry rejected.
- admit does not activate: admitted tool absent from active_names until
  activate() is called; activate dedupes + keeps first-seen order (ltd_09 shape).
- ENC-299 acceptance suite (enc299_referential_test.rs) passes UNCHANGED
  (validation moved, not weakened): typo in argument_repairs still rejects
  create_session with UnknownToolReference.
- Agnostic config session constructs (no catalog, no semantics) and host
  activation of an unknown name still merges (ENC-296 host-owns-registry).
- Existing suites: both configs green, counts >= baseline (see status.md);
  fixture suites unchanged.

## Risks
- Sites that read config.tool_semantics AFTER create_session expecting live data
  (snapshot/restore helpers, tests/support): grep-driven rewire must catch every
  engine read; support/ test helpers that build configs are unaffected (input
  side). Any test reading session internals directly will fail loudly — fix by
  reading through the universe, never by re-adding a second source of truth.
- raw_active desync on admit: push_entry must resync via entries_json exactly
  as from_entries does.
- RepairTurnInputs is a bundle struct (ENC-306 territory) — change ONLY its
  active_tool_names field type + uses; do not restructure it here.

## Chunks / commits (each referencing ENC-304)
1. Model: tool_universe.rs (build/lookups/admit/activate/render) + lib export +
   ToolSchemaSet::push_entry if needed + unit tests in-module. Tree compiles
   (model not yet wired into the engine).
2. Wiring: create_session consumption + InnerSession field swap + all engine
   consumers rewired + merge_active_tool_names deleted.
3. Acceptance: tests/enc304_universe_test.rs + plan/findings docs.
Gates after chunks 2 and 3: fmt/clippy clean locally if available (CI is the
authority), cargo test --locked AND --no-default-features, 0 failed.
