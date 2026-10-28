# ENC-297 Findings — dual review of c632dc8..HEAD (8942a08, 0be1111)
Reviewers: Synthitect + Thermos correctness + Thermos quality. Orchestrator gates: fmt/clippy clean, 565/0 + 540/0.
Field-parity audit: complete both directions (round-trip byte-equal vs legacy fixture).

## Critical
None.

## Major
- J1 (all 3 lenses) src/engine/mod.rs:138 `let _ = config.prepare_tool_semantics();` — invalid struct-literal/serde
  AgentConfig enters create_session, validation error discarded, session runs with stale/empty catalog (wrong gates).
  The silent-fallback pattern ENC-297 exists to delete. Fix: make create_session fallible (typed error), FFI maps to AgentException.

## Minor
- M1 Conversion duplication: agent_config (ffi/types.rs) vs load_policy_semantics (ffi/policy_bridge.rs) hand-roll same 3 steps.
- M2 Lossy From<&ToolSemanticsConfig> drops catalog silently (ffi/semantics.rs:487); rename to explicit without_catalog.
- M3 From<&ArtifactToolSemantics> emits Some("null") vs into_core None->Null asymmetry.
- M4 snippet_max_chars `as` casts truncate on 32-bit; use saturating/try_from.
- M5 agent_config inherits Default scalars implicitly via with_tool_semantics(..Default) overwrite pattern.

## Suggestions
- MIRROR exhaustiveness guard for ffi/semantics.rs; has_local_tool_contract invalid->false (documented, infallible signature);
  AgentException lacks InvalidInput variant (future); tool_context wrapper doc touch-up.

## Thermos-specific
- Exclusions verified: no bare-string restore, From<FfiModelRequest> untouched (ENC-305), no ToolUniverse/ENC-299.

## Disposition
J1 + M1-M5: fix pass before ship.

## Fix pass (31be349)
J1 + M1-M5 fixed. Gates: fmt/clippy clean; 567/0 default, 541/0 no-default (worker + orchestrator recount default). Open Critical/Major: 0.
