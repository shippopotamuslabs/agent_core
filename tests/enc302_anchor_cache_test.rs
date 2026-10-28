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

/// Metadata-less Tool result: no `tool_name` metadata, so the BASE
/// total-field-presence guard applies.
fn bare_result(payload: &str) -> AgentMessage {
    AgentMessage {
        id: format!("r-bare-{payload}"),
        role: AgentRole::Tool,
        content: payload.into(),
        metadata: MessageMeta::new(),
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

    // Stale-intermediate: before the 9-observation supersedes it, the ledger
    // anchor renders the stale 3 with the continuation intact.
    let interim = ledger
        .as_anchor_message()
        .expect("non-empty ledger must render an anchor");
    assert!(
        interim.content.contains("3 blocks"),
        "stale count must render before supersede: {interim:?}"
    );
    assert!(
        interim.content.contains("Continue appending."),
        "continuation must survive the stale render: {interim:?}"
    );

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
    // Production (metadata) shape: an append-tool success without a total
    // field still anchors, with an unknown count.
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

    // Metadata-less shape: BASE total-field-presence guard — a total field
    // present in any form (even non-integer) still anchors as base, with a
    // parsed-or-unknown count; a bare success without the field does not.
    for (payload, expected) in [
        (r#"{"status":"success","totalBlocks":"many"}"#, None),
        (r#"{"status":"success","totalBlocks":null}"#, None),
        (r#"{"status":"success","totalBlocks":-2}"#, None),
        (r#"{"status":"success","totalBlocks":0}"#, Some(0)),
        (r#"{"status":"success","totalBlocks":3}"#, Some(3)),
    ] {
        let messages = vec![append_call("art-1", "Part 1"), bare_result(payload)];
        let anchor = extract_artifact_anchor(&messages, &semantics)
            .expect("metadata-less payload with a total field must anchor as base");
        assert_eq!(
            anchor.observed_total_blocks, expected,
            "metadata-less payload {payload} must observe {expected:?}"
        );
        assert_eq!(anchor.continuation_instruction, "Continue appending.");
    }
    let unengaged = vec![
        append_call("art-1", "Part 1"),
        bare_result(r#"{"status":"success"}"#),
    ];
    assert!(
        extract_artifact_anchor(&unengaged, &semantics).is_none(),
        "metadata-less success without a total field must not anchor (BASE guard)"
    );

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
    // D4: no semantics-config/FFI changes; assert the REAL
    // ToolSemanticsConfig squash section (not just the local helper) still
    // names the total field observations come from.
    let raw = include_str!("support/enclave_tool_semantics.json");
    let cfg: ToolSemanticsConfig = serde_json::from_str(raw).expect("enclave_tool_semantics.json");
    let squash = cfg
        .compaction
        .artifact_squash
        .expect("enclave config must configure artifact_squash");
    assert_eq!(squash.append_tool_name.as_str(), "append_blocks");
    assert_eq!(squash.total_blocks_result_field, "totalBlocks");
    assert_eq!(squash.status_field, "status");
    // The local helper must agree with the real config shape.
    assert_eq!(
        squash_semantics().total_blocks_result_field,
        squash.total_blocks_result_field
    );
}
