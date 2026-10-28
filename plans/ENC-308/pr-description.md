# [ENC-308] Final refactor close-out — deferred Forge Mediums + FFI typing program

One PR for ENC-308, the last ENC-284 child. After this merges, every
ENC-284 child is complete and the next phase (ENC-294 re-vendor, then a
greenfield consumer) starts from a closed boundary. Plan and full run
history: `plans/ENC-308/plan.md`, `plans/ENC-308/status.md`.

## What changed — the 14 items

### Part A — the 7 deferred Forge Mediums (PRs #37/#38)

1. **Redundant `trigger` param removed** from `attach_summary_staging` —
   both callers had already settled `req.trigger`, so the history
   snapshot stamps it directly.
2. **Suppress-summary toggle encapsulated**: `set_suppress_summary_tier`
   is deleted; `CompactionState` owns the begin/end pair
   (`begin_summary_tier_suppression` returns the previous value,
   `end_summary_tier_suppression` restores it), and
   `retry_without_summary` only brackets the re-run.
3. **CompactionPassState simplified**: the borrowing
   `last_history_ref()` accessor is deleted in favor of the single
   consuming `take_history_ref()` read; `message_count_before` is a
   plain construction-fixed field (getter deleted).
4. **ToolSchemaSet serde collapsed into derives** — the hand-rolled
   `Wire` struct and both manual impls are gone. Byte identity with the
   old impl is proven, not assumed: golden fixtures (serialized
   ToolSchemaSet bytes + a full SessionSnapshot) were committed *before*
   the swap, passed on the Wire impl, and pass unchanged on the derives.
5. **ContextStore decoupled from the minted-ID grammar**:
   `validate_restored` now takes a classifier closure and returns the
   store-owned `ContextStoreRestoreError`; the `ctxref-` grammar moved
   to its owner as `sidecar_builder::minted_id_sequence`. The store no
   longer references the grammar or `ToolSemanticsError`; the one call
   site maps the error onto the unchanged import-boundary error.
6. **One first-seen dedup implementation**: new `push_first_seen`
   (tool_universe.rs) is the single first-seen test —
   `merge_tool_names_deduped` delegates to it, and all three
   ffi/types.rs sites (canonical ToolName dedup, context-reference-ID
   dedup, outbound name projection) gate on it. Ordering/equality
   semantics unchanged.
7. **import_snapshot's four gates extracted** into named pure
   validators, called in the established order with identical errors:
   `validate_snapshot_version`, `validate_snapshot_phase_coherence`,
   `validate_snapshot_context_store`, `restore_snapshot_tool_world`.
   The coherence rule is now unit-tested directly (mismatched
   (phase, pending) pairs, no JSON tampering needed).

### Part B — finish the FFI typing program (items 8–12)

8. **Session context typed**: `create_session` takes the new
   `FfiSessionContext` record instead of `session_context_json: String`;
   the lenient `unwrap_or(Value::Null)` parse is deleted and the
   conversion is a total `From`.
9. **State ledger typed**: `FfiSummaryRequest` carries the new
   `FfiStateLedger` record (plus nested `FfiArtifactStateAnchor`) in
   both directions, mirroring `StateLedger` field-for-field; both
   silent `.ok()` conversions are dead.
10. **Strict creation**: `create_session` routes initial messages
    through `agent_message_strict`, exactly like the per-turn path —
    malformed `metadata_json` at creation is an `AgentException`, never
    a silently defaulted message.
11. **Fallible tool-result submit**: `submit_tool_results` returns
    `Result<(), AgentException>`; the batch converts before the engine
    call, blank activated names drop (documented), and a non-blank
    invalid name is a typed error. **Unreachability note:** with
    today's `ToolName` grammar the error arm cannot fire —
    `ToolName::new` rejects only blank names, which take the drop path.
    The fallible contract is deliberate: it is pinned at the conversion
    seam and becomes live the day the name grammar tightens, instead of
    silently absorbing bad names.
