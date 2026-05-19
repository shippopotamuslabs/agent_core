//! Result types for `tool_parse` pipeline.
//!
//! These types are the public output of [`crate::tool_parse::parse_tool_calls`]
//! and friends. They live in their own module so the FFI layer can derive
//! conversions from them without depending on the rest of the pipeline.

use serde::{Deserialize, Serialize};

/// A single successfully parsed and coerced tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolParseField {
    pub tool_name: String,
    pub arguments_json: String,
    pub repaired: bool,
    pub repairs: Vec<String>,
}

/// A single failed envelope.
///
/// `format_hint` carries the discriminator that identifies which envelope
/// dialect was being parsed when the failure occurred (e.g. `"dsml"` for
/// DSML envelopes). Required by `test_spec.md` so callers / observers can
/// distinguish DSML failures from Mistral / `<tool_call>` failures.
///
/// **TDD Red:** the field exists but the pipeline does not yet populate it;
/// it always defaults to `None` until the implementation phase wires it up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolParseFailure {
    pub reason: ToolParseFailureReason,
    pub original: String,
    pub format_hint: Option<String>,
}

/// The complete batch returned by [`crate::tool_parse::parse_tool_calls`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolParseBatch {
    pub successes: Vec<ToolParseField>,
    pub failures: Vec<ToolParseFailure>,
    pub visible_prefix: String,
    pub visible_suffix: String,
}

impl ToolParseBatch {
    /// Construct an empty batch where the entire input is visible prose.
    pub fn empty(raw: &str) -> Self {
        ToolParseBatch {
            successes: Vec::new(),
            failures: Vec::new(),
            visible_prefix: raw.to_string(),
            visible_suffix: String::new(),
        }
    }
}

/// Reason a single envelope failed to parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ToolParseFailureReason {
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
