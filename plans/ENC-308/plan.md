# Plan: ENC-308 — final agent_core refactor close-out

## Goal

Land the last 14 items standing between agent_core and "the refactor is
done": the 7 deferred Forge maintainability Mediums from PRs #37/#38, the
finish of the FFI typing program (typed session context, typed state
ledger, strict metadata at session creation, fallible tool-result
conversion, extras bags deleted per Sean's ruling), and the docs
close-out. Done means: both test configs green, Forge clean on the PR,
and after merge every ENC-284 child is complete — the next phase
(ENC-294 re-vendor, greenfield consumer) starts from a closed boundary.

## Architecture decisions

- **D1 — Closed boundary.** After this ticket the FFI records are closed
  schemas: a key/field the engine does not define is a typed
  `AgentException` at ingress, naming the offender. No silent drops, no
  round-tripped host data. (Sean's ruling, 2026-10-28.)
- **D2 — Two gates, deliberately different.** FFI *ingress* is strict
  (D1). *Stored-state reads* (snapshot import, persisted history) are
  tolerant: rows written by older builds must load, with retired host
  extras dropped at the read seam and every engine-owned value preserved.
  Strictness at the live boundary, tolerance at the archive — never the
  reverse. (Stored-state invariant audit discipline.)
- **D3 — Snapshot format compatibility is byte-level.** The ToolSchemaSet
  serde swap (item 4) must produce byte-identical output to the current
  Wire impl. Proof is golden fixtures generated from the CURRENT code
  before the swap, not the round-trip tests alone. If byte identity
  cannot hold, stop and escalate — do not bump-and-pray.
- **D4 — Ratified JSON stays JSON.** Snapshot blob (D7), value_json
  defaults, arguments/result payloads: untouched except where a chunk
  names them. Do not re-litigate.
- **D5 — Host-visible deltas are enumerated, not absorbed here.** Every
  FFI signature change is listed in the PR description for ENC-294. No
  Enclave-repo changes in this ticket.

## Chunks (serial — shared files and one cargo lane; run in order)

### A1 — Compaction encapsulation trio (items 1–3)
Scope: src/engine/compaction.rs, src/engine/mod.rs.
Remove the redundant `trigger` param on `attach_summary_staging`;
encapsulate the suppress-summary toggle on CompactionState (callers stop
flipping the flag by hand in `retry_without_summary`); simplify
CompactionPassState's history-ref accessors + immutable count handling.
Acceptance:
- Given the existing compaction test suite, when run on both feature
  configs, then all tests pass unmodified (behavior-preserving).
- Given a grep for manual suppress-flag set/reset outside CompactionState,
  then there are no hits.

### A2 — Snapshot serde + store decoupling (items 4–5)
Scope: src/tool_schema.rs, src/context_store.rs (+ tests).
FIRST, before touching the impl: generate golden fixtures from the
current Wire serde (serialized ToolSchemaSet bytes + a full snapshot
containing one) and commit them as tests that pass against the OLD code.
Then collapse Wire into derives; the goldens must still pass.
Decouple ContextStore::validate_restored from the minted-ID grammar and
the tool-semantics error family behind a validator seam the engine
supplies; restore keeps rejecting malformed key sets with a typed error,
and the store stops importing tool-semantics types.
Acceptance:
- Given the pre-change golden fixtures, when the derive impl serializes
  the same values, then bytes are identical; deserialization of
  pre-change snapshots succeeds with equal state.
- Given a restored store with a malformed key set, when validate_restored
  runs, then a typed error results (existing tamper/validation tests
  pass, updated only where the error type moved).
- Given a grep of context_store.rs, then no `ctxref-` literal and no
  ToolSemanticsError reference remain.

### A3 — Dedup consolidation + import validators (items 6–7)
Scope: src/ffi/types.rs, src/tool_universe.rs (visibility only if
needed), src/engine/mod.rs.
Route ffi/types.rs's two HashSet dedups and the outbound Vec::contains
dedup through the canonical first-seen helper (`merge_tool_names_deduped`
or a shared sibling it delegates to). Extract import_snapshot's four
gates (version, (phase, pending) coherence, store validation, universe
restore) into named pure validators the orchestrator calls in the same
order; the coherence rule gets direct unit tests (no JSON tampering
needed to reach it).
Acceptance:
- Given the enc305 behavioral round-trip tests, then they pass
  unmodified.
- Given the coherence validator, when fed mismatched (phase, pending)
  pairs directly, then each mismatch is a typed rejection — covered by
  new unit tests.
- Tamper tests still pass.

### B1 — Session context closed + typed; strict creation (items 8, 10, 12-context)
Scope: src/ffi/mod.rs, src/ffi/types.rs, src/session.rs.
Add `FfiSessionContext` mirroring SessionContext's typed fields;
create_session takes it instead of `session_context_json: String`.
Delete `SessionContext.extras`; unknown keys in any context payload are
an AgentException naming the key. Route create_session's initial
messages through `agent_message_strict` so creation-time metadata is
validated exactly like per-turn metadata.
Stored-state (D2): snapshot import tolerates legacy context JSON that
carries retired extras keys — they drop at the read seam; engine-owned
fields all survive.
Acceptance:
- Given create_session with a context carrying an unknown key, then
  AgentException naming the key; with a malformed typed field, then
  AgentException (never a Null-context session).
- Given initial messages with malformed metadata_json, then
  AgentException, identical in kind to the per-turn path.
- Given a pre-change snapshot fixture whose context has extras keys,
  when imported, then the session loads with typed fields intact.

### B2 — Typed ledger, fallible tool results, metadata bag deleted (items 9, 11, 12-message)
Scope: src/ffi/types.rs, src/message.rs, summary/sidecar call sites.
Introduce `FfiStateLedger` mirroring StateLedger; FfiSummaryRequest
carries it instead of `state_ledger_snapshot_json` in both directions;
both `.ok()` calls die. Convert the tool-result submit path to a
fallible conversion: a non-blank invalid activated tool name is an
AgentException; blank entries keep the documented drop. Delete
`MessageMeta.extra`; strict ingress rejects unknown metadata keys
(typed error); stored-message reads tolerate-and-drop legacy extra keys
at the read seam (D2).
Risk gate: if any StateLedger field cannot cross UniFFI (non-string map
keys, nested generics), STOP and escalate with the field named — do not
improvise a hybrid JSON field.
Acceptance:
- Given a summary request whose ledger is malformed at the old JSON
  layer, then the scenario is unrepresentable — the typed record's
  construction is the only path; round-trip tests cover ledger fidelity
  incl. retirement_artifacts.
- Given tool results with a non-blank invalid activated name, then
  AgentException; with blank entries, then accepted with blanks dropped.
- Given a stored message with legacy extra metadata keys, when read,
  then it loads with typed fields intact and extras gone.
- Given FFI metadata with an unknown key, then AgentException naming it.

### C — Docs + final gates (items 13–14)
Scope: README.md, docs/, crate-level doc comments that still describe
pre-refactor surfaces; plans/ENC-308/status.md.
Refresh the docs to the merged shape (validation model: provider schema
vs engine-enforced hint; the closed boundary; the ratified JSON list).
The agent-core-architecture artifact refresh happens at Ship from the
merged tree (Orchestrator task, noted in status.md).
Acceptance (= ticket definition of done):
- cargo test --locked AND cargo test --locked --no-default-features:
  0 failed. cargo fmt --check clean; clippy -D warnings clean.
- Forge review on the PR: no unresolved findings (fix or disposition
  per the established pattern).

## Test strategy

Tests are written with the code, per chunk, from the acceptance
scenarios. Guard suites: enc305 snapshot tests, compaction tests,
tamper tests, tool_parse tests. Golden fixtures for A2 are generated
from pre-change code and committed BEFORE the change they guard
(stored-state audit: prove they pass on the old impl, then on the new).
Legacy-row fixtures for B1/B2 are raw JSON written by hand to the
pre-change shape, driven through the real import/read paths.

## Risks and unknowns

- Byte drift in A2 → D3 gate: goldens first; escalate on any drift.
- StateLedger UniFFI-representability → B2 risk gate: escalate, don't
  improvise.
- Internal engine code constructing metadata/context with keys that
  become illegal → fix the construction to typed fields; never weaken
  the ingress check to make a test pass. If an internal construction
  reveals a genuinely missing typed field, that is an architecture
  question: stop and escalate.
- Volume: one PR for the ticket (repo convention from the stack). If
  chunks A and B together make the PR unreviewable, the Orchestrator
  may split B into a stacked PR and say so at Ship.

## Out of scope

Enclave repo (ENC-294 absorbs every host-visible delta), ENC-216,
semantics.rs saturating conversions, snapshot blob typing (D7),
value_json typing, any behavior change not named in the 14 items.
Chunk mirroring as Linear sub-issues: skipped — this repo's convention
is plans/ in-repo (plans/ENC-296..306 precedent); plan.md is the record.
