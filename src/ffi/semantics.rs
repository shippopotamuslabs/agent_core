//! Typed tool-semantics records at the UniFFI boundary (ENC-297 D1–D3).
//!
//! [`FfiToolSemantics`] mirrors [`crate::tool_semantics::ToolSemanticsConfig`]
//! field-for-field EXCEPT the catalog: the catalog tools travel as a
//! sibling [`FfiToolSpec`] list on [`FfiAgentConfig`](super::types::FfiAgentConfig)
//! (D1), and conversion assigns them into `semantics.catalog.tools` before
//! the single fallible core constructor validates + builds the policy
//! catalog. `serde_json::Value` fields cannot cross UniFFI, so each one is a
//! JSON string parsed exactly once at conversion (D3); a malformed payload is
//! an [`AgentException`](super::types::AgentException) naming the field.

use std::collections::HashMap;

use crate::catalog::{FieldHint, ToolSchemaHint, ToolSpec};
use crate::tool_name::ToolName;
use crate::tool_semantics::{
    ArtifactSquashSemantics, ArtifactToolSemantics, ParameterAlias, SpecialValueRepair,
    ToolArgumentRepairRule, ToolCanonicalShape, ToolCompactionSemantics, ToolDiscoverySemantics,
    ToolGreedyExtensionSemantics, ToolResultPolicy, ToolSemanticsConfig,
};

use super::types::AgentException;
use crate::catalog::ToolCatalog;

/// Parse one JSON payload at the conversion edge (ENC-297 D3). Malformed
/// input is a typed error naming the field, never a silent default.
fn parse_json_value(field: &str, json: &str) -> Result<serde_json::Value, AgentException> {
    serde_json::from_str(json).map_err(|error| AgentException::Internal {
        detail: format!("{field} is not valid JSON: {error}"),
    })
}

/// Build a core [`ToolName`] at the typed FFI edge: blank input is a typed
/// `AgentException` naming the field, never a silent empty identity
/// (ENC-299: core `ToolName` construction is fallible, and FFI records keep
/// `String` fields so the failure surfaces here).
fn tool_name_from_ffi(field: &str, raw: &str) -> Result<ToolName, AgentException> {
    ToolName::new(raw).map_err(|_| AgentException::Internal {
        detail: format!("{field} must not be blank"),
    })
}

