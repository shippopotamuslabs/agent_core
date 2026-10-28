# Plan: ENC-302 Option A — anchor as derived observation cache (stacked on ENC-301/#33)

## Decisions
- D1 ArtifactStateAnchor.total_blocks: u32 -> observed_total_blocks: Option<u32> with #[serde(alias="total_blocks")] (old JSON/snapshots still load; serialize new name). Docs (struct + state_ledger + artifact_squash): cache of engine-processed results, may be stale, host artifact is truth, never branch on it.
- D2 Anchor existence: newest OBSERVED successful append result (status success) suffices — remove total<=0 kill. observed_total_blocks = parsed total field when present+integer+>0... (0 is a legitimate observed value? host reporting 0 after success is contradictory; map non-integer/absent -> None, integer >=0 -> Some(v) incl 0). Squash ENGAGEMENT scan (is_semantic_append_message / last_append_lookup use for dropping) UNCHANGED unless needed for summary line; document any shared-helper split.
- D3 Renders: as_anchor_message + squash summary {total}: observed value or literal `unknown`; continuation_instruction emitted verbatim regardless of count (pin). Snapshot fallback text same treatment.
- D4 No semantics-config/FFI changes (total_blocks_result_field stays: it names where observations come from).

## Acceptance tests
- Count disagreement: appends observed report total=3, later result reports total=9 (out-of-band growth) -> before new observation anchor still 3 + continuation intact; after extraction anchor=9.
- Missing/non-integer total field on successful append -> anchor exists, observed None, summary renders unknown, continuation present.
- Old JSON with total_blocks deserializes (alias test).
One chunk/commit. Gates both configs. No push/PR/Linear (choice already recorded by orchestrator).
