//! ENC-287: session fixture metadata must deserialize into `MessageMeta` and
//! re-serialize byte-identically (typed fields + flattened catch-all preserve
//! every key, and empty metadata stays JSON `null`).

use std::fs;
use std::path::PathBuf;

use agent_core::message::AgentMessage;
use serde_json::Value;

#[test]
fn fixture_message_metadata_roundtrips_identically() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut paths: Vec<_> = fs::read_dir(&dir)
        .expect("fixtures dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    paths.sort();
    assert!(
        paths.len() >= 5,
        "expected at least 5 fixture JSON files, found {}",
        paths.len()
    );
    for path in &paths {
        let fixture = path.file_stem().unwrap().to_string_lossy().to_string();
        let raw = fs::read_to_string(path).unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        let messages = parsed
            .get("initial_session")
            .and_then(|s| s.get("messages"))
            .and_then(|m| m.as_array())
            .unwrap_or_else(|| panic!("fixture {fixture} has no initial_session.messages"));
        assert!(
            !messages.is_empty(),
            "fixture {fixture} must contain at least one message to exercise metadata"
        );
        for (idx, original) in messages.iter().enumerate() {
            let original_meta = original.get("metadata").cloned().unwrap_or(Value::Null);
            let msg: AgentMessage = serde_json::from_value(original.clone()).unwrap_or_else(|e| {
                panic!("fixture {fixture} message {idx} must deserialize: {e}")
            });
            let back = serde_json::to_value(&msg).unwrap();
            let back_meta = back.get("metadata").cloned().unwrap_or(Value::Null);
            assert_eq!(
                back_meta, original_meta,
                "fixture {fixture} message {idx} metadata must re-serialize identically"
            );
        }
    }
}
