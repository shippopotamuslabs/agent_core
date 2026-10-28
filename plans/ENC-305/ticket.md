# ENC-305 — per-turn requests reference the ToolUniverse by key; typed session snapshot is the only state crossing (vision: Linear ENC-305)

Stacked on ENC-304 (PR #36). Child of ENC-284. Blocked by ENC-304 (landed). Design
ratified by Sean 2026-10-27. Priority High (closes the silent data-loss class Forge
flagged as High #1 + Medium #7 on PR #24).

Problem: FfiModelRequest carries session evolving state as JSON strings
(tools_schema_json, state_ledger_json, tool/file context index JSON + their deltas)
and its infallible From re-parses them per turn; parse failure degrades silently
(empty active set, None ledgers). Host-visible break lands in the ENC-294 batch.

Acceptance (from the ticket):
- No JSON state fields remain on the per-turn request type; no unwrap_or_default /
  .ok()-to-None parse exists on the request path.
- A dangling ToolName or context reference in a request returns the typed error,
  pinned by test.
- A snapshot round-trip (export, simulated process restart, import) preserves
  session behavior, pinned by test.
- Forge High #1 and Medium #7 on PR #24 resolve against this ticket.

Out of scope: FfiSummaryRequest's state_ledger_snapshot_json conversion (summary
path, not the model-request path — adjacent pattern, noted in findings, not fixed
here); core ModelRequest's typed render fields and its provider-bound *_delta_json
render text (engine-internal render output, not session state; the ledger they
diff lives in InnerSession).
