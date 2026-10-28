# Plan: ENC-296 — host owns the tool registry

## Goal
Engine keeps only active/disclosed names + consumed per-tool fields. No registry storage/render/stash remains.

## Decisions (must not drift)
- D1 Registry channel deleted everywhere (ToolSchemaSet, SessionIoParams, FFI). Active+catalog keep ENC-289 parse-once validation; bare-string rejection stays.
- D2 Newly activated tool: render from active channel if present, else existing name-stub shaping. No registry lookup. Host wires full schema provider-side.
- D3 ContextReferenceKind::ToolSchema + ToolCatalogIndex deleted; no registry text stashed in ContextStore. Engine-created kinds untouched (ENC-298).
- D4 ModelRequest carries active set only (existing parsed-entry carriage; ToolName-ref rewrite is ENC-305, do not build).
- D5 FfiToolSchemaEntry loses parameters_json; entry = name, description, reproducible, deferrable, max_result_bytes. Reconstruction drops parameters payload; keep typed AgentException on malformed input, no silent fallback. From<FfiModelRequest> fallback untouched.
- D6 No ENC-304 ToolUniverse/expand, no ENC-305. No bare-string restore.

## Chunk 1 (single implementer, serialized build lane)
Files (expected): src/tool_schema.rs, src/session.rs, src/ffi/types.rs, src/ffi/mod.rs, src/lib.rs (exports if needed),
src/context_store.rs, src/tool_context.rs, src/tool_schema_activation.rs, src/engine/{mod,state,model_request}.rs,
src/sidecar_builder.rs, src/request_pipeline.rs ONLY if its public string API references registry (keep FRG-36 gates; prefer untouched),
tests: tool_context_test, lazy_tool_disclosure_test, tool_search_discovery_test, finish_boundary_test, context_store_test, forgecode_gap_red_test as needed.
Acceptance scenarios:
- Given a session built without any registry input, when constructed via core + FFI paths, then it succeeds and active/catalog validation still rejects malformed/duplicate/bare-string entries with typed errors.
- Given a tool activated mid-session that is in the active channel, render includes its active entry; given one not in active, render includes the name stub (existing stub shape) and nothing reads a registry.
- Given tool-context index build, then no ToolSchema/ToolCatalogIndex reference is minted and ContextStore holds no registry text; read_context_reference for engine-created kinds unchanged.
- Given FfiToolSchemaEntry without parameters_json, FFI session creation works; malformed entries still yield AgentException naming the entry.
- Grep: no registry channel symbols remain in src/ (tool_schema_registry, registry_json(), has_registry, ToolCatalogIndex, ContextReferenceKind::ToolSchema, parameters_json on FfiToolSchemaEntry) outside comments/tests documenting removal. NOTE: unrelated "registry" words (e.g. other domains) — judge by channel semantics, report any survivor with justification.
Tests: update/re-pin affected tests to the new contract (explain each changed expectation; never weaken an assertion to pass).
Add/keep one focused regression test pinning stub-render for activated-but-not-active tool.
Run: cargo fmt --check; cargo clippy --all-targets --locked -- -D warnings; cargo test --locked; cargo test --locked --no-default-features. Record exact pass counts.
Commit: one commit referencing ENC-296 (do not push, do not open PR — orchestrator ships after review).

## Out of scope
ENC-297 typed semantics, ENC-299 ToolName, ENC-300 format port, ENC-304/305, Enclave/Forge repos, bindings regen (ENC-294).
