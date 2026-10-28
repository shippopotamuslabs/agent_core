# ENC-299 Research (scouts, spot-checked)
- Name fields: ToolSpec.name/aliases; semantics tool_name x8 sites (discovery, artifact start/append, repairs, shapes, policies, squash append, greedy) + companion_callable_tools; SchemaEntry.name. Field/param names excluded.
- Three normalizers coexist: (a) full snake (canonicalize, model-output repair), (b) lowercase+[- ]->_ fold (catalog/semantics keys), (c) case-only fuzzy. ToolName canonical = (b)+trim. Model-output snake pipeline (a) stays in canonicalize (repair, not boundary).
- validate() checks dup/alias-alias/non-empty only; no membership check; alias-vs-canonical collision uncaught.
- Seam: create_session (fallible since ENC-297) sees catalog + io.schemas together. Enclave fixture refs (8 names) all in resources/tool_catalog.json (20 specs); test schema sets are subsets — union rule (specs ∪ schemas) is load-bearing.
- agnostic(): Option sections None — strict non-empty ToolName viable at boundary/serde.
