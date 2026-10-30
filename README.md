# agent_core (Rust)

> Agents working in this repo: start with [AGENTS.md](AGENTS.md) —
> it is the governing agent contract. This README describes the
> crate for human evaluators.

The agent loop at the center of Enclave, as a standalone Rust crate. It runs
the deterministic parts of a tool-using agent: the turn loop, context
compaction in tiers, tool call parsing and repair, guardrail checks on tool
use, and per-session accounting of tokens, turns, and tool calls.

Hosts drive it two ways:

- **Native Rust hosts** drive the polling surface directly
  (`next_action` / `submit_model_response` / `submit_tool_results` /
  `submit_summary`): `AgentEngine::new()` takes no host callbacks.
  (ENC-285 removed the former `agent_core::host` port traits and the
  UniFFI callback shims — they were stored but never invoked.)
  The smoke example in `examples/mock_desktop_loop.rs` runs a three-turn
  tool loop through that surface:

  ```bash
  cargo run --example mock_desktop_loop
  ```

- **Android (Kotlin) hosts** go through the UniFFI surface in `src/ffi/`.
  UniFFI is only for the Android bridge. Native Rust hosts should not route
  through the Kotlin callback bridge.

## The UniFFI boundary is closed and typed (ENC-308)

Everything a Kotlin host exchanges with the engine crosses as typed UniFFI
records — no JSON-string parameters remain on the surface:

- `create_session` takes an `FfiSessionContext` record (the former
  `session_context_json` string and its lenient parse are gone) and converts
  initial messages strictly: malformed metadata is an `AgentException`,
  never a silently defaulted session.
- Summary requests carry the state ledger as an `FfiStateLedger` record
  (the former `state_ledger_snapshot_json` string and its silent `.ok()`
  conversions are gone).
- `submit_tool_results` is fallible: the batch converts before the engine
  sees it, and a non-blank invalid activated tool name is an
  `AgentException` (blank entries are dropped, as documented on the
  conversion).
- Message metadata is a closed schema (`MessageMeta`): the keys the engine
  understands are typed fields. At FFI ingress an unknown key is a typed
  error naming the key; in stored state (snapshots, persisted history) an
  unknown key is dropped at the read seam and every engine-owned value
  survives. Host annotations do not round-trip through the engine — hosts
  keep them host-side, keyed by message ID, and reattach them to engine
  emissions themselves. The former `SessionContext` / `MessageMeta` extras
  catch-alls are deleted.

### Validation model

The host's per-tool JSON Schema is the *provider-facing* description: it is
rendered into the model request so the model knows what to emit. The engine
does not validate calls against that document. What the engine enforces at
ingress is the host-declared typed contract — the `ToolSchemaHint`
(required fields, defaults) plus the tool semantics. Model arguments are
parsed exactly once, then checked: required fields present, unknown
parameters rejected, defaults applied, values coerced per the semantics.
Failures surface as typed parse errors the model can repair against.

### What still crosses as JSON, and why

Three crossings remain JSON by design (ratified in ENC-308):

- **The session snapshot** (`export_snapshot` / `import_snapshot` strings):
  the host is a conduit — it stores the blob and hands it back without
  reading it — so a versioned serialized blob with a fully validating
  import is the right contract (ENC-305 D7).
- **Schema-hint default values** (`FfiDefaultValue.value_json`): a
  default's shape is whatever that tool's argument is, so the honest type
  is "a JSON value"; it is parsed exactly once at session setup.
- **Tool arguments and results**: each tool's shape is host-defined, and
  model output is JSON text. The discipline is parse-once plus the ingress
  validation above, not a static type.

## Tests

The suite compiles and passes in a standalone checkout of this repo (no
Enclave tree required):

```bash
cargo test
cargo test --no-default-features
```

Two tests touch the Enclave consumer contract and degrade gracefully outside
the Enclave layout:

- `tsd_26_default_semantics_reference_search_tools` (in
  `tests/tool_search_discovery_test.rs`) reads a vendored snapshot at
  `tests/support/enclave_tool_semantics.json` instead of the Enclave source
  tree, so it runs everywhere. Refresh the snapshot by hand when the Enclave
  production file
  (`core/domain/src/main/resources/agent_loop_enclave_tool_semantics.json`)
  changes.
- `md_02_production_kotlin_sources_do_not_reference_agent` (in
  `tests/tool_budget_agent_meta_test.rs`) scans the Enclave Kotlin sources when
  they are present and skips with a message when they are not. The full
  consumer-contract check still runs inside the Enclave layout.

## CI

`.github/workflows/ci.yml` runs on every pull request and every push to
`main` (required check):

- `cargo fmt --check`
- `cargo clippy --all-targets` with warnings denied, with default features
  and with `--no-default-features`
- `cargo test` with default features (the Android/UniFFI consumer
  configuration; deliberately not `--all-features`, which additionally
  enables the dev-only `lalr-harness` feature whose TDD-Red tests fail by
  design until the long-loop feature work lands)
- `cargo test --no-default-features` (the native/Forge consumer
  configuration), followed by a dependency-tree guard asserting the
  `--no-default-features` tree contains no `uniffi` crates

`.github/workflows/forge.yml` runs the shared Forge Reviewer reusable
workflow on pull requests (opened, synchronize, reopened). It needs the
`OPENROUTER_API_KEY` and `FORGE_APP_PRIVATE_KEY` repo secrets
(`LINEAR_API_KEY` optional); if it fails only on missing secrets, that is
the known-blocked state, not a code defect.

## The tool catalog lives with the consumer

`resources/tool_catalog.json` is the catalog snapshot this crate checks
against. The parity checks from the crate's time inside the Enclave repo
(name parity and schema-hint parity against the Kotlin `ToolDefinition` /
`ToolParameters` code, and the binding-regeneration script) ran from the
parent repo and are not in this repo. The Kotlin side of the contract lives
in Enclave. Any catalog change here has a matching change there, made by
hand until that tooling moves.

## Vendored copy in Enclave

`enclave/third_party/agent_core` is a synced copy (snapshot) of this repo,
not a fork. The vendoring contract — the `.agent_core-source-sha` record
file, the re-vendor procedure, and the freshness check
(`scripts/check-vendored-freshness.sh`) — is documented in
`docs/vendoring.md`. The re-vendor itself runs on the Enclave side
(ENC-294 / Sean), never from this repo.

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
| `src/ffi/` | The UniFFI bridge for Kotlin |
| `src/events.rs`, `src/policy.rs`, `src/guardrails.rs`, `src/budget.rs` | The event schema, policy, guardrails, and session accounting |
| `tests/leak_repro_manual_test.rs` | The leaked-tool-call recovery harness, kept as a source test |

## License

0BSD license. Do whatever you want with the code. Just don't expect it to work or sue me.
