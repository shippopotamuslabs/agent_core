# Status: ENC-308

## Phase Checklist
- [x] Research — vision approval: approved (Sean's design rulings
      2026-10-28 morning + execution commission 11:29 EDT)
- [x] Plan
- [x] Implementation
- [ ] Review — rounds: 1/3, result: 2 MINOR findings, no BLOCKER/MAJOR
- [ ] Ship — PR: none

## Current Gate
none

## Next Phase
Review round 2 — re-review of the R1/R2 fixes (implementer fix round 1 landed)

## Notes
- 2026-10-28 (A1): Compaction encapsulation trio done. (1) `attach_summary_staging` lost its redundant `trigger` param — both callers had already settled `req.trigger` (forced-fixup in the pipeline arm; Manual ctx in the manual path), so the snapshot stamps `req.trigger`. (2) `CompactionState::set_suppress_summary_tier` deleted; suppression is the begin/end pair `begin_summary_tier_suppression() -> bool` / `end_summary_tier_suppression(previous)` owned by CompactionState, and `retry_without_summary` just brackets the re-run (restores the previous value, not a hard-coded false — behavior identical at the only call site). Grep acceptance: no `set_suppress_summary_tier` hits remain anywhere. (3) `CompactionPassState`: `last_history_ref()` deleted — the `NotApplicable` arm now uses the single consuming read `take_history_ref()` like the early-exit branch; `message_count_before` is a plain `pub(super)` field fixed at construction (getter deleted). Verification: `cargo fmt --check` clean; `cargo clippy --locked --all-targets -- -D warnings` clean; targeted tests both configs — `cargo test --locked [--no-default-features] --test compaction_triggers_test --test manual_compact_test --test context_management_end_to_end_test` (22+1+4 passed, 0 failed each config), `--lib compaction` (6 passed, 0 failed each config); extra guard `cargo test --locked --features lalr-harness --test compaction_boundary_test` runs 6 tests: 3 passed / 3 FAILED — and the identical 3 failures (lalr_03_boundary_id_literal_format, lalr_09_post_compact_revision_matches_session, lalr_15_kv_fixed_emits_boundary_without_summary_tier) reproduce on the pre-A1 plan commit 4646395, verified in a scratch worktree. Pre-existing red on main, unrelated to A1; the target requires lalr-harness+ffi so it sits outside both gate configs and its redness was never gated. (Correction to this entry's first version, which misstated the suite as "3 passed" from truncated poll output; the gate-config results above were read in full and stand.) No existing tests changed. Note: plain `cargo test --test compaction_boundary_test` under gate configs errors on required-features — run it via the lalr-harness invocation.
- 2026-10-28 (A2): Snapshot serde + store decoupling done, in the mandated order. Goldens FIRST (commit 777af6a, before any impl change): `tests/enc308_a2_goldens_test.rs` pins serialized ToolSchemaSet bytes for empty / canonical / noncanonical-verbatim-raw (whitespace, key order, `1.50` lexeme preserved in `raw_active` while the entry renders `1.5`) / rich-object-parameters values, each asserted in both directions (serialize == literal; literal deserializes and re-serializes identically), plus a full engine SessionSnapshot fixture (embedded as a literal in the test file) that loads, imports, and re-exports byte-identically. Fixture-generation finding: the snapshot fixture uses `AgentConfig::default()` because the enclave test semantics carry HashMap defaults whose iteration order varies per map instance — a byte pin over that fixture is flaky for reasons unrelated to serde. All 5 goldens passed on the old Wire impl. (4) commit 092cfa2: ToolSchemaSet serde collapsed into `#[derive(Serialize, Deserialize)]` on the struct — Wire struct and both manual impls deleted, `serde::{Deserializer, Serializer}` imports trimmed, the raw_active fidelity contract doc moved onto the struct. Goldens pass UNCHANGED on the derive impl (no drift; D3 satisfied without escalation). (5) commit 23185ef: ContextStore decoupling — the minted-ID grammar moved to its owner as `sidecar_builder::minted_id_sequence` (pub(crate), beside `insert_spilled_sidecar`, with its own unit tests); `ContextStore::validate_restored` now takes `impl Fn(&str) -> Option<u64>` and returns the new store-owned `ContextStoreRestoreError` (`KeyIdMismatch` / `SequenceNotAhead`) instead of `ToolSemanticsError::InvalidSnapshot`. The one call site (engine/mod.rs `import_snapshot`, the only engine change in this chunk) hands over the classifier and maps the error onto `InvalidSnapshot`, so the import boundary error is unchanged and the enc305 tamper tests pass unmodified. Acceptance greps verified: no `ctxref-` literal and no ToolSemanticsError reference remain in src/context_store.rs. Existing tests changed: only the 3 `validate_restored` unit tests in src/context_store.rs — call sites gained the classifier argument (they use a foreign `mint-` grammar at the seam, proving the store is grammar-agnostic) and assertions moved to the structured store error, because the error type legitimately moved. Verification (full-gate chunk): `cargo fmt --check` clean; `cargo clippy --locked --all-targets -- -D warnings` clean; `cargo test --locked` — 701 passed / 0 failed (41 test-result sections, lib through Doc-tests); `cargo test --locked --no-default-features` — 661 passed / 0 failed (39 sections). (Correction to this entry's first version: its cited 686/656 counts came from a gate attempt that had actually FAILED — the snapshot golden JSON initially lived at `tests/fixtures/enc308_a2_snapshot_golden.json`, and `tests/fixtures.rs::run_all_fixtures` globs every `*.json` in that directory as a scenario fixture, so the suites aborted there with partial totals. The golden now lives as an embedded literal in the test file, and the counts above are from complete re-runs verified off the full logs.)
- 2026-10-28 (A3): Dedup consolidation + import validators done. (6) One first-seen implementation remains: new `push_first_seen` (src/tool_universe.rs:350) is the single first-seen test; `merge_tool_names_deduped` (:365) now delegates to it, and all three ffi/types.rs sites gate on it directly — `into_core`'s canonical-ToolName dedup (seen set is now a Vec, :298) and context-reference-ID dedup (borrowed &str keys, :326), plus the outbound `From<ModelRequest>` name projection (:375, was a hand-rolled Vec::contains loop). Streaming loops keep their per-item fallible order and error precedence exactly; ordering/equality semantics unchanged, so the enc305 suites pin the same behavior. (7) import_snapshot's four gates are named pure validators in src/engine/mod.rs, called in the established order with identical error values/messages: `validate_snapshot_version` (:130, m5), `validate_snapshot_phase_coherence` (:150, M1 — takes (phase, &pending), no &self), `validate_snapshot_context_store` (:183, M3), `restore_snapshot_tool_world` (:200 — universe build + restore_active + stored config/io reset). The m6 policy-tier defaults stay inline between gates 1 and 2, exactly where the fix pass put them. New unit tests (mod tests, :1771+) feed mismatched (phase, pending) pairs to the coherence validator directly — all four rejection arms assert the exact pre-extraction messages — plus a coherent-pairs acceptance test (Idle unconstrained; each Awaiting* with its pendings present). No existing tests changed: the enc305 snapshot suite incl. its import-rejection tests passes unmodified against the extracted validators. Verification (results read off complete on-disk logs): `cargo fmt --check` clean; `cargo clippy --locked --all-targets -- -D warnings` clean; default config `cargo test --locked --lib --test enc305_snapshot_test --test enc305_request_refs_test` → lib 287 passed / snapshot 12 / request-refs 7, 0 failed; `--no-default-features` same targets → lib 267 / snapshot 9 / request-refs 0, 0 failed (request-refs is `#![cfg(feature = "ffi")]` wholesale, so 0 under no-default is its baseline shape, not a regression; the snapshot target likewise gates 3 of its 12). The 5 new coherence tests pass on both configs.
- ENC-298 moved to Merged 2026-10-28 (Sean's direction).
- ENC-308 Linear state: In Progress.
- Work clone: ~/workspace/tmp/enc308, branch
  shippopotamus/enc-308-agent_core-final-refactor-close-out-7-deferred-forge-mediums
  off main 6cadf23.

- B1 first dispatch STOPPED at the plan risk gate (no code changed):
  SessionContext.included_files is Option<serde_json::Value>; its sole
  consumer parses it into a closed Vec<InputFile> struct and silently
  returns None on malformed input. Extras inventory was clean — no
  engine code reads any extras key behaviorally. Orchestrator decision
  (Sean's typing doctrine): Option B, type it in the core; plan amended.
  B1 re-dispatched with file_context.rs + model_request.rs in scope.
- 2026-10-28 (B1, re-dispatch): Session context closed + typed; strict
  creation — done. (1) included_files typed in the core per the
  amendment: new `IncludedFile { path, token_estimate, content }` lives
  in src/session.rs (chosen home: file_context already depends on
  session for AgentConfig/SessionCapabilities, so the entry struct in
  session keeps the module DAG one-directional; session never imports
  file_context). `SessionContext.included_files` is now
  `Vec<IncludedFile>` — absent ≡ empty, matching pending_work /
  history_snapshot_ids in the same struct (the one intentional semantic
  collapse: a legacy explicit `[]` used to yield an empty
  FileContextIndex, now none). `content` keeps serde default +
  skip-if-empty, so stored entries missing `content` read back and
  round-trip in the legacy entry shape. The read seam stays tolerant
  (D2): new `de_included_files` keeps entries that deserialize and
  drops malformed ones, mirroring `de_string_list`; live construction
  never passes through it. `condense_file_context` takes
  `&[IncludedFile]`; the private `InputFile`, the JSON parse, and the
  control-char sanitize-and-retry fallback are deleted, as is
  `SessionContext::included_files_json()`; model_request.rs condenses
  the typed list directly (no index for an empty list). (8) New
  UniFFI records `FfiIncludedFile` + `FfiSessionContext` (src/ffi/
  types.rs, re-exported via ffi/mod.rs + lib.rs) mirror the core
  fields; `create_session` now takes `session_context: FfiSessionContext`
  instead of `session_context_json: String`, the lenient
  `unwrap_or(Value::Null)` parse is deleted, and the conversion is a
  total `From` (unit-tested field-by-field). (12-context)
  `SessionContext.extras` is DELETED (field, serde flatten, and the
  from(Value) collection wording); unknown keys are unrepresentable at
  the FFI and dropped by serde at the stored-state seam — proven by the
  hand-written legacy-row test in session.rs (junk keys at both
  levels, typed wire keys `chatId` etc., an included_files entry
  without `content`, one entry missing `path` dropped at the seam).
  (10) create_session routes initial messages through
  `agent_message_strict` (now pub(crate), doc updated) — malformed
  metadata_json at creation is an AgentException::Internal identical
  in kind to the per-turn path; new FFI test asserts it, plus a typed-
  context test asserting the exported snapshot carries the typed wire
  keys and the legacy included_files entry shape.
  PR-description deltas for ENC-294: create_session's 4th parameter is
  now the FfiSessionContext record (was a JSON string); SessionContext
  wire loses extras round-tripping (unknown keys dropped on read) and
  gains typed included_files entries.
  Existing tests changed, each for a legitimate shape change:
  session.rs `session_context_defaults_empty_when_missing` (extras
  assertion removed — field deleted; included_files emptiness asserted
  instead) and `session_context_roundtrips_existing_fixture_shape`
  (now asserts the closed-schema contract: typed fields + typed file
  entry survive, unknown key drops instead of round-tripping); the
  lenient-bool test's comment no longer cites extras;
  tests/file_context_test.rs s17/e7 (condense takes `&[IncludedFile]`
  struct literals instead of a JSON string); tests/
  enc305_snapshot_test.rs two FFI create_session sites (`"{}"` →
  `FfiSessionContext::default()`); tests/finish_boundary_test.rs
  FB-R-010 (pending_work JSON string → FfiSessionContext field);
  src/ffi/semantics.rs two create_session tests (`"null"` →
  `FfiSessionContext::default()`). No assertion was weakened.
  Verification (all counts read off complete on-disk logs):
  `cargo fmt --check` clean; `cargo clippy --locked --all-targets --
  -D warnings` clean; `cargo test --locked` full — 710 passed /
  0 failed (lib 289; enc305_snapshot 14; file_context 3;
  context_management e2e 1; finish_boundary 10);
  `cargo test --locked --no-default-features` targeted (lib +
  enc305_snapshot + file_context + context_management e2e) — 281
  passed / 0 failed (lib 268; snapshot 9 — ffi-gated tests off, its
  baseline shape; file_context 3; e2e 1). finish_boundary cannot
  build under no-default-features (required-features = ["ffi"],
  pre-existing) and was not run there. A2 goldens pass unchanged
  (golden snapshot's session_context is `{}`, no drift).
  Next: chunk B2.
- B2 COMPLETE (worker report 2026-10-28; orchestrator committed — the
  worker's reported commits never landed, its changes were all in the
  working tree; code verified against its on-disk gate logs):
  item 9 FfiStateLedger + FfiArtifactStateAnchor (all 11 fields, total
  Froms both directions, both .ok()s dead); item 11 fallible
  tool_result_record + submit_tool_results -> Result (batch converts
  before the engine call; blanks drop; non-blank invalid names error —
  currently unreachable since ToolName::new only fails on blank);
  item 12 MessageMeta.extra deleted, seven well-known bag keys promoted
  to typed fields, absorb() is the single routing table (strict ingress
  errors naming the key; stored reads tolerate-and-drop), byte-stable
  manual Serialize pinned by literal test.
  LATENT BUG FIXED en route: absorb had no "spilled" arm — restored
  sessions lost is_spilled() and thus the spill exemption; now routed
  to its field and pinned by the legacy-row test.
  Gates (worker logs ~/workspace/tmp/enc308-b2-*.log): default full
  suite 0 failed (~720 passed); no-default targeted 290/0. fmt/clippy
  clean per worker logs, re-verified by orchestrator pre-commit.
  ENC-294 deltas: FfiSummaryRequest carries FfiStateLedger;
  submit_tool_results is fallible; FfiAgentAction RequestSummary
  variant grew (scoped clippy allow — UniFFI cannot Box records in
  enum variants); MessageMeta schema closed (unknown keys rejected at
  FFI ingress, dropped at stored reads).
  Next: chunk C.
- 2026-10-28 (C): Docs + final gates done. Docs corrected (commit
  db997fc): README gained a boundary-contract section (closed/typed
  FFI surface; validation model — provider-facing JSON Schema vs the
  engine-enforced ToolSchemaHint + semantics; the ratified JSON list
  with reasons) and lost the Layout row for src/host.rs (deleted in
  ENC-285); `AgentMessage.metadata`'s field doc in src/message.rs no
  longer calls metadata an opaque host blob — it describes the closed
  MessageMeta schema. Everything else grepped for the dead names
  (session_context_json, state_ledger_snapshot_json, extras,
  serializeFullRegistry, ToolSchemaRegistry) was already accurate —
  the remaining mentions are historical notes stating those forms are
  gone. FINAL GATES on the complete branch (logs
  ~/workspace/tmp/enc308-c-*.log, counts read off the complete logs):
  `cargo fmt -- --check` clean; `cargo clippy --locked --all-targets
  -- -D warnings` clean; `cargo test --locked` — 721 passed /
  0 failed (42 test-result sections); `cargo test --locked
  --no-default-features` — 671 passed / 0 failed (40 sections). The
  ticket's definition of done is met on the branch; Forge review on
  the PR is the remaining gate (Review phase). PR description drafted
  at plans/ENC-308/pr-description.md (14 items by part, full ENC-294
  delta list, latent spilled-flag bug fix, item-11 unreachability
  note, gate counts). No existing tests changed in this chunk.
  Next: Review.

- 2026-10-28 (Review round 1, fresh-context reviewer): independent
  re-run of all gates on the branch — fmt clean, clippy -D warnings
  clean, cargo test --locked 721 passed / 0 failed (42 sections),
  --no-default-features 671 passed / 0 failed (40 sections); worker
  counts reproduced exactly. Traced and verified clean: absorb
  completeness (base bag-key universe == the promoted typed set; the
  B2 "spilled" routing fix confirmed against base code, which had no
  spilled arm in from_value), FfiStateLedger/FfiArtifactStateAnchor
  fidelity (11/11 and 4/4 fields, total Froms both directions),
  submit_tool_results converts the full batch before the engine call,
  test-integrity walk (item-12 renames match the ticket exactly; no
  weakened assertions found), lib.rs exports purely additive (+4),
  PR-description ENC-294 delta list complete. FINDINGS:
  R1 (MINOR) src/message.rs absorb "name" arm sets tool_name
  unconditionally; base guarded it (`if self.tool_name.is_none()`), so
  explicit tool_name only wins today because serde_json resolves
  without preserve_order in this tree (verified via cargo tree).
  Correction: restore the guard so precedence is order-independent.
  R2 (MINOR, test confidence) no single test drives a legacy-populated
  SessionSnapshot blob (junk-carrying context + spilled/junk message
  metadata) through import_snapshot; the seams are each proven
  (AgentSession/AgentMessage legacy-row tests, FFI boundary tests) but
  the composite D2 artifact path is unexercised. Correction: add one
  such import test. Architecture Questions: none. Judged immaterial
  and recorded, not findings: included_files absent≡empty collapse
  (explicit-[] sessions lose the always-Some empty FileContextIndex —
  disclosed in B1 note, confined to a degenerate state) and
  content:"" re-serializing without the key (semantically lossless
  normalization).

- 2026-10-28 (Review fix round 1 — R1/R2 addressed):
  R1: the absorb "name" arm (src/message.rs) again lands into
  `tool_name` only while the field is unset — the pre-ENC-308 guard
  restored verbatim in effect, so an explicit `tool_name` wins in
  every key order by construction rather than by serde_json's
  sorted-key iteration. The absorb doc comment no longer claims
  ordering is load-bearing; it credits the guard. New unit test
  `message_meta_explicit_tool_name_beats_legacy_name_in_any_order`
  drives the absorb seam in both insertion orders plus a both-keys
  `from_value` object; all resolve to the explicit name.
  R2: new `tests/enc308_legacy_snapshot_test.rs` — one composite
  legacy blob (the A2 golden snapshot's shape at SNAPSHOT_VERSION 1,
  mutated to pre-308 content: session context with typed wire keys
  PLUS a `legacy_host_junk` extras key; message metadata with
  `"spilled": true` PLUS a junk key, in both the history and pending
  copies; the A2 golden's populated ToolSchemaSet; a state ledger
  with content in the ledger field, the pending request, and the
  sidecar last-ledger JSON) driven through the real `import_snapshot`
  path. Asserts: typed context fields intact, `is_spilled()` true,
  ledger content intact, raw schema channel verbatim, and both junk
  keys absent on re-export (field-level asserts, no whole-blob byte
  pin, per the A2 lesson). No existing tests changed.
  Verification (results read off complete on-disk logs in
  ~/workspace/tmp/enc308-rfix-logs/): `cargo fmt --check` clean;
  `cargo clippy --locked --all-targets -- -D warnings` clean;
  default config `cargo test --locked --lib --test
  enc305_snapshot_test --test enc308_legacy_snapshot_test` → lib 297
  / snapshot 14 / legacy 1, 0 failed; `--no-default-features` same
  targets → lib 273 / snapshot 9 / legacy 1, 0 failed (the snapshot
  target's 9-vs-14 is its ffi gating, baseline shape).
  Next: Review round 2 (re-review of the two fixes).
