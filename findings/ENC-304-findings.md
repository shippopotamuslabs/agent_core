# ENC-304 Findings — dual review of d7acf2c..HEAD (c5e66b0, 02b39d1, c8122b2, d9f3153)
Reviewers: Synthitect (fresh context) + Thermos lens A (correctness) + Thermos lens B (structure). Orchestrator gates at review time: fmt clean; 609/0 + 581/0 (baseline 588/560). All three reviewers converged on the same three Majors; deduped below.

## Critical
None.

## Major
- M1 admit() runs no shape validation on the SchemaEntry candidate (tool_universe.rs:175-246).
  An empty-sentinel name (SchemaEntry::from on a nameless Value), a non-object value, or a
  name/value-divergent entry is admitted silently: the empty name is skipped by the
  schema_index insert yet lives in schemas.active, leaks through render_runtime_view, and
  resyncs raw_active to text ToolSchemaSet::parse would reject. Violates ticket invariant 1
  (admission runs full validation: shape + referential integrity) on the one new boundary.
  Fix: reject at the top of admit — empty name, non-object value, value["name"] not
  canonicalizing to entry.name — typed error, world unchanged; reuse the single-entry
  shape rules behind validate_entries so parse/admission rules cannot drift.
- M2 build() does not validate the schema set (tool_universe.rs:48-72). Canonical
  duplicates enter via the FFI typed path (tool_schema_entries dedupes on the RAW string,
  ffi/types.rs:845): "Web-Search" + "web_search" both pass, canonicalize identically;
  schema_index or_insert is first-wins where the pre-ENC-304 render map (collect) was
  last-wins — a silent precedence flip — and active_names carries the name twice.
  Fix: build rejects duplicate canonical names and empty names in schemas.active with a
  typed error (create_session already returns the ToolSemanticsError family); build-level
  test with a from_entries set carrying case variants.
- M3 Two live sources of truth for the tool world (engine/mod.rs:157-167, state.rs:115-124).
  Chunk 2 cloned io.schemas/config.tool_semantics into build instead of consuming them;
  guardrails (guardrails.rs:46), micro compaction (micro.rs:53-59,106 — incl. rebuilding
  its own catalog from config), and artifact squash (artifact_squash.rs:239) read the
  stored config copies in live sessions. admit() mutates only the universe, so admission's
  first production caller would silently desync compaction/guardrails from the request
  path. Latent today (no admit caller; copies equal at construction) but it is exactly
  the caller-discipline failure the model exists to eliminate.
  Fix (orchestrator decision): complete D3 as written — create_session consumes the
  inputs (stored io.schemas/config.tool_semantics/tool_catalog become documented
  boundary-input shape), and the tool-data consumers receive universe-derived data at
  the dispatch boundary: an InnerSession helper builds the effective config
  (stored config clone with tool_semantics/tool_catalog replaced from the universe)
  for the compaction pipeline run, and guardrail calls (engine/mod.rs:276,
  finish.rs:186) are fed the universe's semantics. The compaction tier trait and the
  tiers' direct unit tests keep their &AgentConfig shape (ENC-306 owns that redo).

## Minor
- m1 admit() hand-mirrors ToolSemanticsConfig::validate with linear scans and commits
  via the unchecked ToolCatalog::from_specs after mutating (tool_universe.rs:186-262).
  Restructure to validation-by-construction: clone semantics, push candidate spec,
  ToolCatalog::from_semantics(&clone)? (the canonical rule set, failure-atomic),
  then commit entry + semantics + catalog together. The entry-vs-existing-alias check
  stays explicit (entries are not semantics), resolving the owner via catalog.resolve
  so AliasCollision.first_tool names the owner (fixes the first_tool=alias shortcut).
- m2 Spec/entry name mismatch reported as UnknownToolReference{section:"admit"}
  (tool_universe.rs:213-219) — misclassifies a malformed candidate as a dangling
  reference on the error family ENC-305 will branch on. Add a dedicated
  ToolSemanticsError variant carrying both names (AdmissionNameMismatch{entry, spec}).
