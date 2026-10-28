# Plan: ENC-297 — typed tool semantics + catalog at the UniFFI boundary (stacked on ENC-296/#29)

## Goal
Semantics + ToolSpec catalog cross UniFFI as typed records; no semantics/catalog JSON strings on the FFI surface;
no silent fallback; catalog conflation split (schema-render catalog vs policy ToolCatalog vs semantics.catalog).

## Decisions
- D1 FfiToolSemantics record tree mirrors ToolSemanticsConfig EXACTLY (read src/tool_semantics.rs; every struct/enum/field;
  SpecialValueRepair -> UniFFI enum preserving payloads; nested Vec<Record> allowed). Vec<FfiToolSpec>{name,aliases,reproducible,schema_hint}
  is a SIBLING field on FfiAgentConfig (not inside FfiToolSemantics). Conversions are total: every core field populated or
  explicitly defaulted per core Default/serde defaults — enumerate any field that cannot cross and STOP/report.
- D2 FfiAgentConfig.tool_semantics_json deleted. Infallible From<FfiAgentConfig> replaced by fallible conversion
  (pattern: session_io_params) returning AgentException on invalid semantics/catalog (run core validate()); eprintln+agnostic fallback DELETED.
  Agnostic remains available only as explicit core default when host supplies no semantics record (Option<> semantics field: None=agnostic is
  host-declared, not a parse fallback — document; if existing callers always supply, keep non-optional and say so).
- D3 empty_append_payload_value: FFI carries Option<String> JSON payload, parsed ONCE at conversion; malformed -> AgentException naming the field.
  (Recorded on ticket by orchestrator.)
- D4 Core split: ToolSchemaSet catalog channel DELETED (parse/from_entries lose catalog arg; SessionIoParams::new loses catalog_json).
  AgentConfig holds ToolSemanticsConfig + ToolCatalog as separate values, built by ONE fallible constructor from typed/core values
  (the construction surface ENC-299/304 build on; do NOT build ToolUniverse type/expand/key-lookups).
  merge_catalog_json + load_semantics_with_session_catalog deleted; prepare_tool_semantics does no merge (validate only).
  Core (Forge) callers previously passing catalog JSON now pass ToolCatalog/ToolSpec values at config construction.
- D5 Guards re-sourced to ToolCatalog presence (non-empty tools): model_request has_catalog gate + tool_context presence guard.
  Preserve gate semantics exactly for equivalent inputs (catalog present iff host supplied ToolSpecs or, on legacy core path, catalog entries).
  CAUTION: semantics-file object form ('{}' catalog) previously counted has_catalog via raw text; pin new behavior in tests and call out any delta.
- D6 Rename ModelRequest.rendered_tool_catalog -> rendered_runtime_tools; rendered_tool_catalog_json() -> rendered_runtime_tools_json().
  (It renders the active channel through runtime gates.) Update all constructors/tests/FFI conversions (FfiModelRequest has no field; keep drop behavior + From fallback untouched per ENC-305).
- D7 No bare-string restore; no ENC-299 ToolName/referential validation yet; no ENC-304/305.

## Chunks (sequential, one lane)
Chunk 1 core: catalog split (D4,D5), rename (D6), delete merge/load helpers, migrate in-repo callers+fixtures+tests.
Chunk 2 FFI: typed tree (D1), fallible config conversion (D2,D3), delete both JSON string fields, exports, FFI tests
  (round-trip: typed records -> core values equal to legacy JSON fixture parse; malformed Value payload + invalid spec -> AgentException).
Acceptance: grep-clean tool_semantics_json|tool_catalog_json|merge_catalog_json on FFI surface (src/ffi) and no eprintln fallback;
real semantics fixture (find it) constructs unchanged via typed path (test); suite green both configs; fmt/clippy clean.
Commits: per chunk referencing ENC-297. No push/PR/Linear.

## Test strategy
Existing suite + new: FFI conversion round-trip vs JSON fixture; typed-error cases; guard-presence cases (catalog empty/non-empty);
rename covered by existing request tests. Never weaken assertions; explain re-pins.

## Out of scope
ENC-299 validation, ENC-300 format, ENC-304/305, host repos/bindings (ENC-294).
