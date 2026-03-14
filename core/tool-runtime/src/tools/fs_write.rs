use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

pub struct FsWriteTool;

#[async_trait::async_trait]
impl Tool for FsWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "fs_write".into(),
            name: "fs_write".into(),
            description: "Write content to a file within the workspace".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path within the workspace"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
            required_permissions: vec!["fs:write".into()],
            trust_level: 1,
            idempotent: true,
            timeout_seconds: 30,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("path").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'path' is required".into()));
        }
        if args.get("content").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'content' is required".into()));
        }
        Ok(())
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path_str = args["path"].as_str().unwrap();
        let content = args["content"].as_str().unwrap();

        let path = std::path::Path::new(path_str);

        // Reject absolute paths for trust_level 1
        if path.is_absolute() {
            return Ok(ToolOutput::Error {
                error: "Absolute paths are not allowed for fs_write. Use a relative path within the workspace.".into(),
                retryable: false,
            });
        }

        let resolved = ctx.workspace_path.join(path);

        // Create parent directories if needed
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;
        }

        std::fs::write(&resolved, content)
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        let size = content.len();

        Ok(ToolOutput::Success {
            result: json!({
                "path": resolved.display().to_string(),
                "size_bytes": size,
            }),
            tokens_used: None,
        })
    }
}
