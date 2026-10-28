# Status: ENC-299

## Phase Checklist
- [x] Research — vision approval: approved (mission)
- [x] Plan
- [x] Implementation
- [x] Review — rounds: 1/3, result: clean after fix pass (J1 Major + m1-m5 fixed b328ff0)
- [ ] Ship — PR: none (stacked on #30)

## Current Gate
Implementation complete (all end gates observed green on committed tree)

## Next Phase
Review

## Notes
- Base: ENC-297 branch tip (PR #30). Worktree /home/sean/Code/agent_core-ENC-299.
- Implementation done in 2 commits referencing ENC-299:
  - 6024868 Chunk 1: ToolName newtype + retype, per-site folds deleted
  - e7f1d8d Chunk 2: referential validation + alias-vs-canonical fix + acceptance tests
- End gates (exact): cargo fmt --check clean; cargo clippy --all-targets --locked -- -D warnings clean; cargo test --locked 581 passed / 0 failed (35 targets); cargo test --locked --no-default-features 553 passed / 0 failed.
- Grep evidence: catalog::normalize + tool_semantics::normalize_tool_name gone; (b)-fold lives only in ToolName::new (src/tool_name.rs:57); normalize_to_snake_case kept only in canonicalize repair pipeline; fuzzy case-fold kept in tool_name_match (scoring, documented).
- Fixture re-pins: none. Zero byte-identity deltas — no resources/support JSON touched; all suites green unmodified, incl. enclave fixture set (refs all in 20-spec catalog, union rule covers subset schema sets).
- ts_006 rewritten: blank tool_name is now a serde boundary rejection (ToolName::new), not a validate() error.
- No push/PR/Linear (per brief).
