use std::sync::Arc;

use tracing::{debug, info};

use nexmind_agent_engine::{
    AgentDefinition, AgentError, AgentRunResult, BudgetPolicy, ExecutionPolicy, MemoryPolicy,
    ModelConfig, TrustLevel,
};
use nexmind_model_router::TokenUsage;

use crate::config::OpenClawConfig;
use crate::gateway::{GatewayClient, SessionSendRequest, SpawnRequest};
use crate::OpenClawError;

/// OpenClaw external agent — delegates tasks to an OpenClaw Gateway instance.
///
/// This allows NexMind to use OpenClaw as a powerful sub-agent:
/// - Full tool access (files, shell, web, memory, skills)
/// - Multi-model support via OpenClaw's model router
/// - Persistent memory across sessions
/// - Access to OpenClaw's skill ecosystem
///
/// ## Usage
///
/// ```rust,ignore
/// let config = OpenClawConfig::local();
/// let agent = OpenClawAgent::new(config);
///
/// // Simple request/response
/// let response = agent.run("What's the weather in SF?").await?;
///
/// // Spawn isolated task
/// let result = agent.delegate_task("Analyze this codebase and create a summary").await?;
/// ```
pub struct OpenClawAgent {
    client: GatewayClient,
    config: OpenClawConfig,
    session_label: String,
}

impl OpenClawAgent {
    /// Create a new OpenClaw agent with the given config.
    pub fn new(config: OpenClawConfig) -> Self {
        let client = GatewayClient::new(config.clone());
        Self {
            client,
            config,
            session_label: "nexmind-openclaw".into(),
        }
    }

    /// Create with a custom session label (for identifying this NexMind instance).
    pub fn with_label(mut self, label: &str) -> Self {
        self.session_label = label.into();
        self
    }

    /// Send a message to OpenClaw and get a response.
    ///
    /// Uses OpenClaw's main agent session. Good for conversational
    /// interactions where context should be maintained.
    pub async fn run(&self, input: &str) -> Result<String, OpenClawError> {
        info!(input_len = input.len(), "sending to OpenClaw agent");

        let request = SessionSendRequest {
            message: input.into(),
            session_key: None,
            label: Some(self.session_label.clone()),
            agent_id: self.config.default_agent.clone(),
            timeout_seconds: Some(self.config.timeout_secs),
        };

        let response = self.client.send_message(request).await?;

        match response.reply {
            Some(reply) => {
                debug!(reply_len = reply.len(), "OpenClaw response received");
                Ok(reply)
            }
            None => Err(OpenClawError::AgentError(
                "OpenClaw returned no response".into(),
            )),
        }
    }

    /// Delegate a task to an isolated OpenClaw session.
    ///
    /// Spawns a new isolated session that runs the task to completion.
    /// Good for complex, long-running tasks that shouldn't interfere
    /// with the main conversation.
    pub async fn delegate_task(&self, task: &str) -> Result<String, OpenClawError> {
        self.delegate_task_with_options(task, None, None).await
    }

    /// Delegate a task with specific model and timeout options.
    pub async fn delegate_task_with_options(
        &self,
        task: &str,
        model: Option<&str>,
        timeout_secs: Option<u64>,
    ) -> Result<String, OpenClawError> {
        info!(task_len = task.len(), model = ?model, "delegating task to OpenClaw");

        let request = SpawnRequest {
            task: task.into(),
            agent_id: self.config.default_agent.clone(),
            model: model.map(|m| m.into()),
            label: Some(format!("{}-task", self.session_label)),
            mode: Some("run".into()),
            timeout_seconds: timeout_secs.or(Some(self.config.timeout_secs)),
        };

        let response = self.client.spawn_session(request).await?;

        match response.result {
            Some(result) => {
                debug!(result_len = result.len(), "OpenClaw task completed");
                Ok(result)
            }
            None => {
                if let Some(err) = response.error {
                    Err(OpenClawError::AgentError(err))
                } else {
                    // Session spawned but result not yet available
                    // Return session info for polling
                    Ok(format!(
                        "Task spawned. Session: {}",
                        response
                            .session_key
                            .unwrap_or_else(|| "unknown".into())
                    ))
                }
            }
        }
    }

    /// Check if the OpenClaw gateway is available.
    pub async fn is_available(&self) -> bool {
        self.client.is_reachable().await
    }

    /// Get the health status of the connected OpenClaw instance.
    pub async fn health(&self) -> Result<crate::gateway::GatewayHealth, OpenClawError> {
        self.client.health_check().await
    }

