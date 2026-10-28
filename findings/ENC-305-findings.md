# ENC-305 Findings — dual review of 335551f..HEAD (c65e02f, 58f4828, f0c1d5c)
Reviewers: Synthitect (fresh context) + Thermos lens A (correctness) + Thermos lens B (structure). Orchestrator gates at review time: 633/0 + 597/0 (baseline 622/594), fmt clean. Deduped below; reviewer dissents recorded where they split.

## Critical
None.

## Major
- M1 Import accepts incoherent (phase, pending) pairs that panic on first poll
  (engine/mod.rs import_snapshot vs expects at :360/:364/:368, :644, :1004).
  Import validates only the tool world; construction can never produce
  AwaitingModel+call_model=None etc., but a crafted/corrupt/version-skewed
  snapshot can, and next_action then panics across the FFI boundary — the
  exact failure class this ticket exists to eliminate.
  Fix: coherence validation in import_snapshot (AwaitingModel => call_model,
  AwaitingTools => execute, AwaitingSummary => summary_req AND summary_ctx)
  returning a typed error — new ToolSemanticsError::InvalidSnapshot{message}
  variant (the family's Invalid* convention). Never coerce phase to Idle.
  Tamper tests per phase.
- M2 ToolSchemaSet serde drops raw_active (tool_schema.rs custom impls).
  A string-path session's host channel text (whitespace/key order/number
  lexemes; parse("") -> "") is replaced by entries_json canonical text, and
  that text feeds token counting (budget.rs via CompactionContext +
  model_request.rs), so pressure/compaction behavior can drift across a
  restart. The module's own docs keep raw text "untouched... byte-identical"
  as an invariant. Fix: wire form carries raw_active alongside active
  (single shared Wire struct — also resolves the duplicated-Wire Minor);
  Deserialize constructs Self{active, raw_active} directly.
- M3 ContextStore restore invariants unchecked (context_store.rs,
  engine/mod.rs import). Live invariants from insert_entry: map key ==
  entry.reference.id; next_seq ahead of minted IDs (a reset next_seq lets
  future ctxref-*-N IDs collide and silently overwrite live entries);
  import installs the store verbatim. Fix: import-time validation
  (ContextStore::validate_restored or import_snapshot checks): reject
  key != reference.id; reject next_seq not greater than every ctxref-*-N
  suffix present. Content dedup is NOT re-run (entries are data; replaying
  through insert_entry could silently drop them — Synthitect's caution).
  Tamper test for key/id mismatch.
- M4 The round-trip test pins the replayed pending request, not behavior
  (tests/enc305_snapshot_test.rs). Export happens at AwaitingModel, so the
  post-import next_action returns the stored pending request verbatim and
  the headline assertions compare PendingToolTurn against itself; ledger
  CONTENT, delta continuity (last_ledger_json), provider generation,
  counters are never exercised, and no second turn is taken post-import.
  Fix: restructure — (a) control-vs-restored comparison: drive two sessions
  identically, restart one mid-script, submit the pending response on the
  restored session, compare the FOLLOWING build_model_request output (full
  state_ledger equality, state_ledger_delta_json, provider_context);
  (b) mid-turn test: export at AwaitingTools, import, same ExecuteTools
  batch, submit results, turn completes identically; (c) scalar asserts on
  the snapshot (phase, turn_index, counters, cancelled, terminal_action);
  (d) active_json() equality across the round trip (pins M2).

## Minor
- m1 Strict metadata path is syntax-strict only (ffi/types.rs, message.rs):
  "42" / "[1,2]" metadata_json still becomes empty meta via from_value.
  Fix: from_json_str_strict rejects a top level that is neither object nor
  null, with tests. (Thermos A dissented — plan-mandated identical per-field
  handling; orchestrator sides with Synthitect: silently emptying is the
  failure class being closed.)
- m2 Alias resolution loses canonical identity (ffi/types.rs into_core):
  requesting alias "websearch" yields stub {"name":"websearch"} instead of
  web_search's entry. Fix: when spec(name) resolves, canonicalize to
  spec.name before entry lookup/stub and active-membership checks. Also fix
  the pinning test's comment (websearch is web_search's alias). (Thermos A
  judged the stub shape per-spec; orchestrator sides with Synthitect —
  canonical identity is what every other catalog path produces.)
- m3 into_core does not dedupe active_tools / context_reference_ids
  (ffi/types.rs): canonical duplicates ("Web Search" + "web_search")
  produce duplicate schema entries — the deleted path rejected them via
  parse. Fix: dedupe first-seen on the canonical ToolName (activate +
  outbound-projection precedent) and on context IDs. (Thermos A preferred
  rejection; dedupe chosen for consistency with the two live neighbors.)
- m4 Snapshot/InnerSession sync is guarded by prose only (Thermos B):
  export_snapshot must destructure InnerSession by reference WITHOUT `..`
  so a new field is a compile error until snapshotted or explicitly
  excluded.
- m5 No snapshot format version (session_snapshot.rs): the durable
  cross-process format gets `version: u32` + SNAPSHOT_VERSION = 1 now,
  while only one generation exists; import rejects other versions with
  InvalidSnapshot.
- m6 Import skips apply_policy_tier_defaults (Thermos A): idempotent for
  genuine snapshots (Synthitect: skipping is correct for them) but a
  host-assembled snapshot would silently run un-tiered. Apply it at the top
  of import, mirroring create_session's ordering — a no-op for genuine
  round trips.
- m7 Docs/idiom nits: session_snapshot.rs module doc must list config as a
  third transformation (effective at export, consumed at import), not
  "verbatim"; create_session's mem::replace(..., agnostic()) vs import's
  mem::take divergence — unify on mem::take (the ENC-304 clippy fix's
  spelling); drop Debug from SessionSnapshot + PendingToolTurn (derive
  fallout making full pending requests printable; Clone stays).

## Suggestions
- s1 deny_unknown_fields / stricter nested defaults at the snapshot layer
  (Synthitect): partially adopted via m5 (version gate); full strictness
  deferred — nested defaults are the config format's own contract.

## Thermos-specific
- Lens A verified clean: no .ok()/unwrap_or_default remains in the new
  request conversions or snapshot import (remaining sites are the D8
  out-of-scope FfiSummaryRequest + pre-existing tool-result filter);
  blank/dangling name + ID errors match spec; stored-config reset on import
  matches construction exactly; admit-mutated universe exports consistently.
- Lens B: visibility widenings proportionate (PendingToolTurn fields stayed
  pub(super)); into_core placement matches the fallible-conversion grain
  (session_io_params/agent_config); serde sprawl confined to the snapshot
  graph. Export destructure + version + Debug items carried above.

## Disposition
Fix pass complete (c5369a1): M1–M4 and m1–m7 all fixed; the round-trip
suite was rewritten behaviorally (control-vs-restored, mid-turn restart,
tamper tests for coherence/store/version). Final gate: 645 default /
607 no-default, 0 failed (baseline 622/594). Open Critical/Major: 0.
Adjacent, NOT fixed (out of scope per plan D8): FfiSummaryRequest's
state_ledger_snapshot_json .ok() parse — same silent-degradation pattern on
the summary path; flag for Sean as a follow-up candidate.
