# Plan: ENC-305 — per-turn references by key + typed session snapshot (stacked on ENC-304/#36)

## Decisions
- D1 FfiModelRequest loses every JSON state field. Deleted: tools_schema_json,
  state_ledger_json, tool_context_index_json, file_context_index_json,
  state_ledger_delta_json, tool_context_index_delta_json,
  file_context_index_delta_json. Added: `active_tools: Vec<String>` (ToolName
  references — FFI records carry String names per ENC-299 grain; ToolName is
  built at conversion). Renamed: context_references: Vec<FfiContextReference>
  -> `context_reference_ids: Vec<String>` (references BY ID, per the ticket).
  Kept: session_messages, system_prompt, allow_tool_call, cache_hints,
  provider_context, retry_after_ms, model_retry_attempt.
- D2 Outbound conversion `From<ModelRequest> for FfiModelRequest` stays
  infallible (pure projection, no parsing): active_tools = names of
  tool_schemas ++ rendered_runtime_tools, deduped first-seen (the
  tool_context_index is derived from the same active set — build_model_request
  derives it from active_schemas — so it is not a separate source);
  context_reference_ids = context_references' ids in order. The old
  to_string().ok() -> None serializations DELETE with their fields.
- D3 Inbound `From<FfiModelRequest> for ModelRequest` is DELETED (with its
  silent-fallback rationale comment). Replacement: a fallible, session-bound
  conversion on the FFI type:
  `pub fn into_core(self, universe: &ToolUniverse, context_store: &ContextStore)
  -> Result<ModelRequest, AgentException>` (ffi/types.rs).
  Resolution rules:
  * active_tools: ToolName::new per name (blank -> AgentException::Internal
    naming the field, following tool_name_from_ffi precedent); a name resolves
    iff universe.entry(name) is Some, OR universe.spec(name) is Some
    (canonical or alias), OR name is in universe.active_names() (host-activated
    entry-less tools are legitimate tracked state). Resolved tool_schemas =
    entry(name) cloned, else SchemaEntry::stub(name) — the same shape
    render_active_schemas produces. A name resolving NONE of the three ->
    AgentException::Internal{detail} embedding UnknownToolReference
    (create_session precedent, ffi/mod.rs:84-89 style).
  * context_reference_ids: each id must exist in context_store
    (get) -> the store entry's reference populates context_references;
    a miss -> AgentException::ContextReferenceNotFound{reference_id}
    (the dedicated variant, matching read_context_reference).
  * session_messages: converted through a FALLIBLE message conversion —
    MessageMeta parsing must not unwrap_or_default on this path. Add a strict
    sibling where the grain allows (e.g. MessageMeta::from_json_str_strict or
    a fallible From-style helper in ffi/types.rs) and use it here; malformed
    metadata -> AgentException::Internal naming the message index. Other
    From<FfiAgentMessage> users (create_session etc.) are NOT changed.
  * state fields on the produced ModelRequest (state_ledger,
    tool_context_index, file_context_index, *_delta_json,
    rendered_runtime_tools) are None/empty BY CONSTRUCTION — engine-derived
    state is rebuilt by build_model_request per turn and never enters from a
    request. Document this on into_core.
- D4 Typed snapshot — core type in a new module src/session_snapshot.rs:
  `pub struct SessionSnapshot` (Serialize/Deserialize) carrying the full
  restorable session state: session: AgentSession, config: AgentConfig
  (the EFFECTIVE config — tool_semantics/tool_catalog sourced from the
  universe at export, so the snapshot is self-describing), capabilities:
  SessionCapabilities, provider: String, context_window_tokens: u32,
  system_prompt: String (the io triple; SessionIoParams is reconstructed at
  import), schemas: ToolSchemaSet, active_tool_names: Vec<ToolName> (the
  universe's live active set, authoritative over the schemas' initial set),
  context_store: ContextStore, state_ledger: StateLedger,
  guardrails: GuardrailState, loop_metrics: LoopMetrics, phase: EnginePhase,
  pending: PendingToolTurn, sidecar_builder: SidecarBuilder,
  model_requests_this_turn: u32, events: Vec<AgentEvent>, turn_index: u32,
  cancelled: bool, terminal_action: Option<AgentAction>,
  last_model_response_greedy_tool_names: Vec<ToolName>,
  tools_blocked_nudges_this_turn: u32, compaction_passes_this_turn: u32,
  destructive_tiers_this_turn: u32, suppress_summary_tier: bool.
  (Mirror state.rs:98-135 exactly; if a field proves non-restorable, report —
  do not silently drop it.)