    /// Generate a NexMind AgentDefinition for registering OpenClaw as an agent.
    ///
    /// This creates an agent definition that, when selected by NexMind,
    /// routes messages through the OpenClaw gateway instead of a local LLM.
    pub fn as_agent_definition(workspace_id: &str) -> AgentDefinition {
        AgentDefinition {
            id: "agt_openclaw".into(),
            name: "OpenClaw Agent".into(),
            version: 1,
            description: Some(
                "External AI agent powered by OpenClaw. Has access to tools, skills, \
                 persistent memory, and multi-model routing. Delegates complex tasks \
                 to a running OpenClaw instance."
                    .into(),
            ),
            system_prompt: "You are a bridge to an OpenClaw agent instance. \
                Forward user messages to OpenClaw and relay responses back. \
                OpenClaw has full tool access including file operations, shell commands, \
                web browsing, memory, and skills."
                .into(),
            model: ModelConfig {
                primary: "openclaw/default".into(),
                fallback: None,
                temperature: 0.7,
                max_tokens: 8192,
                streaming: false,
            },
            tools: vec![
                "openclaw_send".into(),
                "openclaw_delegate".into(),
                "openclaw_status".into(),
            ],
            memory_policy: MemoryPolicy {
                session: true,
                semantic: false,
                max_context_tokens: 2000,
            },
            execution_policy: ExecutionPolicy {
                max_iterations: 5,
                max_tool_calls_per_iteration: 3,
                timeout_seconds: 300,
                ..Default::default()
            },
            budget: BudgetPolicy {
                max_tokens_per_run: 50_000,
                max_cost_per_run_usd: 0.5,
                max_cost_per_day_usd: 5.0,
            },
            trust_level: TrustLevel::Elevated,
            permissions: vec![
                "network:outbound".into(),
                "openclaw:send".into(),
                "openclaw:delegate".into(),
                "openclaw:status".into(),
            ],
            schedule: None,
            tags: vec![
                "external".into(),
                "openclaw".into(),
                "agent".into(),
            ],
            workspace_id: workspace_id.into(),
        }
    }
}

/// Adapter to make OpenClawAgent work with NexMind's AgentRuntime.
///
/// This wraps the OpenClaw agent so it can be called from the standard
/// agent execution pipeline — the agent runtime calls `execute()`,
/// which forwards to OpenClaw and returns the result.
pub struct OpenClawExecutor {
    agent: Arc<OpenClawAgent>,
}

impl OpenClawExecutor {
    pub fn new(agent: Arc<OpenClawAgent>) -> Self {
        Self { agent }
    }

    /// Execute a task via OpenClaw, returning a result compatible with AgentRuntime.
    pub async fn execute(
        &self,
        input: &str,
        run_id: &str,
    ) -> Result<AgentRunResult, AgentError> {
        let start = std::time::Instant::now();

        let response = self
            .agent
            .run(input)
            .await
            .map_err(|e| AgentError::ExecutionError(format!("OpenClaw error: {}", e)))?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(AgentRunResult {
            run_id: run_id.to_string(),
            status: nexmind_agent_engine::AgentState::Completed,
            response: Some(response),
            tokens_used: TokenUsage {
                input_tokens: 0,  // OpenClaw tracks its own usage
                output_tokens: 0,
                total_tokens: 0,
            },
            iterations: 1,
            duration_ms,
        })
    }

    /// Delegate a complex task to an isolated OpenClaw session.
    pub async fn delegate(
        &self,
        task: &str,
        run_id: &str,
        model: Option<&str>,
    ) -> Result<AgentRunResult, AgentError> {
        let start = std::time::Instant::now();

        let response = self
            .agent
            .delegate_task_with_options(task, model, None)
            .await
            .map_err(|e| AgentError::ExecutionError(format!("OpenClaw delegation error: {}", e)))?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(AgentRunResult {
            run_id: run_id.to_string(),
            status: nexmind_agent_engine::AgentState::Completed,
            response: Some(response),
            tokens_used: TokenUsage::default(),
            iterations: 1,
            duration_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_definition() {
        let def = OpenClawAgent::as_agent_definition("test_workspace");
        assert_eq!(def.id, "agt_openclaw");
        assert_eq!(def.name, "OpenClaw Agent");
        assert!(def.tags.contains(&"openclaw".to_string()));
        assert!(def.tags.contains(&"external".to_string()));
        assert_eq!(def.model.primary, "openclaw/default");
        assert!(def.description.unwrap().contains("OpenClaw"));
    }

    #[test]
    fn test_agent_with_label() {
        let config = OpenClawConfig::default();
        let agent = OpenClawAgent::new(config).with_label("my-nexmind");
        assert_eq!(agent.session_label, "my-nexmind");
    }

    #[test]
    fn test_default_session_label() {
        let config = OpenClawConfig::default();
        let agent = OpenClawAgent::new(config);
        assert_eq!(agent.session_label, "nexmind-openclaw");
    }
}
