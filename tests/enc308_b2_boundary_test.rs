//! ENC-308 chunk B2 — FFI boundary behavior for items 11 + 12:
//! session creation enforces the closed metadata schema (unknown or
//! ill-typed keys are typed errors naming the key), and
//! `submit_tool_results` converts through the fallible path (blank
//! activated names drop, valid names land).
#![cfg(feature = "ffi")]

use agent_core::{
    create_agent_engine, AgentException, FfiAgentConfig, FfiAgentMessage, FfiAgentRole,
    FfiSessionCapabilities, FfiSessionContext, FfiSessionIoParams, FfiToolResultRecord,
    FfiToolSchemaEntry,
};

fn io_params() -> FfiSessionIoParams {
    FfiSessionIoParams {
        provider: "anthropic".to_string(),
        context_window_tokens: 200_000,
        tool_schemas: vec![FfiToolSchemaEntry {
            name: "web_search".to_string(),
            description: Some("Search the web".to_string()),
            reproducible: None,
            deferrable: None,
            max_result_bytes: None,
        }],
        system_prompt: "sys".to_string(),
    }
}

fn message_with_metadata(metadata_json: &str) -> FfiAgentMessage {
    FfiAgentMessage {
        id: "u1".to_string(),
        role: FfiAgentRole::User,
        content: "hi".to_string(),
        metadata_json: metadata_json.to_string(),
    }
}

fn create_with_metadata(metadata_json: &str) -> Result<(), AgentException> {
    let eng = create_agent_engine();
    eng.create_session(
        vec![message_with_metadata(metadata_json)],
        FfiAgentConfig::default(),
        FfiSessionCapabilities::default(),
        FfiSessionContext::default(),
        io_params(),
    )
    .map(|_| ())
}

fn expect_internal_key_error(metadata_json: &str, key: &str) {
    match create_with_metadata(metadata_json) {
        Err(AgentException::Internal { detail }) => {
            assert!(
                detail.contains(key),
                "error names the offending key {key}: {detail}"
            );
        }
        Err(other) => panic!("expected typed Internal error, got {other:?}"),
        Ok(()) => panic!("metadata {metadata_json} must reject at creation"),
    }
}

#[test]
fn enc308_b2_create_session_rejects_unknown_metadata_key_by_name() {
    expect_internal_key_error(r#"{"vision_ref":"asset-1"}"#, "vision_ref");
}

#[test]
fn enc308_b2_create_session_rejects_misfit_metadata_value_by_name() {
    expect_internal_key_error(r#"{"call_id":42}"#, "call_id");
}

#[test]
fn enc308_b2_create_session_accepts_schema_metadata() {
    create_with_metadata(r#"{"call_id":"c1","agent_tool_budget":true,"spilled":true}"#)
        .expect("schema-conformant metadata creates a session");
}

#[test]
fn enc308_b2_submit_tool_results_drops_blanks_and_accepts_valid_names() {
    let eng = create_agent_engine();
    let session = eng
        .create_session(
            vec![message_with_metadata("null")],
            FfiAgentConfig::default(),
            FfiSessionCapabilities::default(),
            FfiSessionContext::default(),
            io_params(),
        )
        .expect("ffi create_session");
    // The conversion is the item-11 surface: blanks drop silently
    // (documented), the valid name canonicalizes, and the batch is
    // accepted by the fallible submit.
    eng.submit_tool_results(
        &session,
        vec![FfiToolResultRecord {
            call_id: "c1".to_string(),
            result_json: "{}".to_string(),
            truncated: false,
            soft_failure: false,
            activated_tool_names: vec![String::new(), "   ".to_string(), "Web Search".to_string()],
        }],
    )
    .expect("blank-dropping batch submits");
}
