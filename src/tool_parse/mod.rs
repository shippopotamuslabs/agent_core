//! Public Rust entry point for canonical tool-call parsing.
//!
//! Dependency direction (ENC-291): this module delegates to
//! [`crate::canonicalize`]; the shared primitives live in the leaf modules
//! [`crate::json_repair`] and [`crate::schema_coerce`] (re-exported below
//! under their historic paths for compatibility), so the
//! `canonicalize` <-> `tool_parse` import is one-way
//! (`tool_parse` -> `canonicalize`).

pub mod envelope;
pub mod envelope_error;
/// Historic path; the implementation lives in [`crate::json_repair`].
pub mod json_repair {
    pub use crate::json_repair::{repair_json, repair_json_value_inplace, JsonRepairError};
}
pub mod result;
/// Historic path; the implementation lives in [`crate::schema_coerce`].
pub mod schema_coerce {
    pub use crate::schema_coerce::{
        coerce_to_schema, extract_array_from_string, try_parse_string_as_value,
    };
}

pub use envelope_error::envelope_failure_json;

use crate::canonicalize::{
    canonicalize_raw_arguments_with_semantics, canonicalize_tool_call_with_semantics,
    CanonicalizeToolCallOutcome, ToolCallCanonicalizeMode,
};
use crate::catalog::{ToolCatalog, ToolSchemaHint};
use crate::tool_semantics::ToolSemanticsConfig;
use crate::tool_stream_gate::normalize_spaced_dsml;
use crate::turn_io::RawToolCall;
pub use envelope::{EnvelopeError, EnvelopeFormat, ExtractedEnvelope};
pub use json_repair::{repair_json, JsonRepairError};
use once_cell::sync::Lazy;
use regex::{Captures, Match, Regex};
pub use result::{ToolParseBatch, ToolParseFailure, ToolParseFailureReason, ToolParseField};
pub use schema_coerce::coerce_to_schema;
use serde_json::Value;
use std::collections::HashSet;
use std::ops::Range;

pub fn parse_tool_calls_with_semantics(
    raw: &str,
    catalog: &ToolCatalog,
    semantics: &ToolSemanticsConfig,
) -> ToolParseBatch {
    let normalized = normalize_model_text(raw);
    match extract_envelopes(&normalized) {
        Ok(extracted) if extracted.is_empty() => ToolParseBatch::empty(raw),
        Ok(extracted) => build_batch_with_semantics(raw, extracted, catalog, semantics),
        Err(error) => ToolParseBatch {
            successes: Vec::new(),
            failures: vec![ToolParseFailure {
                reason: ToolParseFailureReason::MalformedEnvelope,
                original: raw.to_string(),
                format_hint: error.format.and_then(format_hint),
            }],
            visible_prefix: String::new(),
            visible_suffix: String::new(),
        },
    }
}

pub fn coerce_provider_arguments_with_semantics(
    tool_name: &str,
    args_json: &str,
    catalog: &ToolCatalog,
    semantics: &ToolSemanticsConfig,
) -> ToolParseField {
    let raw = RawToolCall {
        id: None,
        name: tool_name.to_string(),
        arguments_json: args_json.to_string(),
    };
    match canonicalize_tool_call_with_semantics(
        &raw,
        catalog,
        semantics,
        ToolCallCanonicalizeMode::ProviderIngress,
    )
    .0
    {
        CanonicalizeToolCallOutcome::Ok(call) => ToolParseField {
            tool_name: call.resolved_name,
            arguments_json: call.arguments_json,
            repaired: call.repair_metadata.is_some() || !call.argument_repairs.is_empty(),
            repairs: call.argument_repairs,
        },
        CanonicalizeToolCallOutcome::Reject {
            error_json_for_model,
        } => ToolParseField {
            tool_name: tool_name.to_string(),
            arguments_json: args_json.to_string(),
            repaired: true,
            repairs: vec![format!(
                "provider argument canonicalization failed: {error_json_for_model}"
            )],
        },
        CanonicalizeToolCallOutcome::Suggest { .. } => ToolParseField {
            tool_name: tool_name.to_string(),
            arguments_json: args_json.to_string(),
            repaired: false,
            repairs: Vec::new(),
        },
    }
}

