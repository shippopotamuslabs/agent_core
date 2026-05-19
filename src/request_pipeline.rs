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
            let name = tool.as_str().unwrap_or("");
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
