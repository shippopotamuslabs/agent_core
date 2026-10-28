# ENC-306 — give the decomposed engine modules owned state: rich domain models, not InnerSession field bundles (vision: Linear ENC-306)

Stacked on ENC-303 (PR #35). Child of ENC-284. Origin: Forge findings on PR #22
(ENC-286 + FRG-47), deferred 2026-10-27 by Sean's call: contained fixes landed in
#22; this ticket is the right-way fix. Sean's framing: each decomposed module
should have its own state — rich domain models.

Problem: ENC-286 sliced the engine god object by file, but the extracted modules
(compaction, policy, finish) still operate on InnerSession's fields through
bundle structs that mirror them — PipelineOutcomeInputs (22 fields, built once
and destructured immediately), PipelineDecision (re-encodes the same 4-arm
outcome match), FinishState (19 &mut fields, built by finish_state() and
consumed-and-returned), SyncSpillInputs + the collect/apply split. The bundles
are proto-state-structs that never got ownership of their fields.

Direction (Sean): move the fields a module actually operates on out of
InnerSession into a state struct that module owns; domain operations become
methods on that struct, invariants included; InnerSession composes the models
and coordinates; the bundle structs are deleted, not renamed.

Scope: audit at minimum compaction, finish, policy. Policy is currently a pure
evaluator — decide during the work whether it owns real state or stays pure;
do not force state where there is none. Behavior change: NONE. Both test
configs green, fixture suites unchanged.
