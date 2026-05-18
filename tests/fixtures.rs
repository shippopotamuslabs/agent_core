//! Golden / fixture harness (AGENT-020). Run: `cargo test --test fixtures`
//!
//! Snapshot updates require explicit review: `cargo insta review`

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use agent_core::action::AgentAction;
use agent_core::engine::{AgentEngine, SessionIoParams};
use agent_core::events::{AgentError, AgentEvent};
use agent_core::host::{AgentToolExecutor, AgentTransport};
use agent_core::message::{AgentMessage, ToolCallRecord, ToolResultRecord};
use agent_core::session::{AgentConfig, SessionCapabilities};
use agent_core::turn_io::{ModelRequest, ModelResponse};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct FixtureFile {
    name: String,
    initial_session: InitialSession,
    model_responses: Vec<ModelResponse>,
    expected_actions: Vec<String>,
    #[serde(default)]
    io: FixtureIo,
}

#[derive(Debug, Deserialize)]
struct FixtureIo {
    #[serde(default = "default_provider")]
    provider: String,
    #[serde(default = "default_window")]
    context_window_tokens: u32,
    #[serde(default)]
    tools_schema_json: String,
    #[serde(default = "default_sys")]
    system_prompt: String,
}

fn default_provider() -> String {
    "anthropic".into()
}
fn default_window() -> u32 {
    200_000
}
fn default_sys() -> String {
    "sys".into()
}

impl Default for FixtureIo {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            context_window_tokens: default_window(),
            tools_schema_json: "[]".into(),
            system_prompt: default_sys(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct InitialSession {
    messages: Vec<AgentMessage>,
    config: AgentConfig,
    capabilities: SessionCapabilities,
    #[serde(default)]
    session_context: serde_json::Value,
}

struct PanicTransport;

impl AgentTransport for PanicTransport {
    fn complete_model_turn(&self, _: ModelRequest) -> Result<ModelResponse, AgentError> {
        panic!("fixtures drive submit_model_response manually");
    }
}

#[derive(Clone, Copy)]
enum ExecMode {
    Ok,
    SoftFail,
}

struct FixtureExecutor(ExecMode);

impl AgentToolExecutor for FixtureExecutor {
    fn execute_tools(
        &self,
        calls: Vec<ToolCallRecord>,
    ) -> Result<Vec<ToolResultRecord>, AgentError> {
        match self.0 {
            ExecMode::Ok => Ok(calls
                .into_iter()
                .map(|c| ToolResultRecord {
                    call_id: c.call_id,
                    result_json: r#"{"status":"success"}"#.into(),
                    truncated: false,
                    soft_failure: false,
                })
                .collect()),
            ExecMode::SoftFail => Ok(calls
                .into_iter()
                .map(|c| ToolResultRecord {
                    call_id: c.call_id,
                    result_json: r#"{"error":"soft"}"#.into(),
                    truncated: false,
                    soft_failure: true,
                })
                .collect()),
        }
    }
}

fn action_name(a: &AgentAction) -> String {
    match a {
        AgentAction::CallModel(_) => "CallModel".into(),
        AgentAction::ExecuteTools(_) => "ExecuteTools".into(),
        AgentAction::RequestSummary(_) => "RequestSummary".into(),
        AgentAction::Finished => "Finished".into(),
        AgentAction::Failed(_) => "Failed".into(),
        AgentAction::ManualCompactCompleted(_) => "ManualCompactCompleted".into(),
    }
}

fn run_fixture(path: &PathBuf, exec_mode: ExecMode) -> (String, Vec<String>, Vec<AgentEvent>) {
    let raw = fs::read_to_string(path).expect("read fixture");
    let f: FixtureFile = serde_json::from_str(&raw).expect("parse fixture");
    let eng = AgentEngine::new(
        Arc::new(PanicTransport),
        Arc::new(FixtureExecutor(exec_mode)),
        Arc::new(agent_core::host::NoOpSummarizer),
    );
    let io = SessionIoParams {
        provider: f.io.provider.clone(),
        context_window_tokens: f.io.context_window_tokens,
        tools_schema_json: f.io.tools_schema_json.clone(),
        system_prompt: f.io.system_prompt.clone(),
    };
    let handle = eng.create_session(
        f.initial_session.messages.clone(),
        f.initial_session.config.clone(),
        f.initial_session.capabilities.clone(),
        f.initial_session.session_context.clone(),
        io,
    );

    let mut actions = Vec::new();
    let mut all_events = Vec::new();
    let mut model_step = 0usize;
    for _ in 0..80 {
        let act = eng.next_action(&handle);
        actions.push(action_name(&act));
        all_events.extend(eng.drain_events(&handle));
        match &act {
            AgentAction::CallModel(_) => {
                let resp = f
                    .model_responses
                    .get(model_step)
                    .cloned()
                    .unwrap_or_else(|| ModelResponse::text_only("done"));
                model_step += 1;
                eng.submit_model_response(&handle, resp);
            }
            AgentAction::ExecuteTools(calls) => {
                let results = FixtureExecutor(exec_mode)
                    .execute_tools(calls.clone())
                    .expect("tools");
                eng.submit_tool_results(&handle, results);
            }
            AgentAction::RequestSummary(_) => panic!("RequestSummary in fixture {}", f.name),
            AgentAction::Finished => break,
            AgentAction::Failed(e) => panic!("fixture {} failed: {e:?}", f.name),
            AgentAction::ManualCompactCompleted(_) => {
                panic!("ManualCompactCompleted in fixture {}", f.name)
            }
        }
        if matches!(act, AgentAction::Finished) {
            break;
        }
    }

    (f.name, actions, all_events)
}

#[test]
fn run_all_fixtures() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut paths: Vec<_> = fs::read_dir(&dir)
        .expect("fixtures dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    paths.sort();
    assert!(paths.len() >= 5, "expected at least 5 fixture JSON files");
    for path in paths {
        let stem = path.file_stem().unwrap().to_string_lossy();
        let exec_mode = if stem.contains("guardrail") {
            ExecMode::SoftFail
        } else {
            ExecMode::Ok
        };
        let raw = fs::read_to_string(&path).unwrap();
        let f: FixtureFile = serde_json::from_str(&raw).unwrap();
        let expected = f.expected_actions.clone();
        let (name, actions, events) = run_fixture(&path, exec_mode);
        assert_eq!(
            actions,
            expected,
            "action mismatch for {name} ({})",
            path.display()
        );
        insta::assert_json_snapshot!(format!("events_{}", name.replace(' ', "_")), events);
    }
}
