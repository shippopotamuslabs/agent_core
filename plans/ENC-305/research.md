# ENC-305 Research (scout, spot-checked by orchestrator)

- FfiModelRequest (ffi/types.rs:194-211): 15 fields; the JSON state fields are
  tools_schema_json, state_ledger_json, tool_context_index_json,
  file_context_index_json + state/tool/file *_delta_json pass-throughs.
  Inbound From<FfiModelRequest> for ModelRequest (:213-248) parses with
  unwrap_or_default / .ok() -> empty active set / None ledgers on any failure;
  rendered_runtime_tools hardcoded Vec::new(). Comment at :213-224 states the
  silent-fallback rationale in code (deleted with the impl).
- KEY FACT: the inbound From has NO call site anywhere (src/tests/examples).
  The host never sends a request inbound through FFI: next_action converts the
  engine-built ModelRequest OUTBOUND (From<ModelRequest> for FfiModelRequest,
  :250-277); submit_model_response takes only a response. All three JSON state
  fields are engine->host echoes of engine-owned state.
- Authoritative state already lives engine-side: StateLedger in
  InnerSession.state_ledger (never rebuilt from the request); tool/file context
  indexes are REBUILT per turn in build_model_request (model_request.rs:101-124)
  from the universe + session_context — they are derived data, not state.
  Sidecar deltas derive from SidecarBuilder (InnerSession) vs the ledger.
- ToolUniverse seams (tool_universe.rs): require_entry (UnknownToolReference,
  "the ENC-305 seam"), entry/spec/active_names/activate/admit. No contains()
  accessor; membership observable via active_names().
- NO session export/import exists today (context_store.rs:15-23 design note
  says it belongs in a new snapshot import/export API — this ticket).
  AgentSession is serde (session.rs:674). InnerSession (state.rs:98-135) is not.
- Serde gaps for a full typed snapshot, all plain-data closable (spot-checked):
  ToolSchemaSet (entries serde-able; raw_active re-derivable via entries_json),
  ContextStore + ContextStoreEntry (BTreeMap<String, entry> + next_seq),
  SidecarBuilder + RollingCacheEntry, GuardrailState (4 u32), LoopMetrics +
  ToolSignatureWindow + PendingWorkItem, EnginePhase + PendingToolTurn
  (ModelRequest/SummaryRequest/RawToolCall already serde; CompactionContext is
  plain data needing derives incl. CompactionTrigger), ToolUniverse (snapshot
  stores schemas+semantics+active; catalog/index rebuilt by build()).
  AgentEvent already serde; AgentConfig serde (tool_catalog skip -> rebuilt).
- Context references: plain String IDs; ContextStore::get/contains exist
  (context_store.rs:142-148); engine read_context_reference already returns
  AgentError::ContextReferenceNotFound -> AgentException::ContextReferenceNotFound
  (dedicated variant, ffi/types.rs:1439). Dangling ToolName at FFI follows the
  create_session precedent: AgentException::Internal{detail} embedding
  UnknownToolReference Debug text (ffi/mod.rs:84-89; pinned by an FFI test
  asserting detail.contains("UnknownToolReference")).
- FFI session surface: all ops are FfiAgentEngine methods taking &FfiSession
  (ffi/mod.rs:64-178); snapshot export/import follow create_session's
  Result<_, AgentException> shape. Bulk state at this FFI historically crosses
  as serde JSON text of typed core values (create_session's
  session_context_json precedent).
- No test constructs FfiModelRequest or pins the silent degradation today.
- Baseline on this branch (ENC-304 tip): see status.md (expected 622/594).
