# ENC-296 — host owns the tool registry (vision, verbatim source: Linear ENC-296 + mission addendum)

Goal: remove the registry channel from SessionIoParams/ToolSchemaSet; delete render-from-registry
(newly activated tool renders from active channel or name stub); tool-context index no longer stashes
registry text; ContextReferenceKind::ToolSchema and ToolCatalogIndex removed; ModelRequest carries
active set only. UniFFI: FfiSessionIoParams loses tool_schema_registry; FfiToolSchemaEntry loses
parameters_json (engine-consumed fields only: name, description, reproducible, deferrable, max_result_bytes).

Vision: state/data belong to whoever created them; host owns registry/catalog/search; engine owns
state it creates; malformed boundary input -> typed error; engine never guesses (no sniffing/precedence).

PR-strategy deviation (recorded on Linear): Stack A fully merged; single fresh PR from main.
Out of scope: ENC-304 ToolUniverse model, ENC-305 per-turn references/typed snapshot. No bare-string
restoration. From<FfiModelRequest> fallback untouched (ENC-305).
Acceptance: grep-clean registry channel; suite green both configs; ENC-298 verified under this ticket.
