//! UniFFI-friendly DTOs (no `serde_json::Value` in the FFI boundary).

use crate::context_store::{ContextReference, ContextReferenceKind};
use crate::long_loop_policy::LongLoopPolicy;
use crate::manual_compact::{ManualCompactRequest, ManualCompactResult};
use crate::message::{AgentMessage, AgentRole, MessageMeta, RepairRecord, ToolCallRecord};
use crate::policy::ContextPolicyTier;
use crate::session::{AgentConfig, IncludedFile, SessionCapabilities, SessionContext};
use crate::sidecar_builder::ProviderContextSnapshot;
use crate::tool_parse::result::{
    ToolParseBatch, ToolParseFailure, ToolParseFailureReason, ToolParseField,
};
use crate::turn_io::{
    FinishReason, ModelRequest, ModelResponse, RawToolCall, SummaryRequest, SummaryResult,
};

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiAgentRole {
    System,
    User,
    Assistant,
    Tool,
    CompactSummary,
}

impl From<AgentRole> for FfiAgentRole {
    fn from(r: AgentRole) -> Self {
        match r {
            AgentRole::System => FfiAgentRole::System,
            AgentRole::User => FfiAgentRole::User,
            AgentRole::Assistant => FfiAgentRole::Assistant,
            AgentRole::Tool => FfiAgentRole::Tool,
            AgentRole::CompactSummary => FfiAgentRole::CompactSummary,
        }
    }
}

