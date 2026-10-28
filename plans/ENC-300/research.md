# ENC-300 Research (scouts)
- EnvelopeFormat today: Dsml/Mistral/ToolCall/Cdata/LiteRtCall/RawJson; sniff dispatch extract_envelopes first-match (TOOL_CALLS>DSML>tool_call>CDATA>call:>name-json). GLM = XmlToolCall sub-shape (<arg_key> payload).
- Structured-wins: leaked_tool_recovery::resolve_model_turn_text + envelope_ingest (native non-empty short-circuits text).
- Gate: hardcoded cross-format marker union (strong 7 + partial 6 + prefix table); FFI-only construction create_tool_stream_gate(bool), allow flag dead; engine never constructs gate.
- Config: no format field anywhere; AgentConfig reaches parse as catalog+semantics only. Fixtures all structured-path.
- Format tests enumerated per format in tool_parse_integration/tool_stream_gate/leaked_tool_recovery/engine_envelope_retry.
