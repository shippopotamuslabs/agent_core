# Status: ENC-297

## Phase Checklist
- [x] Research — vision approval: approved (mission; Sean-ratified design on ticket)
- [x] Plan
- [ ] Implementation
- [ ] Review — rounds: 0/3, result: pending
- [ ] Ship — PR: none (stacked on #29)

## Current Gate
none

## Next Phase
Implementation

## Notes
- Base: ENC-296 branch tip (PR #29). Worktree /home/sean/Code/agent_core-ENC-297.

- ENC-296 PR #29 CI green (all 6 checks pass, polled 02:25Z).

- Implementation done (both chunks, sequential; one commit per chunk).
  - Chunk 1 (8942a08): core split (D4–D6). ToolSchemaSet catalog channel
    deleted; SessionIoParams::new 4-arg; AgentConfig holds tool_semantics +
    tool_catalog via fallible with_tool_semantics constructor (prepare_tool_semantics
    validate-only re-sync at session creation); merge_catalog_json +
    load_semantics_with_session_catalog deleted; guards re-source to
    ToolCatalog presence; rendered_tool_catalog renamed rendered_runtime_tools.
  - Chunk 2 (0be1111): FFI typed tree (D1–D3). New src/ffi/semantics.rs
    (FfiToolSemantics + 13 sibling record/enum types; D3 Option<String> payloads
    parsed once at conversion); FfiAgentConfig reshaped (tool_semantics:
    Option + tool_specs sibling; both JSON strings deleted); infallible From
    replaced by fallible agent_config (session_io_params pattern); FfiSessionIoParams
    lost tool_catalog_json; policy_bridge helpers take typed params; exports updated.
  - Gates: cargo fmt --check clean; clippy --all-targets --locked -D warnings
    clean; cargo test --locked: 34 binaries, 564 passed / 0 failed;
    cargo test --locked --no-default-features: 32 binaries, 540 passed / 0 failed.
    lalr-harness feature tests compile (cargo check --features lalr-harness --tests).
  - Grep evidence: no tool_semantics_json|tool_catalog_json|merge_catalog_json
    code refs left in src/ffi (only doc comments naming the deletion); no
    eprintln anywhere in src; no catalog_json()/has_catalog()/catalog_has_entries()
    refs left in core.
  - Plan amendments: (1) FfiToolSemantics carries no catalog field — the D1
    sibling Vec<FfiToolSpec> on FfiAgentConfig IS the catalog (assigned into
    semantics.catalog.tools at conversion; avoids duplicating the tool list in
    two FFI places). (2) Struct-literal AgentConfig construction still compiles
    (tests); session creation re-syncs the catalog via prepare_tool_semantics,
    so the ONE constructor is the blessed fallible surface (FFI + new tests),
    literals self-heal. (3) tool_context presence guard preserves exact legacy
    semantics (catalog-empty + active-tools still indexes/de-fers); model_request
    gate re-sources to typed presence — pinned delta: legacy '{}' object-form
    channel text no longer counts (new tests enc_297_empty/nonempty_typed_*).
  - Surprises: none architectural; one mechanical note — python-assisted line
    deletion for the 41 SessionIoParams::new call sites (preview-verified per
    site, then full-suite green).
