# ENC-303 Findings — dual review of f6d3ecb + 8161d46
Reviewers: Synthitect + Thermos x2. Gates: 629/0 + 601/0.

## Critical
None.

## Major
- J1 Silent discovery self-lockout: discovery section without self-exemption (and discovery tool not in initial active schemas, non-empty active set) deadlocks activation with a self-referential rejection. Fix: construction-time validation (validate_references seam) — error naming the missing self-exemption.

## Minor
- m1 FIFO confirmation-gate arm vacuous for built plans (HistorySnapshot always in plan; real enforcement is apply-time gate) + doc names wrong field (require_snapshot_before_fifo vs plan.require_snapshot_before_destructive_drop) -> fix docs to state pre-filter vs apply-time enforcement accurately.
- m2 gate_exemptions lacks #[serde(default)] (old companion field had one) -> restore default-empty.
- m3 Test-local companion vocabulary + PressureClass half-consumed doc note + forced-entry/gate incoherence doc note.
- m4 No FFI round-trip test for FfiDiscoveryGateExemption -> add.

## Disposition
J1 + m1-m4 fix pass.
## Resolution
All fixed in ff5fdb8. Orchestrator re-ran both configs: 636/0, 606/0. 0 open Critical/Major.
