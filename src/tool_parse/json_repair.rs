//! Forge-style JSON repair.
//!
//! This module intentionally implements a small tolerant parser instead of a
//! regex-only fixer. Model output often combines several JSON mistakes in the
//! same payload, so parsing the structure directly is less brittle than
//! repeatedly transforming strings and hoping strict JSON accepts the result.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Structured error returned by [`repair_json`] when the input cannot be
/// salvaged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum JsonRepairError {
    UnexpectedEnd { position: usize },
    UnexpectedCharacter { character: String, position: usize },
    ObjectKeyExpected { position: usize },
    ColonExpected { position: usize },
    InvalidUnicodeCharacter { chars: String, position: usize },
    InvalidCharacter { character: String, position: usize },
}

impl std::fmt::Display for JsonRepairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for JsonRepairError {}

/// Repair a JSON-like input into a `serde_json::Value`.
pub fn repair_json(input: &str) -> Result<Value, JsonRepairError> {
    let stripped = strip_markdown_fences(input.trim());
    if stripped.is_empty() {
        return Err(JsonRepairError::UnexpectedEnd { position: 0 });
    }

    if let Ok(mut value) = serde_json::from_str::<Value>(&stripped) {
        normalize_value_strings(&mut value);
        return Ok(value);
    }

    if let Some(array) = parse_newline_delimited_json(&stripped) {
        return Ok(array);
    }

    let mut parser = Parser::new(&stripped);
    parser.parse_value()
}

/// In-place repair of string values that themselves contain JSON-like data.
pub fn repair_json_value_inplace(value: &mut Value) {
    match value {
        Value::String(s) => {
            if let Ok(repaired) = repair_json(s) {
                *value = repaired;
            }
        }
        Value::Array(items) => {
            for item in items {
                repair_json_value_inplace(item);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                repair_json_value_inplace(item);
            }
        }
        _ => {}
    }
}

fn strip_markdown_fences(input: &str) -> String {
    let trimmed = input.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    let mut lines = trimmed.lines();
    let first = lines.next().unwrap_or_default();
    if !first.starts_with("```") {
        return trimmed.to_string();
    }
    let mut body: Vec<&str> = lines.collect();
    if body.last().is_some_and(|line| line.trim() == "```") {
        body.pop();
    }
    body.join("\n")
}

fn parse_newline_delimited_json(input: &str) -> Option<Value> {
    let lines: Vec<&str> = input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() < 2
        || !lines
            .iter()
            .all(|line| line.starts_with('{') || line.starts_with('['))
    {
        return None;
    }

    let values: Option<Vec<Value>> = lines
        .into_iter()
        .map(|line| repair_json(line).ok())
        .collect();
    values.map(Value::Array)
}