12. **Extras bags deleted; schemas closed** (Sean's ruling): the host
    defines nothing the engine hasn't agreed to carry. Host annotations
    stay host-side, keyed by message ID, reattached by the host.
    - `SessionContext.extras` deleted (B1): unknown context keys are
      unrepresentable at the FFI and dropped by serde at the
      stored-state read seam.
    - `MessageMeta.extra` deleted (B2): the seven well-known keys that
      used to hide in the catch-all are typed fields, routed by the
      single `absorb()` table. FFI ingress is strict — an unknown key
      is a typed error naming the key. Stored reads are
      tolerate-and-drop: legacy metadata loads with every engine-owned
      value intact and the retired keys gone. Serialization stays
      byte-stable (pinned by a literal test).

**Amendment rider (B1 risk gate, plan-amended):** `included_files`
typed in the core. The field was `Option<serde_json::Value>`; its sole
consumer parsed it into a closed struct and silently returned `None` on
malformed input. `SessionContext.included_files` is now
`Vec<IncludedFile>` (new core type in session.rs), `condense_file_context`
takes `&[IncludedFile]`, and the JSON parse, the sanitize-and-retry
fallback, and `included_files_json()` are deleted. One intentional
semantic collapse: absent ≡ empty list, matching the struct's other
list fields.

### Part C — docs + gates (items 13–14)

13. **Docs refreshed**: README gains the boundary contract (closed/typed
    FFI, validation model — provider-facing JSON Schema vs the
    engine-enforced `ToolSchemaHint` + semantics — and the ratified
    JSON list with reasons); the deleted `src/host.rs` layout row is
    removed; `AgentMessage.metadata`'s field doc now describes the
    closed schema instead of an opaque host blob.
14. **Final gates**: see Test evidence — all four green, 0 failed.

## Latent bug fixed en route (B2)

`MessageMeta::absorb` had no `"spilled"` arm, so a restored session
silently lost `is_spilled()` — and with it the tool-spill compaction
exemption (ENC-293 reads that flag instead of sniffing content). The
key is now routed to its typed field and pinned by the legacy-row test.
This was a live data-loss bug in stored-state reads, found by the
stored-state invariant audit the plan mandated for B2.

## ENC-294 host-visible deltas (for the re-vendor)

- `create_session`'s 4th parameter is now the `FfiSessionContext`
  record (was `session_context_json: String`); initial messages are
  validated strictly at creation (malformed metadata → `AgentException`).
- `FfiSessionContext` includes `included_files: Vec<FfiIncludedFile>`
  with typed `{path, token_estimate, content}` entries.
- SessionContext wire: extras round-tripping is gone — unknown keys
  are dropped on read; typed fields and typed file entries survive.
- `FfiSummaryRequest` carries `FfiStateLedger` (new record, with
  `FfiArtifactStateAnchor`); the `state_ledger_snapshot_json` string is
  gone in both directions.
- `FfiAgentAction::RequestSummary` grew with the typed ledger (scoped
  clippy allow recorded: UniFFI cannot Box records in enum variants).
- `submit_tool_results` is fallible: `Result<(), AgentException>`.
- MessageMeta schema is closed: unknown keys are rejected at FFI
  ingress (error names the key) and dropped at stored reads; the
  former catch-all keys (`agent_tool_budget`, `truncated`,
  `envelope_*`, `tools_blocked_*`, `provider_reasoning`, `spilled`)
  are typed fields with the same wire keys.

## Test evidence

Per-chunk gates are recorded with counts in `plans/ENC-308/status.md`
(A2 full suites 701/661; B1 full default 710; B2 full default ~720,
no-default targeted 290 — all 0 failed). Final gates on the complete
branch (chunk C):

- `cargo fmt -- --check` — clean
- `cargo clippy --locked --all-targets -- -D warnings` — clean
- `cargo test --locked` — 721 passed / 0 failed (42 test-result sections)
- `cargo test --locked --no-default-features` — 671 passed / 0 failed
  (40 test-result sections)

Known pre-existing red, not gated by this PR: the `lalr-harness`
compaction-boundary suite has 3 failing TDD-Red tests at baseline
(identical on main); it is tracked separately as ENC-309.

## Definition of done

Both test configs green, fmt/clippy clean, no unresolved Forge
findings. With this merged, the agent_core refactor is done: every
ENC-284 child complete, the FFI boundary closed and typed, and the
only JSON crossings left are the ratified three (snapshot blob,
schema-hint default values, tool arguments/results) — each documented
in the README with its reason.