/// True iff `system_prompt` contains a local-tool marker AND names a catalog tool.
pub fn has_local_tool_contract(system_prompt: &str, catalog: &ToolCatalog) -> bool {
    let has_marker = [
        "<tool_call",
        "{\"name\"",
        "<![CDATA[<tool",
        "DSML",
        "invoke name=",
    ]
    .iter()
    .any(|marker| system_prompt.contains(marker));
    has_marker
        && catalog
            .all()
            .iter()
            .any(|tool| system_prompt.contains(&tool.name))
}

fn build_batch_with_semantics(
    raw: &str,
    extracted: Vec<ExtractedEnvelope>,
    catalog: &ToolCatalog,
    semantics: &ToolSemanticsConfig,
) -> ToolParseBatch {
    let visible_prefix = extracted
        .first()
        .map(|env| env.visible_prefix.clone())
        .unwrap_or_default();
    let visible_suffix = extracted
        .last()
        .map(|env| env.visible_suffix.clone())
        .unwrap_or_default();
    let mut successes = Vec::new();
    let mut failures = Vec::new();

    for envelope in extracted {
        match parse_extracted_envelope_with_semantics(&envelope, catalog, semantics) {
            Ok(field) => successes.push(field),
            Err(reason) => failures.push(ToolParseFailure {
                reason,
                original: raw.to_string(),
                format_hint: format_hint(envelope.format),
            }),
        }
    }

    ToolParseBatch {
        successes,
        failures,
        visible_prefix,
        visible_suffix,
    }
}

fn parse_extracted_envelope_with_semantics(
    envelope: &ExtractedEnvelope,
    catalog: &ToolCatalog,
    semantics: &ToolSemanticsConfig,
) -> Result<ToolParseField, ToolParseFailureReason> {
    let canonical_name = catalog
        .resolve(&envelope.tool_name)
        .map(|spec| spec.name.clone())
        .ok_or_else(|| ToolParseFailureReason::UnknownTool {
            name: envelope.tool_name.clone(),
        })?;
    let mut repairs = Vec::new();
    repairs.extend(envelope.repair_hints.clone());
    collect_repairs(&envelope.arguments_raw, &mut repairs);
    if envelope.arguments_raw.contains("[https://") || envelope.arguments_raw.contains("[http://") {
        push_repair(
            &mut repairs,
            "stripped leading bracket from bracketed URL string",
        );
    }

    let canonicalized = canonicalize_raw_arguments_with_semantics(
        &canonical_name,
        &envelope.arguments_raw,
        catalog,
        semantics,
        ToolCallCanonicalizeMode::ProviderIngress,
    )
    .map_err(|error| map_canonicalize_error(&canonical_name, &error.to_string()))?;

    repairs.extend(canonicalized.repairs);
    validate_unknown_and_duplicate(&canonical_name, &canonicalized.value, catalog)?;
    validate_required(&canonical_name, &canonicalized.value, catalog, semantics)?;

    Ok(ToolParseField {
        tool_name: canonical_name,
        arguments_json: canonicalized.arguments_json,
        repaired: canonicalized.repaired || !repairs.is_empty(),
        repairs,
    })
}

fn validate_unknown_and_duplicate(
    tool: &str,
    value: &Value,
    catalog: &ToolCatalog,
) -> Result<(), ToolParseFailureReason> {
    if let Some(duplicate) = value
        .as_str()
        .and_then(|s| s.strip_prefix("__duplicate__:"))
    {
        return Err(ToolParseFailureReason::DuplicateParameter {
            tool: tool.to_string(),
            param: duplicate.to_string(),
        });
    }
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let allowed = allowed_parameters(tool, catalog);
    if allowed.is_empty() {
        return Ok(());
    }
    for key in object.keys() {
        if !allowed.contains(key) {
            return Err(ToolParseFailureReason::UnknownParameter {
                tool: tool.to_string(),
                param: key.clone(),
            });
        }
    }
    Ok(())
}

fn validate_required(
    tool: &str,
    value: &Value,
    catalog: &ToolCatalog,
    semantics: &ToolSemanticsConfig,
) -> Result<(), ToolParseFailureReason> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    for required in required_parameters(tool, catalog) {
        let missing = if semantics.artifact.as_ref().is_some_and(|artifact| {
            artifact.append_tool_name == tool && required == artifact.append_blocks_field
        }) {
            object.get(&required).is_none()
        } else {
            match object.get(&required) {
                None => true,
                Some(value) => is_missing_value(value),
            }
        };
        if missing {
            return Err(ToolParseFailureReason::MissingRequiredParameter {
                tool: tool.to_string(),
                param: required.to_string(),
            });
        }
    }
    Ok(())
}

