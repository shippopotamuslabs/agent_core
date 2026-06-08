//! TDD Red — end-to-end (S19).

#[path = "support/mod.rs"]
mod support;

use support::enclave_test_agent_config::enclave_test_agent_config;
use std::sync::Arc;

use agent_core::action::AgentAction;
use agent_core::budget::TokenLedger;
use agent_core::context_store::ContextReferenceKind;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::AgentError;
use agent_core::host::{AgentToolExecutor, AgentTransport, NoOpSummarizer};
use agent_core::message::{AgentMessage, AgentRole, ToolCallRecord, ToolResultRecord};
use agent_core::session::{AgentConfig, AgentSession, SessionCapabilities};
use agent_core::state_ledger::StateLedger;
use agent_core::turn_io::{FinishReason, ModelRequest, ModelResponse, RawToolCall};

struct PanicTransport;
impl AgentTransport for PanicTransport {
    fn complete_model_turn(&self, _: ModelRequest) -> Result<ModelResponse, AgentError> {
        panic!("unused");
    }
}
struct PanicExec;
impl AgentToolExecutor for PanicExec {
    fn execute_tools(&self, _: Vec<ToolCallRecord>) -> Result<Vec<ToolResultRecord>, AgentError> {
        panic!("unused");
    }
}

#[test]
fn s19_end_to_end_mixed_pressure_preserves_semantic_continuity() {
    // 1) Seed the StateLedger via initial messages.
    let mut ledger = StateLedger::default();
    let objective_msg = AgentMessage {
        id: "u0".into(),
        role: AgentRole::User,
        content: "Objective: ship context manager".into(),
        metadata: serde_json::Value::Null,
    };
    ledger.observe_user_message(&objective_msg);
    assert_eq!(
        ledger.objective.as_deref(),
        Some("ship context manager"),
        "ledger objective extraction must seed the rest of the test"
    );

    // 2) Drive file-backed context through session_context so the engine-owned
    //    ContextStore is the source surfaced in the ModelRequest.
    let mut cfg = enclave_test_agent_config();
    cfg.enable_state_ledger = true;
    cfg.enable_context_store = true;
    cfg.enable_file_condensation = true;
    cfg.tool_spill_threshold_bytes = 32;
    let caps = SessionCapabilities {
        summarization_allowed: true,
        kv_fixed: false,
        file_backed_context_supported: true,
        ..Default::default()
    };
    cfg.file_outline_threshold_tokens = 100;
    let session_context = serde_json::json!({
        "included_files": [
            {
                "path": "src/big.rs",
                "token_estimate": 500,
                "content": "fn alpha(){}\nfn beta(){}"
            }
        ]
    });

    // 3) Drive a tool spill and artifact squash through the engine so its owned
    //    store accrues ToolOutput, HistorySnapshot, ArtifactSnapshot, and file refs.
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(PanicExec),
        Arc::new(NoOpSummarizer),
    );
    let handle = eng.create_session(
        vec![objective_msg.clone()],
        cfg,
        caps,
        session_context,
        SessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 256,
            tool_catalog_json: "[]".into(),
            tool_schema_registry_json: "[]".into(),
            tools_schema_json: "[]".into(),
            system_prompt: "sys".into(),
        },
    );
    let _ = eng.next_action(&handle);
    eng.submit_model_response(
        &handle,
        ModelResponse {
            assistant_text: Some("RAW_LONG_OUTPUT".into()),
            tool_calls: vec![RawToolCall {
                id: Some("c1".into()),
                name: "append_blocks".into(),
                arguments_json: r#"{"artifactId":"art-9","blocks":[]}"#.into(),
            }],
            finish_reason: FinishReason::ToolUse,
            provider_reasoning_content: None,
        },
    );
    let _ = eng.next_action(&handle);
    let huge = format!(
        r#"{{"status":"success","totalBlocks":3,"_bulk":"{}"}}"#,
        "x".repeat(60_000)
    );
    eng.submit_tool_results(
        &handle,
        vec![ToolResultRecord {
            call_id: "c1".into(),
            result_json: huge,
            truncated: false,
            soft_failure: false,
            activated_tool_names: Vec::new(),
            result_metadata: None,
        }],
    );

    // 4) Inspect the next ModelRequest and assert every context channel is
    //    surfaced through the engine-owned request.
    let req = loop {
        match eng.next_action(&handle) {
            AgentAction::CallModel(req) => break req,
            AgentAction::RequestSummary(summary_req) => {
                eng.submit_summary(
                    &handle,
                    agent_core::turn_io::SummaryResult {
                        summary_text: "Retained objective, artifact state, and tool decisions."
                            .into(),
                        last_message_id: summary_req.last_message_id,
                    },
                );
            }
            other => panic!("expected CallModel or RequestSummary, got {other:?}"),
        }
    };

    let req_ledger = req.state_ledger.as_ref().expect("state_ledger required");
    assert_eq!(
        req_ledger.objective.as_deref(),
        Some("ship context manager")
    );
    assert!(
        req_ledger
            .artifact_states
            .iter()
            .any(|anchor| anchor.artifact_id == "art-9" && anchor.total_blocks == 3),
        "artifact state missing from ledger: {:?}",
        req_ledger.artifact_states
    );

    let file_index = req.file_context_index.as_ref().expect("file context index");
    assert_eq!(file_index.included_paths[0].path, "src/big.rs");

    let kinds: Vec<_> = req
        .context_references
        .iter()
        .map(|r| r.kind.clone())
        .collect();
    for required in [
        ContextReferenceKind::ToolOutput,
        ContextReferenceKind::HistorySnapshot,
        ContextReferenceKind::ArtifactSnapshot,
        ContextReferenceKind::FileOutline,
    ] {
        assert!(
            kinds.contains(&required),
            "context_references missing required kind {:?}; got {:?}",
            required,
            kinds
        );
    }

    let pressure = TokenLedger::estimate(
        &AgentSession::new(req.session_messages.clone(), serde_json::Value::Null),
        &AgentConfig::default(),
        &agent_core::budget::PromptBudgetParts {
            provider: "anthropic",
            context_window_tokens: 256,
            tools_schema_json: &req.tools_schema_json,
            system_prompt: &req.system_prompt,
            context_sidecars: "",
        },
    );
    assert!(
        pressure.ratio < 0.85,
        "final pressure ratio must be below 0.85, got {}",
        pressure.ratio
    );
}
