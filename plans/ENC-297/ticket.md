# ENC-297 — typed tool semantics/catalog at UniFFI boundary (vision: Linear ENC-297 + ToolUniverse addendum)
Delete tool_semantics_json + tool_catalog_json strings; FfiToolSemantics record tree + Vec<FfiToolSpec> sibling;
engine holds ToolCatalog + ToolSemanticsConfig separately (merge deleted); invalid input -> AgentException
(no eprintln fallback); empty_append_payload_value typed rep chosen+recorded; rendered_tool_catalog_json renamed.
Stacked on ENC-296 (PR #29). Out of scope: ENC-304 full ToolUniverse, ENC-305, ENC-299 validation (next, on this surface).
