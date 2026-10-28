//! ENC-308 review R2: composite legacy-row coverage for the snapshot
//! artifact path (plan decision D2 — strict at the live boundary,
//! tolerant at the archive).
//!
//! The individual seams were already proven in isolation (legacy-row
//! tests for `AgentSession` / `AgentMessage`, the FFI boundary tests,
//! the B1 context fixture); what was missing is ONE test that drives a
//! legacy-populated `SessionSnapshot` blob — junk-carrying session
//! context, spilled + junk message metadata, a populated schema set,
//! a non-empty state ledger — through the engine's real
//! `import_snapshot` path and asserts the restored state.
//!
//! The blob is the A2 golden snapshot's shape (a genuine pre-ENC-308
//! export layout at the current `SNAPSHOT_VERSION`) with legacy
//! content written into it: the session context carries typed wire
//! keys plus a `legacy_host_junk` extras key (pre-308 exports embedded
//! the flattened catch-all verbatim); the message metadata carries
//! `"spilled": true` (which the pre-308 read path parked in the
//! catch-all, so `is_spilled()` read false on restored messages) plus
//! a junk key; the state ledger carries content in all three places a
//! real export puts it (the ledger field, the pending request, and
//! the sidecar builder's last-ledger JSON).
//!
//! Assertions are field-level, not whole-blob byte pins (the A2
//! lesson: byte pins are for the deterministic shapes; here the point
//! is what survives and what is dropped).

use agent_core::engine::AgentEngine;
use agent_core::session_snapshot::SNAPSHOT_VERSION;
use agent_core::SessionSnapshot;
use serde_json::Value;

