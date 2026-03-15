use std::sync::Arc;

use serde_json::{json, Value};

use nexmind_agent_comm::{
    AgentExecutor, AgentMessage, AgentMessageType, FileRef, MailboxRouter,
};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

// ── AgentSendMessageTool ────────────────────────────────────────

/// Send a direct message to another agent.
pub struct AgentSendMessageTool {
    mailbox_router: Arc<MailboxRouter>,
}

impl AgentSendMessageTool {
    pub fn new(mailbox_router: Arc<MailboxRouter>) -> Self {
        Self { mailbox_router }
    }
}

#[async_trait::async_trait]
impl Tool for AgentSendMessageTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "agent_send_message".into(),
            name: "agent_send_message".into(),
            description: "Send a message to another agent. Use this to communicate, ask questions, or share information with other agents in your team.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "recipient_id": {
                        "type": "string",
                        "description": "The agent ID to send the message to"
                    },
                    "content": {
                        "type": "string",
                        "description": "The message content to send"
                    },
                    "reply_to": {
                        "type": "string",
                        "description": "Optional message ID to reply to"
                    }
                },
                "required": ["recipient_id", "content"]
            }),
            required_permissions: vec!["agent:communicate".into()],
            trust_level: 0,
            idempotent: false,
            timeout_seconds: 30,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("recipient_id").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'recipient_id' is required".into()));
        }
        if args.get("content").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'content' is required".into()));
        }
        Ok(())
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let recipient_id = args["recipient_id"].as_str().unwrap();
        let content = args["content"].as_str().unwrap();
        let reply_to = args.get("reply_to").and_then(|v| v.as_str());

        let mut msg = AgentMessage::direct(&ctx.agent_id, recipient_id, content);
        if let Some(team_id) = &ctx.team_id {
            msg = msg.with_team(team_id);
        }
        if let Some(reply_to) = reply_to {
            msg = msg.with_reply_to(reply_to);
        }
        msg.correlation_id = Some(ctx.correlation_id.clone());

        let message_id = msg.id.clone();

        self.mailbox_router
            .send(msg)
            .await
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        tracing::info!(
            from = %ctx.agent_id,
            to = %recipient_id,
            "agent message sent"
        );

        Ok(ToolOutput::Success {
            result: json!({
                "sent": true,
                "message_id": message_id,
                "recipient_id": recipient_id,
            }),
            tokens_used: None,
        })
    }
}

// ── AgentReceiveMessagesTool ────────────────────────────────────

/// Check inbox for messages from other agents.
pub struct AgentReceiveMessagesTool {
    mailbox_router: Arc<MailboxRouter>,
}

impl AgentReceiveMessagesTool {
    pub fn new(mailbox_router: Arc<MailboxRouter>) -> Self {
        Self { mailbox_router }
    }
}

#[async_trait::async_trait]
impl Tool for AgentReceiveMessagesTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "agent_receive_messages".into(),
            name: "agent_receive_messages".into(),
            description: "Check your inbox for messages from other agents. Returns recent messages addressed to you.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of messages to return (default: 10)"
                    },
                    "from_agent": {
                        "type": "string",
                        "description": "Optional: only return messages from this agent"
                    }
                },
                "required": []
            }),
            required_permissions: vec!["agent:communicate".into()],
            trust_level: 0,
            idempotent: true,
            timeout_seconds: 10,
        }
    }

    fn validate_args(&self, _args: &Value) -> Result<(), ToolError> {
        Ok(())
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;
        let from_agent = args.get("from_agent").and_then(|v| v.as_str());

        let messages = self
            .mailbox_router
            .get_agent_messages(&ctx.agent_id, ctx.team_id.as_deref(), limit)
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        let filtered: Vec<_> = if let Some(from) = from_agent {
            messages.into_iter().filter(|m| m.sender_id == from).collect()
        } else {
            messages
        };

        let message_list: Vec<Value> = filtered
            .iter()
            .map(|m| {
                json!({
                    "id": m.id,
                    "from": m.sender_id,
                    "type": m.message_type.to_string(),
                    "content": m.content,
                    "timestamp": m.timestamp,
                    "reply_to": m.reply_to,
                    "file_refs": m.file_refs,
                })
            })
            .collect();

        Ok(ToolOutput::Success {
            result: json!({
                "messages": message_list,
                "count": message_list.len(),
            }),
            tokens_used: None,
        })
    }
}

// ── AgentSendFileTool ───────────────────────────────────────────

/// Send a file to another agent by sharing via workspace.
pub struct AgentSendFileTool {
    mailbox_router: Arc<MailboxRouter>,
}

