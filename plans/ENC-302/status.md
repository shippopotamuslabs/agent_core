# Status: ENC-302

## Phase Checklist
- [x] Research — vision approval: approved (mission)
- [x] Plan
- [x] Implementation
- [x] Review — rounds: 1/3, result: clean after fix pass (J1+J2+m1-m4 fixed 4e4b2af)
- [ ] Ship — PR: none (stacked on #33)

## Current Gate
none

## Next Phase
Implementation

## Notes
- Base: ENC-301 branch tip (PR #33). Worktree /home/sean/Code/agent_core-ENC-302. PRs #29-32 CI green.

- Decision: Option A recorded on Linear ENC-302.
- Implementation (Option A): `ArtifactStateAnchor.total_blocks: u32` -> `observed_total_blocks: Option<u32>` with `#[serde(alias="total_blocks")]`; total<=0 kill removed; `None` renders as literal `unknown` in anchor message, squash summary, and snapshot fallback; continuation verbatim. Engagement/anchor share `observed_append_result` (success sniff scoped to append tool via `tool_name` metadata); spill exemption scoped the same way. No semantics-config/FFI changes.
- Gates: cargo fmt --check clean; clippy --all-targets --locked -D warnings clean; cargo test --locked green (lib 249 + integration 366); --no-default-features green (lib 231 + integration 356). New: 1 unit + 5 integration tests in tests/enc302_anchor_cache_test.rs.
