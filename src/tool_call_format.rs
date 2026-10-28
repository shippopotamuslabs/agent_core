//! Host-declared tool-call format (ENC-300).
//!
//! The host declares one format per session in
//! [`crate::session::AgentConfig::tool_call_format`]; only the declared
//! parser ever runs. There is no cross-format sniffing: [`ToolCallFormat::Structured`]
//! (the default) mines no text at all, and each envelope shape parses only
//! its own markers. A leaf module by design: both [`crate::tool_parse`] and
//! [`crate::tool_stream_gate`] depend on this type, so it must not depend on
//! either of them.

use serde::{Deserialize, Serialize};

/// Wire-level tool-call shape the host declares for a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolCallFormat {
    /// Native structured calls only; body text is never mined or truncated.
    #[default]
    Structured,
    /// `<|DSML|tool_calls>` blocks.
    Dsml,
    /// `<![CDATA[<tool> … </tool>]]>` payloads.
    Cdata,
    /// `[TOOL_CALLS] […]` arrays.
    BracketArray,
    /// `<tool_call> … </tool_call>` envelopes, including the `<arg_key>` /
    /// `<arg_value>` payload sub-shape (parsed deterministically inside the
    /// declared-format parser, never sniffed).
    XmlToolCall,
    /// Bare `{ "name": …, … }` objects.
    RawJson,
    /// `call: name { … }` braces.
    CallBrace,
}

impl ToolCallFormat {
    /// True for [`ToolCallFormat::Structured`]: body text is never inspected.
    pub fn is_structured(self) -> bool {
        matches!(self, Self::Structured)
    }

    /// Failure-hint string naming this format for model-facing errors.
    /// [`ToolCallFormat::Structured`] carries no hint (it never parses).
    /// The `"dsml"` hint is kept byte-identical for back-compat.
    pub fn format_hint(self) -> Option<&'static str> {
        match self {
            Self::Structured => None,
            Self::Dsml => Some("dsml"),
            Self::BracketArray => Some("bracket-array"),
            Self::XmlToolCall => Some("xml-tool-call"),
            Self::Cdata => Some("cdata"),
            Self::RawJson => Some("raw-json"),
            Self::CallBrace => Some("call-brace"),
        }
    }
}