impl From<FfiAgentRole> for AgentRole {
    fn from(r: FfiAgentRole) -> Self {
        match r {
            FfiAgentRole::System => AgentRole::System,
            FfiAgentRole::User => AgentRole::User,
            FfiAgentRole::Assistant => AgentRole::Assistant,
            FfiAgentRole::Tool => AgentRole::Tool,
            FfiAgentRole::CompactSummary => AgentRole::CompactSummary,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAgentMessage {
    pub id: String,
    pub role: FfiAgentRole,
    pub content: String,
    /// JSON object or array as a string (e.g. `"{}"` or `"null"`).
    pub metadata_json: String,
}

impl From<FfiAgentMessage> for AgentMessage {
    fn from(m: FfiAgentMessage) -> Self {
        Self {
            id: m.id,
            role: m.role.into(),
            content: m.content,
            metadata: MessageMeta::from_json_str(&m.metadata_json),
        }
    }
}

impl From<AgentMessage> for FfiAgentMessage {
    fn from(m: AgentMessage) -> Self {
        Self {
            id: m.id,
            role: m.role.into(),
            content: m.content,
            metadata_json: m.metadata.to_json_string(),
        }
    }
}

// ENC-296: the host-owned ToolSchema/ToolCatalogIndex kinds are deleted
// with the tool registry; the remaining engine-created kinds map 1:1.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiContextReferenceKind {
    ToolOutput,
    TerminalOutput,
    HistorySnapshot,
    ArtifactSnapshot,
    ToolIndex,
    FileOutline,
    FileChunk,
}

impl From<ContextReferenceKind> for FfiContextReferenceKind {
    fn from(kind: ContextReferenceKind) -> Self {
        match kind {
            ContextReferenceKind::ToolOutput => FfiContextReferenceKind::ToolOutput,
            ContextReferenceKind::TerminalOutput => FfiContextReferenceKind::TerminalOutput,
            ContextReferenceKind::HistorySnapshot => FfiContextReferenceKind::HistorySnapshot,
            ContextReferenceKind::ArtifactSnapshot => FfiContextReferenceKind::ArtifactSnapshot,
            ContextReferenceKind::ToolIndex => FfiContextReferenceKind::ToolIndex,
            ContextReferenceKind::FileOutline => FfiContextReferenceKind::FileOutline,
            ContextReferenceKind::FileChunk => FfiContextReferenceKind::FileChunk,
        }
    }
}

impl From<FfiContextReferenceKind> for ContextReferenceKind {
    fn from(kind: FfiContextReferenceKind) -> Self {
        match kind {
            FfiContextReferenceKind::ToolOutput => ContextReferenceKind::ToolOutput,
            FfiContextReferenceKind::TerminalOutput => ContextReferenceKind::TerminalOutput,
            FfiContextReferenceKind::HistorySnapshot => ContextReferenceKind::HistorySnapshot,
            FfiContextReferenceKind::ArtifactSnapshot => ContextReferenceKind::ArtifactSnapshot,
            FfiContextReferenceKind::ToolIndex => ContextReferenceKind::ToolIndex,
            FfiContextReferenceKind::FileOutline => ContextReferenceKind::FileOutline,
            FfiContextReferenceKind::FileChunk => ContextReferenceKind::FileChunk,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiContextReference {
    pub id: String,
    pub kind: FfiContextReferenceKind,
    pub title: String,
    pub byte_len: u64,
    pub token_estimate: u32,
    pub preview: String,
    pub retrieval_hint: String,
}

impl From<ContextReference> for FfiContextReference {
    fn from(reference: ContextReference) -> Self {
        Self {
            id: reference.id,
            kind: reference.kind.into(),
            title: reference.title,
            byte_len: reference.byte_len,
            token_estimate: reference.token_estimate,
            preview: reference.preview,
            retrieval_hint: reference.retrieval_hint,
        }
    }
}

impl From<FfiContextReference> for ContextReference {
    fn from(reference: FfiContextReference) -> Self {
        Self {
            id: reference.id,
            kind: reference.kind.into(),
            title: reference.title,
            byte_len: reference.byte_len,
            token_estimate: reference.token_estimate,
            preview: reference.preview,
            retrieval_hint: reference.retrieval_hint,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProviderContextSnapshot {
    pub transcript_revision: u64,
    pub sidecar_generation: u32,
    pub sidecar_byte_len: u32,
    pub compaction_passes_this_turn: u32,
    pub destructive_tiers_this_turn: u32,
    pub pressure_ratio_at_call_model_millis: u32,
}

impl From<ProviderContextSnapshot> for FfiProviderContextSnapshot {
    fn from(snapshot: ProviderContextSnapshot) -> Self {
        Self {
            transcript_revision: snapshot.transcript_revision,
            sidecar_generation: snapshot.sidecar_generation,
            sidecar_byte_len: snapshot.sidecar_byte_len,
            compaction_passes_this_turn: snapshot.compaction_passes_this_turn,
            destructive_tiers_this_turn: snapshot.destructive_tiers_this_turn,
            pressure_ratio_at_call_model_millis: snapshot.pressure_ratio_at_call_model_millis,
        }
    }
}

impl From<FfiProviderContextSnapshot> for ProviderContextSnapshot {
    fn from(snapshot: FfiProviderContextSnapshot) -> Self {
        Self {
            transcript_revision: snapshot.transcript_revision,
            sidecar_generation: snapshot.sidecar_generation,
            sidecar_byte_len: snapshot.sidecar_byte_len,
            compaction_passes_this_turn: snapshot.compaction_passes_this_turn,
            destructive_tiers_this_turn: snapshot.destructive_tiers_this_turn,
            pressure_ratio_at_call_model_millis: snapshot.pressure_ratio_at_call_model_millis,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiModelRequest {
    pub session_messages: Vec<FfiAgentMessage>,
    pub system_prompt: String,
    /// Active tools by name (ENC-305 D1): `ToolName` references into the
    /// session's `ToolUniverse`, resolved fallibly by
    /// [`FfiModelRequest::into_core`]. No JSON state crosses on the request.
    pub active_tools: Vec<String>,
    pub allow_tool_call: bool,
    /// Context references by ID (ENC-305 D1): resolved against the
    /// session's `ContextStore` by [`FfiModelRequest::into_core`].
    pub context_reference_ids: Vec<String>,
    pub cache_hints: Vec<String>,
    pub provider_context: FfiProviderContextSnapshot,
    pub retry_after_ms: Option<u64>,
    pub model_retry_attempt: Option<u32>,
}

/// Fallible strict conversion of one FFI message for the request path
/// (ENC-305 D3): metadata parsing must not fall back to an empty default,
/// so this uses [`MessageMeta::from_json_str_strict`] instead of the
/// lenient `From<FfiAgentMessage>` conversion. Malformed metadata is an
/// `AgentException` naming the message index. `create_session` routes
/// its initial messages through here too (ENC-308 item 10), so creation
/// validates metadata exactly like the per-turn path.
pub(crate) fn agent_message_strict(
    m: FfiAgentMessage,
    index: usize,
) -> Result<AgentMessage, AgentException> {
    let metadata = MessageMeta::from_json_str_strict(&m.metadata_json).map_err(|error| {
        AgentException::Internal {
            detail: format!("session_messages[{index}] metadata_json is not valid JSON: {error}"),
        }
    })?;
    Ok(AgentMessage {
        id: m.id,
        role: m.role.into(),
        content: m.content,
        metadata,
    })
}

impl FfiModelRequest {
    /// Fallible, session-bound inbound conversion (ENC-305 D3), replacing
    /// the deleted infallible `From<FfiModelRequest> for ModelRequest`.
    ///
    /// Resolution rules:
    /// - `active_tools`: each name builds a `ToolName` (blank ->
    ///   `AgentException::Internal` naming the field, following the
    ///   `tool_name_from_ffi` precedent in `ffi/semantics.rs`). When a
    ///   catalog spec resolves for the name (canonical or alias), the
    ///   name is canonicalized to `spec.name` FIRST (ENC-305 fix pass,
    ///   m2) — entry lookup, stub identity, and active-membership all
    ///   use the canonical name, because the canonical name is what
    ///   the entry index and the active set hold; the original name is
    ///   additionally accepted for active membership if it is itself
    ///   in the active set (nothing more: host-activated aliases are
    ///   not a thing). A name resolves iff the universe carries an
    ///   entry for the canonical name, or a catalog spec for it, or
    ///   either form is in the universe's active names (host-activated
    ///   entry-less tools are legitimate tracked state). Resolved
    ///   schemas are the entry cloned, else
    ///   `SchemaEntry::stub(canonical_name)`. A name resolving none of
    ///   these is an `AgentException::Internal` embedding the
    ///   `UnknownToolReference` debug form (the `create_session`
    ///   precedent in `ffi/mod.rs`). The resolved list is deduped
    ///   first-seen on the canonical `ToolName` (m3), so an alias and
    ///   its canonical name in one request yield one schema.
    /// - `context_reference_ids`: each id must exist in `context_store`;
    ///   the store entry's reference populates `context_references`. A
    ///   miss is `AgentException::ContextReferenceNotFound`. IDs are
    ///   deduped first-seen (m3).
    /// - `session_messages`: converted strictly (see
    ///   [`agent_message_strict`]); malformed `metadata_json` is an
    ///   `AgentException::Internal` naming the message index.
    ///
    /// Engine-derived state on the produced `ModelRequest`
    /// (`state_ledger`, `tool_context_index`, `file_context_index`, the
    /// `*_delta_json` fields, and `rendered_runtime_tools`) is
    /// `None`/empty BY CONSTRUCTION: that state is rebuilt by
    /// `build_model_request` per turn and never enters from a request.
    pub fn into_core(
        self,
        universe: &crate::tool_universe::ToolUniverse,
        context_store: &crate::context_store::ContextStore,
    ) -> Result<crate::turn_io::ModelRequest, AgentException> {
        let mut tool_schemas = Vec::with_capacity(self.active_tools.len());
        // m3: dedupe first-seen on the canonical ToolName (post-m2
        // canonicalization), mirroring the activate + outbound-projection
        // precedent, through the crate's one first-seen implementation
        // (`push_first_seen`, ENC-308 item 6).
        let mut seen_tools: Vec<crate::tool_name::ToolName> = Vec::new();
        for raw_name in &self.active_tools {
            let name = crate::tool_name::ToolName::new(raw_name).map_err(|_| {
                AgentException::Internal {
                    detail: "active_tools[] must not be blank".to_string(),
                }
            })?;
            // m2: a resolving spec canonicalizes the name before entry
            // lookup / stub / active-membership logic.
            let spec = universe.spec(&name);
            let canonical = match spec {
                Some(spec) => spec.name.clone(),
                None => name.clone(),
            };
            if !crate::tool_universe::push_first_seen(&mut seen_tools, canonical.clone()) {
                continue;
            }
            let entry = universe.entry(&canonical);
            let resolves = entry.is_some()
                || spec.is_some()
                || universe.active_names().contains(&canonical)
                || universe.active_names().contains(&name);
            if !resolves {
                let error = crate::tool_semantics::ToolSemanticsError::UnknownToolReference {
                    section: "request".to_string(),
                    name: canonical.as_str().to_string(),
                };
                return Err(AgentException::Internal {
                    detail: format!("invalid tool semantics: {error:?}"),
                });
            }
            tool_schemas.push(match entry {
                Some(entry) => entry.clone(),
                None => crate::tool_schema::SchemaEntry::stub(&canonical),
            });
        }

        let mut context_references = Vec::with_capacity(self.context_reference_ids.len());
        // m3: dedupe first-seen on the ID, through `push_first_seen`
        // (ENC-308 item 6).
        let mut seen_ids: Vec<&str> = Vec::new();
        for id in &self.context_reference_ids {
            if !crate::tool_universe::push_first_seen(&mut seen_ids, id.as_str()) {
                continue;
            }
            match context_store.get(id) {
                Some(entry) => context_references.push(entry.reference.clone()),
                None => {
                    return Err(AgentException::ContextReferenceNotFound {
                        reference_id: id.clone(),
                    })
                }
            }
        }

        let session_messages = self
            .session_messages
            .into_iter()
            .enumerate()
            .map(|(index, message)| agent_message_strict(message, index))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ModelRequest {
            session_messages,
            system_prompt: self.system_prompt,
            tool_schemas,
            rendered_runtime_tools: Vec::new(),
            allow_tool_call: self.allow_tool_call,
            context_references,
            state_ledger: None,
            tool_context_index: None,
            file_context_index: None,
            cache_hints: self.cache_hints,
            provider_context: self.provider_context.into(),
            state_ledger_delta_json: None,
            tool_context_index_delta_json: None,
            file_context_index_delta_json: None,
            retry_after_ms: self.retry_after_ms,
            model_retry_attempt: self.model_retry_attempt,
        })
    }
}

impl From<ModelRequest> for FfiModelRequest {
    fn from(m: ModelRequest) -> Self {
        // FFI outbound edge (ENC-305 D2): an infallible pure projection —
        // no parsing or serialization. Active tools project to the names
        // of `tool_schemas` ++ `rendered_runtime_tools`, deduped
        // first-seen; context references project to their IDs in order.
        let mut active_tools: Vec<String> = Vec::new();
        for entry in m.tool_schemas.iter().chain(m.rendered_runtime_tools.iter()) {
            crate::tool_universe::push_first_seen(
                &mut active_tools,
                entry.name.as_str().to_string(),
            );
        }
        Self {
            session_messages: m.session_messages.into_iter().map(Into::into).collect(),
            system_prompt: m.system_prompt,
            active_tools,
            allow_tool_call: m.allow_tool_call,
            context_reference_ids: m
                .context_references
                .iter()
                .map(|reference| reference.id.clone())
                .collect(),
            cache_hints: m.cache_hints,
            provider_context: m.provider_context.into(),
            retry_after_ms: m.retry_after_ms,
            model_retry_attempt: m.model_retry_attempt,
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiModelApiErrorClass {
    Transient,
    Permanent,
}

impl From<crate::model_api_retry::ModelApiErrorClass> for FfiModelApiErrorClass {
    fn from(value: crate::model_api_retry::ModelApiErrorClass) -> Self {
        match value {
            crate::model_api_retry::ModelApiErrorClass::Transient => Self::Transient,
            crate::model_api_retry::ModelApiErrorClass::Permanent => Self::Permanent,
        }
    }
}

impl From<FfiModelApiErrorClass> for crate::model_api_retry::ModelApiErrorClass {
    fn from(value: FfiModelApiErrorClass) -> Self {
        match value {
            FfiModelApiErrorClass::Transient => Self::Transient,
            FfiModelApiErrorClass::Permanent => Self::Permanent,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiModelTurnFailureReport {
    pub error_class: FfiModelApiErrorClass,
    pub http_status: Option<u16>,
    pub provider_code: Option<String>,
    pub message: String,
    pub had_partial_output: bool,
    pub partial_output_chars: u32,
    pub retry_after_ms: Option<u64>,
}

impl From<FfiModelTurnFailureReport> for crate::model_api_retry::ModelTurnFailureReport {
    fn from(value: FfiModelTurnFailureReport) -> Self {
        Self {
            error_class: value.error_class.into(),
            http_status: value.http_status,
            provider_code: value.provider_code,
            message: value.message,
            had_partial_output: value.had_partial_output,
            partial_output_chars: value.partial_output_chars,
            retry_after_ms: value.retry_after_ms,
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiFinishReason {
    ToolUse,
    Stop,
    MaxTokens,
    Other { reason: String },
}

impl From<FinishReason> for FfiFinishReason {
    fn from(r: FinishReason) -> Self {
        match r {
            FinishReason::ToolUse => FfiFinishReason::ToolUse,
            FinishReason::Stop => FfiFinishReason::Stop,
            FinishReason::MaxTokens => FfiFinishReason::MaxTokens,
            FinishReason::Other(s) => FfiFinishReason::Other { reason: s },
        }
    }
}

impl From<FfiFinishReason> for FinishReason {
    fn from(r: FfiFinishReason) -> Self {
        match r {
            FfiFinishReason::ToolUse => FinishReason::ToolUse,
            FfiFinishReason::Stop => FinishReason::Stop,
            FfiFinishReason::MaxTokens => FinishReason::MaxTokens,
            FfiFinishReason::Other { reason } => FinishReason::Other(reason),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiRawToolCall {
    pub id: Option<String>,
    pub name: String,
    pub arguments_json: String,
}

impl From<FfiRawToolCall> for RawToolCall {
    fn from(t: FfiRawToolCall) -> Self {
        Self {
            id: t.id,
            name: t.name,
            arguments_json: t.arguments_json,
        }
    }
}

impl From<RawToolCall> for FfiRawToolCall {
    fn from(t: RawToolCall) -> Self {
        Self {
            id: t.id,
            name: t.name,
            arguments_json: t.arguments_json,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiModelResponse {
    pub assistant_text: Option<String>,
    pub tool_calls: Vec<FfiRawToolCall>,
    pub finish_reason: FfiFinishReason,
    pub provider_reasoning_content: Option<String>,
}

impl From<FfiModelResponse> for ModelResponse {
    fn from(m: FfiModelResponse) -> Self {
        Self {
            assistant_text: m.assistant_text,
            tool_calls: m.tool_calls.into_iter().map(Into::into).collect(),
            finish_reason: m.finish_reason.into(),
            provider_reasoning_content: m.provider_reasoning_content,
        }
    }
}

impl From<ModelResponse> for FfiModelResponse {
    fn from(m: ModelResponse) -> Self {
        Self {
            assistant_text: m.assistant_text,
            tool_calls: m.tool_calls.into_iter().map(Into::into).collect(),
            finish_reason: m.finish_reason.into(),
            provider_reasoning_content: m.provider_reasoning_content,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiRepairRecord {
    pub original_name: String,
    pub applied_aliases: Vec<String>,
    pub confidence: f32,
}

impl From<RepairRecord> for FfiRepairRecord {
    fn from(r: RepairRecord) -> Self {
        Self {
            original_name: r.original_name,
            applied_aliases: r.applied_aliases,
            confidence: r.confidence,
        }
    }
}

impl From<FfiRepairRecord> for RepairRecord {
    fn from(r: FfiRepairRecord) -> Self {
        Self {
            original_name: r.original_name,
            applied_aliases: r.applied_aliases,
            confidence: r.confidence,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolCallRecord {
    pub call_id: String,
    pub name: String,
    pub arguments_json: String,
    pub repair_metadata: Option<FfiRepairRecord>,
}

impl From<FfiToolCallRecord> for ToolCallRecord {
    fn from(t: FfiToolCallRecord) -> Self {
        Self {
            call_id: t.call_id,
            name: t.name,
            arguments_json: t.arguments_json,
            attempt_metadata: None,
            repair_metadata: t.repair_metadata.map(Into::into),
        }
    }
}

impl From<ToolCallRecord> for FfiToolCallRecord {
    fn from(t: ToolCallRecord) -> Self {
        Self {
            call_id: t.call_id,
            name: t.name,
            arguments_json: t.arguments_json,
            repair_metadata: t.repair_metadata.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolResultRecord {
    pub call_id: String,
    pub result_json: String,
    pub truncated: bool,
    pub soft_failure: bool,
    pub activated_tool_names: Vec<String>,
}

impl From<FfiToolResultRecord> for crate::message::ToolResultRecord {
    fn from(t: FfiToolResultRecord) -> Self {
        // Host-declared boundary names: ToolName::new, no snake-splitting;
        // blank entries are dropped (the submit-time merge dedupes anyway).
        Self {
            call_id: t.call_id,
            result_json: t.result_json,
            truncated: t.truncated,
            soft_failure: t.soft_failure,
            activated_tool_names: t
                .activated_tool_names
                .iter()
                .filter_map(|name| crate::tool_name::ToolName::new(name).ok())
                .collect(),
            result_metadata: None,
        }
    }
}

impl From<crate::message::ToolResultRecord> for FfiToolResultRecord {
    fn from(t: crate::message::ToolResultRecord) -> Self {
        Self {
            call_id: t.call_id,
            result_json: t.result_json,
            truncated: t.truncated,
            soft_failure: t.soft_failure,
            activated_tool_names: t
                .activated_tool_names
                .iter()
                .map(|name| name.as_str().to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolParseField {
    pub tool_name: String,
    pub arguments_json: String,
    pub repaired: bool,
    pub repairs: Vec<String>,
}

impl From<ToolParseField> for FfiToolParseField {
    fn from(field: ToolParseField) -> Self {
        Self {
            tool_name: field.tool_name,
            arguments_json: field.arguments_json,
            repaired: field.repaired,
            repairs: field.repairs,
        }
    }
}

impl From<FfiToolParseField> for ToolParseField {
    fn from(field: FfiToolParseField) -> Self {
        Self {
            tool_name: field.tool_name,
            arguments_json: field.arguments_json,
            repaired: field.repaired,
            repairs: field.repairs,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolParseFailure {
    pub reason: FfiToolParseFailureReason,
    pub original: String,
    pub format_hint: Option<String>,
}

impl From<ToolParseFailure> for FfiToolParseFailure {
    fn from(failure: ToolParseFailure) -> Self {
        Self {
            reason: failure.reason.into(),
            original: failure.original,
            format_hint: failure.format_hint,
        }
    }
}

impl From<FfiToolParseFailure> for ToolParseFailure {
    fn from(failure: FfiToolParseFailure) -> Self {
        Self {
            reason: failure.reason.into(),
            original: failure.original,
            format_hint: failure.format_hint,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolParseBatch {
    pub successes: Vec<FfiToolParseField>,
    pub failures: Vec<FfiToolParseFailure>,
    pub visible_prefix: String,
    pub visible_suffix: String,
}

impl From<ToolParseBatch> for FfiToolParseBatch {
    fn from(batch: ToolParseBatch) -> Self {
        Self {
            successes: batch.successes.into_iter().map(Into::into).collect(),
            failures: batch.failures.into_iter().map(Into::into).collect(),
            visible_prefix: batch.visible_prefix,
            visible_suffix: batch.visible_suffix,
        }
    }
}

impl From<FfiToolParseBatch> for ToolParseBatch {
    fn from(batch: FfiToolParseBatch) -> Self {
        Self {
            successes: batch.successes.into_iter().map(Into::into).collect(),
            failures: batch.failures.into_iter().map(Into::into).collect(),
            visible_prefix: batch.visible_prefix,
            visible_suffix: batch.visible_suffix,
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiToolParseFailureReason {
    MalformedEnvelope,
    InvalidJson {
        detail: String,
    },
    UnknownTool {
        name: String,
    },
    UnknownParameter {
        tool: String,
        param: String,
    },
    DuplicateParameter {
        tool: String,
        param: String,
    },
    MissingRequiredParameter {
        tool: String,
        param: String,
    },
    SchemaCoercionFailed {
        tool: String,
        param: String,
        expected: String,
    },
}

impl From<ToolParseFailureReason> for FfiToolParseFailureReason {
    fn from(reason: ToolParseFailureReason) -> Self {
        match reason {
            ToolParseFailureReason::MalformedEnvelope => Self::MalformedEnvelope,
            ToolParseFailureReason::InvalidJson { detail } => Self::InvalidJson { detail },
            ToolParseFailureReason::UnknownTool { name } => Self::UnknownTool { name },
            ToolParseFailureReason::UnknownParameter { tool, param } => {
                Self::UnknownParameter { tool, param }
            }
            ToolParseFailureReason::DuplicateParameter { tool, param } => {
                Self::DuplicateParameter { tool, param }
            }
            ToolParseFailureReason::MissingRequiredParameter { tool, param } => {
                Self::MissingRequiredParameter { tool, param }
            }
            ToolParseFailureReason::SchemaCoercionFailed {
                tool,
                param,
                expected,
            } => Self::SchemaCoercionFailed {
                tool,
                param,
                expected,
            },
        }
    }
}

impl From<FfiToolParseFailureReason> for ToolParseFailureReason {
    fn from(reason: FfiToolParseFailureReason) -> Self {
        match reason {
            FfiToolParseFailureReason::MalformedEnvelope => Self::MalformedEnvelope,
            FfiToolParseFailureReason::InvalidJson { detail } => Self::InvalidJson { detail },
            FfiToolParseFailureReason::UnknownTool { name } => Self::UnknownTool { name },
            FfiToolParseFailureReason::UnknownParameter { tool, param } => {
                Self::UnknownParameter { tool, param }
            }
            FfiToolParseFailureReason::DuplicateParameter { tool, param } => {
                Self::DuplicateParameter { tool, param }
            }
            FfiToolParseFailureReason::MissingRequiredParameter { tool, param } => {
                Self::MissingRequiredParameter { tool, param }
            }
            FfiToolParseFailureReason::SchemaCoercionFailed {
                tool,
                param,
                expected,
            } => Self::SchemaCoercionFailed {
                tool,
                param,
                expected,
            },
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSummaryRequest {
    pub messages_to_summarize: Vec<FfiAgentMessage>,
    pub instruction: String,
    pub last_message_id: String,
    pub trigger: FfiCompactionTrigger,
    pub focus_instruction: Option<String>,
    pub history_ref: Option<String>,
    pub state_ledger_snapshot_json: Option<String>,
}

impl From<FfiSummaryRequest> for SummaryRequest {
    fn from(s: FfiSummaryRequest) -> Self {
        Self {
            messages_to_summarize: s
                .messages_to_summarize
                .into_iter()
                .map(Into::into)
                .collect(),
            instruction: s.instruction,
            last_message_id: s.last_message_id,
            trigger: s.trigger.into(),
            focus_instruction: s.focus_instruction,
            history_ref: s.history_ref,
            state_ledger_snapshot: s
                .state_ledger_snapshot_json
                .and_then(|json| serde_json::from_str(&json).ok()),
        }
    }
}

impl From<SummaryRequest> for FfiSummaryRequest {
    fn from(s: SummaryRequest) -> Self {
        Self {
            messages_to_summarize: s
                .messages_to_summarize
                .into_iter()
                .map(Into::into)
                .collect(),
            instruction: s.instruction,
            last_message_id: s.last_message_id,
            trigger: s.trigger.into(),
            focus_instruction: s.focus_instruction,
            history_ref: s.history_ref,
            state_ledger_snapshot_json: s
                .state_ledger_snapshot
                .and_then(|ledger| serde_json::to_string(&ledger).ok()),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSummaryResult {
    pub summary_text: String,
    pub last_message_id: String,
}

impl From<FfiSummaryResult> for SummaryResult {
    fn from(s: FfiSummaryResult) -> Self {
        Self {
            summary_text: s.summary_text,
            last_message_id: s.last_message_id,
        }
    }
}

impl From<SummaryResult> for FfiSummaryResult {
    fn from(s: SummaryResult) -> Self {
        Self {
            summary_text: s.summary_text,
            last_message_id: s.last_message_id,
        }
    }
}

/// One tool-schema entry at the FFI session boundary (ENC-295, registry
/// removed ENC-296).
///
/// The five typed fields ARE the contract: the host builds these records in
/// its own language, so a malformed entry (missing/duplicate name) is
/// rejected at conversion — unlike the legacy raw JSON strings, whose shape
/// was only assumed. Keys outside these fields are not carried across the
/// boundary. Only the engine-consumed fields travel here (name, description,
/// reproducible, deferrable, max_result_bytes); the host keeps full
/// parameter schemas in its own registry.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolSchemaEntry {
    pub name: String,
    pub description: Option<String>,
    pub reproducible: Option<bool>,
    pub deferrable: Option<bool>,
    pub max_result_bytes: Option<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSessionIoParams {
    pub provider: String,
    pub context_window_tokens: u32,
    pub tool_schemas: Vec<FfiToolSchemaEntry>,
    pub system_prompt: String,
}

/// Rebuild one entry's schema object from the typed fields: every present
/// field is set on the object. Key order normalizes sorted on
/// serialization, matching the legacy host-string renders.
fn tool_schema_entry_value(entry: &FfiToolSchemaEntry) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "name".to_string(),
        serde_json::Value::String(entry.name.clone()),
    );
    if let Some(description) = &entry.description {
        object.insert(
            "description".to_string(),
            serde_json::Value::String(description.clone()),
        );
    }
    if let Some(reproducible) = entry.reproducible {
        object.insert(
            "reproducible".to_string(),
            serde_json::Value::Bool(reproducible),
        );
    }
    if let Some(deferrable) = entry.deferrable {
        object.insert(
            "deferrable".to_string(),
            serde_json::Value::Bool(deferrable),
        );
    }
    if let Some(max_result_bytes) = entry.max_result_bytes {
        object.insert(
            "max_result_bytes".to_string(),
            serde_json::Value::Number(max_result_bytes.into()),
        );
    }
    serde_json::Value::Object(object)
}

/// Convert typed boundary entries into parsed [`SchemaEntry`]s in one
/// pass: each entry's schema object is built by [`tool_schema_entry_value`],
/// then wrapped directly — no serialize-to-string / re-parse round-trip.
/// Names are validated by the shared [`crate::tool_schema::EntryNameChecker`]
/// (the same check the JSON-text channels run): a violation is a typed
/// `AgentException` naming the entry, never a silent empty channel.
fn tool_schema_entries(
    entries: &[FfiToolSchemaEntry],
) -> Result<Vec<crate::tool_schema::SchemaEntry>, AgentException> {
    let mut converted = Vec::with_capacity(entries.len());
    let mut names = crate::tool_schema::EntryNameChecker::new();
    for (index, entry) in entries.iter().enumerate() {
        names
            .check(index, &entry.name)
            .map_err(|error| match error {
                crate::tool_schema::EntryNameError::InvalidName { index } => {
                    AgentException::Internal {
                        detail: format!(
                            "tool schema entry {index} has no valid \"name\" (missing or empty)"
                        ),
                    }
                }
                crate::tool_schema::EntryNameError::DuplicateName { name } => {
                    AgentException::Internal {
                        detail: format!("tool schema entry '{name}' is a duplicate"),
                    }
                }
            })?;
        converted.push(crate::tool_schema::SchemaEntry::from(
            tool_schema_entry_value(entry),
        ));
    }
    Ok(converted)
}

/// FFI session edge: convert the typed params into `SessionIoParams`,
/// constructing the parsed schema set directly from the typed entries.
/// (ENC-297: the catalog travels only on the typed `AgentConfig` path, so
/// there is no catalog channel here at all.)
/// Fallible (never panics): a malformed entry is an `AgentException` the
/// caller must handle.
pub(crate) fn session_io_params(
    p: FfiSessionIoParams,
) -> Result<crate::engine::SessionIoParams, AgentException> {
    let active = tool_schema_entries(&p.tool_schemas)?;
    let schemas = crate::tool_schema::ToolSchemaSet::from_entries(active);
    Ok(crate::engine::SessionIoParams {
        provider: p.provider,
        context_window_tokens: p.context_window_tokens,
        schemas,
        system_prompt: p.system_prompt,
    })
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLongLoopPolicy {
    pub max_tool_turns: u32,
    pub sidecar_byte_budget_per_call: u64,
    pub sidecar_total_byte_budget: u64,
    pub summarize_after_tool_turns: u32,
    pub compact_every_n_tool_turns: u32,
}

impl From<FfiLongLoopPolicy> for LongLoopPolicy {
    fn from(f: FfiLongLoopPolicy) -> Self {
        Self {
            max_tool_turns: f.max_tool_turns,
            sidecar_byte_budget_per_call: f.sidecar_byte_budget_per_call as usize,
            sidecar_total_byte_budget: f.sidecar_total_byte_budget as usize,
            summarize_after_tool_turns: f.summarize_after_tool_turns,
            compact_every_n_tool_turns: f.compact_every_n_tool_turns,
        }
    }
}

impl From<LongLoopPolicy> for FfiLongLoopPolicy {
    fn from(l: LongLoopPolicy) -> Self {
        Self {
            max_tool_turns: l.max_tool_turns,
            sidecar_byte_budget_per_call: l.sidecar_byte_budget_per_call as u64,
            sidecar_total_byte_budget: l.sidecar_total_byte_budget as u64,
            summarize_after_tool_turns: l.summarize_after_tool_turns,
            compact_every_n_tool_turns: l.compact_every_n_tool_turns,
        }
    }
}

/// Host-declared tool-call format (ENC-300 D2): the FFI mirror of
/// [`crate::tool_call_format::ToolCallFormat`]. NOT inside `tool_semantics`;
/// the provider string is untouched.
///
/// Host migration note (ENC-300, intended per ratified design — not a
/// defect): hosts that never set this field get `Structured` and stop mining
/// body text; text-envelope hosts must declare their shape. The FFI surface
/// is breaking on purpose (new field/params).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiToolCallFormat {
    Structured,
    Dsml,
    Cdata,
    BracketArray,
    XmlToolCall,
    RawJson,
    CallBrace,
}

impl From<crate::tool_call_format::ToolCallFormat> for FfiToolCallFormat {
    fn from(format: crate::tool_call_format::ToolCallFormat) -> Self {
        match format {
            crate::tool_call_format::ToolCallFormat::Structured => Self::Structured,
            crate::tool_call_format::ToolCallFormat::Dsml => Self::Dsml,
            crate::tool_call_format::ToolCallFormat::Cdata => Self::Cdata,
            crate::tool_call_format::ToolCallFormat::BracketArray => Self::BracketArray,
            crate::tool_call_format::ToolCallFormat::XmlToolCall => Self::XmlToolCall,
            crate::tool_call_format::ToolCallFormat::RawJson => Self::RawJson,
            crate::tool_call_format::ToolCallFormat::CallBrace => Self::CallBrace,
        }
    }
}

impl From<FfiToolCallFormat> for crate::tool_call_format::ToolCallFormat {
    fn from(format: FfiToolCallFormat) -> Self {
        match format {
            FfiToolCallFormat::Structured => Self::Structured,
            FfiToolCallFormat::Dsml => Self::Dsml,
            FfiToolCallFormat::Cdata => Self::Cdata,
            FfiToolCallFormat::BracketArray => Self::BracketArray,
            FfiToolCallFormat::XmlToolCall => Self::XmlToolCall,
            FfiToolCallFormat::RawJson => Self::RawJson,
            FfiToolCallFormat::CallBrace => Self::CallBrace,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAgentConfig {
    pub max_turns: u32,
    pub threshold_ratio: f32,
    pub micro_keep_recent: u32,
    pub tool_retry_limit: u32,
    pub model_api_retry_limit: u32,
    pub model_api_retry_base_delay_ms: u64,
    /// Typed tool semantics (ENC-297 D1–D2): `None` is host-declared
    /// agnostic — the default when the host supplies no semantics record,
    /// not a parse fallback.
    pub tool_semantics: Option<super::semantics::FfiToolSemantics>,
    /// Policy catalog specs, sibling of the semantics tree (ENC-297 D1):
    /// assigned into `semantics.catalog.tools` at conversion, then validated
    /// by the single fallible core constructor.
    pub tool_specs: Vec<super::semantics::FfiToolSpec>,
    /// Host-declared tool-call format (ENC-300 D2): `Structured` (default)
    /// mines no text; each envelope shape parses only its own markers.
    /// Migration: unset means `Structured` (text mining stops); declare the
    /// shape to keep parsing text envelopes.
    pub tool_call_format: FfiToolCallFormat,
    pub tool_result_budget_cloud_bytes: u64,
    pub tool_result_budget_relaxed_bytes: u64,
    pub tool_result_truncation_pressure_ratio: f32,
    pub mid_turn_threshold_ratio: f32,
    pub post_tool_result_threshold_ratio: f32,
    pub manual_compact_threshold_ratio: f32,
    pub allow_fifo_on_preflight: bool,
    pub allow_fifo_on_mid_turn: bool,
    pub allow_fifo_on_post_tool_result: bool,
    pub require_snapshot_before_fifo: bool,
    pub enable_context_store: bool,
    pub enable_state_ledger: bool,
    pub enable_tool_schema_deferral: bool,
    pub enable_file_condensation: bool,
    pub enable_manual_compact: bool,
    pub tool_spill_threshold_bytes: u64,
    pub terminal_spill_threshold_bytes: u64,
    pub history_snapshot_threshold_tokens: u32,
    pub file_outline_threshold_tokens: u32,
    pub prompt_cache_anchor_threshold_tokens: u32,
    pub output_reserve_ratio: f32,
    pub output_reserve_floor_tokens: u32,
    pub output_reserve_max_tokens: u32,
    pub preflight_token_threshold: Option<u32>,
    pub mid_turn_token_threshold: Option<u32>,
    pub post_tool_result_token_threshold: Option<u32>,
    pub message_threshold: Option<u32>,
    pub retention_window_tokens: u32,
    pub max_model_requests_per_turn: u32,
    pub enable_loop_hooks: bool,
    pub enable_pending_work_guard: bool,
    pub long_loop: FfiLongLoopPolicy,
}

impl Default for FfiAgentConfig {
    fn default() -> Self {
        let c = AgentConfig::default();
        Self {
            max_turns: c.max_turns,
            threshold_ratio: c.threshold_ratio,
            micro_keep_recent: c.micro_keep_recent,
            tool_retry_limit: c.tool_retry_limit,
            model_api_retry_limit: c.model_api_retry_limit,
            model_api_retry_base_delay_ms: c.model_api_retry_base_delay_ms,
            // None is host-declared agnostic (D2): the default FFI config
            // carries no semantics record and no specs.
            tool_semantics: None,
            tool_specs: Vec::new(),
            tool_call_format: FfiToolCallFormat::from(c.tool_call_format),
            tool_result_budget_cloud_bytes: c.tool_result_budget_cloud_bytes as u64,
            tool_result_budget_relaxed_bytes: c.tool_result_budget_relaxed_bytes as u64,
            tool_result_truncation_pressure_ratio: c.tool_result_truncation_pressure_ratio,
            mid_turn_threshold_ratio: c.mid_turn_threshold_ratio,
            post_tool_result_threshold_ratio: c.post_tool_result_threshold_ratio,
            manual_compact_threshold_ratio: c.manual_compact_threshold_ratio,
            allow_fifo_on_preflight: c.allow_fifo_on_preflight,
            allow_fifo_on_mid_turn: c.allow_fifo_on_mid_turn,
            allow_fifo_on_post_tool_result: c.allow_fifo_on_post_tool_result,
            require_snapshot_before_fifo: c.require_snapshot_before_fifo,
            enable_context_store: c.enable_context_store,
            enable_state_ledger: c.enable_state_ledger,
            enable_tool_schema_deferral: c.enable_tool_schema_deferral,
            enable_file_condensation: c.enable_file_condensation,
            enable_manual_compact: c.enable_manual_compact,
            tool_spill_threshold_bytes: c.tool_spill_threshold_bytes as u64,
            terminal_spill_threshold_bytes: c.terminal_spill_threshold_bytes as u64,
            history_snapshot_threshold_tokens: c.history_snapshot_threshold_tokens,
            file_outline_threshold_tokens: c.file_outline_threshold_tokens,
            prompt_cache_anchor_threshold_tokens: c.prompt_cache_anchor_threshold_tokens,
            output_reserve_ratio: c.output_reserve_ratio,
            output_reserve_floor_tokens: c.output_reserve_floor_tokens,
            output_reserve_max_tokens: c.output_reserve_max_tokens,
            preflight_token_threshold: c.preflight_token_threshold,
            mid_turn_token_threshold: c.mid_turn_token_threshold,
            post_tool_result_token_threshold: c.post_tool_result_token_threshold,
            message_threshold: c.message_threshold,
            retention_window_tokens: c.retention_window_tokens,
            max_model_requests_per_turn: c.max_model_requests_per_turn,
            enable_loop_hooks: c.enable_loop_hooks,
            enable_pending_work_guard: c.enable_pending_work_guard,
            long_loop: FfiLongLoopPolicy::from(c.long_loop),
        }
    }
}

/// FFI config edge: convert the typed config into an [`AgentConfig`],
/// following the `session_io_params` fallible pattern (ENC-297 D2).
///
/// The sibling [`FfiToolSpec`]s are assigned into `semantics.catalog.tools`
/// before the single fallible core constructor validates + builds the
/// policy catalog. `None` semantics is host-declared agnostic (not a parse
/// fallback). Invalid semantics (malformed `Value` payloads, duplicate tool
/// names, alias collisions, empty required fields) is an `AgentException`
/// naming the failure — never an `eprintln` + agnostic fallback.
pub(crate) fn agent_config(f: FfiAgentConfig) -> Result<AgentConfig, AgentException> {
    // ENC-297 M1: the single shared conversion — sibling specs assigned,
    // validated, catalog built — with the one error mapping.
    let (semantics, catalog) = super::semantics::typed_to_core(f.tool_semantics, f.tool_specs)?;
    // ENC-297 M5: every AgentConfig field listed explicitly. Fields the FFI
    // record does not carry take their documented core defaults visibly
    // here, so no `..Default::default()` tail can silently inherit (or
    // later drop) a scalar.
    let defaults = AgentConfig::default();
    Ok(AgentConfig {
        max_turns: f.max_turns,
        threshold_ratio: f.threshold_ratio,
        micro_keep_recent: f.micro_keep_recent,
        tool_retry_limit: f.tool_retry_limit,
        model_api_retry_limit: f.model_api_retry_limit,
        model_api_retry_base_delay_ms: f.model_api_retry_base_delay_ms,
        tool_result_budget_cloud_bytes: f.tool_result_budget_cloud_bytes as usize,
        tool_result_budget_relaxed_bytes: f.tool_result_budget_relaxed_bytes as usize,
        tool_result_truncation_pressure_ratio: f.tool_result_truncation_pressure_ratio,
        token_correction: defaults.token_correction,
        mid_turn_threshold_ratio: f.mid_turn_threshold_ratio,
        post_tool_result_threshold_ratio: f.post_tool_result_threshold_ratio,
        manual_compact_threshold_ratio: f.manual_compact_threshold_ratio,
        allow_fifo_on_preflight: f.allow_fifo_on_preflight,
        allow_fifo_on_mid_turn: f.allow_fifo_on_mid_turn,
        allow_fifo_on_post_tool_result: f.allow_fifo_on_post_tool_result,
        require_snapshot_before_fifo: f.require_snapshot_before_fifo,
        enable_context_store: f.enable_context_store,
        enable_state_ledger: f.enable_state_ledger,
        enable_tool_schema_deferral: f.enable_tool_schema_deferral,
        enable_file_condensation: f.enable_file_condensation,
        enable_manual_compact: f.enable_manual_compact,
        tool_spill_threshold_bytes: f.tool_spill_threshold_bytes as usize,
        terminal_spill_threshold_bytes: f.terminal_spill_threshold_bytes as usize,
        history_snapshot_threshold_tokens: f.history_snapshot_threshold_tokens,
        file_outline_threshold_tokens: f.file_outline_threshold_tokens,
        prompt_cache_anchor_threshold_tokens: f.prompt_cache_anchor_threshold_tokens,
        output_reserve_ratio: f.output_reserve_ratio,
        output_reserve_floor_tokens: f.output_reserve_floor_tokens,
        output_reserve_max_tokens: f.output_reserve_max_tokens,
        preflight_token_threshold: f.preflight_token_threshold,
        mid_turn_token_threshold: f.mid_turn_token_threshold,
        post_tool_result_token_threshold: f.post_tool_result_token_threshold,
        turn_threshold: defaults.turn_threshold,
        message_threshold: f.message_threshold,
        compact_on_turn_end: defaults.compact_on_turn_end,
        retention_window_messages: defaults.retention_window_messages,
        retention_window_tokens: f.retention_window_tokens,
        eviction_window_ratio: defaults.eviction_window_ratio,
        enable_loop_hooks: f.enable_loop_hooks,
        enable_doom_loop_guard: defaults.enable_doom_loop_guard,
        enable_pending_work_guard: f.enable_pending_work_guard,
        require_read_before_edit: defaults.require_read_before_edit,
        default_tool_timeout_ms: defaults.default_tool_timeout_ms,
        max_model_requests_per_turn: f.max_model_requests_per_turn,
        long_loop: LongLoopPolicy::from(f.long_loop),
        tool_semantics: semantics,
        tool_call_format: f.tool_call_format.into(),
        tool_catalog: catalog,
    })
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSessionCapabilities {
    pub summarization_allowed: bool,
    pub kv_fixed: bool,
    pub tool_result_budget_tokens: Option<u32>,
    pub context_policy_tier: FfiContextPolicyTier,
    pub tool_schema_deferral_supported: bool,
    pub file_backed_context_supported: bool,
    pub prompt_cache_supported: bool,
    pub history_rewrite_supported: bool,
    pub lifecycle_hooks_supported: bool,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiContextPolicyTier {
    HostedMutable,
    HostedProxy,
    LocalMutable,
    FixedKv,
    HighContext,
    SmallContext,
}

impl From<ContextPolicyTier> for FfiContextPolicyTier {
    fn from(tier: ContextPolicyTier) -> Self {
        match tier {
            ContextPolicyTier::HostedMutable => FfiContextPolicyTier::HostedMutable,
            ContextPolicyTier::HostedProxy => FfiContextPolicyTier::HostedProxy,
            ContextPolicyTier::LocalMutable => FfiContextPolicyTier::LocalMutable,
            ContextPolicyTier::FixedKv => FfiContextPolicyTier::FixedKv,
            ContextPolicyTier::HighContext => FfiContextPolicyTier::HighContext,
            ContextPolicyTier::SmallContext => FfiContextPolicyTier::SmallContext,
        }
    }
}

impl From<FfiContextPolicyTier> for ContextPolicyTier {
    fn from(tier: FfiContextPolicyTier) -> Self {
        match tier {
            FfiContextPolicyTier::HostedMutable => ContextPolicyTier::HostedMutable,
            FfiContextPolicyTier::HostedProxy => ContextPolicyTier::HostedProxy,
            FfiContextPolicyTier::LocalMutable => ContextPolicyTier::LocalMutable,
            FfiContextPolicyTier::FixedKv => ContextPolicyTier::FixedKv,
            FfiContextPolicyTier::HighContext => ContextPolicyTier::HighContext,
            FfiContextPolicyTier::SmallContext => ContextPolicyTier::SmallContext,
        }
    }
}

impl Default for FfiSessionCapabilities {
    fn default() -> Self {
        let caps = SessionCapabilities::default();
        Self {
            summarization_allowed: caps.summarization_allowed,
            kv_fixed: caps.kv_fixed,
            tool_result_budget_tokens: caps.tool_result_budget_tokens,
            context_policy_tier: caps.context_policy_tier.into(),
            tool_schema_deferral_supported: caps.tool_schema_deferral_supported,
            file_backed_context_supported: caps.file_backed_context_supported,
            prompt_cache_supported: caps.prompt_cache_supported,
            history_rewrite_supported: caps.history_rewrite_supported,
            lifecycle_hooks_supported: caps.lifecycle_hooks_supported,
        }
    }
}

impl From<FfiSessionCapabilities> for SessionCapabilities {
    fn from(f: FfiSessionCapabilities) -> Self {
        Self {
            summarization_allowed: f.summarization_allowed,
            kv_fixed: f.kv_fixed,
            tool_result_budget_tokens: f.tool_result_budget_tokens,
            context_policy_tier: f.context_policy_tier.into(),
            tool_schema_deferral_supported: f.tool_schema_deferral_supported,
            file_backed_context_supported: f.file_backed_context_supported,
            prompt_cache_supported: f.prompt_cache_supported,
            history_rewrite_supported: f.history_rewrite_supported,
            request_transform_pipeline_supported: false,
            lifecycle_hooks_supported: f.lifecycle_hooks_supported,
            delegation_tools_supported: false,
            host_policy_callbacks_supported: false,
        }
    }
}

/// FFI twin of [`IncludedFile`] (ENC-308 B1): one host-supplied file in
/// the session context's `included_files` list.
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct FfiIncludedFile {
    pub path: String,
    pub token_estimate: u32,
    pub content: String,
}

impl From<FfiIncludedFile> for IncludedFile {
    fn from(f: FfiIncludedFile) -> Self {
        Self {
            path: f.path,
            token_estimate: f.token_estimate,
            content: f.content,
        }
    }
}

/// Typed session context at the FFI (ENC-308 item 8): mirrors
/// [`SessionContext`]'s closed schema field-for-field, replacing the
/// `session_context_json: String` parameter `create_session` used to
/// parse leniently. The conversion is total — every field maps
/// directly, there is no parse and no fallback.
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct FfiSessionContext {
    pub chat_id: Option<String>,
    pub user_message_id: Option<String>,
    pub assistant_message_id: Option<String>,
    pub history_snapshot_ids: Vec<String>,
    pub pending_work: Vec<String>,
    pub force_finish_blocked: bool,
    pub included_files: Vec<FfiIncludedFile>,
}

impl From<FfiSessionContext> for SessionContext {
    fn from(f: FfiSessionContext) -> Self {
        Self {
            chat_id: f.chat_id,
            user_message_id: f.user_message_id,
            assistant_message_id: f.assistant_message_id,
            history_snapshot_ids: f.history_snapshot_ids,
            pending_work: f.pending_work,
            force_finish_blocked: f.force_finish_blocked,
            included_files: f
                .included_files
                .into_iter()
                .map(IncludedFile::from)
                .collect(),
        }
    }
}

// ── Actions / events / errors (FFI) ─────────────────────────────────────────

use crate::action::AgentAction;
use crate::events::{AgentError, AgentEvent, CompactionEvent, CompactionTier, CompactionTrigger};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, uniffi::Enum)]
pub enum FfiCompactionTrigger {
    Preflight,
    MidTurn,
    PostToolResult,
    Manual,
}

impl From<CompactionTrigger> for FfiCompactionTrigger {
    fn from(t: CompactionTrigger) -> Self {
        match t {
            CompactionTrigger::Preflight => FfiCompactionTrigger::Preflight,
            CompactionTrigger::MidTurn => FfiCompactionTrigger::MidTurn,
            CompactionTrigger::PostToolResult => FfiCompactionTrigger::PostToolResult,
            CompactionTrigger::Manual => FfiCompactionTrigger::Manual,
        }
    }
}

impl From<FfiCompactionTrigger> for CompactionTrigger {
    fn from(t: FfiCompactionTrigger) -> Self {
        match t {
            FfiCompactionTrigger::Preflight => CompactionTrigger::Preflight,
            FfiCompactionTrigger::MidTurn => CompactionTrigger::MidTurn,
            FfiCompactionTrigger::PostToolResult => CompactionTrigger::PostToolResult,
            FfiCompactionTrigger::Manual => CompactionTrigger::Manual,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, uniffi::Enum)]
pub enum FfiCompactionTier {
    Micro,
    Fifo,
    ArtifactSquash,
    Summary,
    ToolSpill,
    HistorySnapshot,
    TextTruncate,
}

impl From<CompactionTier> for FfiCompactionTier {
    fn from(t: CompactionTier) -> Self {
        match t {
            CompactionTier::Micro => FfiCompactionTier::Micro,
            CompactionTier::Fifo => FfiCompactionTier::Fifo,
            CompactionTier::ArtifactSquash => FfiCompactionTier::ArtifactSquash,
            CompactionTier::Summary => FfiCompactionTier::Summary,
            CompactionTier::ToolSpill => FfiCompactionTier::ToolSpill,
            CompactionTier::HistorySnapshot => FfiCompactionTier::HistorySnapshot,
            CompactionTier::TextTruncate => FfiCompactionTier::TextTruncate,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiCompactionEvent {
    pub tier: FfiCompactionTier,
    pub tokens_before: u32,
    pub tokens_after: u32,
    pub trigger: FfiCompactionTrigger,
}

impl From<CompactionEvent> for FfiCompactionEvent {
    fn from(e: CompactionEvent) -> Self {
        Self {
            tier: e.tier.into(),
            tokens_before: e.tokens_before,
            tokens_after: e.tokens_after,
            trigger: e.trigger.into(),
        }
    }
}

/// UniFFI mirror of [`crate::transcript_revision::CompactionBoundary`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct FfiCompactionBoundary {
    pub boundary_id: String,
    pub revision_before: u64,
    pub revision_after: u64,
    pub trigger: FfiCompactionTrigger,
    pub tiers_applied: Vec<FfiCompactionTier>,
    pub message_count_before: u32,
    pub message_count_after: u32,
    pub destructive: bool,
}

impl From<crate::transcript_revision::CompactionBoundary> for FfiCompactionBoundary {
    fn from(b: crate::transcript_revision::CompactionBoundary) -> Self {
        Self {
            boundary_id: b.boundary_id,
            revision_before: b.revision_before,
            revision_after: b.revision_after,
            trigger: b.trigger.into(),
            tiers_applied: b.tiers_applied.into_iter().map(Into::into).collect(),
            message_count_before: b.message_count_before,
            message_count_after: b.message_count_after,
            destructive: b.destructive,
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiAgentEvent {
    HookInvoked {
        checkpoint: String,
    },
    TurnStarted {
        turn_index: u32,
        allow_tool_call: bool,
    },
    TurnCompleted {
        turn_index: u32,
        tool_call_count: u32,
        terminal: bool,
    },
    CompactionApplied {
        event: FfiCompactionEvent,
    },
    ContextPressure {
        ratio: f32,
    },
    ToolRepaired {
        original_name: String,
        resolved_name: String,
        confidence: f32,
    },
    ToolSoftFailure {
        tool_name: String,
        turn_index: u32,
    },
    KvCacheInvalidated {
        reason: String,
    },
    PreCompact {
        trigger: FfiCompactionTrigger,
    },
    PostCompact {
        trigger: FfiCompactionTrigger,
        summary_ref: Option<String>,
        history_ref: Option<String>,
        transcript_revision_after: u64,
    },
    CompactionBoundary {
        boundary: FfiCompactionBoundary,
    },
    ToolPolicyDenied {
        tool_name: String,
    },
    ToolTimedOut {
        tool_name: String,
        timeout_ms: u64,
    },
    VisibleAssistantText {
        text: String,
    },
    ModelApiRetryScheduled {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        reason: String,
    },
}

impl From<AgentEvent> for FfiAgentEvent {
    fn from(ev: AgentEvent) -> Self {
        match ev {
            AgentEvent::HookInvoked { checkpoint } => FfiAgentEvent::HookInvoked { checkpoint },
            AgentEvent::TurnStarted {
                turn_index,
                allow_tool_call,
            } => FfiAgentEvent::TurnStarted {
                turn_index,
                allow_tool_call,
            },
            AgentEvent::TurnCompleted {
                turn_index,
                tool_call_count,
                terminal,
            } => FfiAgentEvent::TurnCompleted {
                turn_index,
                tool_call_count,
                terminal,
            },
            AgentEvent::CompactionApplied(e) => {
                FfiAgentEvent::CompactionApplied { event: e.into() }
            }
            AgentEvent::ContextPressure { ratio } => FfiAgentEvent::ContextPressure { ratio },
            AgentEvent::ToolRepaired {
                original_name,
                resolved_name,
                confidence,
            } => FfiAgentEvent::ToolRepaired {
                original_name,
                resolved_name,
                confidence,
            },
            AgentEvent::ToolSoftFailure {
                tool_name,
                turn_index,
            } => FfiAgentEvent::ToolSoftFailure {
                tool_name,
                turn_index,
            },
            AgentEvent::KvCacheInvalidated { reason } => {
                FfiAgentEvent::KvCacheInvalidated { reason }
            }
            AgentEvent::PreCompact { trigger } => FfiAgentEvent::PreCompact {
                trigger: trigger.into(),
            },
            AgentEvent::PostCompact {
                trigger,
                summary_ref,
                history_ref,
                transcript_revision_after,
            } => FfiAgentEvent::PostCompact {
                trigger: trigger.into(),
                summary_ref,
                history_ref,
                transcript_revision_after,
            },
            AgentEvent::CompactionBoundary(boundary) => FfiAgentEvent::CompactionBoundary {
                boundary: boundary.into(),
            },
            AgentEvent::ToolPolicyDenied { tool_name } => {
                FfiAgentEvent::ToolPolicyDenied { tool_name }
            }
            AgentEvent::ToolTimedOut {
                tool_name,
                timeout_ms,
            } => FfiAgentEvent::ToolTimedOut {
                tool_name,
                timeout_ms,
            },
            AgentEvent::VisibleAssistantText { text } => {
                FfiAgentEvent::VisibleAssistantText { text }
            }
            AgentEvent::ModelApiRetryScheduled {
                attempt,
                max_attempts,
                delay_ms,
                reason,
            } => FfiAgentEvent::ModelApiRetryScheduled {
                attempt,
                max_attempts,
                delay_ms,
                reason,
            },
        }
    }
}

#[derive(Debug, Clone, thiserror::Error, uniffi::Error)]
pub enum AgentException {
    #[error("repair failed")]
    RepairFailed { name: String, reason: String },
    #[error("guardrail")]
    GuardrailExceeded {
        kind: String,
        detail: Option<String>,
    },
    #[error("model stopped")]
    ModelStopped { reason: String },
    #[error("compaction")]
    CompactionFailed { tier: String, reason: String },
    #[error("invalid transition")]
    InvalidTransition { from: String, to: String },
    #[error("internal")]
    Internal { detail: String },
    #[error("context reference not found")]
    ContextReferenceNotFound { reference_id: String },
    #[error("model api unavailable")]
    ModelApiUnavailable { detail: String, attempts: u32 },
}

impl From<AgentError> for AgentException {
    fn from(e: AgentError) -> Self {
        match e {
            AgentError::UnsupportedTool { name } => AgentException::Internal {
                detail: format!("Unsupported tool: {name}"),
            },
            AgentError::RepairFailed { name, reason } => {
                AgentException::RepairFailed { name, reason }
            }
            AgentError::GuardrailExceeded { kind, detail } => {
                AgentException::GuardrailExceeded { kind, detail }
            }
            AgentError::ModelStopped { reason } => AgentException::ModelStopped { reason },
            AgentError::CompactionFailed { tier, reason } => {
                AgentException::CompactionFailed { tier, reason }
            }
            AgentError::InvalidTransition { from, to } => {
                AgentException::InvalidTransition { from, to }
            }
            AgentError::Internal(m) => AgentException::Internal { detail: m },
            AgentError::ContextReferenceNotFound { reference_id } => {
                AgentException::ContextReferenceNotFound { reference_id }
            }
            AgentError::ContextStoreUnavailable { reason } => AgentException::Internal {
                detail: format!("Context store unavailable: {reason}"),
            },
            AgentError::PolicyRejected { reason } => AgentException::Internal {
                detail: format!("Policy rejected: {reason}"),
            },
            AgentError::ModelApiUnavailable { message, attempts } => {
                AgentException::ModelApiUnavailable {
                    detail: message,
                    attempts,
                }
            }
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiManualCompactRequest {
    pub focus_instruction: Option<String>,
    pub force_summary: bool,
}

impl From<FfiManualCompactRequest> for ManualCompactRequest {
    fn from(request: FfiManualCompactRequest) -> Self {
        Self {
            focus_instruction: request.focus_instruction,
            force_summary: request.force_summary,
        }
    }
}

impl From<ManualCompactRequest> for FfiManualCompactRequest {
    fn from(request: ManualCompactRequest) -> Self {
        Self {
            focus_instruction: request.focus_instruction,
            force_summary: request.force_summary,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiManualCompactResult {
    pub summary_ref: Option<String>,
    pub history_ref: Option<String>,
    pub tokens_before: u32,
    pub tokens_after: u32,
}

impl From<ManualCompactResult> for FfiManualCompactResult {
    fn from(result: ManualCompactResult) -> Self {
        Self {
            summary_ref: result.summary_ref,
            history_ref: result.history_ref,
            tokens_before: result.tokens_before,
            tokens_after: result.tokens_after,
        }
    }
}

impl From<FfiManualCompactResult> for ManualCompactResult {
    fn from(result: FfiManualCompactResult) -> Self {
        Self {
            summary_ref: result.summary_ref,
            history_ref: result.history_ref,
            tokens_before: result.tokens_before,
            tokens_after: result.tokens_after,
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiAgentAction {
    CallModel { request: FfiModelRequest },
    ExecuteTools { calls: Vec<FfiToolCallRecord> },
    RequestSummary { request: FfiSummaryRequest },
    Finished,
    Failed { error: AgentException },
    ManualCompactCompleted { result: FfiManualCompactResult },
}

impl From<AgentAction> for FfiAgentAction {
    fn from(a: AgentAction) -> Self {
        match a {
            AgentAction::CallModel(r) => FfiAgentAction::CallModel { request: r.into() },
            AgentAction::ExecuteTools(calls) => FfiAgentAction::ExecuteTools {
                calls: calls.into_iter().map(Into::into).collect(),
            },
            AgentAction::RequestSummary(req) => FfiAgentAction::RequestSummary {
                request: req.into(),
            },
            AgentAction::Finished => FfiAgentAction::Finished,
            AgentAction::Failed(e) => FfiAgentAction::Failed { error: e.into() },
            AgentAction::ManualCompactCompleted(result) => FfiAgentAction::ManualCompactCompleted {
                result: result.into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_store::ContextReferenceKind;
    use crate::manual_compact::ManualCompactResult;

    #[test]
    fn ffi_agent_action_manual_compact_completed_not_finished() {
        let action = AgentAction::ManualCompactCompleted(ManualCompactResult {
            summary_ref: None,
            history_ref: Some("history-u0-a0".into()),
            tokens_before: 100,
            tokens_after: 60,
        });

        match FfiAgentAction::from(action) {
            FfiAgentAction::ManualCompactCompleted { result } => {
                assert_eq!(result.history_ref.as_deref(), Some("history-u0-a0"));
                assert_eq!(result.tokens_before, 100);
                assert_eq!(result.tokens_after, 60);
            }
            other => panic!("expected ManualCompactCompleted, got {other:?}"),
        }
    }

    #[test]
    fn ffi_context_reference_kind_round_trip() {
        let ffi = FfiContextReferenceKind::from(ContextReferenceKind::ArtifactSnapshot);
        let core = ContextReferenceKind::from(ffi);
        assert_eq!(core, ContextReferenceKind::ArtifactSnapshot);
    }

    fn io_with_active(entries: Vec<FfiToolSchemaEntry>) -> FfiSessionIoParams {
        // ENC-296: no tool_schema_registry field; the host owns the registry.
        // ENC-297: no tool_catalog_json field; the catalog travels typed on FfiAgentConfig.
        FfiSessionIoParams {
            provider: "anthropic".into(),
            context_window_tokens: 200_000,
            tool_schemas: entries,
            system_prompt: "sys".into(),
        }
    }

    fn bare_entry(name: &str) -> FfiToolSchemaEntry {
        FfiToolSchemaEntry {
            name: name.into(),
            description: None,
            reproducible: None,
            deferrable: None,
            max_result_bytes: None,
        }
    }

    #[test]
    fn session_io_params_full_entry_round_trips_to_schema_object() {
        // ENC-296: parameters_json is gone (host-owned registry); only the
        // engine-consumed fields cross the boundary.
        let mut entry = bare_entry("read_file");
        entry.description = Some("Read a file".into());
        entry.reproducible = Some(false);
        entry.deferrable = Some(true);
        entry.max_result_bytes = Some(4096);

        let params = session_io_params(io_with_active(vec![entry])).expect("valid entry converts");
        let active: serde_json::Value =
            serde_json::from_str(params.schemas.active_json()).expect("active channel is JSON");
        assert_eq!(
            active,
            serde_json::json!([{
                "name": "read_file",
                "description": "Read a file",
                "reproducible": false,
                "deferrable": true,
                "max_result_bytes": 4096,
            }])
        );
    }

    #[test]
    fn session_io_params_name_only_entry_yields_name_only_object() {
        let params =
            session_io_params(io_with_active(vec![bare_entry("web_search")])).expect("converts");
        let active: serde_json::Value =
            serde_json::from_str(params.schemas.active_json()).expect("active channel is JSON");
        assert_eq!(active, serde_json::json!([{"name": "web_search"}]));
    }

    #[test]
    fn session_io_params_rejects_blank_entry_name() {
        // ENC-296: with parameters_json deleted, the remaining malformed
        // input is a missing/blank name — still a typed error naming the
        // entry index, never a silent fallback.
        let error = session_io_params(io_with_active(vec![bare_entry("  ")])).err();
        match error {
            Some(AgentException::Internal { detail }) => {
                assert!(
                    detail.contains("has no valid"),
                    "unexpected detail: {detail}"
                );
            }
            other => panic!("expected typed Internal error, got {other:?}"),
        }
    }

    #[test]
    fn session_io_params_rejects_duplicate_entry_names() {
        let error = session_io_params(io_with_active(vec![
            bare_entry("web_search"),
            bare_entry("web_search"),
        ]))
        .err();
        match error {
            Some(AgentException::Internal { detail }) => {
                assert!(detail.contains("web_search"), "unexpected detail: {detail}");
                assert!(detail.contains("duplicate"), "unexpected detail: {detail}");
            }
            other => panic!("expected typed Internal error, got {other:?}"),
        }
    }

    #[test]
    fn ffi_session_context_converts_every_field() {
        // ENC-308 item 8: the conversion is total — each record field
        // lands on its SessionContext counterpart, included_files as
        // typed entries.
        let context = SessionContext::from(FfiSessionContext {
            chat_id: Some("chat-1".into()),
            user_message_id: Some("u-1".into()),
            assistant_message_id: Some("a-1".into()),
            history_snapshot_ids: vec!["m0".into()],
            pending_work: vec!["task".into()],
            force_finish_blocked: true,
            included_files: vec![FfiIncludedFile {
                path: "src/big.rs".into(),
                token_estimate: 42,
                content: "fn x() {}".into(),
            }],
        });
        assert_eq!(context.chat_id.as_deref(), Some("chat-1"));
        assert_eq!(context.user_message_id.as_deref(), Some("u-1"));
        assert_eq!(context.assistant_message_id.as_deref(), Some("a-1"));
        assert_eq!(context.history_snapshot_ids, vec!["m0".to_string()]);
        assert_eq!(context.pending_work, vec!["task".to_string()]);
        assert!(context.force_finish_blocked);
        assert_eq!(
            context.included_files,
            vec![IncludedFile {
                path: "src/big.rs".to_string(),
                token_estimate: 42,
                content: "fn x() {}".to_string(),
            }]
        );
    }
}
