# Status: ENC-308

## Phase Checklist
- [x] Research — vision approval: approved (Sean's design rulings
      2026-10-28 morning + execution commission 11:29 EDT)
- [x] Plan
- [ ] Implementation
- [ ] Review — rounds: 0/3, result: pending
- [ ] Ship — PR: none

## Current Gate
none

## Next Phase
Implementation (chunk A1)

## Notes
- 2026-10-28 (A1): Compaction encapsulation trio done. (1) `attach_summary_staging` lost its redundant `trigger` param — both callers had already settled `req.trigger` (forced-fixup in the pipeline arm; Manual ctx in the manual path), so the snapshot stamps `req.trigger`. (2) `CompactionState::set_suppress_summary_tier` deleted; suppression is the begin/end pair `begin_summary_tier_suppression() -> bool` / `end_summary_tier_suppression(previous)` owned by CompactionState, and `retry_without_summary` just brackets the re-run (restores the previous value, not a hard-coded false — behavior identical at the only call site). Grep acceptance: no `set_suppress_summary_tier` hits remain anywhere. (3) `CompactionPassState`: `last_history_ref()` deleted — the `NotApplicable` arm now uses the single consuming read `take_history_ref()` like the early-exit branch; `message_count_before` is a plain `pub(super)` field fixed at construction (getter deleted). Verification: `cargo fmt --check` clean; `cargo clippy --locked --all-targets -- -D warnings` clean; targeted tests both configs — `cargo test --locked [--no-default-features] --test compaction_triggers_test --test manual_compact_test --test context_management_end_to_end_test` (22+1+4 passed, 0 failed each config), `--lib compaction` (6 passed, 0 failed each config); extra guard `cargo test --locked --features lalr-harness --test compaction_boundary_test` runs 6 tests: 3 passed / 3 FAILED — and the identical 3 failures (lalr_03_boundary_id_literal_format, lalr_09_post_compact_revision_matches_session, lalr_15_kv_fixed_emits_boundary_without_summary_tier) reproduce on the pre-A1 plan commit 4646395, verified in a scratch worktree. Pre-existing red on main, unrelated to A1; the target requires lalr-harness+ffi so it sits outside both gate configs and its redness was never gated. (Correction to this entry's first version, which misstated the suite as "3 passed" from truncated poll output; the gate-config results above were read in full and stand.) No existing tests changed. Note: plain `cargo test --test compaction_boundary_test` under gate configs errors on required-features — run it via the lalr-harness invocation.
- ENC-298 moved to Merged 2026-10-28 (Sean's direction).
- ENC-308 Linear state: In Progress.
- Work clone: ~/workspace/tmp/enc308, branch
  shippopotamus/enc-308-agent_core-final-refactor-close-out-7-deferred-forge-mediums
  off main 6cadf23.
