use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

/// Agent delegation tool — allows an agent to delegate a subtask to another agent.
pub struct DelegateTool;

#[async_trait::async_trait]
impl Tool for DelegateTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "delegate_to_agent".into(),
            name: "delegate_to_agent".into(),
            description: "Delegate a subtask to another agent and get the result. Use this when a task is better handled by a specialized agent.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The ID of the agent to delegate to"
                    },
                    "task": {
                        "type": "string",
                        "description": "The task description to send to the delegated agent"
                    },
                    "context": {
                        "type": "string",
                        "description": "Optional context to provide to the delegated agent"
                    }
                },
                "required": ["agent_id", "task"]
            }),
            required_permissions: vec!["agent:delegate".into()],
            trust_level: 1,
            idempotent: false,
            timeout_seconds: 120,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("agent_id").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'agent_id' is required".into()));
        }
        if args.get("task").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'task' is required".into()));
        }
        Ok(())
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let agent_id = args["agent_id"].as_str().unwrap();
        let task = args["task"].as_str().unwrap();
        let context = args.get("context").and_then(|v| v.as_str()).unwrap_or("");

        // Build the full input for the delegated agent
        let full_input = if context.is_empty() {
            task.to_string()
        } else {
            format!("Context: {}\n\nTask: {}", context, task)
        };

        // Delegation is handled by the agent runtime — this tool returns
        // a delegation request that the runtime intercepts.
        Ok(ToolOutput::Success {
            result: json!({
                "delegation_request": true,
                "target_agent_id": agent_id,
                "input": full_input,
                "requesting_agent_id": ctx.agent_id,
                "workspace_id": ctx.workspace_id,
                "status": "Agent delegation requested. The runtime will execute the target agent and return the result."
            }),
            tokens_used: None,
        })
    }
}
