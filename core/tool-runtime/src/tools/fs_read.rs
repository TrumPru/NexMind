use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

pub struct FsReadTool;

#[async_trait::async_trait]
impl Tool for FsReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "fs_read".into(),
            name: "fs_read".into(),
            description: "Read the contents of a file".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read"
                    }
                },
                "required": ["path"]
            }),
            required_permissions: vec!["fs:read".into()],
            trust_level: 0,
            idempotent: true,
            timeout_seconds: 30,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("path").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'path' is required".into()));
        }
        Ok(())
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path_str = args["path"].as_str().unwrap();
        let path = std::path::Path::new(path_str);

        // If relative, resolve within workspace
        let resolved = if path.is_relative() {
            ctx.workspace_path.join(path)
        } else {
            // Absolute path: verify it's within workspace or allowed
            path.to_path_buf()
        };

        if !resolved.exists() {
            return Ok(ToolOutput::Error {
                error: format!("File not found: {}", resolved.display()),
                retryable: false,
            });
        }

        let metadata = std::fs::metadata(&resolved)
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        if metadata.len() > 1_048_576 {
            return Ok(ToolOutput::Error {
                error: "File too large (>1MB)".into(),
                retryable: false,
            });
        }

        let content = std::fs::read_to_string(&resolved)
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        Ok(ToolOutput::Success {
            result: json!({
                "content": content,
                "size_bytes": metadata.len(),
                "path": resolved.display().to_string(),
            }),
            tokens_used: None,
        })
    }
}
