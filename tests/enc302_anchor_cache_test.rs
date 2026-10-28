//! ENC-302 Option A acceptance tests: the artifact anchor is a derived
//! observation cache, not ground truth.

use agent_core::compact::artifact_squash::extract_artifact_anchor;
use agent_core::message::{AgentMessage, AgentRole, MessageMeta};
use agent_core::state_ledger::{ArtifactStateAnchor, StateLedger};
use agent_core::tool_name::ToolName;
use agent_core::tool_semantics::{ArtifactSquashSemantics, ToolSemanticsConfig};
use agent_core::turn_io::RawToolCall;

fn squash_semantics() -> ArtifactSquashSemantics {
    ArtifactSquashSemantics {
        append_tool_name: ToolName::new("append_blocks").expect("valid tool name"),
        id_field: "artifactId".into(),
        blocks_field: "blocks".into(),
        total_blocks_result_field: "totalBlocks".into(),
        status_field: "status".into(),
        success_status_value: "success".into(),
        summary_template: "Artifact generation in progress. {total} blocks written so far. Last block: `{snippet}`. Continue appending.".into(),
        continuation_instruction: "Continue appending.".into(),
        snippet_max_chars: 200,
    }
}

fn append_call(id: &str, text: &str) -> AgentMessage {
    AgentMessage {
        id: format!("a-{id}-{text}"),
        role: AgentRole::Assistant,
        content: format!(
            "call:append_blocks{{\"artifactId\":\"{id}\",\"blocks\":[{{\"type\":\"paragraph\",\"text\":\"{text}\"}}]}}"
        ),
        metadata: MessageMeta::new().with_raw_tool_calls(vec![RawToolCall {
            id: None,
            name: "append_blocks".into(),
            arguments_json: format!("{{\"artifactId\":\"{id}\"}}"),
        }]),
    }
}

fn append_result(id: &str, payload: &str) -> AgentMessage {
    AgentMessage {
        id: format!("r-{id}-{payload}"),
        role: AgentRole::Tool,
        content: payload.into(),
        metadata: MessageMeta::new().with_tool_name("append_blocks"),
    }
}

#[test]
fn enc302_count_disagreement_newest_observation_wins() {
    let semantics = squash_semantics();
    // Out-of-band growth: the host reports 3, then 9 with no new local
    // append between the observations.
    let first = vec![
        append_call("art-9", "Part 1"),
        append_result("art-9", r#"{"status":"success","totalBlocks":3}"#),
    ];
    let anchor = extract_artifact_anchor(&first, &semantics).expect("anchor must exist");
    assert_eq!(anchor.artifact_id, "art-9");
    assert_eq!(anchor.observed_total_blocks, Some(3));
    assert_eq!(anchor.continuation_instruction, "Continue appending.");

    let mut ledger = StateLedger::default();
    ledger.observe_artifact_anchor(anchor);

    let mut both = first;
    both.push(append_call("art-9", "Part 9"));
    both.push(append_result(
        "art-9",
        r#"{"status":"success","totalBlocks":9}"#,
    ));
    let anchor = extract_artifact_anchor(&both, &semantics).expect("anchor must exist");
    assert_eq!(anchor.observed_total_blocks, Some(9));
    assert_eq!(anchor.continuation_instruction, "Continue appending.");

    // Keyed replace by artifact_id: the stale 3 is superseded by 9.
    ledger.observe_artifact_anchor(anchor);
    assert_eq!(ledger.artifact_states.len(), 1);
    assert_eq!(ledger.artifact_states[0].observed_total_blocks, Some(9));
    assert_eq!(
        ledger.artifact_states[0].continuation_instruction,
        "Continue appending."
    );
}

#[test]
fn enc302_missing_and_non_integer_total_anchor_exists_unknown() {
    let semantics = squash_semantics();
    for payload in [
        r#"{"status":"success"}"#,
        r#"{"status":"success","totalBlocks":"many"}"#,
        r#"{"status":"success","totalBlocks":null}"#,
        r#"{"status":"success","totalBlocks":-2}"#,
    ] {
        let messages = vec![
            append_call("art-1", "Part 1"),
            append_result("art-1", payload),
        ];
        let anchor = extract_artifact_anchor(&messages, &semantics).expect("anchor must exist");
        assert_eq!(
            anchor.observed_total_blocks, None,
            "payload {payload} must yield an unknown count, not a 0 stand-in"
        );
        assert_eq!(anchor.continuation_instruction, "Continue appending.");
    }

    // A host-reported 0 after success is contradictory but recorded verbatim.
    let messages = vec![
        append_call("art-1", "Part 1"),
        append_result("art-1", r#"{"status":"success","totalBlocks":0}"#),
    ];
    let anchor = extract_artifact_anchor(&messages, &semantics).expect("anchor must exist");
    assert_eq!(anchor.observed_total_blocks, Some(0));
}

#[test]
fn enc302_unknown_count_renders_unknown_with_continuation() {
    let mut ledger = StateLedger::default();
    ledger.observe_artifact_anchor(ArtifactStateAnchor {
        artifact_id: "art-1".into(),
        observed_total_blocks: None,
        last_block_preview: "gamma".into(),
        continuation_instruction: "Continue appending.".into(),
    });
    let anchor = ledger
        .as_anchor_message()
        .expect("non-empty ledger must render an anchor");
    assert!(
        anchor.content.contains("unknown blocks"),
        "unobserved count must render literally: {anchor:?}"
    );
    assert!(
        anchor.content.contains("Continue appending."),
        "continuation must be present regardless of count: {anchor:?}"
    );

    // Observed counts still render numerically.
    let mut ledger = StateLedger::default();
    ledger.observe_artifact_anchor(ArtifactStateAnchor {
        artifact_id: "art-2".into(),
        observed_total_blocks: Some(9),
        last_block_preview: "omega".into(),
        continuation_instruction: "Continue appending.".into(),
    });
    let anchor = ledger.as_anchor_message().expect("anchor");
    assert!(anchor.content.contains("9 blocks"), "{anchor:?}");
}

#[test]
fn enc302_old_json_total_blocks_deserializes_via_alias() {
    let old: ArtifactStateAnchor = serde_json::from_str(
        r#"{"artifact_id":"art-1","total_blocks":3,"last_block_preview":"gamma","continuation_instruction":"Continue appending."}"#,
    )
    .expect("pre-ENC-302 JSON must still load");
    assert_eq!(old.observed_total_blocks, Some(3));

    let new_json = serde_json::to_string(&old).expect("serialize");
    assert!(
        new_json.contains("observed_total_blocks"),
        "serialize the new name, not the alias: {new_json}"
    );
    let roundtrip: ArtifactStateAnchor =
        serde_json::from_str(&new_json).expect("new JSON must load");
    assert_eq!(roundtrip.observed_total_blocks, Some(3));
}

#[test]
fn enc302_semantics_config_shape_unchanged() {
    // D4: no semantics-config/FFI changes; the total field still names
    // where observations come from.
    let cfg = ToolSemanticsConfig::agnostic();
    let _ = cfg.compaction.artifact_squash;
    assert_eq!(squash_semantics().total_blocks_result_field, "totalBlocks");
}
