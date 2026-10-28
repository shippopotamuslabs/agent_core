# ENC-304 — ToolUniverse: the session tool world as a rich domain model (vision: Linear ENC-304)

Stacked on ENC-299 (PR #31). Child of ENC-284. Design ratified by Sean 2026-10-27.

The session tool universe — every tool the engine can deterministically act on, with its
schema entry, semantics, and spec — becomes a single rich domain model owned by the
session, built once at construction from the strongly typed boundary params (ENC-295
entries; ENC-297 semantics + specs), keyed by ToolName (ENC-299). All validation lives
inside the model; nothing outside it mutates it.

Invariants:
1. Expansion is admission, and admission is the only growth path for world entries.
   Admission runs full validation (shape + referential integrity) on the candidate.
   Mid-session activation (ENC-296 discovery flow) goes through the model, not
   channel mutation.
2. Every reference resolves or errors: lookups by ToolName; a miss returns the typed
   error (UnknownToolReference family). No empty/default substitute.
3. Validation is internal: construction and admission are fallible and own their
   checks (duplicates, dangling references, alias resolution). Callers never
   re-validate world data.

Out of scope: ENC-305 (per-turn references + typed snapshot; FfiModelRequest From
untouched here), ENC-306. No bare-string restore.
