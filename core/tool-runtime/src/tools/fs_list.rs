use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

pub struct FsListTool;

#[async_trait::async_trait]
impl Tool for FsListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "fs_list".into(),
            name: "fs_list".into(),
            description: "List directory contents".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path to list (relative to workspace or absolute)"
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

        let resolved = if path.is_relative() {
            ctx.workspace_path.join(path)
        } else {
            path.to_path_buf()
        };

        if !resolved.is_dir() {
            return Ok(ToolOutput::Error {
                error: format!("Not a directory: {}", resolved.display()),
                retryable: false,
            });
        }

        let mut entries = Vec::new();
        let dir = std::fs::read_dir(&resolved)
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        for entry in dir {
            let entry = entry.map_err(|e| ToolError::ExecutionError(e.to_string()))?;
            let meta = entry.metadata().ok();
            entries.push(json!({
                "name": entry.file_name().to_string_lossy(),
                "is_dir": meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                "size_bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0),
            }));
        }

        Ok(ToolOutput::Success {
            result: json!({
                "path": resolved.display().to_string(),
                "entries": entries,
                "count": entries.len(),
            }),
            tokens_used: None,
        })
    }
}
