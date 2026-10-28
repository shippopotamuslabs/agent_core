# ENC-296 Research (orchestrator synthesis of scout reports, spot-checked)

What exists: ToolSchemaSet{active,registry,catalog} (src/tool_schema.rs:130-137) with parse/parse_lenient/from_entries
taking a registry channel; render path merges registry entries for newly activated names (tool_schema.rs:369-374)
and stubs missing ones; registry_json()/has_registry() feed tool_context stash (tool_context.rs:109-128).
FfiSessionIoParams.tool_schema_registry: Vec<FfiToolSchemaEntry> (ffi/types.rs:786) -> from_entries(active,registry,catalog) (875).
ContextReferenceKind::ToolSchema/ToolCatalogIndex minted by tool-context index build; engine state/model_request/sidecar/activation touch registry.
Direction (ratified, no alternative open): delete registry channel end-to-end; activated tools render from active channel or name stub;
delete both host-owned reference kinds + stash; FFI loses registry field and parameters_json.
Risks: lazy-disclosure tests pin registry-merge order (must re-pin to active/stub semantics, not weaken);
public string wrappers build_tool_context_index* keep boundary behavior minus registry; Forge API break is intended (flagged on ticket).
Gate: vision approved by mission. ENC-304/305 out of scope.
