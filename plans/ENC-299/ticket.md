# ENC-299 — ToolName newtype + construction-time referential validation (vision: Linear ENC-299)
Layer 1: ToolName newtype for every tool-referring field; boundary construction normalizes once; internal lookups key on ToolName; per-site normalization deleted. UniFFI wire stays String.
Layer 2: closed-world validation at session construction: known set = ToolSpec names + active schema names; every semantics reference resolves (aliases -> canonical) or construction fails with UnknownToolReference naming section+dangling name via AgentException.
Execute as construction behavior on the ENC-297 surface (with_tool_semantics / fallible create_session). No ENC-304 model. Stacked on ENC-297 (#30).
