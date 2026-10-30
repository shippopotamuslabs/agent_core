# AGENTS.md — agent_core contributor notes

Agents working in this repo start here. README.md describes the crate
for human evaluators; this file is the governing agent contract.

## What this crate is

A generic tool-using-agent engine. It owns the deterministic parts of
the loop — turn state machine, tiered context compaction, tool-call
parsing and repair, guardrails, per-session accounting — and nothing
else. Tools are host-owned: the catalog and semantics are consumer
data passed in per session, never baked into the engine. The FFI
layer is a parallel mirror (`Ffi*` types plus `From`/`Into`
conversions in `src/ffi/`); the engine API never takes a UniFFI type.

## The native surface (pull-based)

Hosts drive `AgentEngine` (`src/engine/mod.rs`) by polling — there
are no host callbacks, and no port traits to implement:

1. `AgentEngine::new()` — non-generic since ENC-285 (the removal
   rationale is documented once on `AgentEngine` at
   `src/engine/mod.rs:107-122`).
2. `create_session(...)` once per review/task.
3. `next_action()` → answer with `submit_model_response`,
   `submit_model_turn_failure`, `submit_tool_results`, or
   `submit_summary`, per the returned `AgentAction`
   (`src/action.rs:16`): `CallModel`, `ExecuteTools`,
   `RequestSummary` (only when the session allows summarization),
   `Finished`, `Failed`, `ManualCompactCompleted`. Tools are
   host-executed, never engine callbacks.

The former `src/host.rs` port traits (`AgentTransport` /
`AgentToolExecutor` / `AgentSummarizer`) and every `*Callback`
shim are gone with ENC-285 — no `Callback` symbol remains under
`src/`, and no `NoOpSummarizer` type exists. Do not reintroduce
them: propose a polling-surface extension instead.

## The FFI placement rule

`uniffi::setup_scaffolding!()` lives at the crate root in
`src/lib.rs:1-2`, gated in place behind `#[cfg(feature = "ffi")]`
together with `pub mod ffi` (`src/lib.rs:5`) and the `Ffi*`
re-export block (`src/lib.rs:8-26`). It must never be relocated
into a submodule — root placement is the UniFFI scaffolding
convention. The `ffi` gate is `default = ["ffi"]`,
`ffi = ["dep:uniffi"]` (`Cargo.toml:23-26`). The `src/ffi/`
bridge (`mod.rs`, `types.rs`, `semantics.rs`,
`policy_bridge.rs`, `stream_gate.rs`) carries `Ffi*` mirrors plus
free functions and `#[uniffi::export]` impl blocks, with explicitly
no host callback parameters (see `src/ffi/mod.rs:1-4`).

## Consumers

- **Enclave** consumes via the UniFFI/Kotlin bindings (default `ffi`
  build; bindings regenerate on API change — see the release process
  in `README.md` — and the change is verified against them).
- **Chomp** (the `chomp` binary in forge-reviewer's sibling `agent/`
  crate) consumes the native surface with `--no-default-features`.

A change is not done until its effect on BOTH consumers is stated —
run the consumer you did not change, or record why it is unaffected.

## Gospels

The cross-repo canon lives in the `shippopotamuslabs/gospels` repo,
pinned in this repo as a submodule at `contracts/gospel/`. Read the
gospels there; bump the pin deliberately, and never edit the
submodule's contents from this repo.

- Architecture follows
  `contracts/gospel/RUST-ARCHITECTURE-GOSPEL.md` — the dependency
  rule in Rust form: crates as boundaries, traits only at genuine
  ports, checked facts as types, typed errors. (Its sibling,
  `ARCHITECTURE-GOSPEL.md`, is the Android/Kotlin statement of the
  same intent; it does not govern this repo.)
- Tests follow `contracts/gospel/TESTING-GOSPEL.md`: the general
  sections apply in full; its Android-specific §6 does not apply
  here.
- Anything a person reads — docs, comments, plan write-ups —
  follows `contracts/gospel/WUMBO-GOSPEL.md`.
- The UI gospels (Palette, Compose, Glass Bars, Button Shape) do
  not apply to this repo; it has no UI surface.

## Tests

CI runs the following, and a change should pass them locally
before pushing:

- `cargo fmt --check`
- `cargo clippy --all-targets --locked -- -D warnings`, and the
  same with `--no-default-features`
- `cargo test --locked`
- `cargo test --locked --no-default-features`
- `cargo test --locked --features lalr-harness`
- `bash scripts/check-vendored-freshness.sh --self-test`

## Conventions

- No code changes ride with docs-only tickets, and no docs-only
  ticket edits `src/`.
- PR titles follow the org convention — `type(scope): imperative
  subject (KEY-123)` — and branch names follow Linear's suggested
  branch for the ticket key.
- Any exported-API change (UniFFI surface, `Ffi*` types, event
  schema) carries a bindings check: regenerate the Kotlin bindings
  per the release process in `README.md` before and after the
  change and diff; byte-identical unless the PR intends the API
  change. The Enclave-side QA and re-vendor run on the Enclave
  side (see `docs/vendoring.md`), never from this repo.
