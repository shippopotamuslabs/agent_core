//! Stream gate push/finish results (TOOL-PARSE-002).

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PushResult {
    pub emitted_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamGateFinish {
    pub trailing_flush: String,
    pub has_tool_marker: bool,
}
