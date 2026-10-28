# ENC-302 Findings — dual review of f694650 (Option A)
Reviewers: Synthitect + Thermos x2. Gates: 615/0 default (orchestrator recount).

## Critical
None.

## Major
- J1 Engagement widened beyond plan: metadata-less {"status":"success"} (no total field, any tool/user text) now engages squash (messages DROPPED) + spill exemption + anchor. Base required total-field presence. Fix: engagement/exemption for metadata-less path restores total-field-presence guard; anchor existence follows the engaged set (metadata appends may lack total -> observed None). Squash drop behavior returns to base-identical.
- J2 (quality) semantic_success_total_blocks -> Option<Option<u32>> nested-Option trap + triple re-parse wrappers + duplicated tool_name scoping in tool_spill. Fix: single ObservedAppend enum + one message-level predicate shared by squash scan, anchor extraction, spill exemption.

## Minor
- m1 Integer-valued float totals (3.0) -> None; accept integral floats.
- m2 Stale `total_blocks` bindings/doc (ENC-288 comment) -> observed_* renames.
- m3 Missing stale-intermediate assertion (anchor still 3 before 9 observed) + snapshot fallback unknown untested -> add.
- m4 Config-shape test asserts helper only -> assert real ToolSemanticsConfig squash field or drop. Snapshot module cache disclaimer.
- Note (no fix): new serialization emits observed_total_blocks/null — host note in PR body (alias loads old).

## Disposition
J1+J2+m1-m4 fix pass.

## Fix pass (4e4b2af)
J1+J2+m1-m4 fixed; gates 620/0 + 592/0. Open Critical/Major: 0.