/// Build core [`ToolName`]s for a `Vec<String>` FFI field.
fn tool_names_from_ffi(field: &str, raw: Vec<String>) -> Result<Vec<ToolName>, AgentException> {
    raw.into_iter()
        .map(|name| tool_name_from_ffi(field, &name))
        .collect()
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiFieldHint {
    pub name: String,
    pub ty: String,
    pub description: String,
}

impl From<FieldHint> for FfiFieldHint {
    fn from(hint: FieldHint) -> Self {
        Self {
            name: hint.name,
            ty: hint.ty,
            description: hint.description,
        }
    }
}

impl From<FfiFieldHint> for FieldHint {
    fn from(hint: FfiFieldHint) -> Self {
        Self {
            name: hint.name,
            ty: hint.ty,
            description: hint.description,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDefaultValue {
    pub key: String,
    pub value_json: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolSchemaHint {
    pub required_fields: Vec<FfiFieldHint>,
    pub defaults: Vec<FfiDefaultValue>,
}

impl From<&ToolSchemaHint> for FfiToolSchemaHint {
    fn from(hint: &ToolSchemaHint) -> Self {
        let mut defaults: Vec<FfiDefaultValue> = hint
            .defaults
            .iter()
            .map(|(key, value)| FfiDefaultValue {
                key: key.clone(),
                // Value serializes infallibly (string-keyed map by construction).
                value_json: serde_json::to_string(value).expect("json value serializes"),
            })
            .collect();
        defaults.sort_by(|a, b| a.key.cmp(&b.key));
        Self {
            required_fields: hint
                .required_fields
                .iter()
                .cloned()
                .map(FfiFieldHint::from)
                .collect(),
            defaults,
        }
    }
}

impl FfiToolSchemaHint {
    fn into_core(self, tool_name: &str) -> Result<ToolSchemaHint, AgentException> {
        let mut defaults = HashMap::with_capacity(self.defaults.len());
        for entry in self.defaults {
            let value = parse_json_value(
                &format!(
                    "tool_specs[{tool_name}].schema_hint.defaults[{entry_key}]",
                    entry_key = entry.key
                ),
                &entry.value_json,
            )?;
            defaults.insert(entry.key, value);
        }
        Ok(ToolSchemaHint {
            required_fields: entry_required_fields(self.required_fields),
            defaults,
        })
    }
}

fn entry_required_fields(fields: Vec<FfiFieldHint>) -> Vec<FieldHint> {
    fields.into_iter().map(FieldHint::from).collect()
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolSpec {
    pub name: String,
    pub aliases: Vec<String>,
    pub reproducible: bool,
    pub schema_hint: Option<FfiToolSchemaHint>,
}

impl From<&ToolSpec> for FfiToolSpec {
    fn from(spec: &ToolSpec) -> Self {
        Self {
            name: spec.name.as_str().to_string(),
            aliases: spec
                .aliases
                .iter()
                .map(|alias| alias.as_str().to_string())
                .collect(),
            reproducible: spec.reproducible,
            schema_hint: spec.schema_hint.as_ref().map(FfiToolSchemaHint::from),
        }
    }
}

impl FfiToolSpec {
    pub(crate) fn into_core(self) -> Result<ToolSpec, AgentException> {
        let schema_hint = self
            .schema_hint
            .map(|hint| hint.into_core(&self.name))
            .transpose()?;
        Ok(ToolSpec {
            name: tool_name_from_ffi("tool_specs[].name", &self.name)?,
            aliases: tool_names_from_ffi("tool_specs[].aliases[]", self.aliases)?,
            reproducible: self.reproducible,
            schema_hint,
        })
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiSpecialValueRepair {
    ScalarToArray { source: String, target: String },
    StripLeadingBracketFromArrayStrings { field: String },
    ValidateStringifiedJsonArray { field: String },
}

impl From<SpecialValueRepair> for FfiSpecialValueRepair {
    fn from(repair: SpecialValueRepair) -> Self {
        match repair {
            SpecialValueRepair::ScalarToArray { source, target } => {
                Self::ScalarToArray { source, target }
            }
            SpecialValueRepair::StripLeadingBracketFromArrayStrings { field } => {
                Self::StripLeadingBracketFromArrayStrings { field }
            }
            SpecialValueRepair::ValidateStringifiedJsonArray { field } => {
                Self::ValidateStringifiedJsonArray { field }
            }
        }
    }
}

impl From<FfiSpecialValueRepair> for SpecialValueRepair {
    fn from(repair: FfiSpecialValueRepair) -> Self {
        match repair {
            FfiSpecialValueRepair::ScalarToArray { source, target } => {
                Self::ScalarToArray { source, target }
            }
            FfiSpecialValueRepair::StripLeadingBracketFromArrayStrings { field } => {
                Self::StripLeadingBracketFromArrayStrings { field }
            }
            FfiSpecialValueRepair::ValidateStringifiedJsonArray { field } => {
                Self::ValidateStringifiedJsonArray { field }
            }
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiParameterAlias {
    pub alias: String,
    pub canonical: String,
}

impl From<ParameterAlias> for FfiParameterAlias {
    fn from(alias: ParameterAlias) -> Self {
        Self {
            alias: alias.alias,
            canonical: alias.canonical,
        }
    }
}

impl From<FfiParameterAlias> for ParameterAlias {
    fn from(alias: FfiParameterAlias) -> Self {
        Self {
            alias: alias.alias,
            canonical: alias.canonical,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolArgumentRepairRule {
    pub tool_name: String,
    pub aliases: Vec<FfiParameterAlias>,
    pub special_values: Vec<FfiSpecialValueRepair>,
}

impl From<ToolArgumentRepairRule> for FfiToolArgumentRepairRule {
    fn from(rule: ToolArgumentRepairRule) -> Self {
        Self {
            tool_name: rule.tool_name.as_str().to_string(),
            aliases: rule.aliases.into_iter().map(Into::into).collect(),
            special_values: rule.special_values.into_iter().map(Into::into).collect(),
        }
    }
}

impl FfiToolArgumentRepairRule {
    fn into_core(self) -> Result<ToolArgumentRepairRule, AgentException> {
        Ok(ToolArgumentRepairRule {
            tool_name: tool_name_from_ffi("argument_repairs[].tool_name", &self.tool_name)?,
            aliases: self.aliases.into_iter().map(Into::into).collect(),
            special_values: self.special_values.into_iter().map(Into::into).collect(),
        })
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolCanonicalShape {
    pub tool_name: String,
    pub ordered_fields: Vec<String>,
}

impl From<ToolCanonicalShape> for FfiToolCanonicalShape {
    fn from(shape: ToolCanonicalShape) -> Self {
        Self {
            tool_name: shape.tool_name.as_str().to_string(),
            ordered_fields: shape.ordered_fields,
        }
    }
}

impl FfiToolCanonicalShape {
    fn into_core(self) -> Result<ToolCanonicalShape, AgentException> {
        Ok(ToolCanonicalShape {
            tool_name: tool_name_from_ffi("canonical_shapes[].tool_name", &self.tool_name)?,
            ordered_fields: self.ordered_fields,
        })
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolResultPolicy {
    pub tool_name: String,
    pub spill_stored_copy_only: bool,
    pub skip_result_truncation: bool,
    pub preserve_in_micro_compaction: bool,
}

impl From<ToolResultPolicy> for FfiToolResultPolicy {
    fn from(policy: ToolResultPolicy) -> Self {
        Self {
            tool_name: policy.tool_name.as_str().to_string(),
            spill_stored_copy_only: policy.spill_stored_copy_only,
            skip_result_truncation: policy.skip_result_truncation,
            preserve_in_micro_compaction: policy.preserve_in_micro_compaction,
        }
    }
}

impl FfiToolResultPolicy {
    fn into_core(self) -> Result<ToolResultPolicy, AgentException> {
        Ok(ToolResultPolicy {
            tool_name: tool_name_from_ffi("result_policies[].tool_name", &self.tool_name)?,
            spill_stored_copy_only: self.spill_stored_copy_only,
            skip_result_truncation: self.skip_result_truncation,
            preserve_in_micro_compaction: self.preserve_in_micro_compaction,
        })
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolDiscoverySemantics {
    pub tool_name: String,
    pub request_field_name: String,
    pub always_callable: bool,
    pub companion_callable_tools: Vec<String>,
}

impl From<ToolDiscoverySemantics> for FfiToolDiscoverySemantics {
    fn from(semantics: ToolDiscoverySemantics) -> Self {
        Self {
            tool_name: semantics.tool_name.as_str().to_string(),
            request_field_name: semantics.request_field_name,
            always_callable: semantics.always_callable,
            companion_callable_tools: semantics
                .companion_callable_tools
                .iter()
                .map(|tool| tool.as_str().to_string())
                .collect(),
        }
    }
}

impl FfiToolDiscoverySemantics {
    fn into_core(self) -> Result<ToolDiscoverySemantics, AgentException> {
        Ok(ToolDiscoverySemantics {
            tool_name: tool_name_from_ffi("discovery.tool_name", &self.tool_name)?,
            request_field_name: self.request_field_name,
            always_callable: self.always_callable,
            companion_callable_tools: tool_names_from_ffi(
                "discovery.companion_callable_tools[]",
                self.companion_callable_tools,
            )?,
        })
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiArtifactToolSemantics {
    pub start_tool_name: String,
    pub append_tool_name: String,
    pub id_field: String,
    pub append_blocks_field: String,
    pub completion_field: String,
    pub status_field: String,
    pub success_status_value: String,
    pub synthetic_call_id_prefix: String,
    /// JSON payload for `empty_append_payload_value` (ENC-297 D3): parsed
    /// once at conversion. `None` maps to JSON null, which is also the core
    /// serde default for the field, so `None` round-trips symmetrically with
    /// a JSON-null payload. Malformed input is an `AgentException` naming
    /// this field.
    pub empty_append_payload_value_json: Option<String>,
}

impl From<&ArtifactToolSemantics> for FfiArtifactToolSemantics {
    fn from(semantics: &ArtifactToolSemantics) -> Self {
        Self {
            start_tool_name: semantics.start_tool_name.as_str().to_string(),
            append_tool_name: semantics.append_tool_name.as_str().to_string(),
            id_field: semantics.id_field.clone(),
            append_blocks_field: semantics.append_blocks_field.clone(),
            completion_field: semantics.completion_field.clone(),
            status_field: semantics.status_field.clone(),
            success_status_value: semantics.success_status_value.clone(),
            synthetic_call_id_prefix: semantics.synthetic_call_id_prefix.clone(),
            // ENC-297 M3: JSON null emits None so the FFI tree round-trips
            // symmetrically (None converts back to Null). Non-null payloads
            // serialize infallibly (string-keyed map by construction).
            empty_append_payload_value_json: if semantics.empty_append_payload_value.is_null() {
                None
            } else {
                Some(
                    serde_json::to_string(&semantics.empty_append_payload_value)
                        .expect("json value serializes"),
                )
            },
        }
    }
}

impl FfiArtifactToolSemantics {
    fn into_core(self) -> Result<ArtifactToolSemantics, AgentException> {
        let empty_append_payload_value = match self.empty_append_payload_value_json {
            Some(json) => parse_json_value("empty_append_payload_value", &json)?,
            None => serde_json::Value::Null,
        };
        Ok(ArtifactToolSemantics {
            start_tool_name: tool_name_from_ffi("artifact.start_tool_name", &self.start_tool_name)?,
            append_tool_name: tool_name_from_ffi(
                "artifact.append_tool_name",
                &self.append_tool_name,
            )?,
            id_field: self.id_field,
            append_blocks_field: self.append_blocks_field,
            completion_field: self.completion_field,
            status_field: self.status_field,
            success_status_value: self.success_status_value,
            synthetic_call_id_prefix: self.synthetic_call_id_prefix,
            empty_append_payload_value,
        })
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiArtifactSquashSemantics {
    pub append_tool_name: String,
    pub id_field: String,
    pub blocks_field: String,
    pub total_blocks_result_field: String,
    pub status_field: String,
    pub success_status_value: String,
    pub summary_template: String,
    pub continuation_instruction: String,
    pub snippet_max_chars: u64,
}

impl From<ArtifactSquashSemantics> for FfiArtifactSquashSemantics {
    fn from(semantics: ArtifactSquashSemantics) -> Self {
        Self {
            append_tool_name: semantics.append_tool_name.as_str().to_string(),
            id_field: semantics.id_field,
            blocks_field: semantics.blocks_field,
            total_blocks_result_field: semantics.total_blocks_result_field,
            status_field: semantics.status_field,
            success_status_value: semantics.success_status_value,
            summary_template: semantics.summary_template,
            continuation_instruction: semantics.continuation_instruction,
            // ENC-297 M4: saturating conversion, never a truncating `as`.
            snippet_max_chars: u64::try_from(semantics.snippet_max_chars).unwrap_or(u64::MAX),
        }
    }
}

impl FfiArtifactSquashSemantics {
    fn into_core(self) -> Result<ArtifactSquashSemantics, AgentException> {
        Ok(ArtifactSquashSemantics {
            append_tool_name: tool_name_from_ffi(
                "compaction.artifact_squash.append_tool_name",
                &self.append_tool_name,
            )?,
            id_field: self.id_field,
            blocks_field: self.blocks_field,
            total_blocks_result_field: self.total_blocks_result_field,
            status_field: self.status_field,
            success_status_value: self.success_status_value,
            summary_template: self.summary_template,
            continuation_instruction: self.continuation_instruction,
            // ENC-297 M4: saturating conversion, never a truncating `as`.
            snippet_max_chars: usize::try_from(self.snippet_max_chars).unwrap_or(usize::MAX),
        })
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolCompactionSemantics {
    pub artifact_squash: Option<FfiArtifactSquashSemantics>,
}

impl From<ToolCompactionSemantics> for FfiToolCompactionSemantics {
    fn from(semantics: ToolCompactionSemantics) -> Self {
        Self {
            artifact_squash: semantics.artifact_squash.map(Into::into),
        }
    }
}

impl FfiToolCompactionSemantics {
    fn into_core(self) -> Result<ToolCompactionSemantics, AgentException> {
        Ok(ToolCompactionSemantics {
            artifact_squash: self
                .artifact_squash
                .map(FfiArtifactSquashSemantics::into_core)
                .transpose()?,
        })
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolGreedyExtensionSemantics {
    pub tool_name: String,
    pub extra_turns: u32,
}

impl From<ToolGreedyExtensionSemantics> for FfiToolGreedyExtensionSemantics {
    fn from(semantics: ToolGreedyExtensionSemantics) -> Self {
        Self {
            tool_name: semantics.tool_name.as_str().to_string(),
            extra_turns: semantics.extra_turns,
        }
    }
}

impl FfiToolGreedyExtensionSemantics {
    fn into_core(self) -> Result<ToolGreedyExtensionSemantics, AgentException> {
        Ok(ToolGreedyExtensionSemantics {
            tool_name: tool_name_from_ffi("greedy_extension_tools[].tool_name", &self.tool_name)?,
            extra_turns: self.extra_turns,
        })
    }
}

/// Typed mirror of [`ToolSemanticsConfig`] minus the catalog (ENC-297 D1):
/// the catalog tools travel as a sibling `Vec<FfiToolSpec>` on
/// `FfiAgentConfig`, so conversion leaves `catalog` empty and the agent
/// config constructor assigns the sibling specs before validating.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolSemantics {
    pub discovery: Option<FfiToolDiscoverySemantics>,
    pub artifact: Option<FfiArtifactToolSemantics>,
    pub argument_repairs: Vec<FfiToolArgumentRepairRule>,
    pub canonical_shapes: Vec<FfiToolCanonicalShape>,
    pub result_policies: Vec<FfiToolResultPolicy>,
    pub compaction: FfiToolCompactionSemantics,
    pub greedy_extension_tools: Vec<FfiToolGreedyExtensionSemantics>,
}

impl FfiToolSemantics {
    /// Named core-to-FFI conversion (ENC-297 M2): mirrors every
    /// [`ToolSemanticsConfig`] field EXCEPT the policy catalog, which
    /// travels as sibling [`FfiToolSpec`]s, never inside this tree. A named
    /// constructor (not `From`) so the lossy catalog drop is explicit at
    /// every call site. Test-only: the live boundary converts host→core;
    /// core→FFI exists to support round-trip fixture tests.
    #[cfg(test)]
    pub(crate) fn without_catalog(config: &ToolSemanticsConfig) -> Self {
        Self {
            discovery: config.discovery.clone().map(Into::into),
            artifact: config.artifact.as_ref().map(Into::into),
            argument_repairs: config
                .argument_repairs
                .clone()
                .into_iter()
                .map(Into::into)
                .collect(),
            canonical_shapes: config
                .canonical_shapes
                .clone()
                .into_iter()
                .map(Into::into)
                .collect(),
            result_policies: config
                .result_policies
                .clone()
                .into_iter()
                .map(Into::into)
                .collect(),
            compaction: config.compaction.clone().into(),
            greedy_extension_tools: config
                .greedy_extension_tools
                .clone()
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

/// Shared typed conversion (ENC-297 M1): the single place that assigns the
/// sibling [`FfiToolSpec`]s into `semantics.catalog.tools` and runs the
/// fallible core validation + policy-catalog build. Both the fallible
/// [`AgentConfig`](crate::session::AgentConfig) edge and the infallible
/// policy helpers convert here, so the error mapping lives in exactly one
/// spot: invalid input is an [`AgentException`] naming the failure.
/// `None` semantics is host-declared agnostic (not a parse fallback).
pub(crate) fn typed_to_core(
    semantics: Option<FfiToolSemantics>,
    specs: Vec<FfiToolSpec>,
) -> Result<(ToolSemanticsConfig, ToolCatalog), AgentException> {
    let mut semantics = match semantics {
        Some(tree) => tree.into_core()?,
        None => ToolSemanticsConfig::agnostic(),
    };
    semantics.catalog.tools = specs
        .into_iter()
        .map(|spec| spec.into_core())
        .collect::<Result<Vec<_>, _>>()?;
    let catalog =
        ToolCatalog::from_semantics(&semantics).map_err(|error| AgentException::Internal {
            detail: format!("invalid tool semantics: {error:?}"),
        })?;
    Ok((semantics, catalog))
}

impl FfiToolSemantics {
    pub(crate) fn into_core(self) -> Result<ToolSemanticsConfig, AgentException> {
        Ok(ToolSemanticsConfig {
            catalog: crate::tool_semantics::ToolCatalogConfig { tools: Vec::new() },
            discovery: self
                .discovery
                .map(FfiToolDiscoverySemantics::into_core)
                .transpose()?,
            artifact: self
                .artifact
                .map(|artifact| artifact.into_core())
                .transpose()?,
            argument_repairs: self
                .argument_repairs
                .into_iter()
                .map(FfiToolArgumentRepairRule::into_core)
                .collect::<Result<Vec<_>, _>>()?,
            canonical_shapes: self
                .canonical_shapes
                .into_iter()
                .map(FfiToolCanonicalShape::into_core)
                .collect::<Result<Vec<_>, _>>()?,
            result_policies: self
                .result_policies
                .into_iter()
                .map(FfiToolResultPolicy::into_core)
                .collect::<Result<Vec<_>, _>>()?,
            compaction: self.compaction.into_core()?,
            greedy_extension_tools: self
                .greedy_extension_tools
                .into_iter()
                .map(FfiToolGreedyExtensionSemantics::into_core)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::types::{agent_config, FfiAgentConfig};

    fn legacy_semantics() -> ToolSemanticsConfig {
        serde_json::from_str(include_str!(
            "../../tests/support/enclave_tool_semantics.json"
        ))
        .expect("enclave_tool_semantics.json")
    }

    fn legacy_specs() -> Vec<ToolSpec> {
        #[derive(serde::Deserialize)]
        struct CatalogFile {
            tools: Vec<ToolSpec>,
        }
        let raw = include_str!("../../resources/tool_catalog.json");
        serde_json::from_str::<CatalogFile>(raw)
            .expect("tool_catalog.json")
            .tools
    }

    fn core_json(config: &ToolSemanticsConfig) -> serde_json::Value {
        serde_json::to_value(config).expect("semantics serializes")
    }

    #[test]
    fn ffi_tree_round_trips_legacy_json_fixture() {
        // The real semantics fixture crosses the boundary as typed records
        // and converts back to core values equal to the legacy JSON parse —
        // every type/field must survive, including Value payloads.
        let semantics = legacy_semantics();
        let specs = legacy_specs();
        assert!(!specs.is_empty());

        let ffi_tree = FfiToolSemantics::without_catalog(&semantics);
        let ffi_specs: Vec<FfiToolSpec> = specs.iter().map(FfiToolSpec::from).collect();

        let mut round_tripped = ffi_tree.into_core().expect("typed tree converts");
        round_tripped.catalog.tools = ffi_specs
            .into_iter()
            .map(|spec| spec.into_core())
            .collect::<Result<Vec<_>, _>>()
            .expect("typed specs convert");
        let mut expected = semantics.clone();
        expected.catalog.tools = specs.clone();
        assert_eq!(core_json(&round_tripped), core_json(&expected));

        // Full config conversion: catalog builds from the sibling specs.
        let ffi_tree = FfiToolSemantics::without_catalog(&semantics);
        let ffi_specs: Vec<FfiToolSpec> = specs.iter().map(FfiToolSpec::from).collect();
        let config = agent_config(FfiAgentConfig {
            tool_semantics: Some(ffi_tree),
            tool_specs: ffi_specs,
            ..Default::default()
        })
        .expect("valid typed config converts");
        assert_eq!(config.tool_catalog.len(), specs.len());
        for spec in &specs {
            assert_eq!(
                config.tool_catalog.resolve(&spec.name).unwrap().name,
                spec.name
            );
        }
        assert_eq!(core_json(&config.tool_semantics), core_json(&expected));
    }

    #[test]
    fn malformed_value_payload_is_agent_exception() {
        // D3: malformed empty_append_payload_value JSON is a typed error
        // naming the field.
        let mut tree = FfiToolSemantics::without_catalog(&legacy_semantics());
        let mut artifact = tree.artifact.clone().expect("fixture has artifact");
        artifact.empty_append_payload_value_json = Some("{not json".into());
        tree.artifact = Some(artifact);
        let error = agent_config(FfiAgentConfig {
            tool_semantics: Some(tree),
            tool_specs: Vec::new(),
            ..Default::default()
        })
        .expect_err("malformed payload must fail");
        match error {
            AgentException::Internal { detail } => {
                assert!(detail.contains("empty_append_payload_value"), "{detail}");
            }
            other => panic!("expected Internal, got {other:?}"),
        }

        // Malformed schema-hint default payload names its field too.
        let tree = FfiToolSemantics::without_catalog(&legacy_semantics());
        let hinted = FfiToolSpec {
            name: "web_search".into(),
            aliases: Vec::new(),
            reproducible: true,
            schema_hint: Some(FfiToolSchemaHint {
                required_fields: Vec::new(),
                defaults: vec![FfiDefaultValue {
                    key: "done".into(),
                    value_json: "{bad".into(),
                }],
            }),
        };
        let error = agent_config(FfiAgentConfig {
            tool_semantics: Some(tree),
            tool_specs: vec![hinted],
            ..Default::default()
        })
        .expect_err("malformed default must fail");
        match error {
            AgentException::Internal { detail } => {
                assert!(detail.contains("schema_hint.defaults"), "{detail}");
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    #[test]
    fn invalid_spec_is_agent_exception() {
        // Normalized-duplicate tool names fail core validation at conversion.
        let specs = vec![
            FfiToolSpec {
                name: "web_search".into(),
                aliases: Vec::new(),
                reproducible: true,
                schema_hint: None,
            },
            FfiToolSpec {
                name: "web-search".into(),
                aliases: Vec::new(),
                reproducible: true,
                schema_hint: None,
            },
        ];
        let error = agent_config(FfiAgentConfig {
            tool_semantics: None,
            tool_specs: specs,
            ..Default::default()
        })
        .expect_err("duplicate specs must fail");
        match error {
            AgentException::Internal { detail } => {
                assert!(detail.contains("invalid tool semantics"), "{detail}");
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    #[test]
    fn typed_fixture_config_creates_working_session() {
        // End-to-end through the typed path: the real fixture crosses as
        // records + sibling specs, converts, and drives a live session.
        use crate::action::AgentAction;
        use crate::engine::{AgentEngine, SessionIoParams};
        use crate::message::{AgentMessage, AgentRole, MessageMeta};

        let semantics = legacy_semantics();
        let specs = legacy_specs();
        let config = agent_config(FfiAgentConfig {
            tool_semantics: Some(FfiToolSemantics::without_catalog(&semantics)),
            tool_specs: specs.iter().map(FfiToolSpec::from).collect(),
            ..Default::default()
        })
        .expect("valid typed config converts");
        let engine = AgentEngine::new();
        let handle = engine
            .create_session(
                vec![AgentMessage {
                    id: "u1".into(),
                    role: AgentRole::User,
                    content: "hi".into(),
                    metadata: MessageMeta::new(),
                }],
                config,
                Default::default(),
                serde_json::Value::Null,
                SessionIoParams::new("anthropic".into(), 200_000, "[]".into(), "sys".into())
                    .expect("valid io params"),
            )
            .expect("valid config creates session");
        assert!(matches!(
            engine.next_action(&handle),
            AgentAction::CallModel(_)
        ));
    }

    #[test]
    fn null_payload_emits_none_and_round_trips() {
        // ENC-297 M3: a JSON-null payload emits None (not Some("null")),
        // and None converts back to Null — symmetric with the serde default.
        let mut semantics = legacy_semantics();
        semantics
            .artifact
            .as_mut()
            .expect("fixture has artifact")
            .empty_append_payload_value = serde_json::Value::Null;
        let tree = FfiToolSemantics::without_catalog(&semantics);
        assert!(
            tree.artifact
                .as_ref()
                .expect("artifact mirrors")
                .empty_append_payload_value_json
                .is_none(),
            "null payload must emit None"
        );
        let round_tripped = tree.into_core().expect("typed tree converts");
        assert!(round_tripped
            .artifact
            .expect("artifact converts")
            .empty_append_payload_value
            .is_null());
    }

    #[test]
    fn none_semantics_is_host_declared_agnostic() {
        // D2: None is the explicit host-declared default, not a fallback —
        // it converts to the agnostic core config with an empty catalog.
        let config = agent_config(FfiAgentConfig {
            tool_semantics: None,
            tool_specs: Vec::new(),
            ..Default::default()
        })
        .expect("None semantics converts");
        assert!(config.tool_catalog.is_empty());
        assert_eq!(
            core_json(&config.tool_semantics),
            core_json(&ToolSemanticsConfig::agnostic())
        );

        // Specs without a semantics tree attach to the agnostic base.
        let config = agent_config(FfiAgentConfig {
            tool_semantics: None,
            tool_specs: vec![FfiToolSpec {
                name: "web_search".into(),
                aliases: vec!["websearch".into()],
                reproducible: true,
                schema_hint: None,
            }],
            ..Default::default()
        })
        .expect("specs attach to agnostic base");
        assert_eq!(config.tool_catalog.len(), 1);
        assert_eq!(
            config
                .tool_catalog
                .resolve(&ToolName::new("websearch").expect("valid name"))
                .unwrap()
                .name
                .as_str(),
            "web_search"
        );
    }
}
