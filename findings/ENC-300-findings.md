# ENC-300 Findings — dual review of d7acf2c..HEAD (d73c7f1, 434bb8e)
Reviewers: Synthitect + Thermos x2. Orchestrator gates: fmt/clippy clean; 597/0 + 569/0. Live paths sniff-free (verified).

## Critical
None.

## Major
- (Intended, document-do-not-fix) Persisted configs without tool_call_format default to Structured and stop mining text; FFI surface breaking
  (new field/params). Both per ratified design; host migration note goes in PR body + ENC-294 comment. NOT code defects.
- J1 Dead union marker API retained + re-exported; resolve_dsml_partial still confirms against UNION set (Dsml hold confirmable by <tool_call text),
  violating declared-only streaming. Fix: delete union fns, migrate 3 test files to _for variants, declared-only dsml-partial resolution.

## Minor
- m1 ParseFailed arms return native_tool_calls (envelope_ingest.rs:105) vs native-ignored contract -> Vec::new().
- m2 extract_bracket_array sentinel-miss parses from 0 instead of loud absent-marker error (FFI path).
- m3 has_local_tool_contract markers too loose ("name", "call:") -> tighten to {"name" / call:\s*\w+\s*\{.
- m4 EnvelopeParser::envelope_format() unused; hint mapping duplicates match -> use it or drop it.
- m5 Dsml-spaced normalization runs for all formats -> gate on Dsml-declared (engine/recovery paths).
- m6 Test/fn naming leftovers mistral/litert/glm -> shape names. Gate-level wrong-format-unheld test missing for non-Structured gate.
- m7 FFI parse_tool_calls markerless-text failure contract: document on the fn (loud by design).

## Suggestions
- RawJson ^-anchor vs unanchored extract mismatch (pre-existing) — follow-up, out of scope.

## Disposition
J1 + m1-m7 fix pass. Migration note in PR body.

## Fix pass (01c269b)
J1 + m1-m7 fixed; gates 599/0 + 571/0. Intended-Major items documented (migration note in code + PR). Open Critical/Major: 0.
