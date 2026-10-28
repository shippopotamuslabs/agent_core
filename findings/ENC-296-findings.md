# ENC-296 Findings — dual review of 3c59354 (fc89283..HEAD)

Reviewers: Synthitect fresh-context + Thermos correctness + Thermos quality (3 independent passes).
Orchestrator gates re-run: fmt clean, clippy -D warnings clean, cargo test --locked 556/0, --no-default-features 538/0.

## Critical
None.

## Major
None. (Verified: D1-D6 fidelity; empty active_tool_names equivalence via initial_active_names seed;
no silent-fallback regressions; bare-string pin intact; From<FfiModelRequest> untouched per ENC-305 disposition.)

## Minor
- M1 Dead store params: build_tool_context_index{,_from_set} take &mut ContextStore they ignore (tool_context.rs:38,69).
- M2 tool_schema_entry_value returns Result but is infallible after parameters_json removal (ffi/types.rs:785).
- M3 Tool-index spill mints ctxref-ledger-N via ArtifactSnapshot prefix table (sidecar_builder.rs:160) — prefix lies.
- M4 Stale test name ..._falls_back_to_full_schema_json (tests/tool_context_test.rs:138).
- M5 Browsable-kind test hardcodes variant list; future variant would not fail it (context_store.rs:331).

## Suggestions
- build_tool_context_index string wrapper + parse_lenient retained as preserved public boundary (PR #24); unused in-tree.
- Spilled tool index is now browsable (advertisement-only payload, no schema text) — accepted; documented in code.
- Deleted pub fns build_tool_context_index_with_registry/read_full_schema_registry: consumers fail loud at compile; ENC-294 covers host adaptation.

## Thermos-specific
- Registry-survivor grep clean in src/ outside removal comments; HookRegistry unrelated, kept.
- FFI breaks (registry field, parameters_json, 2 enum variants) intended per ticket; loud compile failures downstream.

## Disposition
M1-M5: fix pass before ship (orchestrator call; none blocks on severity alone).

## Fix pass (e4156c7)
M1-M5 fixed; gates re-run by worker: fmt/clippy clean, 556/0 + 538/0. Open Critical/Major: 0.
