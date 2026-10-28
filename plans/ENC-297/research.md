# ENC-297 Research (scout synthesis, orchestrator)
- Catalog conflation mapped: ToolSchemaSet.catalog entries have NO per-turn reader; only guards read it:
  catalog_has_entries (tool_context index guard), has_catalog (model_request tool_schemas gate), and
  session.rs prepare_tool_semantics merge_catalog_json(catalog_json) into ToolSemanticsConfig.catalog.
  rendered_tool_catalog actually renders the ACTIVE channel via render_runtime_view (misnamed).
  FfiModelRequest carries no catalog field (dropped both directions).
- merge_catalog_json accepts [ToolSpec] array or {tools:} object; no-op empty; ToolSpec{name,aliases,reproducible,schema_hint} (catalog.rs).
- FfiAgentConfig.tool_semantics_json -> From parses + eprintln/agnostic fallback (to delete).
- Direction: FFI typed tree + Vec<FfiToolSpec> sibling on FfiAgentConfig; core holds ToolCatalog + ToolSemanticsConfig
  separately on AgentConfig; ToolSchemaSet catalog channel deleted; guards re-source to ToolCatalog presence;
  rendered_tool_catalog renamed rendered_runtime_tools. Gate: mission-approved.
