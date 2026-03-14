use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

pub struct ShellExecTool;

#[async_trait::async_trait]
impl Tool for ShellExecTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "shell_exec".into(),
            name: "shell_exec".into(),
            description: "Execute a shell command. Requires user approval.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute"
                    }
                },
                "required": ["command"]
            }),
            required_permissions: vec!["shell:exec".into()],
            trust_level: 2, // Requires approval
            idempotent: false,
            timeout_seconds: 60,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("command").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'command' is required".into()));
        }
        Ok(())
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        // This tool has trust_level 2, so ToolRegistry will return NeedsApproval
        // before reaching this code. If we do reach here (after approval), execute:
        let command = args["command"].as_str().unwrap();

        let output = tokio::process::Command::new(if cfg!(target_os = "windows") { "cmd" } else { "sh" })
            .args(if cfg!(target_os = "windows") { vec!["/C", command] } else { vec!["-c", command] })
            .output()
            .await
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Truncate outputs
        let stdout = if stdout.len() > 51200 {
            format!("{}... [truncated]", &stdout[..51200])
        } else {
            stdout.to_string()
        };
        let stderr = if stderr.len() > 51200 {
            format!("{}... [truncated]", &stderr[..51200])
        } else {
            stderr.to_string()
        };

        Ok(ToolOutput::Success {
            result: json!({
                "stdout": stdout,
                "stderr": stderr,
                "exit_code": output.status.code().unwrap_or(-1),
            }),
            tokens_used: None,
        })
    }
}
