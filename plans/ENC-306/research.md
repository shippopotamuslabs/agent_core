# ENC-306 Research (scout, spot-checked by orchestrator)

- PipelineOutcomeInputs: engine/compaction.rs:203-225 (22 fields, ~10 &mut of
  InnerSession + split pending.summary_req/ctx + 4 &mut to run_compaction_pass
  LOCALS sync_applied/last_history_ref/destructive_tiers + values). One
  construction site (mod.rs:1330-1355); decide_pipeline_outcome
  (compaction.rs:234) destructures it and matches CompactionOutcome in 4 arms
  (:262-339) performing effects incl. *phase write, returning PipelineDecision
  (3 variants, compaction.rs:228-232); dispatch re-matches PipelineDecision at
  mod.rs:1356-1362 (the re-encoding). The compaction.rs header (:1-8) documents
  the bundle idiom as an invariant ("no &mut InnerSession... phase effects are
  returned, never applied") — already violated by decide (*phase write :285).
- FinishState: engine/finish.rs:28-49 (20 fields: 10 &mut, 6 &, 4 values —
  20 of InnerSession's 22 fields, all but cancelled +
  tools_blocked_nudges_this_turn; pending borrowed whole). Built ONLY by
  InnerSession::finish_state() (mod.rs:1020-1042), consumed by exactly two
  functions in finish.rs: finish_after_model_stop (by value, returns ()) and
  before_finish_pending_work_action (consume-and-return). Finish mutates:
  phase, terminal_action, pending.{execute,execute_skip_guardrail_count,
  auto_finalize_finish,call_model}, session.messages (reminder), events,
  state_ledger, model_requests_this_turn. It never operates context_store /
  sidecar_builder directly (forwards them into ModelRequestInputs).
  finish.rs:51 has a private set_terminal duplicating InnerSession::set_terminal
  (mod.rs:1083).
- SyncSpillInputs (compaction.rs:29-37, 7 fields) + SyncSpillEffect (:41-47):
  collect_sync_spill_effect (:49-100) MUTATES session/store directly (spill +
  snapshot inserts) and returns an effect bag; dispatch (mod.rs:1295-1311)
  applies the bag (compaction events, history ref -> ledger + snapshot ids).
  Dead weight: effect.spill_applied is never read anywhere; snapshot_ids is
  consumed only when a history ref exists. One call site (mod.rs:1278-1294).
- StageSummaryInputs (compaction.rs:102-111, 7 fields): attach_summary_staging
  (:112) has TWO construction sites — inside decide_pipeline_outcome
  (compaction.rs:270-280) and InnerSession::stage_summary_request
  (mod.rs:1054-1064, manual-compact path).
- Compaction persistent state today: InnerSession.compaction_passes_this_turn
  (:121) + destructive_tiers_this_turn (:124) — incremented ONLY in decide's
  Applied arm, reset ONLY in step_idle (mod.rs:1374-1375), read as telemetry
  by model_request.rs:157-158 — and suppress_summary_tier (:125), written only
  by retry_without_summary (mod.rs:1072/1080), read by make_ctx/build plan.
  Per-pass working state exists only as run_compaction_pass locals
  (mod.rs:1165-1168). Tiers (src/compact/*) are stateless unit structs taking
  (session, &config, &ctx) — NOT bundles of InnerSession; out of scope.
- Policy verdict (evidence-complete): PURE evaluator, no state. Zero
  statics/interior-mutability/&mut-self in src/policy.rs + src/compact/policy.rs;
  FireConditionInputs is all &/Copy (no &mut), MidTurnStallGate is 5 bools by
  value (call-shape structs, per the ticket's own framing); ContextPolicy is
  returned fresh per call and never stored. DECISION: policy stays a pure
  evaluator; no changes.
- Other bundles exist (ModelRequestInputs 15, RepairTurnInputs, ApplyProcessed-
  Inputs, ProcessOneResultInputs, AppendAssistantInputs, IngestArmInputs,
  RejectFlushInputs) — intake/tool-results/model-request modules, NOT in this
  ticket's named scope (compaction/finish/policy). No test constructs any
  bundle directly (all pub(super)); behavior is pinned through the public
  engine API (finish_boundary_test, compaction_triggers_test,
  compaction_boundary_test, manual_compact_test, state_ledger tests).
- Baseline on this branch (ENC-303 tip aa6afce): 636/0 + 606/0 (ENC-303's
  orchestrator verification; re-verified before implementation — status.md).
