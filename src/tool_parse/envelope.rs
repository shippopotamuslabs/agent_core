//! Envelope detection: DSML, GLM, CDATA, `<tool_call>`, Mistral `[TOOL_CALLS]`,
//! raw JSON object/array shapes.
//!
//! Red phase stub: all behavior is unimplemented.

use serde::{Deserialize, Serialize};

/// Marker enum that lets the failure path tell callers whether the malformed
/// envelope used DSML delimiters (so the Kotlin facade can format the right
/// exception message).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvelopeFormat {
    Dsml,
    Mistral,
    ToolCall,
    Cdata,
    LiteRtCall,
    RawJson,
}

/// A single envelope extracted from raw text.
#[derive(Debug, Clone)]
pub struct ExtractedEnvelope {
    pub tool_name: String,
    pub arguments_raw: String,
    pub visible_prefix: String,
    pub visible_suffix: String,
    pub format: EnvelopeFormat,
    pub repair_hints: Vec<String>,
}

/// Error variant for the envelope extraction phase.
#[derive(Debug, Clone)]
pub struct EnvelopeError {
    pub format: Option<EnvelopeFormat>,
    pub detail: String,
}
