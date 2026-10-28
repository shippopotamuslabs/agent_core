# Status: ENC-296

## Phase Checklist
- [x] Research — vision approval: approved (mission; Sean-ratified design on ticket)
- [x] Plan
- [x] Implementation
- [x] Review — rounds: 1/3, result: clean (0 Critical/Major; Minors M1-M5 fixed e4156c7)
- [ ] Ship — PR: none

## Current Gate
none

## Next Phase
Review

## Notes
- Base: origin/main fc89283 (Stack A #24-#28 merged). Branch shippopotamus/enc-296-agent_core-host-owns-the-tool-registry-remove-registry. Worktree /home/sean/Code/agent_core-ENC-296.
- Implementation (Chunk 1) done: registry channel deleted end-to-end (ToolSchemaSet.registry/raw_registry/registry_json()/has_registry/3rd parse arg/from_entries registry/REGISTRY_CHANNEL; SessionIoParams::new 5-arg; FFI FfiSessionIoParams.tool_schema_registry + FfiToolSchemaEntry.parameters_json; ContextReferenceKind::ToolSchema/ToolCatalogIndex + FFI enum arms; ToolContextIndex.full_schema_ref + all ContextStore schema stashing; sidecar tool-index spill now ArtifactSnapshot; tool_schema_activation::read_full_schema_registry stub). render_active_schemas now reads the active channel + name stubs. FFI entries validate blank/duplicate names with typed AgentException naming the entry.
- Gates (plan order, all green): cargo fmt --check clean (after one cargo fmt); cargo clippy --all-targets --locked -- -D warnings clean; cargo test --locked 556 passed 0 failed; cargo test --locked --no-default-features 538 passed 0 failed. Extra probe: --all-features compaction_boundary_test has 3 failures (lalr_03/lalr_09/lalr_15) reproducing identically on clean base fc89283 — pre-existing, out of scope.
- Files changed (src): tool_schema.rs, engine/state.rs, engine/model_request.rs, tool_context.rs, context_store.rs, sidecar_builder.rs, tool_schema_activation.rs, ffi/types.rs, engine/mod.rs (dead import + test args), lalr_harness.rs (test args). Untouched per plan: request_pipeline.rs, session.rs, lib.rs, ffi/mod.rs. Tests: tool_schema.rs + ffi/types.rs unit tests; tool_context_test (s13/s14/m6 re-pinned, s14/m6 renamed, stash-read assertions replaced with no-stash assertions); lazy_tool_disclosure_test (LTD-08 re-pinned off stored_registry, LTD-09 extended with stub-shape assertions, new enc_296_activated_tool_renders_active_entry_or_name_stub); tool_search_discovery_test (registry fixture deleted); context_store_test (browsable test re-pinned, in-src kind test now asserts all remaining kinds browsable); sidecar_budget_test (lalr_18 repurposed to spill-under-ArtifactSnapshot; needs lalr-harness feature, outside default gates); finish_boundary_test (FFI struct field); tool_semantics_engine_test (dead REGISTRY_JSON const); mechanical 6-arg to 5-arg SessionIoParams::new updates across 23 call sites (tests + examples/mock_desktop_loop.rs).
- Surprises: (1) HookRegistry in loop_hooks.rs is an unrelated hooks registry — kept, not channel semantics. (2) Keeping any schema stash was impossible without breaking deferral: every remaining ContextReferenceKind is model-browsable, so a kept stash would leak schema text into ModelRequest.context_references; hence full_schema_ref was removed (not left dangling) and sidecar omission logic deleted. (3) FFI blank/duplicate-name validation added in tool_schema_entries since parameters_json was the only prior failure mode there.
- Grep evidence: src/ hits for tool_schema_registry|registry_json()|has_registry|ToolCatalogIndex|ContextReferenceKind::ToolSchema|parameters_json are removal-documenting comments only (sidecar_builder.rs, context_store.rs, tool_schema.rs:176, ffi/types.rs); zero code references.

- Commits: 3c59354 (impl), e4156c7 (fix pass). Gates: fmt/clippy clean; cargo test --locked 556/0; --no-default-features 538/0 (orchestrator re-ran pre-fix; worker re-ran post-fix).
- Findings: findings/ENC-296-findings.md (0 open Critical/Major).
