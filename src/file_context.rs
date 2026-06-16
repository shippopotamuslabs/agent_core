//! File and folder condensation into outline/chunk references.
//!
//! Input shape:
//! ```json
//! [
//!   { "path": "src/big.rs", "token_estimate": 500, "content": "fn ..." }
//! ]
//! ```

use serde::{Deserialize, Serialize};

use crate::context_store::{
    make_preview, ContextReference, ContextReferenceKind, ContextStore, ContextStoreEntry,
};
use crate::session::{AgentConfig, SessionCapabilities};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileCondensationLevel {
    Full,
    Outline,
    FilenameOnly,
    Excluded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContextSummary {
    pub path: String,
    pub condensation_level: FileCondensationLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chunk_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContextIndex {
    pub root_ref: String,
    pub included_paths: Vec<FileContextSummary>,
}

/// Threshold above which content is treated as unparseable / oversized and
/// demoted to filename-only.
const FILENAME_ONLY_TOKEN_CEILING: u32 = 16_000;

#[derive(Debug, Deserialize)]
struct InputFile {
    path: String,
    token_estimate: u32,
    #[serde(default)]
    content: String,
}

fn looks_parseable(content: &str) -> bool {
    // Heuristic: parseable when it contains structural separators (fn/class
    // keywords or curly braces). Pure binary blobs fail.
    let lower = content.to_lowercase();
    lower.contains("fn ")
        || lower.contains("class ")
        || lower.contains("def ")
        || content.contains('{')
        || content.contains('}')
        || content.contains(';')
}

fn build_outline(content: &str) -> String {
    // Tiny outline: list of fn/class/def signatures we can pluck out cheaply.
    let mut lines: Vec<String> = Vec::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with("fn ")
            || line.starts_with("pub fn ")
            || line.starts_with("class ")
            || line.starts_with("def ")
            || line.starts_with("struct ")
            || line.starts_with("enum ")
        {
            lines.push(line.to_string());
        }
    }
    if lines.is_empty() {
        // Fallback to first-line preview so we always emit something useful.
        if let Some(first) = content.lines().next() {
            lines.push(first.to_string());
        }
    }
    lines.join("\n")
}

pub fn condense_file_context(
    included_files_json: &str,
    store: &mut ContextStore,
    config: &AgentConfig,
    capabilities: &SessionCapabilities,
    turn_index: u32,
) -> Option<FileContextIndex> {
    if !config.enable_file_condensation {
        return None;
    }
    let files: Vec<InputFile> = match serde_json::from_str(included_files_json) {
        Ok(files) => files,
        Err(_) => {
            let sanitized: String = included_files_json
                .chars()
                .map(|ch| {
                    if ch.is_control() && !matches!(ch, '\n' | '\r' | '\t') {
                        ' '
                    } else {
                        ch
                    }
                })
                .collect();
            serde_json::from_str(&sanitized).ok()?
        }
    };

    let outline_threshold = config.file_outline_threshold_tokens.max(1);
    let allow_outline = capabilities.file_backed_context_supported;

    let mut summaries: Vec<FileContextSummary> = Vec::with_capacity(files.len());
    for file in &files {
        let level: FileCondensationLevel;
        let mut outline_ref: Option<String> = None;
        let mut chunk_refs: Vec<String> = Vec::new();

        let is_unparseable_or_huge =
            file.token_estimate > FILENAME_ONLY_TOKEN_CEILING || !looks_parseable(&file.content);

        if is_unparseable_or_huge {
            level = FileCondensationLevel::FilenameOnly;
        } else if file.token_estimate > outline_threshold {
            if allow_outline {
                // Persist outline + one chunk reference in the store.
                let outline_id = format!("file-outline-{}", file.path);
                let outline_body = build_outline(&file.content);
                let outline_entry = ContextStoreEntry {
                    reference: ContextReference {
                        id: outline_id.clone(),
                        kind: ContextReferenceKind::FileOutline,
                        title: crate::context_store::append_turn_suffix(
                            format!("outline:{}", file.path),
                            turn_index,
                        ),
                        byte_len: outline_body.len() as u64,
                        token_estimate: (outline_body.len() / 4) as u32,
                        preview: make_preview(&outline_body),
                        retrieval_hint: format!("read_context_reference(\"{outline_id}\")"),
                    },
                    content: outline_body,
                    source_message_ids: vec![],
                    created_turn_index: turn_index,
                    expires_after_turn: None,
                };
                let outline = store.insert_entry(outline_entry);
                outline_ref = Some(outline.id.clone());

                let chunk_id = format!("file-chunk-{}-0", file.path);
                let chunk_entry = ContextStoreEntry {
                    reference: ContextReference {
                        id: chunk_id.clone(),
                        kind: ContextReferenceKind::FileChunk,
                        title: crate::context_store::append_turn_suffix(
                            format!("chunk:{}", file.path),
                            turn_index,
                        ),
                        byte_len: file.content.len() as u64,
                        token_estimate: file.token_estimate,
                        preview: make_preview(&file.content),
                        retrieval_hint: format!("read_context_reference(\"{chunk_id}\")"),
                    },
                    content: file.content.clone(),
                    source_message_ids: vec![],
                    created_turn_index: turn_index,
                    expires_after_turn: None,
                };
                let chunk = store.insert_entry(chunk_entry);
                chunk_refs.push(chunk.id);
                level = FileCondensationLevel::Outline;
            } else {
                level = FileCondensationLevel::FilenameOnly;
            }
        } else {
            level = FileCondensationLevel::Full;
        }

        summaries.push(FileContextSummary {
            path: file.path.clone(),
            condensation_level: level,
            outline_ref,
            chunk_refs,
        });
    }

    Some(FileContextIndex {
        root_ref: "file-context-root".into(),
        included_paths: summaries,
    })
}
