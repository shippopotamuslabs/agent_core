# Plan: ENC-306 — module-owned state for compaction + finish (stacked on ENC-303/#35)

## Decisions
- D1 Compaction owns persistent state: new `CompactionState` in
  src/engine/compaction.rs (grain: engine-module state type; state.rs holds
  the field):
  ```rust
  pub(super) struct CompactionState {
      passes_this_turn: u32,
      destructive_tiers_this_turn: u32,
      suppress_summary_tier: bool,
  }
  ```
  Private fields; methods: `begin_turn(&mut self)` (zeroes both counters —
  called from step_idle where the resets live today; suppress flag NOT reset
  by begin_turn: today it is written only by retry_without_summary),
  `record_applied(&mut self, destructive: bool)` (passes += 1; destructive
  count += 1 iff destructive — the Applied arm's exact effects),
  `passes_this_turn()`, `destructive_tiers_this_turn()`,
  `suppress_summary_tier()`, `set_suppress_summary_tier(&mut self, bool)`
  (retry_without_summary's writes become this call).
  InnerSession: the three fields DELETE, replaced by `compaction:
  CompactionState`. Every reader rewires: model_request.rs telemetry
  (passes/destructive counts into ProviderContextSnapshot), step_idle resets,
  make_ctx + build_compaction_plan reads of suppress flag, finish's
  forwarding (it passes the two counters into ModelRequestInputs — reads via
  accessors), retry_without_summary.
- D2 The compaction operations become `impl InnerSession` methods IN
  src/engine/compaction.rs (a second impl block; mod.rs keeps dispatch/
  coordination). This supersedes the compaction.rs header invariant ("no &mut
  InnerSession, phase effects returned never applied") — rewrite the header
  to state the new idiom: module state lives in CompactionState, operations
  are InnerSession methods defined in the module that owns them, bundles are
  gone. Methods:
  * `fn sync_spill_pass(&mut self, pressure_total_tokens: u32)` — the FUSED
    collect+apply: guards on enable_context_store as today (call site keeps
    the guard or the method does — match today's structure), runs
    spill_tool_messages_in_session + snapshot_history_range against
    self.session/self.context_store, and applies effects directly:
    push_compaction_event + emit for spill/snapshot events,
    remember_history_snapshot_ids, state_ledger.add_history_ref — in today's
    exact order (mod.rs:1295-1311 + compaction.rs:59-99). Takes the per-pass
    state as a parameter: `pass: &mut CompactionPassState` (D3).
  * `fn decide_pipeline_outcome(&mut self, outcome: CompactionOutcome,
    pass: &mut CompactionPassState, pass_ctx: PipelinePassContext) ->
    Result<Option<AgentAction>, AgentError>` — the 4-arm match ONCE, effects
    applied directly (session replace, events, counters via
    self.compaction.record_applied, pending summary writes, phase write,
    boundary recording in the NotApplicable arm). LoopAgain is expressed by
    the CALLER's loop structure: run_compaction_pass restructures so the
    outcome decision returns `OutcomeFlow` — NO: see D4.
  * `fn attach_summary_staging(&mut self, sess: AgentSession, req:
    SummaryRequest)` — staging as a method (replaces StageSummaryInputs at
    BOTH sites: decide's NeedsHostCall arm and stage_summary_request, which
    itself becomes a thin caller or is folded into request_manual_compact's
    path — keep stage_summary_request as the manual path's entry if other
    callers use it; it calls the method).
  Small pure helpers in compaction.rs stay free functions:
  apply_manual_focus_instruction, probe_turn_flags, apply_force_override,
  last_message_role (no InnerSession state — not bundles).
- D3 Per-pass working state is an OWNED struct, not a borrow bundle:
  `CompactionPassState` in engine/compaction.rs — OWNS its fields (no
  lifetimes): sync_applied: bool, last_history_ref: Option<String>,
  destructive_tiers: Vec<CompactionTier>, message_count_before: u32.
  Constructed by run_compaction_pass (which already creates these as
  locals), passed &mut to the D2 methods. It is pass-scoped domain state
  with a home and a name, distinct from the deleted bundles in kind: it
  mirrors NO InnerSession fields.
- D4 PipelineDecision DELETES. decide_pipeline_outcome returns
  `Result<OutcomeStep, AgentError>` where OutcomeStep is a 2-variant
  module-local enum { LoopAgain, Done(Option<AgentAction>) }? — that is a
  rename, not a deletion. INSTEAD: run_compaction_pass's loop calls a single
  method `fn run_pipeline_once(&mut self, trigger, pass) ->
  Result<Option<AgentAction>, AgentError>`?? — too big a restructure of the
  pass loop (plan building, fire gates, spill, pipeline run all live in the
  loop body in mod.rs).
  FINAL SHAPE: decide_pipeline_outcome(&mut self, ...) ->
  Result<DecisionFlow, AgentError> is rejected per above. The dispatch loop
  in run_compaction_pass becomes:
  ```
  match self.decide_pipeline_outcome(outcome, &mut pass, ctx) {
      Ok(Flow::Loop) => continue,
      Ok(Flow::Done(action)) => return Ok(action),
      Err(e) => return Err(e),
  }
  ```
  with `Flow` a private 2-variant enum in compaction.rs. Rationale recorded
  in findings if reviewers push: the ticket kills the RE-ENCODED 4-arm match
  (two matches saying the same thing) and the 22-field bundle; a minimal
  loop-control signal at the pass-loop boundary is ordinary control flow,
  not a re-encoding — the 4-arm CompactionOutcome match now exists exactly
  once, inside the model. (If the implementer finds the loop can be
  restructured so decide returns Result<Option<AgentAction>, AgentError>
  with loop-again distinguished by a pass flag WITHOUT changing iteration
  semantics, prefer that and report it.)
- D5 Finish: FinishState + InnerSession::finish_state() DELETE.
  finish_after_model_stop and before_finish_pending_work_action become
  `impl InnerSession` methods IN src/engine/finish.rs:
  `fn finish_after_model_stop(&mut self, reason: FinishReason)` and
  `fn before_finish_pending_work_action(&mut self) -> Option<AgentAction>`
  (no consume-and-return). Call sites in mod.rs (:351, :367) become
  `g.finish_after_model_stop(reason)`. Inside finish.rs, field accesses
  become self.*; the ModelRequestInputs construction reads self.compaction
  counters via D1 accessors and self.universe-equivalents as they exist ON
  THIS BRANCH (active_tool_names field, config.tool_semantics — this branch
  predates ENC-304; do NOT import ToolUniverse concepts). finish.rs's
  private set_terminal: delete it in favor of InnerSession::set_terminal
  (mod.rs:1083) IF the semantics match at every call (check all 4 call
  sites: :64, :74, :82, :123 — the finish one sets phase Idle + terminal;
  if InnerSession::set_terminal does the same, use it; else keep a method
  `fn set_terminal_action(&mut self, action)` on InnerSession in finish.rs
  — report the choice). Header docs in finish.rs updated to the new idiom.
  Verdict recorded: finish owns NO persistent fields — the state it operates
  (phase, pending, terminal_action) is turn-coordination state shared with
  dispatch (20+ write sites in mod.rs); forcing it into a finish-owned
  struct would invert the coordinator relationship. Its domain model is the
  transition operation set itself. (Same judgment pattern the ticket
  sanctions for policy.)
- D6 Policy: NO CHANGES. Pure evaluator verdict (research.md evidence):
  FireConditionInputs/MidTurnStallGate are call-shape structs over plain
  data; ContextPolicy is per-call; nothing is stored. Recorded in findings.
- D7 Behavior preservation: every effect, event, error, and ordering from
  the scout's arm-by-arm table (research / compaction.rs:262-339,
  mod.rs:1295-1311) must be preserved exactly — including quirks:
  post_pressure.expect(...) in the NotApplicable arm, spill_applied
  re-derivation, snapshot_ids consumed only under a history ref,
  forced_by_non_ratio trigger restoration in NeedsHostCall,
  destructive-tier recording order. The dead spill_applied FIELD of
  SyncSpillEffect disappears with the struct (it was never read); the pass
  state's sync_applied is the live one the NotApplicable arm reads — as
  today (dispatch local).
- D8 Out of scope: ModelRequestInputs, RepairTurnInputs, ApplyProcessed-
  Inputs, ProcessOneResultInputs, AppendAssistantInputs, IngestArmInputs,
  RejectFlushInputs (intake/tool-results/model-request modules — not named
  by the ticket); src/compact/* tiers (stateless, individual args);
  ToolUniverse stack (different branch lineage).

## Acceptance scenarios
- Both suites green with counts >= baseline 636/606, 0 failed; fixture
  suites (compaction_triggers, compaction_boundary, finish_boundary,
  manual_compact, state_ledger, pending_work) unchanged and passing.
- Grep pins: PipelineOutcomeInputs, PipelineDecision, FinishState,
  SyncSpillInputs, SyncSpillEffect, StageSummaryInputs — zero definitions
  or uses remain in src/. finish_state() — gone.
- No new bundle structs introduced (PassState/CompactionState own their
  fields; no lifetime-mirror structs added).

## Risks
- Borrow checker: &mut self methods that also need owned values computed
  from self (loop_ctx: CompactionContext is already an owned value in the
  loop — fine). decide's NeedsHostCall arm calls attach_summary_staging
  (another &mut self method) while holding `sess`/`req` by value — fine.
  The Applied arm replaces *session while events borrow — sequential, fine.
- run_compaction_pass loop restructure (D4) must preserve iteration
  semantics exactly: LoopAgain falls through to the NEXT iteration with
  pass state carried — the pass-state-as-parameter design keeps this.
- suppress_summary_tier readers in model_request/finish forwarding: two
  counters + flag move behind accessors; ProviderContextSnapshot values
  must be identical.

## Chunks / commits (each referencing ENC-306)
1. CompactionState + PassState + sync-spill fusion (SyncSpill* deleted).
2. decide_pipeline_outcome as InnerSession method (PipelineOutcomeInputs +
   PipelineDecision deleted) + attach_summary_staging method
   (StageSummaryInputs deleted).
3. Finish methods (FinishState + finish_state() deleted).
Gates after each chunk: cargo check both configs; full suites after
chunks 2 and 3 (orchestrator-run), 0 failed.