impl AgentSendFileTool {
    pub fn new(mailbox_router: Arc<MailboxRouter>) -> Self {
        Self { mailbox_router }
    }
}

#[async_trait::async_trait]
impl Tool for AgentSendFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "agent_send_file".into(),
            name: "agent_send_file".into(),
            description: "Send a file to another agent. The file is shared via the team workspace directory.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "recipient_id": {
                        "type": "string",
                        "description": "The agent ID to send the file to"
                    },
                    "file_path": {
                        "type": "string",
                        "description": "Path to the file (relative to workspace)"
                    },
                    "message": {
                        "type": "string",
                        "description": "Optional message to accompany the file"
                    }
                },
                "required": ["recipient_id", "file_path"]
            }),
            required_permissions: vec!["agent:communicate".into(), "fs:read".into()],
            trust_level: 0,
            idempotent: false,
            timeout_seconds: 30,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("recipient_id").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'recipient_id' is required".into()));
        }
        if args.get("file_path").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'file_path' is required".into()));
        }
        Ok(())
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let recipient_id = args["recipient_id"].as_str().unwrap();
        let file_path = args["file_path"].as_str().unwrap();
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("File shared");

        // Resolve file path within workspace
        let full_path = ctx.workspace_path.join(file_path);
        if !full_path.exists() {
            return Ok(ToolOutput::Error {
                error: format!("File not found: {}", file_path),
                retryable: false,
            });
        }

        let metadata = std::fs::metadata(&full_path)
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        let filename = full_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| file_path.to_string());

        // Copy file to shared directory if team_id is set
        let shared_path = if let Some(team_id) = &ctx.team_id {
            let shared_dir = ctx.workspace_path.join("shared").join(team_id);
            std::fs::create_dir_all(&shared_dir)
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;
            let dest = shared_dir.join(&filename);
            if full_path != dest {
                std::fs::copy(&full_path, &dest)
                    .map_err(|e| ToolError::ExecutionError(e.to_string()))?;
            }
            format!("shared/{}/{}", team_id, filename)
        } else {
            file_path.to_string()
        };

        let mime_type = match full_path.extension().and_then(|e| e.to_str()) {
            Some("txt") => Some("text/plain".to_string()),
            Some("json") => Some("application/json".to_string()),
            Some("pdf") => Some("application/pdf".to_string()),
            Some("png") => Some("image/png".to_string()),
            Some("jpg") | Some("jpeg") => Some("image/jpeg".to_string()),
            Some("csv") => Some("text/csv".to_string()),
            Some("md") => Some("text/markdown".to_string()),
            Some("rs") => Some("text/x-rust".to_string()),
            Some("py") => Some("text/x-python".to_string()),
            _ => None,
        };

        let file_ref = FileRef {
            path: shared_path.clone(),
            filename: filename.clone(),
            mime_type,
            size_bytes: metadata.len(),
        };

        let mut msg = AgentMessage::file_share(&ctx.agent_id, recipient_id, message, file_ref.clone());
        if let Some(team_id) = &ctx.team_id {
            msg = msg.with_team(team_id);
        }
        msg.correlation_id = Some(ctx.correlation_id.clone());

        self.mailbox_router
            .send(msg)
            .await
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        tracing::info!(
            from = %ctx.agent_id,
            to = %recipient_id,
            file = %filename,
            "file shared with agent"
        );

        Ok(ToolOutput::Success {
            result: json!({
                "sent": true,
                "file_ref": {
                    "path": shared_path,
                    "filename": filename,
                    "size_bytes": metadata.len(),
                },
                "recipient_id": recipient_id,
            }),
            tokens_used: None,
        })
    }
}

// ── AgentListTeamTool ───────────────────────────────────────────

/// List available agents in the current team.
pub struct AgentListTeamTool {
    mailbox_router: Arc<MailboxRouter>,
}

impl AgentListTeamTool {
    pub fn new(mailbox_router: Arc<MailboxRouter>) -> Self {
        Self { mailbox_router }
    }
}

