# ENC-308 — research (all facts verified by the Orchestrator on main 6cadf23, 2026-10-28)

## Part A sites
- A1: `attach_summary_staging` redundant `trigger` param (src/engine/compaction.rs);
  suppress flag hand-toggled in `retry_without_summary` (src/engine/mod.rs);
  CompactionPassState history-ref accessors (src/engine/compaction.rs).
  CompactionState already owns the counters with accessors +
  `restore(passes, destructive, suppress)` (added during the #37 restack).
- A2: ToolSchemaSet hand-rolled serde via private `Wire {active, raw_active}`
  at src/tool_schema.rs:167-190. This type is inside the session snapshot's
  serialized form — byte drift breaks stored snapshots.
  ContextStore::validate_restored (src/context_store.rs:150-185) re-implements
  the `ctxref-` minted-ID grammar and returns ToolSemanticsError::InvalidSnapshot.
- A3: ffi/types.rs dedups at :283 (HashSet<ToolName>) and :323 (HashSet<&str>);
  outbound From<ModelRequest> uses Vec::contains. Canonical helper:
  `merge_tool_names_deduped` (src/tool_universe.rs:347, pub(crate)).
  import_snapshot is 108 lines in src/engine/mod.rs with 4 inline gates.

## Part B sites
- create_session (src/ffi/mod.rs:67-76): takes `session_context_json: String`,
  parses leniently `unwrap_or(Value::Null)`. Typed core struct EXISTS:
  SessionContext (src/session.rs:541) — typed fields + `extras` flatten map.
  Initial messages convert via lenient `From<FfiAgentMessage>`
  (MessageMeta::from_json_str, ffi/types.rs:65); the strict converter
  `agent_message_strict` exists (ffi/types.rs:222) and is used per-turn.
- MessageMeta (src/message.rs:30-51): typed fields + `#[serde(flatten)] extra`
  map. from_value pulls known keys out of the map; leftovers stay in extra.
  Its Deserialize impl is lenient by construction (stored-state reads).
- FfiSummaryRequest.state_ledger_snapshot_json (ffi/types.rs:814); inbound
  parse `.ok()` at :832 -> silent None; outbound serialize `.ok()` at :852.
  StateLedger is engine-owned (state_ledger.rs; includes the
  `retirement_artifacts: Vec<String>` provenance field from cf3f523).
- FfiToolResultRecord conversion (ffi/types.rs:600-613): filter_map drops
  invalid activated_tool_names silently; comment ratifies dropping blanks.

## Validation model (for the docs chunk)
- No JSON Schema validator exists in the crate. Enforcement at ingress:
  tool_parse checks against the catalog's ToolSchemaHint — required fields
  present/non-empty (MissingRequiredParameter), unknown params rejected
  (validate_unknown_and_duplicate vs allowed = required + defaults keys),
  defaults injected, semantics-driven coercion. The JSON Schema document
  itself is rendered to the provider (the model's copy of the map).

## Stored-state audit seeds (invariant-audit skill)
- Snapshot JSON produced by 6cadf23 code, including: ToolSchemaSet wire
  form; a SessionContext WITH extras keys; message metadata WITH extra
  keys; a state ledger with retirement_artifacts. These fixtures are the
  read-path tests for chunks A2/B1/B2: old rows must import; engine-owned
  meaning must survive; host extras are dropped by design (Sean's ruling),
  never silently mutated into engine fields.
