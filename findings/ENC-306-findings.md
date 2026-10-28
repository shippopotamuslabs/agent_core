# ENC-306 Findings — dual review of aa6afce..HEAD (4af7b8d, 20be044, 13a3ee2)
Reviewers: Synthitect (fresh context) + Thermos lens A (correctness) + Thermos lens B (structure). Orchestrator gates at review time: 636/606, 0 failed (== baseline), fmt clean.

## Critical
None. (Lens A arm-by-arm comparison against aa6afce found NO behavior
difference anywhere — effect order, counter/flag timing, trigger
provenance at both staging sites, Flow-vs-LoopAgain fall-through, finish
set_terminal dedup and its deliberate non-use on the plain-Stop path, and
the FinishState by-value snapshot vs live-read timing all verified.)

## Major
None.

## Minor
- m1 (Thermos B Medium; Synthitect S1) CompactionPassState kept the bag
  shape: all-pub fields, struct-literal construction, direct field pokes
  from the pass loop — asymmetrical with CompactionState's private fields
  + behavior methods in the same file, and the compaction.rs header's
  "field-bundle idiom is fully gone" claim overreaches because of it.
  Fix: private fields, CompactionPassState::new(message_count_before),
  behavior methods for the actual mutations (mark_sync_applied,
  remember_history_ref, record_tier, take_history_ref) + read accessors;
  header claim adjusted to match reality.
- m2 (Thermos B Medium) decide_pipeline_outcome's NotApplicable arm still
  inlines emit_post_compact (a hand copy, comment admits it) — a leftover
  of the bundle era; decide is an InnerSession method now and
  run_compaction_pass's early-exit branch already calls
  self.emit_post_compact with the same values. Fix: call
  self.emit_post_compact(trigger, None, pass.last_history_ref.clone(),
  &pass.destructive_tiers, pass.message_count_before) (post-m1 accessors),
  delete the inline copy + comment.
- m3 (Thermos B Low) PipelinePassContext carries the trigger twice
  (standalone field + loop_ctx.trigger; same value by construction at the
  sole site; decide never reads loop_ctx.trigger). Fix: drop the
  standalone field, read loop_ctx.trigger.
- m4 (Thermos B Low) stage_summary_request is a pure forwarding wrapper
  (mod.rs) giving staging two names. Fix: delete it; its sole caller
  (request_manual_compact) calls g.attach_summary_staging directly.

## Suggestions (accepted, NOT fixed — rationale recorded)
- s1 (Thermos B Low) post_pressure's Option + expect contract across
  modules: KEPT per plan D7, which names the NotApplicable
  post_pressure.expect(...) as a preserved quirk; the producer
  (matches!(outcome, NotApplicable).then(...)) guarantees the invariant,
  and lens A verified the pairing. Revisit only with a decide-signature
  redesign.
- s2 (Thermos B Low) suppress-flag lifecycle lives in
  retry_without_summary rather than in CompactionState: KEPT — the setter
  has exactly one writer today and begin_turn documents the reset
  contract; named-transition methods are a reasonable future polish, not
  a correctness or ownership gap.
- s3 (Synthitect) status.md bookkeeping — handled by the orchestrator at
  ship time.

## Thermos-specific
- Lens A verified clean (no JSON findings): sync-spill fusion order,
  snapshot_ids/ledger gating, staging statement order incl. ledger
  snapshot cloned AFTER add_history_ref, Applied counters unobservably
  reordered inside record_applied only, begin_turn placement identical to
  the old resets, pass state constructed outside the loop and carried
  across iterations in both versions, no widened visibility surface.
- Lens B verified clean: Flow is minimal (2 variants, 1 producer, 1
  consumer, no re-encoded outcome data — not a PipelineDecision rename);
  no stale references to any deleted type in src/ or tests/; finish.rs
  header accurate; impl-InnerSession split coherent with the codebase's
  existing impl-AgentEngine split.

## Policy verdict (plan D6, restated for the record)
src/policy.rs and src/compact/* are byte-identical to the base — the
scout proved policy a pure evaluator with no owned state to model, and
the ticket's framing sanctions call-shape structs there. No changes.

## Pre-existing, out of scope
compaction_boundary_test / pending_work targets under --features
lalr-harness: lalr_03/lalr_09/lalr_15 fail identically on pristine
aa6afce (verified by chunks 1 and 3 via stash). These targets are skipped
in the plain-config suites (required-features unmet) and are untouched by
this diff.

## Disposition
Fix pass complete (d68a48a): m1–m4 all fixed (CompactionPassState
encapsulated, emit_post_compact deduplicated onto the real method,
single trigger source in PipelinePassContext, staging wrapper deleted).
Final gate (orchestrator-run): 636 default / 606 no-default, 0 failed —
baseline preserved exactly. Open Critical/Major: 0.