- D5 Serde closures (add Serialize/Deserialize derives; all verified plain
  data): ToolSchemaSet (raw_active is derived state: serialize entries only,
  rebuild raw_active via entries_json on deserialize — custom impl or a
  serde helper; keep the type's other invariants), SchemaEntry if not already
  serde (check), ContextStore + ContextStoreEntry, SidecarBuilder +
  RollingCacheEntry (private struct — derive inside its module),
  GuardrailState, LoopMetrics + ToolSignatureWindow + PendingWorkItem,
  EnginePhase, PendingToolTurn, CompactionContext + CompactionTrigger.
  AgentAction: check existing serde status (ModelRequest/SummaryRequest are
  serde); if AgentAction is not serde, snapshot terminal_action via the same
  treatment its variants allow, else report.
- D6 Export/import on the engine (src/engine/):
  `AgentEngine::export_snapshot(&self, handle: &SessionHandle) -> SessionSnapshot`
  (infallible projection from live state; tool data sourced from the universe)
  and `AgentEngine::import_snapshot(&self, snapshot: SessionSnapshot)
  -> Result<SessionHandle, ToolSemanticsError>`.
  Import validation is EXACTLY construction's: ToolUniverse::build(schemas,
  semantics-from-snapshot-config) runs the full build validation; the
  snapshot's active_tool_names then replace the built universe's initial
  active set via a new `pub(crate) fn restore_active(&mut self, Vec<ToolName>)`
  on ToolUniverse (deduped through the existing merge helper; snapshot data
  came from a live universe, and construction imposes no membership rule on
  activation — same rule as activate()). Stored config/io are then reset
  exactly as create_session does (io.schemas emptied via SessionIoParams
  built from the snapshot triple + empty set; config.tool_semantics taken,
  tool_catalog defaulted). InnerSession is assembled directly (NOT via
  create_session: no ledger re-seeding, no counter resets).
- D7 FFI surface (ffi/mod.rs + types): `export_snapshot(session: &FfiSession)
  -> String` — the typed snapshot's canonical serde JSON (bulk state crosses
  this FFI as serde text of typed core values; create_session's
  session_context_json is the precedent) — serialization of the typed struct
  is infallible in practice; on the impossible error, return the JSON of a
  marker? NO: use Result<String, AgentException> if uniffi shape allows
  cleanly, else expect-free to_string via serde_json::to_string with
  unwrap_or_else to a typed-error JSON is FORBIDDEN (silent fallback) —
  implementer: pick Result<String, AgentException> for export.
  `import_snapshot(snapshot_json: String) -> Result<Arc<FfiSession>,
  AgentException>` — serde_json::from_str fallibly (bad JSON ->
  AgentException::Internal naming the failure; NO unwrap_or_default), then
  AgentEngine::import_snapshot with ToolSemanticsError mapped as
  create_session maps it (Internal{detail: "invalid tool semantics: {:?}"}
  style — follow the exact create_session wording for consistency).
- D8 NOT touched: core ModelRequest (typed render + provider-bound delta text
  stay), build_model_request, From<FfiSummaryRequest> (adjacent pattern —
  findings note only), tool_context.rs string API, MessageMeta::from_json_str
  itself (the strict path is additive).

## Acceptance scenarios (tests)
- tests/enc305_request_refs_test.rs (new) or ffi/types.rs module tests:
  * into_core with a dangling active tool name -> AgentException whose detail
    names UnknownToolReference + the tool (mirrors the create_session FFI pin).
  * into_core with a dangling context reference id ->
    AgentException::ContextReferenceNotFound with that id.
  * into_core happy path against a built universe + store: entry-bearing,
    catalog-only, and host-activated entry-less names all resolve (stub shape
    for the entry-less); context ids resolve to the store's references;
    produced ModelRequest has None state fields.
  * into_core with malformed message metadata_json -> AgentException (no
    silent default meta).
  * Outbound From: a core ModelRequest with schemas + runtime tools +
    references converts to name/id lists, deduped, order-preserved; the FFI
    struct has no *_json state fields (compile-level: fields absent).
  * Grep pin: no unwrap_or_default / .ok() remains in ffi/types.rs request
    conversions (state it in findings, verified by grep).
- Snapshot round-trip (tests/enc305_snapshot_test.rs, new): build a session
  (enclave fixtures), run turns including a tool result that activates a
  host-only tool and a spill creating context-store entries, export via
  AgentEngine::export_snapshot, serialize to JSON (simulated process death:
  drop the engine/handle), import into a FRESH AgentEngine, then assert
  behavioral identity: next_action produces a ModelRequest whose
  tools_schema_json / active advertised set / state_ledger presence /
  context references match the pre-export session's next request, and
  read_context_reference resolves a pre-export store entry.
  * Import validation: a snapshot whose semantics carry a dangling reference
    (tampered post-export) -> import Err(ToolSemanticsError::
    UnknownToolReference), mirroring create_session rejection.
  * FFI: export_snapshot/import_snapshot round-trip through JSON text;
    import_snapshot("not json") -> AgentException.

## Risks
- Serde derive additions can disturb existing serde shapes (AgentSession is
  already serde; adding derives to previously non-serde types is additive).
  ToolSchemaSet custom Deserialize must reproduce from_entries semantics
  exactly (raw_active = entries_json).
- PendingToolTurn/CompactionContext derives may hit a non-plain field deep in
  the graph (e.g. CompactionTrigger payload) — smallest faithful adaptation,
  reported explicitly; never silently drop a field from the snapshot.
- UniFFI: new export/import methods change the generated bindings surface —
  expected (ENC-294 host batch absorbs host-visible breaks).

## Chunks / commits (each referencing ENC-305)
1. Request redesign: FfiModelRequest fields + outbound From + into_core +
   strict message-metadata path + request tests.
2. Serde closures for the snapshot graph.
3. SessionSnapshot type + engine export/import + ToolUniverse::restore_active
   + FFI export/import + snapshot round-trip tests.
Gates after chunks 1 and 3: cargo test --locked AND --no-default-features,
0 failed, counts >= baseline 622/594 (see status.md). fmt clean.
