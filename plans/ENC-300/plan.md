# Plan: ENC-300 — host-declared tool-call format (stacked on ENC-299/#31)

## Decisions
- D1 `ToolCallFormat` core enum: Structured (default), Dsml, Cdata, BracketArray, XmlToolCall, RawJson, CallBrace (LiteRt `call: name {}` shape name).
  EnvelopeFormat renamed shape-only: Mistral->BracketArray, ToolCall->XmlToolCall, LiteRtCall->CallBrace (serde/FFI format_hint strings updated, dsml hint kept).
  GLM arg_key payload stays a sub-shape inside the XmlToolCall parser (deterministic within declared format; document).
- D2 AgentConfig.tool_call_format: ToolCallFormat (#[serde(default)] = Structured). FfiAgentConfig gains FfiToolCallFormat enum field mapped explicitly in agent_config(). NOT inside tool_semantics; provider string untouched.
- D3 Port: trait EnvelopeParser + one impl per envelope format wrapping existing extract_* fns; `parser_for(ToolCallFormat) -> Option<&dyn EnvelopeParser>` (None for Structured).
  extract_envelopes sniff chain DELETED. parse_tool_calls_with_semantics(format, raw, catalog, semantics): Structured => empty batch (no mining).
  FFI policy_bridge parse_tool_calls + has_local_tool_contract take the format (host declares per call); has_local_tool_contract checks declared-format markers only (Structured => false).
- D4 Engine ingest by declared format:
  * Structured: native calls only; body text never mined/truncated; no leak recovery. (ToolsBlocked when !allow_tool_call preserved.)
  * Envelope(fmt): ONLY fmt parser runs on text; text-parsed calls are the calls (native channel ignored — documented contract; re-pin cl_tr_02 under Structured).
    Declared strong marker present+malformed => existing ParseFailed. OTHER-format strong marker present while declared absent => ParseFailed naming declared format (loud, no fallback). No markers at all => plain text turn.
  * Leak-recovery visible-text truncation only in envelope mode, markers = declared format's set via shared marker fn.
- D5 Stream gate: ToolStreamGate::new(allow_tool_call, format). Marker tables tagged by format; detection/resolution consult only declared format's strong+tentative+prefix sets; Structured = pure pass-through (no holds, has_tool_marker=false at finish, trailing flush = all). FFI create_tool_stream_gate(allow, FfiToolCallFormat). Other-format text streams through unheld in envelope mode.
- D6 Argument repair (canonicalize/semantics coercion) untouched. No ENC-304/305 changes; From<FfiModelRequest> untouched.

## Chunks (sequential)
C1: format enum + renames + port + parse signature + config/FFI plumbing + engine ingest/recovery + test migration (each format test passes its format; engine DSML tests set config Dsml).
C2: stream-gate per-format markers + FFI gate signature + gate tests (declared-only holds; Structured pass-through; wrong-format streams unheld).
Acceptance: grep — no contains-dispatch in extract path; EnvelopeFormat shape names only; dual-form tests: Structured ignores DSML text (executes native, text intact), Dsml session given [TOOL_CALLS] text => ParseFailed (no BracketArray parse), declared-format malformed => ParseFailed. Gates both configs.
Commits per chunk referencing ENC-300. No push/PR/Linear. Enclave impact noted in PR (declare format per session) for ENC-294.
