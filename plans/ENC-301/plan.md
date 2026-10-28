# Plan: ENC-301 — state-ledger precedence (stacked on ENC-300/#32)

## The precedence rule (lands in state_ledger module docs verbatim in substance)
1. Surviving log wins: non-superseded session messages are ground truth for anything they still contain.
2. Ledger is a derived cache, newest observation wins per fact class: objective = last-write-wins; artifact anchors = keyed replace by artifact_id (unchanged);
   decisions/constraints/open_questions/touched_files = set semantics (exact normalized-text dedupe; first-seen order kept);
   tool_decisions = append-only event history (never retired).
3. Explicit retirement tags (same declared tag protocol as existing extraction, case-insensitive):
   `reversal:` / `supersedes:` retire matching decisions+constraints; `resolved:` retires matching open_questions.
   Match = normalized full entry text (trim, collapse whitespace, case-fold for COMPARISON key only; stored text unchanged).
   Unmatched reversal/resolved appends as a decision (visible, never silent). NO natural-language reversal inference.
4. Summary is older than the log tail: merge_summary observations are tombstone-filtered — retired texts (ledger tombstone set) are NOT resurrected by summary re-observation. A live user/assistant re-assertion clears the tombstone and re-adds. (Ledger gets `retired: Vec<String>` tombstones, serde default; cap not required but dedupe.)
5. HistorySnapshot has no precedence: verbatim archive behind read_context_reference; it never overrides log/ledger/summary in render. summary_refs/history_refs deduped exact.

## Decisions
- D1 observe_text gains source (Live vs Summary) internally; public observe_user/assistant = Live; merge_summary = Summary. Objective: Live always replaces; Summary replaces only if its value differs AND no Live objective observed after? Simpler deterministic: objective last-write-wins from Live; Summary sets objective only when currently None (summary is older). Pin in tests.
- D2 Tombstones stored normalized; retirement removes matching entries + records tombstone; Live re-assert of same text removes tombstone + appends.
- D3 Docs: state_ledger.rs module docs carry the full rule (replace unqualified "source of truth" with derived-cache wording + precedence list). Crate-level mention only if a natural home exists (lib.rs doc?) — module docs are the requirement.
- D4 No metadata/FFI changes; no compaction-tier behavior changes beyond ledger observation. as_anchor_message format unchanged (retired entries simply absent).

## Acceptance tests
- Decision -> reversal -> compaction tier (drive via engine: messages + summary merge or compaction pass as existing tests do) -> next ModelRequest/anchor carries only surviving decision; summary re-merge does not resurrect; live re-assert after retirement restores.
- Objective last-wins (Live) + summary-does-not-clobber-live; dedupe exact duplicates (incl. merge_summary duplicates); resolved: retires question; unmatched reversal appends visibly.
Existing tests pinning append-duplicates/set-once (m4 etc.): re-pin ONLY where rule changes semantics, with explanation comments; never weaken unrelated assertions.
Gates both configs. One chunk, one commit (small surface: state_ledger.rs + callers for source threading + tests). No push/PR/Linear.
