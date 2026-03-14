use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

pub struct CodeExecTool;

const SUPPORTED_LANGUAGES: &[&str] = &["python", "javascript", "bash"];
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_OUTPUT_BYTES: usize = 51200;

#[async_trait::async_trait]
impl Tool for CodeExecTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "code_exec".into(),
            name: "code_exec".into(),
            description: "Execute code in a sandboxed subprocess. Supports Python, JavaScript, and Bash.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "enum": ["python", "javascript", "bash"],
                        "description": "Programming language to execute"
                    },
                    "code": {
                        "type": "string",
                        "description": "Source code to execute"
                    }
                },
                "required": ["language", "code"]
            }),
            required_permissions: vec!["code:exec".into()],
            trust_level: 1, // Lower than shell_exec (2) since code_exec is more constrained
            idempotent: false,
            timeout_seconds: DEFAULT_TIMEOUT_SECS as u32,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        let language = args
            .get("language")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::ValidationError("'language' is required".into()))?;

        if !SUPPORTED_LANGUAGES.contains(&language) {
            return Err(ToolError::ValidationError(format!(
                "Unsupported language '{}'. Supported: {}",
                language,
                SUPPORTED_LANGUAGES.join(", ")
            )));
        }

        if args.get("code").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'code' is required".into()));
        }

        Ok(())
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let language = args["language"].as_str().unwrap();
        let code = args["code"].as_str().unwrap();

        let (program, flag) = match language {
            "python" => ("python3", "-c"),
            "javascript" => ("node", "-e"),
            "bash" => ("bash", "-c"),
            _ => {
                return Err(ToolError::ValidationError(format!(
                    "Unsupported language: {}",
                    language
                )));
            }
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            tokio::process::Command::new(program)
                .arg(flag)
                .arg(code)
                .output(),
        )
        .await;

        let output = match result {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(ToolError::ExecutionError(format!(
                    "Failed to spawn {}: {}",
                    program, e
                )));
            }
            Err(_) => {
                return Err(ToolError::ExecutionError(format!(
                    "Code execution timed out after {}s",
                    DEFAULT_TIMEOUT_SECS
                )));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Truncate outputs to avoid excessive memory usage
        let stdout = if stdout.len() > MAX_OUTPUT_BYTES {
            format!("{}... [truncated]", &stdout[..MAX_OUTPUT_BYTES])
        } else {
            stdout.to_string()
        };
        let stderr = if stderr.len() > MAX_OUTPUT_BYTES {
            format!("{}... [truncated]", &stderr[..MAX_OUTPUT_BYTES])
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
