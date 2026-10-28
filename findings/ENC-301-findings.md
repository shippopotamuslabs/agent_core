# ENC-301 Findings — dual review of 014065d
Reviewers: Synthitect + Thermos x2. Gates: 603/0 + 575/0, fmt/clippy clean. Rule fidelity high.

## Critical
None.

## Major
- J1 Summary-sourced reversal:/resolved: retire LIVE entries (no source gate) — stale destruction, same clobber class as objective rule. Fix: retirement arms Live-only; Summary retirement tags ignored (live original already observed incrementally).
- J2 Tag matching lacks left word-boundary: Unresolved:/Indecision: false-fire. Fix: require start-or-non-alphanumeric before tag.
- J3 Nested tags double-fire: scanner advances past prefix only, inner quoted reversal: inside a value retires live facts. Fix: advance past extracted value span.

## Minor
- m1 touched_files tombstone-filtered though no tag targets them -> scope filter to decisions/constraints/open_questions.
- m2 normalize_key: strip trailing ?/. so Resolved: x matches stored "x?".
- m3 Unmatched-reversal repeat self-cancels (appended copy retired on repeat) -> repeat must be no-op, entry stays.
- m4 parse_lines dead + suppressor -> delete. Case-insensitive retirement test missing -> add.

## Suggestions
- O(n·m) normalize recompute (fine at ledger scale, waived); tombstone cap waived per plan.

## Disposition
J1-J3 + m1-m4 fix pass.

## Fix pass (3d71f32)
J1-J3 + m1-m4 fixed (red-state verified 6/15 fail pre-fix); gates 609/0 + 581/0. Open Critical/Major: 0.
