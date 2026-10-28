# Plan: ENC-299 — ToolName newtype + referential validation (stacked on ENC-297/#30)

## Decisions
- D1 New `ToolName(String)` (src/tool_name.rs or catalog.rs — follow codebase grain): `ToolName::new(&str) -> Result<_, ToolNameError-ish in ToolSemanticsError/ToolSchemaError family>`; canonical form = trim + lowercase + replace('-',' ' -> '_'); blank rejected. as_str/Display = canonical. Eq/Hash/Ord on canonical. Serde transparent string via new() (blank -> serde error). KEEP one crate-internal unchecked path ONLY where legacy leniency is contract (SchemaEntry From<Value> "" sentinel): `ToolName::from_raw_lenient` documented, still canonicalizes; strict parse (validate_entries) rejects empty as today. No other unchecked construction.
- D2 Retype: ToolSpec.name: ToolName, aliases: Vec<ToolName>; all semantics tool_name fields + companion_callable_tools: Vec<ToolName>; SchemaEntry.name: ToolName (Value passthrough untouched — byte identity); ToolCatalog index HashMap<ToolName, usize>; initial_active_names/active_tool_names and render_active_schemas key on ToolName (String at serde/FFI edges only). FFI records keep String fields; conversions build ToolName (blank/dup -> AgentException as today).
- D3 Delete per-site (b)-fold normalization at lookup sites: catalog::normalize + tool_semantics::normalize_tool_name die (fold lives in ToolName::new); schema_hint_for_semantics/result_policy_for take &ToolName, compare equality; semantic_rule/canonical_shapes compares become ToolName equality (inputs already canonicalized by repair pipeline). KEEP canonicalize (a) snake pipeline for raw model output (repair), producing ToolName after; KEEP fuzzy matcher case-fold (scoring, not identity) — note in code why it is not normalization-at-lookup.
- D4 Referential validation: new ToolSemanticsError::UnknownToolReference{section, name}. In engine create_session after prepare_tool_semantics: known = catalog canonical names ∪ catalog aliases (resolve to canonical) ∪ active schema ToolNames. Check: discovery.tool_name, companion_callable_tools[], artifact start/append, argument_repairs[], canonical_shapes[], result_policies[], compaction.artifact_squash.append, greedy_extension_tools[]. Only when the containing section is Some/non-empty (agnostic passes). Expose as testable fn (e.g. ToolSemanticsConfig::validate_references(catalog, schema_names)). FFI: propagates via existing create_session->AgentException path (verify + test).
- D5 Close alias-vs-canonical collision in validate() (AliasCollision). First-alias-wins index behavior only reachable post-validation; keep.
- D6 No ENC-304 ToolUniverse type; no bare-string restore; From<FfiModelRequest> untouched.

## Acceptance scenarios (tests)
- Typo in argument_repairs tool_name (not in specs/schemas) -> create_session Err UnknownToolReference naming section+name (FRG-36 regression).
- Rule referencing declared alias resolves + session constructs; alias shadows another canonical -> AliasCollision.
- Enclave fixture set (enclave_tool_semantics.json + resources/tool_catalog.json overlay) constructs unchanged.
- ToolName unit: " Web-Search " -> "web_search"; blank rejected; serde round-trip; duplicate detection unchanged.
- Grep: normalize_tool_name/catalog::normalize gone from lookup sites.

## Risks
Tests with semantics referencing tools in neither specs nor schemas will now fail construction: fix fixtures by adding the ToolSpec/schema (explain), never by weakening validation. Byte-identity: schema names host-supplied non-snake could fold in initial_active_names — check existing byte-identity tests still pin renders (Value passthrough should keep them green); report any render delta explicitly.
Commits per chunk (1: newtype+retype, 2: validation) referencing ENC-299. Gates both configs + fmt/clippy. No push/PR/Linear.
