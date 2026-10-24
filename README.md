# agent_core (Rust)

The agent loop at the center of Enclave, as a standalone Rust crate. It runs
the deterministic parts of a tool-using agent: the turn loop, context
compaction in tiers, tool call parsing and repair, guardrail checks on tool
use, and per-session accounting of tokens, turns, and tool calls.

Hosts drive it two ways:

- **Native Rust hosts** use the traits in `agent_core::host`:
  `AgentTransport` for model turns, `AgentToolExecutor` for repaired tool
  calls, `AgentSummarizer` for summary-tier compaction. The smoke example in
  `examples/mock_desktop_loop.rs` runs a three-turn tool loop through that
  surface:

  ```bash
  cargo run --example mock_desktop_loop
  ```

- **Android (Kotlin) hosts** go through the UniFFI surface in `src/ffi/`.
  UniFFI is only for the Android bridge. Native Rust hosts should not route
  through the Kotlin callback bridge.

## Tests

```bash
cargo test
```

A CI config left over from before the move to GitHub ran
`cargo fmt --check`, clippy with warnings denied, and `cargo test
--all-features`. That config is a record of the old pipeline; no GitHub Actions
workflow replaces it yet.

## The tool catalog lives with the consumer

`resources/tool_catalog.json` is the catalog snapshot this crate checks
against. The parity checks from the crate's time inside the Enclave repo
(name parity and schema-hint parity against the Kotlin `ToolDefinition` /
`ToolParameters` code, and the binding-regeneration script) ran from the
parent repo and are not in this repo. The Kotlin side of the contract lives
in Enclave. Any catalog change here has a matching change there, made by
hand until that tooling moves.

## Versioning

Current version: 0.3.0 (`Cargo.toml`). Bump `version` when the serialized
event schema or the UniFFI surface changes in a breaking way.

Semver rules:

- Major: breaking native trait, UniFFI, serialized event, or fixture schema
  changes.
- Minor: backwards-compatible event additions, new compaction tiers, new
  optional capabilities.
- Patch: bug fixes with no host-facing contract changes.

Release process, from the Enclave side:

1. Tag Rust releases as `agent_core-vMAJOR.MINOR.PATCH`.
2. Align the consuming app's release notes with the exact `agent_core` tag
   or revision.
3. Regenerate the Kotlin bindings after each tag bump and commit the
   generated bindings and Android native libraries in the same change as the
   Rust bump.
4. Run Android QA against the generated bindings before merging the release
   change.

## Layout

| Path | Contents |
| --- | --- |
| `src/engine/`, `src/compact/`, `src/repair/`, `src/canonicalize/` | The loop, the compaction tiers, tool-call repair, and message canonicalization |
| `src/host.rs` | The native host trait surface |
| `src/ffi/` | The UniFFI bridge for Kotlin |
| `src/events.rs`, `src/policy.rs`, `src/guardrails.rs`, `src/budget.rs` | The event schema, policy, guardrails, and session accounting |
| `tests/leak_repro_manual_test.rs` | The leaked-tool-call recovery harness, kept as a source test |
