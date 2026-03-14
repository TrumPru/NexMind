use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::info;

use nexmind_tool_runtime::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

use crate::OpenClawAgent;

// ── openclaw_send ───────────────────────────────────────────────────

/// Tool: Send a message to OpenClaw and get a response.
///
/// This is the primary tool for conversational interaction with OpenClaw.
/// The message is sent to OpenClaw's agent, which processes it using its
/// full tool suite (files, shell, web, memory, skills) and returns a response.
pub struct OpenClawSendTool {
    agent: Arc<OpenClawAgent>,
}

impl OpenClawSendTool {
    pub fn new(agent: Arc<OpenClawAgent>) -> Self {
        Self { agent }
    }
}

#[async_trait]
impl Tool for OpenClawSendTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "openclaw_send".into(),
            name: "openclaw_send".into(),
            description: "Send a message to the OpenClaw AI agent and receive a response. \
                OpenClaw has access to tools (files, shell, web, memory, skills) \
                and can perform complex tasks on the host machine."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "The message to send to OpenClaw"
                    }
                },
                "required": ["message"]
            }),
            required_permissions: vec!["openclaw:send".into()],
            trust_level: 1,
            idempotent: true,
            timeout_seconds: 120,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("message").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError(
                "missing required 'message' parameter".into(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::ValidationError("missing 'message' parameter".into()))?;

        info!(message_len = message.len(), "openclaw_send tool called");

        match self.agent.run(message).await {
            Ok(response) => Ok(ToolOutput::Success {
                result: json!({ "response": response }),
                tokens_used: None,
            }),
            Err(e) => Ok(ToolOutput::Error {
                error: format!("OpenClaw error: {}", e),
                retryable: e.is_retryable(),
            }),
        }
    }
}

// ── openclaw_delegate ───────────────────────────────────────────────

/// Tool: Delegate a complex task to an isolated OpenClaw session.
///
/// Spawns a new isolated session in OpenClaw that runs independently.
/// Best for complex, long-running tasks like code analysis, research,
/// or multi-step workflows.
pub struct OpenClawDelegateTool {
    agent: Arc<OpenClawAgent>,
}

impl OpenClawDelegateTool {
    pub fn new(agent: Arc<OpenClawAgent>) -> Self {
        Self { agent }
    }
}

#[async_trait]
impl Tool for OpenClawDelegateTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "openclaw_delegate".into(),
            name: "openclaw_delegate".into(),
            description: "Delegate a complex task to an isolated OpenClaw agent session. \
                The task runs independently with full tool access. \
                Use for long-running tasks like code analysis, research, or multi-step work."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "Description of the task to delegate"
                    },
                    "model": {
                        "type": "string",
                        "description": "Optional: specific model to use (e.g., 'anthropic/claude-opus-4-6')"
                    }
                },
                "required": ["task"]
            }),
            required_permissions: vec!["openclaw:delegate".into()],
            trust_level: 1,
            idempotent: false,
            timeout_seconds: 300,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("task").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError(
                "missing required 'task' parameter".into(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::ValidationError("missing 'task' parameter".into()))?;

        let model = args.get("model").and_then(|v| v.as_str());

        info!(task_len = task.len(), model = ?model, "openclaw_delegate tool called");

        match self
            .agent
            .delegate_task_with_options(task, model, None)
            .await
        {
            Ok(result) => Ok(ToolOutput::Success {
                result: json!({ "result": result }),
                tokens_used: None,
            }),
            Err(e) => Ok(ToolOutput::Error {
                error: format!("OpenClaw delegation error: {}", e),
                retryable: e.is_retryable(),
            }),
        }
    }
}

// ── openclaw_status ─────────────────────────────────────────────────

/// Tool: Check the status of the OpenClaw gateway.
pub struct OpenClawStatusTool {
    agent: Arc<OpenClawAgent>,
}

