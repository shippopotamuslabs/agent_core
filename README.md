# `agent_core` (Rust)

Deterministic agent loop: compaction tiers, tool repair, guardrails, and session accounting.

## Tests

```bash
cd third_party/agent_core
cargo test
```

## Keeping the tool catalog in sync (AGENT-038 / AGENT-050)

- **Name parity:** `./scripts/check-tool-catalog-sync.sh` — every `name = "..."` in Kotlin `ToolDefinition` must exist in `resources/tool_catalog.json`.
- **Schema hint parity:** `python3 scripts/check-catalog-schema-parity.py` — for each `schema_hint.required_fields` entry in the JSON catalog, the matching Kotlin `ToolParameters` data class must declare a property with the same name.

Regenerate Android bindings after changing the FFI surface:

```bash
./scripts/build-agent-bindings.sh
```

## Versioning (AGENT-042)

Bump `version` in `third_party/agent_core/Cargo.toml` when the serialized event schema or UniFFI surface changes in a breaking way; regenerate Kotlin and commit `agent_core.kt` + `libagent_core.so` together.

Release process:

1. Tag Rust releases as `agent_core-vMAJOR.MINOR.PATCH`.
2. Align Pocket Crew release notes with the exact `agent_core` tag or submodule revision.
3. Run `./scripts/build-agent-bindings.sh` after each Rust tag bump.
4. Commit generated Kotlin bindings and Android native libraries in the same change as the Rust bump.
5. Run Android QA against the generated bindings before merging the release change.

Semver rules:

- Major: breaking native trait, UniFFI, serialized event, or fixture schema changes.
- Minor: backwards-compatible event additions, new compaction tiers, or new optional capabilities.
- Patch: bug fixes with no host-facing contract changes.

## Desktop integration (AGENT-041)

Future desktop consumers can depend on this crate directly:

```toml
[dependencies]
agent_core = { path = "../pocket-crew/third_party/agent_core" }
```

Desktop hosts use native Rust traits from `agent_core::host`:

- `AgentTransport` for model turns
- `AgentToolExecutor` for repaired tool calls
- `AgentSummarizer` for summary-tier compaction

UniFFI is only required by Android. Native Rust hosts should not go through the Kotlin callback bridge. The smoke example
in `examples/mock_desktop_loop.rs` exercises a three-turn tool loop through the native trait surface:

```bash
cd third_party/agent_core
cargo run --example mock_desktop_loop
```