struct Parser<'a> {
    chars: Vec<char>,
    index: usize,
    source: &'a str,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            chars: source.chars().collect(),
            index: 0,
            source,
        }
    }

    fn parse_value(&mut self) -> Result<Value, JsonRepairError> {
        self.skip_ignorable();
        let Some(char) = self.peek() else {
            return Err(JsonRepairError::UnexpectedEnd {
                position: self.index,
            });
        };

        match char {
            '{' => self.parse_object(),
            '[' => self.parse_array(),
            '"' | '\'' | '`' | '\u{2018}' | '\u{2019}' | '\u{201C}' | '\u{201D}' | '\u{00B4}' => {
                self.parse_string_with_concat().map(Value::String)
            }
            '/' if self.peek_next() != Some('/') && self.peek_next() != Some('*') => {
                Ok(Value::String(self.parse_bare_token()))
            }
            _ => {
                let token = self.parse_bare_token();
                Ok(self.value_from_token(token))
            }
        }
    }

    fn parse_object(&mut self) -> Result<Value, JsonRepairError> {
        self.consume();
        let mut map = serde_json::Map::new();
        loop {
            self.skip_ignorable();
            if self.peek() == Some('}') {
                self.consume();
                break;
            }
            if self.is_end() {
                break;
            }

            let key = match self.peek() {
                Some('"') | Some('\'') | Some('`') | Some('\u{2018}') | Some('\u{2019}')
                | Some('\u{201C}') | Some('\u{201D}') | Some('\u{00B4}') => self.parse_string()?,
                Some(_) => self.parse_key_token(),
                None => break,
            };

            self.skip_ignorable();
            if self.peek() == Some(':') {
                self.consume();
            }
            self.skip_ignorable();

            let value = if matches!(self.peek(), Some('}') | Some(',')) || self.peek().is_none() {
                Value::Null
            } else {
                self.parse_value()?
            };
            map.insert(key, value);

            self.skip_ignorable();
            if self.peek() == Some(',') {
                self.consume();
            }
        }
        Ok(Value::Object(map))
    }

    fn parse_array(&mut self) -> Result<Value, JsonRepairError> {
        self.consume();
        let mut items = Vec::new();
        loop {
            self.skip_ignorable();
            if self.peek() == Some(']') {
                self.consume();
                break;
            }
            if self.is_end() {
                break;
            }
            if self.starts_with("...") || self.peek() == Some('\u{2026}') {
                self.skip_ellipsis();
                continue;
            }
            items.push(self.parse_value()?);
            self.skip_ignorable();
            if self.peek() == Some(',') {
                self.consume();
            }
        }
        Ok(Value::Array(items))
    }

    fn parse_string(&mut self) -> Result<String, JsonRepairError> {
        let delimiter = self.consume().ok_or(JsonRepairError::UnexpectedEnd {
            position: self.index,
        })?;
        let normalized_delimiter = normalize_quote(delimiter).unwrap_or(delimiter);
        let mut value = String::new();
        let mut escape = false;

        while let Some(char) = self.consume() {
            if escape {
                match char {
                    '"' => value.push('"'),
                    '\'' => value.push('\''),
                    '\\' => value.push('\\'),
                    '/' => value.push('/'),
                    'b' => value.push('\u{0008}'),
                    'f' => value.push('\u{000C}'),
                    'n' => value.push('\n'),
                    'r' => value.push('\r'),
                    't' => value.push('\t'),
                    'u' => value.push(self.parse_unicode_escape()?),
                    other => value.push(other),
                }
                escape = false;
                continue;
            }

            if char == '\\' {
                escape = true;
                continue;
            }

            if normalize_quote(char) == Some(normalized_delimiter) {
                if normalized_delimiter == '"' && self.is_inner_double_quote() {
                    value.push('"');
                    continue;
                }
                break;
            }

            match char {
                '\u{201C}' | '\u{201D}' => value.push('"'),
                '\u{2018}' | '\u{2019}' | '\u{00B4}' => value.push('\''),
                '\n' => value.push('\n'),
                '\r' => value.push('\r'),
                '\t' => value.push('\t'),
                other => value.push(other),
            }
        }

        Ok(value)
    }

    fn parse_string_with_concat(&mut self) -> Result<String, JsonRepairError> {
        let mut value = self.parse_string()?;
        loop {
            let saved = self.index;
            self.skip_ignorable();
            if self.peek() != Some('+') {
                self.index = saved;
                break;
            }
            self.consume();
            self.skip_ignorable();
            match self.peek() {
                Some('"') | Some('\'') | Some('`') => value.push_str(&self.parse_string()?),
                _ => {
                    self.index = saved;
                    break;
                }
            }
        }
        Ok(value)
    }

    fn parse_unicode_escape(&mut self) -> Result<char, JsonRepairError> {
        let start = self.index;
        let chars: String = (0..4).filter_map(|_| self.consume()).collect();
        if chars.len() != 4 {
            return Ok('\u{FFFD}');
        }
        u32::from_str_radix(&chars, 16)
            .ok()
            .and_then(char::from_u32)
            .ok_or(JsonRepairError::InvalidUnicodeCharacter {
                chars,
                position: start,
            })
    }

    fn parse_key_token(&mut self) -> String {
        let mut token = String::new();
        while let Some(char) = self.peek() {
            if char.is_whitespace() || matches!(char, ':' | ',' | '}' | ']') {
                break;
            }
            token.push(char);
            self.consume();
        }
        token.trim().to_string()
    }

    fn parse_bare_token(&mut self) -> String {
        if self.peek() == Some('/') && self.peek_next().is_some_and(|c| c != '/' && c != '*') {
            return self.parse_regex_literal();
        }

        let mut token = String::new();
        let mut paren_depth = 0usize;
        while let Some(char) = self.peek() {
            if paren_depth == 0 && (matches!(char, ',' | ']' | '}') || char.is_whitespace()) {
                break;
            }
            if char == '(' {
                paren_depth += 1;
            } else if char == ')' && paren_depth > 0 {
                paren_depth -= 1;
                if paren_depth == 0 {
                    token.push(char);
                    self.consume();
                    break;
                }
            }
            token.push(char);
            self.consume();
        }

        let collapsed = self.consume_string_concatenation(token);
        collapsed.trim().to_string()
    }

    fn consume_string_concatenation(&mut self, first: String) -> String {
        let mut parts = vec![first];
        loop {
            let saved = self.index;
            self.skip_ignorable();
            if self.peek() != Some('+') {
                self.index = saved;
                break;
            }
            self.consume();
            self.skip_ignorable();
            let next = match self.peek() {
                Some('"') | Some('\'') | Some('`') => self.parse_string().unwrap_or_default(),
                _ => {
                    self.index = saved;
                    break;
                }
            };
            parts.push(next);
        }
        parts.join("")
    }

    fn parse_regex_literal(&mut self) -> String {
        let mut token = String::new();
        let mut escape = false;
        while let Some(char) = self.consume() {
            token.push(char);
            if escape {
                escape = false;
            } else if char == '\\' {
                escape = true;
            } else if char == '/' && token.len() > 1 {
                while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                    token.push(self.consume().unwrap_or_default());
                }
                break;
            }
        }
        token
    }

    fn value_from_token(&self, token: String) -> Value {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            return Value::Null;
        }
        match trimmed {
            "null" | "None" | "none" | "undefined" => Value::Null,
            "true" | "True" => Value::Bool(true),
            "false" | "False" => Value::Bool(false),
            _ if trimmed.ends_with(')') && trimmed.contains('(') => {
                let inner = trimmed
                    .split_once('(')
                    .and_then(|(_, rest)| rest.strip_suffix(')'))
                    .unwrap_or(trimmed);
                repair_json(inner).unwrap_or_else(|_| Value::String(trimmed.to_string()))
            }
            _ if is_invalid_leading_zero_number(trimmed) => Value::String(trimmed.to_string()),
            _ => serde_json::from_str::<Value>(trimmed)
                .unwrap_or_else(|_| Value::String(trimmed.to_string())),
        }
    }

    fn skip_ignorable(&mut self) {
        loop {
            while self.peek().is_some_and(|c| c.is_whitespace() || c == ',') {
                self.consume();
            }
            if self.starts_with("//") {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.consume();
                }
                continue;
            }
            if self.starts_with("/*") {
                self.consume();
                self.consume();
                while !self.is_end() && !self.starts_with("*/") {
                    self.consume();
                }
                if self.starts_with("*/") {
                    self.consume();
                    self.consume();
                }
                continue;
            }
            break;
        }
    }

    fn skip_ellipsis(&mut self) {
        if self.peek() == Some('\u{2026}') {
            self.consume();
        } else {
            for _ in 0..3 {
                self.consume();
            }
        }
        if self.peek() == Some(',') {
            self.consume();
        }
    }

    fn is_inner_double_quote(&self) -> bool {
        let mut lookahead = self.index;
        while lookahead < self.chars.len() && self.chars[lookahead].is_whitespace() {
            lookahead += 1;
        }
        !matches!(
            self.chars.get(lookahead),
            None | Some(',')
                | Some('}')
                | Some(']')
                | Some(':')
                | Some('+')
                | Some('"')
                | Some('\'')
        )
    }

    fn starts_with(&self, needle: &str) -> bool {
        self.source
            .chars()
            .skip(self.index)
            .take(needle.chars().count())
            .collect::<String>()
            == needle
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    fn peek_next(&self) -> Option<char> {
        self.chars.get(self.index + 1).copied()
    }

    fn consume(&mut self) -> Option<char> {
        let char = self.peek()?;
        self.index += 1;
        Some(char)
    }

    fn is_end(&self) -> bool {
        self.index >= self.chars.len()
    }
}

fn normalize_quote(char: char) -> Option<char> {
    match char {
        '"' | '\u{201C}' | '\u{201D}' => Some('"'),
        '\'' | '`' | '\u{2018}' | '\u{2019}' | '\u{00B4}' => Some('\''),
        _ => None,
    }
}

fn is_invalid_leading_zero_number(token: &str) -> bool {
    let digits = token.strip_prefix('-').unwrap_or(token);
    digits.len() > 1 && digits.starts_with('0') && digits.chars().all(|c| c.is_ascii_digit())
}

fn normalize_value_strings(value: &mut Value) {
    match value {
        Value::String(s) => {
            *s = s.replace(['\u{201C}', '\u{201D}'], "\"");
            *s = s.replace(['\u{2018}', '\u{2019}', '\u{00B4}'], "'");
        }
        Value::Array(items) => {
            for item in items {
                normalize_value_strings(item);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                normalize_value_strings(item);
            }
        }
        _ => {}
    }
}