const LEGACY_SNAPSHOT: &str = r###"{"version":1,"session":{"messages":[{"id":"u1","role":"user","content":"hi","metadata":{"spilled":true,"legacy_junk_key":"drop-me"}}],"tool_trace":[],"compaction_log":[],"ledger_snapshot":{"total_tokens":2116,"threshold_tokens":150000,"usable_prompt_tokens":190000,"ratio":0.01058},"session_context":{"chatId":"chat-42","userMessageId":"um-7","assistantMessageId":"am-9","history_snapshot_ids":["snap-1"],"pending_work":["file the report"],"force_finish_blocked":true,"legacy_host_junk":{"origin":"old-build","flags":[1,2]}},"transcript_revision":0},"config":{"max_turns":20,"threshold_ratio":0.8,"micro_keep_recent":4,"tool_retry_limit":3,"model_api_retry_limit":3,"model_api_retry_base_delay_ms":1000,"tool_result_budget_cloud_bytes":16384,"tool_result_budget_relaxed_bytes":131072,"tool_result_truncation_pressure_ratio":0.85,"token_correction":{},"mid_turn_threshold_ratio":0.93,"post_tool_result_threshold_ratio":0.92,"manual_compact_threshold_ratio":0.8,"allow_fifo_on_preflight":true,"allow_fifo_on_mid_turn":false,"allow_fifo_on_post_tool_result":false,"require_snapshot_before_fifo":true,"enable_context_store":true,"enable_state_ledger":true,"enable_tool_schema_deferral":true,"enable_file_condensation":true,"enable_manual_compact":true,"tool_spill_threshold_bytes":8192,"terminal_spill_threshold_bytes":8192,"history_snapshot_threshold_tokens":1024,"file_outline_threshold_tokens":2048,"prompt_cache_anchor_threshold_tokens":0,"output_reserve_ratio":0.05,"output_reserve_floor_tokens":4096,"output_reserve_max_tokens":16384,"preflight_token_threshold":150000,"mid_turn_token_threshold":180000,"post_tool_result_token_threshold":null,"turn_threshold":null,"message_threshold":200,"compact_on_turn_end":false,"retention_window_messages":0,"retention_window_tokens":16384,"eviction_window_ratio":0.0,"enable_loop_hooks":false,"enable_doom_loop_guard":false,"enable_pending_work_guard":false,"require_read_before_edit":false,"default_tool_timeout_ms":null,"max_model_requests_per_turn":1,"long_loop":{"max_tool_turns":40,"sidecar_byte_budget_per_call":8192,"sidecar_total_byte_budget":16384,"summarize_after_tool_turns":0,"compact_every_n_tool_turns":0},"tool_semantics":{"catalog":{"tools":[]},"discovery":null,"artifact":null,"argument_repairs":[],"canonical_shapes":[],"result_policies":[],"compaction":{"artifact_squash":null},"greedy_extension_tools":[]},"tool_call_format":"Structured"},"capabilities":{"summarization_allowed":false,"kv_fixed":false,"context_policy_tier":"hosted_mutable","tool_schema_deferral_supported":false,"file_backed_context_supported":false,"prompt_cache_supported":false,"history_rewrite_supported":true,"request_transform_pipeline_supported":false,"lifecycle_hooks_supported":false,"delegation_tools_supported":false,"host_policy_callbacks_supported":false},"provider":"anthropic","context_window_tokens":200000,"system_prompt":"sys","schemas":{"active":[{"description":"Search the web","name":"web_search","parameters":{"type":"object"},"weight":1.5}],"raw_active":"[ { \"parameters\" : { \"type\" : \"object\" }, \"name\" : \"web_search\", \"description\" : \"Search the web\", \"weight\" : 1.50 } ]"},"active_tool_names":["web_search"],"context_store":{"entries":{},"next_seq":0},"state_ledger":{"objective":"legacy objective","constraints":["keep it typed"],"decisions":["chose the closed schema"],"open_questions":["q-one"],"touched_files":["src/message.rs"],"tool_decisions":[],"artifact_states":[],"summary_refs":["sum-1"],"history_refs":["hist-1"],"retired":[],"retirement_artifacts":[]},"guardrails":{"tool_calls_executed":0,"consecutive_soft_failure_turns":0,"consecutive_envelope_parse_failures":0,"consecutive_model_api_failures":0},"loop_metrics":{"turn_count":1,"message_count":0,"files_read":[],"repeated_tool_signatures":[],"tool_attempts":{},"pending_work_items":[]},"phase":"AwaitingModel","pending":{"call_model":{"session_messages":[{"id":"u1","role":"user","content":"hi","metadata":{"spilled":true,"legacy_junk_key":"drop-me"}}],"system_prompt":"sys","tool_schemas":[{"description":"Search the web","name":"web_search","parameters":{"type":"object"},"weight":1.5}],"rendered_runtime_tools":[{"description":"Search the web","name":"web_search","parameters":{"type":"object"},"weight":1.5}],"allow_tool_call":true,"state_ledger":{"objective":"legacy objective","constraints":["keep it typed"],"decisions":["chose the closed schema"],"open_questions":["q-one"],"touched_files":["src/message.rs"],"tool_decisions":[],"artifact_states":[],"summary_refs":["sum-1"],"history_refs":["hist-1"],"retired":[],"retirement_artifacts":[]},"provider_context":{"transcript_revision":0,"sidecar_generation":0,"sidecar_byte_len":205,"compaction_passes_this_turn":0,"destructive_tiers_this_turn":0,"pressure_ratio_at_call_model_millis":11}},"execute":null,"execute_skip_guardrail_count":false,"auto_finalize_finish":false,"finalize_after_tool_stop":false,"summary_req":null,"summary_ctx":null,"tool_turn_parallel":0,"reject_count_this_turn":0,"artifact_ids":[],"tool_turn_request_order":[],"reject_tool_by_call_id":{}},"sidecar_builder":{"generation":1,"rolling_cache":[{"generation":0,"byte_len":205}],"last_ledger_json":"{\"objective\":\"legacy objective\",\"constraints\":[\"keep it typed\"],\"decisions\":[\"chose the closed schema\"],\"open_questions\":[\"q-one\"],\"touched_files\":[\"src/message.rs\"],\"tool_decisions\":[],\"artifact_states\":[],\"summary_refs\":[\"sum-1\"],\"history_refs\":[\"hist-1\"],\"retired\":[],\"retirement_artifacts\":[]}"},"model_requests_this_turn":1,"events":[{"type":"turn_started","turn_index":1,"allow_tool_call":true}],"turn_index":1,"cancelled":false,"terminal_action":null,"last_model_response_greedy_tool_names":[],"tools_blocked_nudges_this_turn":0,"compaction_passes_this_turn":0,"destructive_tiers_this_turn":0,"suppress_summary_tier":false}"###;

