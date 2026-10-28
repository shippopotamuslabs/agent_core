//! Envelope detection, one parser per declared shape (ENC-300).
//!
//! Shapes: DSML, BracketArray (`[TOOL_CALLS]`), XmlToolCall (`<tool_call>`,
//! including the `<arg_key>`/`<arg_value>` payload sub-shape), CDATA,
//! CallBrace (`call: name { … }`), and raw JSON objects/arrays.

use serde::{Deserialize, Serialize};

/// Marker enum that lets the failure path tell callers which envelope shape
/// was being parsed when extraction failed (so the Kotlin facade can format
/// the right exception message). Shape-only names (ENC-300 D1): the former
/// provider-flavored names (`Mistral`, `ToolCall`, `LiteRtCall`) are gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvelopeFormat {
    Dsml,
    BracketArray,
    XmlToolCall,
    Cdata,
    CallBrace,
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
