# ENC-299 Findings — dual review of a827033..HEAD (6024868, e7f1d8d)
Reviewers: Synthitect + Thermos x2. Orchestrator gates: fmt/clippy clean; 581/0 + 553/0. Field/section coverage audited complete.

## Critical
None.

## Major
- J1 gate_tool_name (model_intake) strips prefixes + (b)-fold only; repair resolution also snake-splits camelCase/punctuation.
  Model emitting webSearch resolves in canonicalize but gates as websearch -> spurious not-active; companion path can insert
  garbage identity. Same root at greedy matching + ad-hoc ToolName::new(&call.name).ok() sites (punctuated input silently misses policy).
  Fix: single model-output constructor (prefix-strip + snake + ToolName::new) used by gate/greedy/policy lookups of raw model names.

## Minor
- m1 validate_entries dedupes raw string, not folded ToolName (Web-Search + web_search both pass).
- m2 record_artifact_progress_with_parsed raw &str compare vs canonical (artifact_finalize.rs).
- m3 String plumbing: ToolResultRecord.activated_tool_names + greedy_tool_names Vec<String>; generic merge bridge; filter_map blank-drops. (ToolCallRecord.name String stays: history record.)
- m4 create_session computes initial_active_names twice; SchemaEntry::stub embeds raw name in value.
- m5 Rejection tests cover 2/8 sections; add parametrized all-sections test. ts_006 layer pin (serde error mentions field).

## Suggestions
- Shared strip-prefix helper; debug_assert invariant at policy blank-drop sites (subsumed by J1 constructor).

## Thermos-specific
- Byte-identity verified (Value passthrough); from_raw_lenient confined; alias-vs-canonical correct.

## Disposition
J1 + m1-m5 fix pass before ship. Behavior delta noted: punctuation-only model name now Rejects (was Resolved("")) — desired, documented in PR.

## Fix pass (b328ff0)
J1 + m1-m5 fixed; gates 588/0 + 560/0 (worker; orchestrator recount default). Open Critical/Major: 0.
Note: serde_path_to_error 0.1 added for field-path serde errors in from_json.
