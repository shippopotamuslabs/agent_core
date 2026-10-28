# Plan: ENC-303 — bypass collapse + pressure margin (stacked on ENC-302/#34)

## Part 1 decisions
- P1 Replace ToolDiscoverySemantics{always_callable: bool, companion_callable_tools: Vec<ToolName>} with gate_exemptions: Vec<DiscoveryGateExemption{tool_name: ToolName, activates: bool, reason: String}> (core + FFI mirror FfiDiscoveryGateExemption in FfiToolDiscoverySemantics). No serde back-compat shims (sweep already breaking; fixtures updated).
- P2 Gate behavior preserved exactly: exemption with activates=false => callable_now only; activates=true => callable + merge into active_tool_names. is_companion_callable_tool -> gate_exemption_for(name) helper (single decider).
- P3 Validation: reason non-blank else typed ToolSemanticsError (new variant or InvalidDiscoveryConfig with clear message — follow existing family); duplicate exemption names rejected; ENC-299 referential validation covers gate_exemptions section (rename section string); discovery tool_name itself still validated.
- P4 greedy_extension_tools unchanged mechanically; docs (tool_semantics + guardrails) state: budget mechanism, not a gate bypass; relevance judge = host-declared list, engine matches last model response by exact ToolName.
- Fixtures: enclave semantics -> exemptions [tool_discovery_tool{activates:false, reason:"discovery entry point"}, read_context_reference{activates:true, reason:"context retrieval companion"}]; support helpers likewise.

## Part 2 decisions
- Q1 compact/policy.rs (or compact/mod.rs): PressureClass per tier {Reversible, EstimateAllowed, ConfirmationGated} + fn tier_pressure_class(TierEnum) + module docs: estimation method/error budget (cl100k vs provider tokenizer, overhead approximations, token_correction host calibration), safety margin = output reserve / hard ceiling (named constants referenced), rule table.
- Q2 Enforcement (smallest gate in plan construction/policy): FIFO and TextTruncate are ConfirmationGated — included in a ratio-triggered plan ONLY if hard-ceiling exceeded OR trigger==Manual OR (FIFO only) snapshot-first satisfied per existing require_snapshot_before_fifo flow. If current architecture already guarantees FIFO snapshot + TextTruncate hard-ceiling-only, encode as explicit plan-filter + tests rather than new behavior; STOP and report if enforcement would change pinned plan expectations beyond adding the gate (re-pins explained are ok).
- Q3 Tests: (a) ratio-triggered plan without snapshot-allowed FIFO excludes FIFO/TextTruncate; (b) hard-ceiling/Manual includes them; (c) tier_pressure_class table test. Docs cite thresholds.

## Acceptance
Part1: one exemption list, per-tool reasons in fixture, gate tests re-pinned green; Part2: margin rule written in compact docs + tests above. Gates both configs. Two commits (P1, P2) referencing ENC-303. No push/PR/Linear.