fn is_missing_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

fn allowed_parameters(tool: &str, catalog: &ToolCatalog) -> HashSet<String> {
    let Some(spec) = catalog.resolve(tool) else {
        return HashSet::new();
    };
    spec.schema_hint
        .as_ref()
        .map(schema_parameter_names)
        .unwrap_or_default()
}

fn required_parameters(tool: &str, catalog: &ToolCatalog) -> Vec<String> {
    let Some(spec) = catalog.resolve(tool) else {
        return Vec::new();
    };
    spec.schema_hint
        .as_ref()
        .map(|hint| {
            hint.required_fields
                .iter()
                .map(|field| field.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn schema_parameter_names(hint: &ToolSchemaHint) -> HashSet<String> {
    hint.required_fields
        .iter()
        .map(|field| field.name.clone())
        .chain(hint.defaults.keys().cloned())
        .collect()
}

fn map_canonicalize_error(tool: &str, detail: &str) -> ToolParseFailureReason {
    let missing_prefix = "Missing required field '";
    if let Some(rest) = detail.strip_prefix(missing_prefix) {
        if let Some((param, _)) = rest.split_once('\'') {
            return ToolParseFailureReason::MissingRequiredParameter {
                tool: tool.to_string(),
                param: param.to_string(),
            };
        }
    }
    ToolParseFailureReason::InvalidJson {
        detail: detail.to_string(),
    }
}

fn extract_envelopes(raw: &str) -> Result<Vec<ExtractedEnvelope>, EnvelopeError> {
    if raw.contains("[TOOL_CALLS]") {
        return extract_mistral(raw);
    }
    if raw.contains("DSML") {
        return extract_dsml(raw);
    }
    if raw.contains("<tool_call") {
        return extract_tool_call(raw);
    }
    if raw.contains("<![CDATA[<tool") {
        return extract_cdata(raw);
    }
    if Regex::new(r"(?is)\bcall\s*:\s*[a-zA-Z_][a-zA-Z0-9_]*\s*\{")
        .unwrap()
        .is_match(raw)
    {
        return extract_litert_call(raw);
    }
    if raw.contains("\"name\"") || raw.contains("'name'") || raw.contains("\u{201C}name\u{201D}") {
        return extract_raw_json(raw);
    }
    Ok(Vec::new())
}

fn extract_litert_call(raw: &str) -> Result<Vec<ExtractedEnvelope>, EnvelopeError> {
    let call = Regex::new(r"(?is)\bcall\s*:\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*\{").unwrap();
    let captures = call
        .captures(raw)
        .ok_or_else(|| malformed(Some(EnvelopeFormat::LiteRtCall), "missing call envelope"))?;
    let full = captures.get(0).unwrap();
    let arguments_start = full.end() - 1;
    let arguments_end = matching_delimiter(raw, arguments_start, '{', '}');

    let (arguments_raw, visible_suffix, repair_hints) = if let Some(end) = arguments_end {
        let args = normalize_litert_arguments(&raw[arguments_start..=end]);
        let suffix = raw[end + 1..].trim_start().to_string();
        (
            args,
            suffix,
            vec!["converted call: tool syntax to canonical envelope".to_string()],
        )
    } else {
        // No closing brace found. Recover only when this envelope runs to
        // end of text (EOS truncation) AND there is actual argument content
        // after the opening brace. If there is a second call: opener after
        // this one, or the brace is the last non-whitespace character, keep
        // hard-failure semantics.
        let after_open = full.end();
        let has_later_envelope = call.find_at(raw, after_open).is_some();
        let has_argument_content = !raw[after_open..].trim().is_empty();
        if has_later_envelope || !has_argument_content {
            return Err(malformed(
                Some(EnvelopeFormat::LiteRtCall),
                "unclosed call envelope",
            ));
        }
        let args = normalize_litert_arguments(&raw[arguments_start..]);
        let hints = vec![
            "converted call: tool syntax to canonical envelope".to_string(),
            "recovered unclosed call envelope truncated at end of output".to_string(),
        ];
        (args, String::new(), hints)
    };

    Ok(vec![ExtractedEnvelope {
        tool_name: captures.get(1).unwrap().as_str().trim().to_string(),
        arguments_raw,
        visible_prefix: raw[..full.start()].trim_end().to_string(),
        visible_suffix,
        format: EnvelopeFormat::LiteRtCall,
        repair_hints,
    }])
}

fn normalize_litert_arguments(raw: &str) -> String {
    let sentinel_normalized = raw.replace("<|\"|>", "\"");
    let mut normalized = String::with_capacity(sentinel_normalized.len());
    let mut in_string = false;
    let mut escape = false;

    for char in sentinel_normalized.chars() {
        if in_string {
            normalized.push(char);
            if escape {
                escape = false;
            } else if char == '\\' {
                escape = true;
            } else if char == '"' {
                in_string = false;
            }
            continue;
        }

        match char {
            '"' => {
                in_string = true;
                normalized.push(char);
            }
            '=' => normalized.push(':'),
            _ => normalized.push(char),
        }
    }
    normalized
}

fn extract_mistral(raw: &str) -> Result<Vec<ExtractedEnvelope>, EnvelopeError> {
    let sentinel = "[TOOL_CALLS]";
    let start = raw.find(sentinel).unwrap_or_default();
    let after = start + sentinel.len();
    let Some(array_start_rel) = raw[after..].find('[') else {
        return Err(malformed(
            Some(EnvelopeFormat::Mistral),
            "missing Mistral array",
        ));
    };
    let array_start = after + array_start_rel;
    let Some(array_end) = matching_delimiter(raw, array_start, '[', ']') else {
        return Err(malformed(
            Some(EnvelopeFormat::Mistral),
            "unclosed Mistral array",
        ));
    };
    let payload = &raw[array_start..=array_end];
    let values = repair_json(payload)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .ok_or_else(|| malformed(Some(EnvelopeFormat::Mistral), "invalid Mistral array"))?;
    calls_from_values(
        values,
        EnvelopeFormat::Mistral,
        raw[..start].trim().to_string(),
        raw[array_end + 1..].to_string(),
        Vec::new(),
    )
}

/// Compiled matchers for the DSML envelope shape, compiled once for the
/// process and shared by every extraction.
struct DsmlPatterns {
    block_open: Regex,
    block_close: Regex,
    invoke_open: Regex,
    invoke_close: Regex,
    parameter_open: Regex,
    parameter_close: Regex,
}

impl DsmlPatterns {
    fn compile() -> DsmlPatterns {
        DsmlPatterns {
            block_open: Regex::new(r#"(?is)<\s*\|*\s*DSML\s*\|*\s*tool_calls\s*>"#).unwrap(),
            block_close: Regex::new(r#"(?is)</\s*\|*\s*DSML\s*\|*\s*tool_calls\s*>"#).unwrap(),
            invoke_open: Regex::new(
                r#"(?is)<\s*\|*\s*DSML\s*\|*\s*invoke\s+name\s*=\s*"([^"]+)"\s*>"#,
            )
            .unwrap(),
            invoke_close: Regex::new(r#"(?is)</\s*\|*\s*DSML\s*\|*\s*invoke\s*>"#).unwrap(),
            parameter_open: Regex::new(
                r#"(?is)<\s*\|*\s*DSML\s*\|*\s*parameter\s+name\s*=\s*"([^"]+)"(?:\s+string\s*=\s*"([^"]*)")?\s*>"#,
            )
            .unwrap(),
            parameter_close: Regex::new(r#"(?is)</\s*\|*\s*DSML\s*\|*\s*parameter\s*>"#).unwrap(),
        }
    }
}

static DSML_PATTERNS: Lazy<DsmlPatterns> = Lazy::new(DsmlPatterns::compile);

fn extract_dsml(raw: &str) -> Result<Vec<ExtractedEnvelope>, EnvelopeError> {
    let starts: Vec<_> = DSML_PATTERNS.block_open.find_iter(raw).collect();
    if starts.is_empty() {
        return Err(malformed(Some(EnvelopeFormat::Dsml), "missing DSML block"));
    }
    let mut envelopes = Vec::new();
    for block_start in &starts {
        envelopes.extend(DsmlBlock::locate(raw, block_start)?.envelopes()?);
    }
    Ok(envelopes)
}

/// One located `<tool_calls>` block: the raw output, the block's inner
/// slice, and the raw offsets bounding it. Invoke and parameter parsing
/// are methods here, so visible prefix/suffix slicing lives in exactly
/// one place instead of being threaded as bare, swappable offsets
/// through free functions.
struct DsmlBlock<'a> {
    raw: &'a str,
    block: &'a str,
    /// Raw offset where the block's open tag starts (visible prefix end).
    start: usize,
    /// Raw offset just past the block's close tag (visible suffix start).
    end: usize,
}

impl<'a> DsmlBlock<'a> {
    fn locate(raw: &'a str, block_start: &Match) -> Result<DsmlBlock<'a>, EnvelopeError> {
        let block_end = DSML_PATTERNS
            .block_close
            .find_at(raw, block_start.end())
            .ok_or_else(|| malformed(Some(EnvelopeFormat::Dsml), "unclosed DSML block"))?;
        Ok(DsmlBlock {
            raw,
            block: &raw[block_start.end()..block_end.start()],
            start: block_start.start(),
            end: block_end.end(),
        })
    }

    fn visible_prefix(&self) -> String {
        self.raw[..self.start].to_string()
    }

    fn visible_suffix(&self) -> String {
        self.raw[self.end..].to_string()
    }

    /// Scan the block: per-invoke parse, then collect.
    fn envelopes(&self) -> Result<Vec<ExtractedEnvelope>, EnvelopeError> {
        let mut envelopes = Vec::new();
        let mut search_at = 0usize;
        let mut found_invoke = false;
        while let Some(invoke) = DSML_PATTERNS.invoke_open.captures(&self.block[search_at..]) {
            found_invoke = true;
            let (parsed, resume_at) = self.parse_invoke(&invoke, search_at)?;
            envelopes.extend(parsed);
            search_at = resume_at;
        }
        if !found_invoke {
            return Err(malformed(Some(EnvelopeFormat::Dsml), "missing DSML invoke"));
        }
        Ok(envelopes)
    }

    /// Parse one `<invoke>` element into its envelopes: each duplicate parameter
    /// yields a soft-failure envelope (parsing continues, first value wins),
    /// followed by the invoke's success envelope. Returns the invoke's envelopes
    /// plus the block offset where scanning for the next invoke resumes.
    fn parse_invoke(
        &self,
        invoke: &Captures<'_>,
        search_at: usize,
    ) -> Result<(Vec<ExtractedEnvelope>, usize), EnvelopeError> {
        let full = invoke.get(0).unwrap();
        let invoke_start = search_at + full.start();
        let invoke_content_start = search_at + full.end();
        let close = DSML_PATTERNS
            .invoke_close
            .find_at(self.block, invoke_content_start)
            .ok_or_else(|| malformed(Some(EnvelopeFormat::Dsml), "unclosed DSML invoke"))?;
        let body = &self.block[invoke_content_start..close.start()];
        let tool_name = invoke.get(1).unwrap().as_str().trim().to_string();
        let (args, mut envelopes) = self.collect_parameters(body, &tool_name)?;
        envelopes.push(ExtractedEnvelope {
            tool_name,
            arguments_raw: Value::Object(args).to_string(),
            visible_prefix: self.visible_prefix(),
            visible_suffix: self.visible_suffix(),
            format: EnvelopeFormat::Dsml,
            repair_hints: Vec::new(),
        });
        Ok((envelopes, close.end().max(invoke_start + 1)))
    }

    /// Collect one invoke body's `<parameter>` elements into an arguments map.
    /// Each duplicate parameter yields a soft-failure envelope in the returned
    /// vec (parsing continues, first value wins); structural problems are `Err`.
    fn collect_parameters(
        &self,
        body: &str,
        tool_name: &str,
    ) -> Result<(serde_json::Map<String, Value>, Vec<ExtractedEnvelope>), EnvelopeError> {
        let mut args = serde_json::Map::new();
        let mut envelopes = Vec::new();
        let mut seen = HashSet::new();
        let mut parameter_search_at = 0usize;
        let mut found_parameter = false;
        while let Some(parameter) = DSML_PATTERNS
            .parameter_open
            .captures(&body[parameter_search_at..])
        {
            found_parameter = true;
            let p_full = parameter.get(0).unwrap();
            let p_start = parameter_search_at + p_full.start();
            let p_body_start = parameter_search_at + p_full.end();
            let p_close = DSML_PATTERNS
                .parameter_close
                .find_at(body, p_body_start)
                .ok_or_else(|| malformed(Some(EnvelopeFormat::Dsml), "unclosed DSML parameter"))?;
            if DSML_PATTERNS
                .invoke_close
                .is_match(&body[p_body_start..p_close.start()])
            {
                return Err(malformed(
                    Some(EnvelopeFormat::Dsml),
                    "unclosed DSML parameter",
                ));
            }
            let param_name = parameter.get(1).unwrap().as_str().trim().to_string();
            if !seen.insert(param_name.clone()) {
                envelopes.push(failure_envelope(
                    tool_name.to_string(),
                    format!("__duplicate__:{param_name}"),
                    self.start,
                    self.end,
                    self.raw,
                ));
                parameter_search_at = p_close.end();
                continue;
            }
            let body_raw = body[p_body_start..p_close.start()].trim();
            let is_json = parameter.get(2).is_some_and(|m| m.as_str() == "false");
            args.insert(param_name, dsml_parameter_value(body_raw, is_json));
            parameter_search_at = p_close.end().max(p_start + 1);
        }
        if !found_parameter {
            return Err(malformed(
                Some(EnvelopeFormat::Dsml),
                "missing DSML parameter",
            ));
        }
        Ok((args, envelopes))
    }
}

/// Open and close tag spans of one `<tool_call>` envelope in the raw
/// output: payload is `open.end..close.start`, visible prefix ends at
/// `open.start`, visible suffix starts at `close.end`.
struct ToolCallSpan {
    open: Range<usize>,
    close: Range<usize>,
}

fn extract_tool_call(raw: &str) -> Result<Vec<ExtractedEnvelope>, EnvelopeError> {
    let open = Regex::new(r#"(?is)<\s*tool_call\b[^>]*>"#).unwrap();
    let Some(start) = open.find(raw) else {
        return Ok(Vec::new());
    };
    let Some(close_start_rel) = raw[start.end()..].to_ascii_lowercase().find("</tool_call>") else {
        // No closing tag. Recovery is allowed only for EOS truncation: the
        // open tag must be the last one in the text and the payload must be
        // non-empty. GLM form requires an explicit close and never recovers.
        if open.find_at(raw, start.end()).is_some() {
            return Err(malformed(
                Some(EnvelopeFormat::ToolCall),
                "unclosed tool_call",
            ));
        }
        let payload = raw[start.end()..].trim();
        if payload.is_empty() {
            return Err(malformed(
                Some(EnvelopeFormat::ToolCall),
                "invalid tool_call payload",
            ));
        }
        if payload.contains("<arg_key>") {
            return Err(malformed(
                Some(EnvelopeFormat::ToolCall),
                "unclosed tool_call",
            ));
        }
        return recover_truncated_tool_call(raw, &start);
    };
    let close_start = start.end() + close_start_rel;
    parse_closed_tool_call(
        raw,
        ToolCallSpan {
            open: start.start()..start.end(),
            close: close_start..close_start + "</tool_call>".len(),
        },
    )
}

/// Parse a `<tool_call>` envelope with both tags present.
fn parse_closed_tool_call(
    raw: &str,
    span: ToolCallSpan,
) -> Result<Vec<ExtractedEnvelope>, EnvelopeError> {
    let payload = raw[span.open.end..span.close.start].trim();
    if payload.contains("<arg_key>") {
        return extract_glm_tool_call(raw, span.open.start, span.close.end, payload);
    }
    let value = repair_json(payload)
        .map_err(|_| malformed(Some(EnvelopeFormat::ToolCall), "invalid tool_call payload"))?;
    let repair_hints = if payload.contains('\'') {
        vec!["converted single quotes to double quotes".to_string()]
    } else {
        Vec::new()
    };
    values_from_payload(
        value,
        EnvelopeFormat::ToolCall,
        raw[..span.open.start].to_string(),
        raw[span.close.end..].to_string(),
        repair_hints,
    )
}

/// Recover an EOS-truncated `<tool_call>` envelope: the open tag is the last
/// one in the text and the payload holds non-GLM content.
fn recover_truncated_tool_call(
    raw: &str,
    start: &Match,
) -> Result<Vec<ExtractedEnvelope>, EnvelopeError> {
    let payload = raw[start.end()..].trim();
    let value = repair_json(payload)
        .map_err(|_| malformed(Some(EnvelopeFormat::ToolCall), "invalid tool_call payload"))?;
    let mut repair_hints =
        vec!["recovered unclosed tool_call envelope truncated at end of output".to_string()];
    if payload.contains('\'') {
        repair_hints.push("converted single quotes to double quotes".to_string());
    }
    values_from_payload(
        value,
        EnvelopeFormat::ToolCall,
        raw[..start.start()].to_string(),
        String::new(),
        repair_hints,
    )
}

fn extract_glm_tool_call(
    raw: &str,
    start: usize,
    end: usize,
    payload: &str,
) -> Result<Vec<ExtractedEnvelope>, EnvelopeError> {
    let glm = Regex::new(r#"(?is)^\s*([a-zA-Z0-9_]+)\s*<arg_key>([^<]+)</arg_key>\s*<arg_value>(.*?)</arg_value>\s*$"#)
        .unwrap();
    let caps = glm
        .captures(payload)
        .ok_or_else(|| malformed(Some(EnvelopeFormat::ToolCall), "invalid GLM payload"))?;
    let key = caps.get(2).unwrap().as_str().trim();
    let raw_value = caps.get(3).unwrap().as_str().trim();
    let value = repair_json(raw_value)
        .unwrap_or_else(|_| Value::String(raw_value.trim_matches('"').to_string()));
    Ok(vec![ExtractedEnvelope {
        tool_name: caps.get(1).unwrap().as_str().trim().to_string(),
        arguments_raw: Value::Object([(key.to_string(), value)].into_iter().collect()).to_string(),
        visible_prefix: raw[..start].to_string(),
        visible_suffix: raw[end..].to_string(),
        format: EnvelopeFormat::ToolCall,
        repair_hints: Vec::new(),
    }])
}

fn extract_cdata(raw: &str) -> Result<Vec<ExtractedEnvelope>, EnvelopeError> {
    let regex = Regex::new(r#"(?is)<!\[CDATA\[<tool>\s*(.*?)\s*</tool>\]\]>"#).unwrap();
    let caps = regex
        .captures(raw)
        .ok_or_else(|| malformed(Some(EnvelopeFormat::Cdata), "invalid CDATA payload"))?;
    let full = caps.get(0).unwrap();
    let payload = caps.get(1).unwrap().as_str();
    let value = repair_json(payload)
        .map_err(|_| malformed(Some(EnvelopeFormat::Cdata), "invalid CDATA json"))?;
    values_from_payload(
        value,
        EnvelopeFormat::Cdata,
        raw[..full.start()].to_string(),
        raw[full.end()..].to_string(),
        Vec::new(),
    )
}

fn extract_raw_json(raw: &str) -> Result<Vec<ExtractedEnvelope>, EnvelopeError> {
    for (start, char) in raw.char_indices() {
        if char != '{' {
            continue;
        }
        if let Some(end) = matching_delimiter(raw, start, '{', '}') {
            let payload = &raw[start..=end];
            if !payload.contains("name") {
                continue;
            }
            let value = repair_json(payload)
                .map_err(|_| malformed(Some(EnvelopeFormat::RawJson), "invalid raw json"))?;
            return values_from_payload(
                value,
                EnvelopeFormat::RawJson,
                raw[..start].trim_end_matches([',', ' ']).to_string(),
                raw[end + 1..].trim_start_matches([',', ' ']).to_string(),
                Vec::new(),
            );
        }
    }
    Err(malformed(
        Some(EnvelopeFormat::RawJson),
        "raw name marker without object",
    ))
}

fn values_from_payload(
    value: Value,
    format: EnvelopeFormat,
    visible_prefix: String,
    visible_suffix: String,
    repair_hints: Vec<String>,
) -> Result<Vec<ExtractedEnvelope>, EnvelopeError> {
    match value {
        Value::Array(items) => {
            calls_from_values(items, format, visible_prefix, visible_suffix, repair_hints)
        }
        Value::Object(_) => calls_from_values(
            vec![value],
            format,
            visible_prefix,
            visible_suffix,
            repair_hints,
        ),
        _ => Err(malformed(Some(format), "payload is not object or array")),
    }
}

fn calls_from_values(
    values: Vec<Value>,
    format: EnvelopeFormat,
    visible_prefix: String,
    visible_suffix: String,
    repair_hints: Vec<String>,
) -> Result<Vec<ExtractedEnvelope>, EnvelopeError> {
    values
        .into_iter()
        .map(|value| {
            let object = value
                .as_object()
                .ok_or_else(|| malformed(Some(format.clone()), "tool call entry is not object"))?;
            let tool_name = object
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed(Some(format.clone()), "missing tool name"))?
                .to_string();
            let arguments = object
                .get("arguments")
                .or_else(|| object.get("parameters"))
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default()));
            Ok(ExtractedEnvelope {
                tool_name,
                arguments_raw: arguments.to_string(),
                visible_prefix: visible_prefix.clone(),
                visible_suffix: visible_suffix.clone(),
                format: format.clone(),
                repair_hints: repair_hints.clone(),
            })
        })
        .collect()
}

fn failure_envelope(
    tool_name: String,
    arguments_raw: String,
    start: usize,
    end: usize,
    raw: &str,
) -> ExtractedEnvelope {
    ExtractedEnvelope {
        tool_name,
        arguments_raw,
        visible_prefix: raw[..start].to_string(),
        visible_suffix: raw[end..].to_string(),
        format: EnvelopeFormat::Dsml,
        repair_hints: Vec::new(),
    }
}

fn dsml_parameter_value(raw: &str, is_json: bool) -> Value {
    if is_json {
        repair_json(raw).unwrap_or_else(|_| Value::String(unescape_json_string(raw)))
    } else {
        Value::String(unescape_json_string(raw))
    }
}

fn normalize_model_text(raw: &str) -> String {
    let folded = normalize_spaced_dsml(&raw.replace('｜', "|"));
    let mut lines = Vec::new();
    for line in folded.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            continue;
        }
        if trimmed.starts_with('<')
            || trimmed.starts_with("[TOOL_CALLS]")
            || trimmed.starts_with('{')
        {
            lines.push(trimmed.to_string());
        } else {
            lines.push(line.to_string());
        }
    }
    lines.join("\n")
}

fn matching_delimiter(text: &str, start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut delimiter = '"';
    let mut escape = false;
    for (idx, char) in text.char_indices().skip_while(|(idx, _)| *idx < start) {
        if in_string {
            if escape {
                escape = false;
            } else if char == '\\' {
                escape = true;
            } else if char == delimiter {
                in_string = false;
            }
            continue;
        }
        match char {
            '"' | '\'' => {
                in_string = true;
                delimiter = char;
            }
            c if c == open => depth += 1,
            c if c == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn collect_repairs(raw: &str, repairs: &mut Vec<String>) {
    if raw.contains('\'') {
        push_repair(repairs, "converted single quotes to double quotes");
    }
    if Regex::new(r#",\s*[\]}]"#).unwrap().is_match(raw) {
        push_repair(repairs, "removed trailing comma");
    }
}

fn push_repair(repairs: &mut Vec<String>, repair: &str) {
    if !repairs.iter().any(|item| item == repair) {
        repairs.push(repair.to_string());
    }
}

fn format_hint(format: EnvelopeFormat) -> Option<String> {
    match format {
        EnvelopeFormat::Dsml => Some("dsml".to_string()),
        _ => None,
    }
}

fn malformed(format: Option<EnvelopeFormat>, detail: &str) -> EnvelopeError {
    EnvelopeError {
        format,
        detail: detail.to_string(),
    }
}

fn unescape_json_string(value: &str) -> String {
    let mut result = String::new();
    let mut chars = value.chars();
    while let Some(char) = chars.next() {
        if char != '\\' {
            result.push(char);
            continue;
        }
        match chars.next() {
            Some('"') => result.push('"'),
            Some('\\') => result.push('\\'),
            Some('n') => result.push('\n'),
            Some('r') => result.push('\r'),
            Some('t') => result.push('\t'),
            Some('b') => result.push('\u{0008}'),
            Some('f') => result.push('\u{000C}'),
            Some(other) => result.push(other),
            None => result.push('\\'),
        }
    }
    result
}