- m3 A spec alias can claim a host-activated entry-less active name (Thermos A):
  alias loop never consults self.active. Reject alias == an active name other than
  the candidate's own (AliasCollision), with a world-unchanged test. (Admitting an
  ENTRY for an active entry-less name stays legal — it upgrades a stub to a schema.)
- m4 prepare_tool_semantics is dead code that still writes config.tool_catalog
  (session.rs:285-298; only remaining references are docs at tool_semantics.rs:146
  and catalog.rs:38, both now stale). Delete the method; update both docs to name
  ToolUniverse::build as the construction-time validation point.
- m5 Module doc on tool_universe.rs:1-14 must state the post-M3 contract exactly
  (universe = the session's live tool world; config/io tool fields = boundary input
  consumed at construction). RepairTurnInputs' semantics/catalog fields get a
  provenance doc (turn-start clones of universe data, taken before the &mut borrow).
  ToolUniverse::schema_hint documents its canonical-only contract (aliases resolve
  via spec()/catalog, as before — preserved behavior, not a regression).

## Suggestions
- s1 merge dedupe lives twice: ToolUniverse::activate and the hand copy for
  result.activated_tool_names in activate_tools_for_spill_reference (mod.rs:88-104).
  Give it one home: a crate-private merge helper both sides call.
- s2 (declined) Trim integration tests duplicating in-module scenarios (Thermos B):
  the integration copies run against the enclave fixture world (20 specs) while the
  unit copies run against minimal worlds — different inputs, both cheap. Kept.

## Thermos-specific
- Lens A verified sound: build validation order == pre-304 create_session; push_entry
  raw_active resync identical to from_entries; admit's fallible checks all precede
  mutation (failure-atomic in practice); model_intake re-borrow preserves
  merge-then-gate semantics; no panics/unwraps in new production code.
- Lens B: encapsulation otherwise disciplined (private fields, no &mut leak; all
  engine tool-data reads rewired); accessors return pub-field interior types
  (schemas()/semantics()/catalog()) — a discipline risk only, since InnerSession is
  pub(crate). Follow-up material, not this ticket.

## Disposition
Fix pass required: M1, M2, M3, m1–m5, s1 + new tests (empty-name admission,
name/value divergence, canonical-duplicate build, alias-vs-alias via validate path,
alias-vs-active-name, render byte-identity: universe render == ToolSchemaSet render
on the same input; active_json equality before/after failed admission).
Open Critical/Major must be 0 before ship.

## Fix pass (383da49, b1857dc)
M1 + M2 + M3 + m1–m5 + s1 fixed. Orchestrator gates after fix pass: 622/0 default,
594/0 no-default-features (baseline 588/560; +34/+34 across the ticket), fmt clean.
Open Critical/Major: 0.
- M1: admit shape gate (validate_schema_entry_shape) — empty name / non-object value /
  name-value divergence rejected with new ToolSemanticsError::InvalidSchemaEntry{message}
  (family's Invalid* convention; sharing validate_entries would have distorted its
  channel-shaped error surface — mirror kept to one helper).
- M2: build rejects empty + duplicate canonical schema names (DuplicateToolName),
  placed after validate_references so pre-existing error order is preserved (pinned).
- M3: create_session consumes the inputs per D3 (stored io.schemas emptied,
  config.tool_semantics agnostic, config.tool_catalog defaulted);
  InnerSession::effective_tool_config() derives tool data from the universe at the
  dispatch boundary for run_plan, manual-compact summary tier, both guardrail
  allow_tool_call sites, tool_budget_snapshot/on_tool_turn_finished paths, and
  should_stop_tool_rounds. Compaction tier trait unchanged (ENC-306 owns that redo).
- m1: admission validates by construction (candidate semantics clone ->
  ToolCatalog::from_semantics -> atomic commit). m2: AdmissionNameMismatch{entry,spec}
  variant. m3: alias claiming an active entry-less name rejected; entry-for-active-name
  stays legal. m4: prepare_tool_semantics deleted, stale docs repointed to
  ToolUniverse::build. m5: module/field provenance docs. s1: merge_tool_names_deduped
  single home for activate + result-record merge.