/// The raw schema channel text of the blob's ToolSchemaSet (the A2
/// golden's non-canonical channel: verbatim host text, `1.50` lexeme
/// and all).
const LEGACY_RAW_CHANNEL: &str = "[ { \"parameters\" : { \"type\" : \"object\" }, \"name\" : \"web_search\", \"description\" : \"Search the web\", \"weight\" : 1.50 } ]";

#[test]
fn legacy_snapshot_imports_with_typed_state_intact_and_junk_dropped() {
    // The literal is written against the current format version; the
    // import below enforces that (any other version is rejected).
    assert_eq!(SNAPSHOT_VERSION, 1);
    let snapshot: SessionSnapshot =
        serde_json::from_str(LEGACY_SNAPSHOT).expect("legacy snapshot deserializes");
    let eng = AgentEngine::new();
    let handle = eng
        .import_snapshot(snapshot)
        .expect("legacy snapshot imports");
    let restored = eng.export_snapshot(&handle);

    // Session context: every typed field survives the tolerant read.
    let ctx = &restored.session.session_context;
    assert_eq!(ctx.chat_id.as_deref(), Some("chat-42"));
    assert_eq!(ctx.user_message_id.as_deref(), Some("um-7"));
    assert_eq!(ctx.assistant_message_id.as_deref(), Some("am-9"));
    assert_eq!(ctx.history_snapshot_ids, ["snap-1"]);
    assert_eq!(ctx.pending_work, ["file the report"]);
    assert!(ctx.force_finish_blocked);

    // Message metadata: `spilled` routed to its typed field (the
    // pre-308 catch-all parking made this read false after a restore).
    assert!(restored.session.messages[0].metadata.is_spilled());

    // State ledger: content intact after the round trip.
    let ledger = &restored.state_ledger;
    assert_eq!(ledger.objective.as_deref(), Some("legacy objective"));
    assert_eq!(ledger.constraints, ["keep it typed"]);
    assert_eq!(ledger.decisions, ["chose the closed schema"]);
    assert_eq!(ledger.open_questions, ["q-one"]);
    assert_eq!(ledger.touched_files, ["src/message.rs"]);
    assert_eq!(ledger.summary_refs, ["sum-1"]);
    assert_eq!(ledger.history_refs, ["hist-1"]);

    // Schema set: populated, raw channel text verbatim (A2 fidelity).
    assert_eq!(restored.schemas.active_json(), LEGACY_RAW_CHANNEL);
    assert_eq!(restored.active_tool_names.len(), 1);

    // Junk keys are gone on re-export: neither the context extras key
    // nor the metadata junk key crosses back out.
    let value = serde_json::to_value(&restored).expect("restored snapshot serializes");
    let ctx_json = value
        .pointer("/session/session_context")
        .and_then(Value::as_object)
        .expect("session_context serializes as an object");
    assert!(
        !ctx_json.contains_key("legacy_host_junk"),
        "context extras key must not survive import: {ctx_json:?}"
    );
    assert_eq!(ctx_json.get("chatId"), Some(&serde_json::json!("chat-42")));
    let meta_json = value
        .pointer("/session/messages/0/metadata")
        .and_then(Value::as_object)
        .expect("message metadata serializes as an object");
    assert_eq!(meta_json.get("spilled"), Some(&serde_json::json!(true)));
    assert!(
        !meta_json.contains_key("legacy_junk_key"),
        "metadata junk key must not survive import: {meta_json:?}"
    );
}