#[async_trait::async_trait]
impl Tool for AgentListTeamTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "agent_list_team".into(),
            name: "agent_list_team".into(),
            description: "List all agents in your current team with their roles and capabilities.".into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            required_permissions: vec!["agent:communicate".into()],
            trust_level: 0,
            idempotent: true,
            timeout_seconds: 10,
        }
    }

    fn validate_args(&self, _args: &Value) -> Result<(), ToolError> {
        Ok(())
    }

    async fn execute(&self, _args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let team_id = ctx.team_id.as_deref().unwrap_or("default");

        let members = self.mailbox_router.get_team_members(team_id).await;

        if members.is_empty() {
            return Ok(ToolOutput::Success {
                result: json!({
                    "team_id": team_id,
                    "members": [],
                    "count": 0,
                    "note": "No team members registered. You may be running standalone."
                }),
                tokens_used: None,
            });
        }

        let member_list: Vec<Value> = members
            .iter()
            .map(|m| {
                json!({
                    "agent_id": m.agent_id,
                    "name": m.name,
                    "role": m.role,
                    "description": m.description,
                })
            })
            .collect();

        Ok(ToolOutput::Success {
            result: json!({
                "team_id": team_id,
                "members": member_list,
                "count": member_list.len(),
            }),
            tokens_used: None,
        })
    }
}

// ── AgentDelegateTaskTool ───────────────────────────────────────

/// Delegate a task to another agent and wait for the result.
pub struct AgentDelegateTaskTool {
    mailbox_router: Arc<MailboxRouter>,
    agent_executor: Arc<dyn AgentExecutor>,
}

impl AgentDelegateTaskTool {
    pub fn new(
        mailbox_router: Arc<MailboxRouter>,
        agent_executor: Arc<dyn AgentExecutor>,
    ) -> Self {
        Self {
            mailbox_router,
            agent_executor,
        }
    }
}

#[async_trait::async_trait]
impl Tool for AgentDelegateTaskTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "agent_delegate_task".into(),
            name: "agent_delegate_task".into(),
            description: "Delegate a task to another agent and wait for the result. The target agent will execute the task and return its response.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The agent ID to delegate the task to"
                    },
                    "task": {
                        "type": "string",
                        "description": "The task description/instructions for the target agent"
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Maximum seconds to wait for result (default: 120)"
                    }
                },
                "required": ["agent_id", "task"]
            }),
            required_permissions: vec!["agent:communicate".into(), "agent:delegate".into()],
            trust_level: 1,
            idempotent: false,
            timeout_seconds: 300,
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
        let timeout_secs = args
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(120);

        tracing::info!(
            delegator = %ctx.agent_id,
            delegate = %agent_id,
            "delegating task to agent"
        );

        // Send delegation message
        let mut msg = AgentMessage::delegation(&ctx.agent_id, agent_id, task);
        if let Some(team_id) = &ctx.team_id {
            msg = msg.with_team(team_id);
        }
        msg.correlation_id = Some(ctx.correlation_id.clone());

        let delegation_id = msg.id.clone();
        self.mailbox_router
            .send(msg)
            .await
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        // Execute the agent with timeout
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            self.agent_executor.execute_agent(
                agent_id,
                task,
                &ctx.workspace_id,
                ctx.team_id.as_deref(),
            ),
        )
        .await;

        match result {
            Ok(Ok(response)) => {
                // Send result message back
                let mut result_msg = AgentMessage {
                    id: format!("msg_{}", ulid::Ulid::new()),
                    sender_id: agent_id.to_string(),
                    recipient_id: Some(ctx.agent_id.clone()),
                    team_id: ctx.team_id.clone(),
                    task_id: None,
                    message_type: AgentMessageType::TaskResult,
                    content: response.clone(),
                    file_refs: Vec::new(),
                    correlation_id: Some(ctx.correlation_id.clone()),
                    reply_to: Some(delegation_id),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                };
                if let Some(team_id) = &ctx.team_id {
                    result_msg = result_msg.with_team(team_id);
                }
                let _ = self.mailbox_router.send(result_msg).await;

                tracing::info!(
                    delegator = %ctx.agent_id,
                    delegate = %agent_id,
                    "delegation completed successfully"
                );

                Ok(ToolOutput::Success {
                    result: json!({
                        "completed": true,
                        "agent_id": agent_id,
                        "response": response,
                    }),
                    tokens_used: None,
                })
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    delegator = %ctx.agent_id,
                    delegate = %agent_id,
                    error = %e,
                    "delegation failed"
                );

                Ok(ToolOutput::Error {
                    error: format!("Agent '{}' failed: {}", agent_id, e),
                    retryable: true,
                })
            }
            Err(_) => {
                tracing::warn!(
                    delegator = %ctx.agent_id,
                    delegate = %agent_id,
                    timeout_secs,
                    "delegation timed out"
                );

                Ok(ToolOutput::Error {
                    error: format!(
                        "Agent '{}' did not complete within {}s timeout",
                        agent_id, timeout_secs
                    ),
                    retryable: true,
                })
            }
        }
    }
}
