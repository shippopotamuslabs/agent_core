# ENC-304 Research (scout, spot-checked by orchestrator)

- Construction seam: `AgentEngine::create_session` (src/engine/mod.rs:141) is the ONLY
  InnerSession construction. Today: `config.prepare_tool_semantics()?` (:153),
  `io.schemas.initial_active_names()` (:162), `config.tool_semantics.validate_references
  (&config.tool_catalog, &names)?` (:163-165), `active_tool_names` stored on
  InnerSession (src/engine/state.rs:117).
- Tool data lives in three places today: `io.schemas: ToolSchemaSet` (tool_schema.rs:146;
  only `active: Vec<SchemaEntry>` + `raw_active` post ENC-296/297), `config.tool_semantics:
  ToolSemanticsConfig` (session.rs:132), derived `config.tool_catalog: ToolCatalog`
  (session.rs:137, serde skip). Specs stored twice (semantics.catalog.tools + catalog).
- AgentConfig is Serialize/Deserialize (struct-literal/serde configs re-synced at
  create_session) — semantics must stay the serde-carried input on AgentConfig;
  the universe consumes it at construction.
- ToolSchemaSet has NO by-name lookup/index: `render_active_schemas` builds a fresh
  HashMap per call (tool_schema.rs:306-327, stub fallback for missing names);
  `render_runtime_view` linear-filters (:330-348). Lookups into semantics are linear
  scans keyed by ToolName equality (result_policy_for, schema_hint_for_semantics,
  semantic_rule, canonical shapes, greedy, companion).
- Active-set growth: ALL paths flow through `merge_active_tool_names`
  (engine/mod.rs:72-81, dedupe + push, NO validation): host-activated names in
  submit_tool_results (:533-538), spill-reference activation (:92-106, hardcoded
  read_context_reference hint), companion auto-merge (model_intake.rs:174-177).
- Post-ENC-296 reality: host owns the registry; full schemas wired provider-side.
  Discovery activation merges NAMES only; activated names are catalog specs without
  schema entries (rendered as stubs) or, under agnostic semantics, names with no
  spec at all. Activation therefore cannot require world membership — the host is
  the authority for what it discloses; the model owns recording it.
- Admission (new world entries with schema entry + spec) has NO production caller
  today — no flow supplies schema entries mid-session. It is the model's expand
  path required by the ticket surface, exercised by tests; ENC-305 builds on it.
- UnknownToolReference is a ToolSemanticsError variant (tool_semantics.rs:311-314);
  create_session already returns ToolSemanticsError, FFI maps it to AgentException
  at ffi/mod.rs:87-89. ToolCatalog::from_semantics (catalog.rs:63-66) runs
  semantics validate() + builds the index. validate_references:
  tool_semantics.rs:148-215 (known = catalog canonical + aliases ∪ schema names).
- Consumers of tool data inside engine: tool_semantics 32 refs / 7 files;
  io.schemas 5 refs; active_tool_names 35 refs crate-wide. Outside-engine pure
  functions take &ToolSemanticsConfig / &ToolCatalog params (canonicalize, compact)
  — they keep doing so, fed from the universe at engine call sites.
- tool_schema_activation.rs is a dead unimplemented!() stub with no callers
  (leave it; not part of this ticket).
- Baseline gates on this branch (enc-299 tip d7acf2c): see status.md.