impl OpenClawStatusTool {
    pub fn new(agent: Arc<OpenClawAgent>) -> Self {
        Self { agent }
    }
}

#[async_trait]
impl Tool for OpenClawStatusTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "openclaw_status".into(),
            name: "openclaw_status".into(),
            description: "Check the status and health of the connected OpenClaw gateway. \
                Returns version, uptime, and connectivity information."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            required_permissions: vec!["openclaw:status".into()],
            trust_level: 0,
            idempotent: true,
            timeout_seconds: 10,
        }
    }

    fn validate_args(&self, _args: &Value) -> Result<(), ToolError> {
        Ok(())
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        info!("openclaw_status tool called");

        match self.agent.health().await {
            Ok(health) => Ok(ToolOutput::Success {
                result: json!({
                    "status": health.status.unwrap_or_else(|| "unknown".into()),
                    "version": health.version.unwrap_or_else(|| "unknown".into()),
                    "ok": health.ok.unwrap_or(false),
                    "available": true,
                }),
                tokens_used: None,
            }),
            Err(e) => Ok(ToolOutput::Success {
                result: json!({
                    "available": false,
                    "error": e.to_string(),
                }),
                tokens_used: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_send_tool_definition() {
        let config = crate::config::OpenClawConfig::default();
        let agent = Arc::new(OpenClawAgent::new(config));
        let tool = OpenClawSendTool::new(agent);
        let def = tool.definition();

        assert_eq!(def.id, "openclaw_send");
        assert_eq!(def.name, "openclaw_send");
        assert!(def.description.contains("OpenClaw"));
        assert!(def.required_permissions.contains(&"openclaw:send".into()));
        assert_eq!(def.trust_level, 1);
        assert!(def.idempotent);
        assert_eq!(def.timeout_seconds, 120);
    }

    #[test]
    fn test_delegate_tool_definition() {
        let config = crate::config::OpenClawConfig::default();
        let agent = Arc::new(OpenClawAgent::new(config));
        let tool = OpenClawDelegateTool::new(agent);
        let def = tool.definition();

        assert_eq!(def.id, "openclaw_delegate");
        assert!(def.required_permissions.contains(&"openclaw:delegate".into()));
        assert!(!def.idempotent);
        assert_eq!(def.timeout_seconds, 300);
    }

    #[test]
    fn test_status_tool_definition() {
        let config = crate::config::OpenClawConfig::default();
        let agent = Arc::new(OpenClawAgent::new(config));
        let tool = OpenClawStatusTool::new(agent);
        let def = tool.definition();

        assert_eq!(def.id, "openclaw_status");
        assert_eq!(def.trust_level, 0);
        assert!(def.idempotent);
        assert_eq!(def.timeout_seconds, 10);
    }

    #[test]
    fn test_send_validate_args_missing_message() {
        let config = crate::config::OpenClawConfig::default();
        let agent = Arc::new(OpenClawAgent::new(config));
        let tool = OpenClawSendTool::new(agent);

        let result = tool.validate_args(&json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn test_send_validate_args_valid() {
        let config = crate::config::OpenClawConfig::default();
        let agent = Arc::new(OpenClawAgent::new(config));
        let tool = OpenClawSendTool::new(agent);

        let result = tool.validate_args(&json!({"message": "hello"}));
        assert!(result.is_ok());
    }

    #[test]
    fn test_delegate_validate_args_missing_task() {
        let config = crate::config::OpenClawConfig::default();
        let agent = Arc::new(OpenClawAgent::new(config));
        let tool = OpenClawDelegateTool::new(agent);

        let result = tool.validate_args(&json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn test_status_validate_args_empty() {
        let config = crate::config::OpenClawConfig::default();
        let agent = Arc::new(OpenClawAgent::new(config));
        let tool = OpenClawStatusTool::new(agent);

        let result = tool.validate_args(&json!({}));
        assert!(result.is_ok());
    }
}
