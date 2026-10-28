use crate::events::AgentError;
use crate::loop_metrics::LoopMetrics;
use crate::session::{AgentConfig, SessionCapabilities};
use crate::turn_io::ModelRequest;
use std::sync::Arc;

pub trait RequestTransformer: Send + Sync {
    fn transform(&self, request: ModelRequest) -> Result<ModelRequest, AgentError>;
}

pub struct RequestPipeline {
    pub transformers: Vec<Arc<dyn RequestTransformer>>,
}

impl RequestPipeline {
    pub fn transform(&self, request: ModelRequest) -> Result<ModelRequest, AgentError> {
        let mut req = request;
        for t in &self.transformers {
            req = t.transform(req)?;
        }
        Ok(req)
    }
}

pub fn render_runtime_tool_view(
    catalog_json: &str,
    config: &AgentConfig,
    capabilities: &SessionCapabilities,
    metrics: &LoopMetrics,
) -> Result<String, AgentError> {
    let tools: serde_json::Value =
        serde_json::from_str(catalog_json).map_err(|e| AgentError::Internal(e.to_string()))?;
    let arr = tools
        .as_array()
        .ok_or_else(|| AgentError::Internal("catalog_json must be a JSON array".into()))?;

    let filtered: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|tool| {
            // Catalog entries are schema objects ({"name": ..., ...}), so
            // extract the tool name from the "name" field.
            let name: &str = tool.get("name").and_then(|n| n.as_str()).unwrap_or("");
            // Remove patch_file and write_file when require_read_before_edit and no files read yet
            if config.require_read_before_edit
                && metrics.files_read.is_empty()
                && (name == "patch_file" || name == "write_file")
            {
                return false;
            }
            // Remove task_delegate when delegation tools not supported
            if !capabilities.delegation_tools_supported && name == "task_delegate" {
                return false;
            }
            true
        })
        .collect();

    serde_json::to_string(&filtered).map_err(|e| AgentError::Internal(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn tool_entry(name: &str) -> String {
        format!(r#"{{"name":{name:?},"description":"d","parameters":{{"type":"object"}}}}"#)
    }

    fn catalog(names: &[&str]) -> String {
        let entries: Vec<String> = names.iter().map(|n| tool_entry(n)).collect();
        format!("[{}]", entries.join(","))
    }

    fn names_in(rendered: &str) -> Vec<String> {
        let tools: Vec<Value> = serde_json::from_str(rendered).expect("valid JSON array");
        tools
            .iter()
            .map(|t| {
                t.get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn edit_tools_gated_until_first_read() {
        let config = AgentConfig {
            require_read_before_edit: true,
            ..AgentConfig::default()
        };
        let capabilities = SessionCapabilities::default();
        let metrics = LoopMetrics::default();
        let catalog_json = catalog(&["read_file", "patch_file", "write_file"]);

        let rendered =
            render_runtime_tool_view(&catalog_json, &config, &capabilities, &metrics).unwrap();
        let names = names_in(&rendered);
        assert!(names.contains(&"read_file".to_string()));
        assert!(!names.contains(&"patch_file".to_string()));
        assert!(!names.contains(&"write_file".to_string()));

        let mut after_read = LoopMetrics::default();
        after_read.record_file_read("src/main.rs");
        let rendered =
            render_runtime_tool_view(&catalog_json, &config, &capabilities, &after_read).unwrap();
        let names = names_in(&rendered);
        assert!(names.contains(&"patch_file".to_string()));
        assert!(names.contains(&"write_file".to_string()));
    }

    #[test]
    fn delegation_gate_tracks_capability() {
        let config = AgentConfig::default();
        let catalog_json = catalog(&["read_file", "task_delegate"]);

        let unsupported = SessionCapabilities {
            delegation_tools_supported: false,
            ..SessionCapabilities::default()
        };
        let rendered = render_runtime_tool_view(
            &catalog_json,
            &config,
            &unsupported,
            &LoopMetrics::default(),
        )
        .unwrap();
        assert!(!names_in(&rendered).contains(&"task_delegate".to_string()));

        let supported = SessionCapabilities {
            delegation_tools_supported: true,
            ..SessionCapabilities::default()
        };
        let rendered =
            render_runtime_tool_view(&catalog_json, &config, &supported, &LoopMetrics::default())
                .unwrap();
        assert!(names_in(&rendered).contains(&"task_delegate".to_string()));
    }
}
