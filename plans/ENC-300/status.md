# Status: ENC-300

## Phase Checklist
- [x] Research — vision approval: approved (mission)
- [x] Plan
- [x] Implementation
- [x] Review — rounds: 1/3, result: clean after fix pass (J1 + m1-m7 fixed 01c269b; intended breaks documented)
- [ ] Ship — PR: none (stacked on #31)

## Current Gate
none

## Next Phase
Implementation

## Notes
- Base: ENC-299 branch tip (PR #31). Worktree /home/sean/Code/agent_core-ENC-300. PR #29/#30 CI green.
- Implementation complete (2 chunks, commits d73c7f1 + 434bb8e, no push/PR/Linear):
  - C1: ToolCallFormat + shape renames + EnvelopeParser port + parse/config/FFI
    plumbing + engine ingest/recovery + test migration (incl. etr_r_09 Dsml
    session, tb_06 Dsml config found during full-suite run).
  - C2: declared-format stream gate + FFI gate signature + gate tests.
  - Gates: cargo fmt --check clean, clippy --all-targets --locked -D warnings
    clean, cargo test --locked 597 passed/0 failed (35 targets),
    cargo test --locked --no-default-features 569 passed/0 failed.
  - Dual-form pins: cl_tr_02 (+envelope counterpart), cl_e_02, cl_md_01 under
    Structured; Dsml+[TOOL_CALLS]=>ParseFailed; Structured pass-through gate;
    wrong-format streams unheld.
  - Surprise: none architectural; two latent envelope-text engine tests
    (etr_r_09, tb_06) needed explicit Dsml configs under declared-format
    semantics. Enclave impact for ENC-294 PR note: hosts must declare
    tool_call_format per session (default Structured mines no text).
