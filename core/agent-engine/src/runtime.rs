use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use tracing::{error, info, warn};
use ulid::Ulid;

use nexmind_event_bus::{Event, EventSource, EventType};
use nexmind_memory::{
    MemoryQuery, MemoryStoreImpl, MemoryType, NewSessionMessage,
};
use nexmind_model_router::{
    ChatMessage, CompletionRequest, ModelRouter, StreamChunk, TokenUsage,
};
use nexmind_tool_runtime::{ToolContext, ToolOutput, ToolRegistry};

use crate::definition::AgentDefinition;
use crate::state::AgentState;
use crate::AgentError;

/// Context for a single agent run.
#[derive(Debug, Clone)]
pub struct RunContext {
    pub workspace_id: String,
    pub run_id: String,
    pub correlation_id: String,
    pub session_id: String,
    pub workspace_path: PathBuf,
}

impl RunContext {
    pub fn new(workspace_id: &str) -> Self {
        let run_id = Ulid::new().to_string();
        Self {
            workspace_id: workspace_id.to_string(),
            run_id: run_id.clone(),
            correlation_id: run_id.clone(),
            session_id: run_id,
            workspace_path: PathBuf::from("./data/workspace"),
        }
    }

    pub fn with_session(mut self, session_id: &str) -> Self {
        self.session_id = session_id.to_string();
        self
    }

    pub fn with_workspace_path(mut self, path: PathBuf) -> Self {
        self.workspace_path = path;
        self
    }
}

/// Result of an agent run.
#[derive(Debug, Clone)]
pub struct AgentRunResult {
    pub run_id: String,
    pub status: AgentState,
    pub response: Option<String>,
    pub tokens_used: TokenUsage,
    pub iterations: u32,
    pub duration_ms: u64,
}

/// Agent runtime — executes agents using the model router, memory, and tools.
pub struct AgentRuntime {
    model_router: Arc<ModelRouter>,
    event_bus: Arc<nexmind_event_bus::EventBus>,
    db: Arc<nexmind_storage::Database>,
    memory_store: Option<Arc<MemoryStoreImpl>>,
    tool_registry: Option<Arc<ToolRegistry>>,
    approval_manager: Option<Arc<crate::approval::ApprovalManager>>,
    cost_tracker: Option<Arc<crate::cost::CostTracker>>,
}

impl AgentRuntime {
    pub fn new(
        model_router: Arc<ModelRouter>,
        event_bus: Arc<nexmind_event_bus::EventBus>,
        db: Arc<nexmind_storage::Database>,
    ) -> Self {
        Self {
            model_router,
            event_bus,
            db,
            memory_store: None,
            tool_registry: None,
            approval_manager: None,
            cost_tracker: None,
        }
    }

    pub fn with_memory(mut self, memory_store: Arc<MemoryStoreImpl>) -> Self {
        self.memory_store = Some(memory_store);
        self
    }

    pub fn with_tools(mut self, tool_registry: Arc<ToolRegistry>) -> Self {
        self.tool_registry = Some(tool_registry);
        self
    }

    pub fn with_approvals(mut self, approval_manager: Arc<crate::approval::ApprovalManager>) -> Self {
        self.approval_manager = Some(approval_manager);
        self
    }

    pub fn with_cost_tracker(mut self, cost_tracker: Arc<crate::cost::CostTracker>) -> Self {
        self.cost_tracker = Some(cost_tracker);
        self
    }

    /// Run an agent to completion.
    pub async fn run(
        &self,
        agent: &AgentDefinition,
        input: &str,
        context: RunContext,
    ) -> Result<AgentRunResult, AgentError> {
        let start = Instant::now();
        let mut total_usage = TokenUsage::default();
        let mut iterations: u32 = 0;

        info!(
            agent_id = %agent.id,
            run_id = %context.run_id,
            "agent run starting"
        );

        // Emit AgentStarted event
        self.event_bus.emit(Event::new(
            EventSource::Agent,
            EventType::AgentStarted,
            serde_json::json!({
                "agent_id": agent.id,
                "run_id": context.run_id,
                "input_preview": &input[..input.len().min(100)],
            }),
            &context.workspace_id,
            Some(context.correlation_id.clone()),
        ));

        // Record run in database
        self.record_run_start(&context, &agent.id).ok();

        // ── Build initial messages with memory context ───────────────

        let mut messages: Vec<ChatMessage> = Vec::new();
        messages.push(ChatMessage::system(&agent.system_prompt));

        // Retrieve relevant memories
        if let Some(ref memory_store) = self.memory_store {
            let memory_block = self
                .build_memory_context(memory_store, input, &context, agent)
                .await;
            if !memory_block.is_empty() {
                messages.push(ChatMessage::system(&format!(
                    "<memory>\n{}\n</memory>",
                    memory_block
                )));
            }

            // Load conversation history
            let session_history = memory_store
                .get_session_history(&context.session_id, 50, 8000)
                .unwrap_or_default();

            if !session_history.is_empty() {
                for msg in &session_history {
                    match msg.role.as_str() {
                        "user" => messages.push(ChatMessage::user(&msg.content)),
                        "assistant" => messages.push(ChatMessage::assistant_text(&msg.content)),
                        _ => {}
                    }
                }
            }
        }

        messages.push(ChatMessage::user(input));

        // ── Build tool definitions for LLM ───────────────────────────

        let tool_defs: Option<Vec<nexmind_model_router::ToolDefinition>> =
            if let Some(ref tool_registry) = self.tool_registry {
                let available = tool_registry.get_available_tools(&agent.permissions);
                if available.is_empty() {
                    None
                } else {
                    Some(
                        available
                            .into_iter()
                            .map(|td| nexmind_model_router::ToolDefinition {
                                name: td.name,
                                description: td.description,
                                input_schema: td.parameters,
                            })
                            .collect(),
                    )
                }
            } else {
                None
            };

        let mut final_response: Option<String> = None;
        #[allow(unused_assignments)]
        let mut state = AgentState::Executing;

        // ── Iteration loop ───────────────────────────────────────────

        loop {
            iterations += 1;

            if iterations > agent.execution_policy.max_iterations {
                warn!(
                    agent_id = %agent.id,
                    iterations,
                    "max iterations reached"
                );
                state = AgentState::Failed {
                    error: "max iterations reached".into(),
                };
                break;
            }

            // Budget check
            if total_usage.total_tokens as u64 > agent.budget.max_tokens_per_run {
                warn!(
                    agent_id = %agent.id,
                    tokens = total_usage.total_tokens,
                    "budget exceeded"
                );
                state = AgentState::Failed {
                    error: "token budget exceeded".into(),
                };
                break;
            }

            // Emit LlmCallStarted
            self.event_bus.emit(Event::new(
                EventSource::Agent,
                EventType::LlmCallStarted,
                serde_json::json!({
                    "agent_id": agent.id,
                    "run_id": context.run_id,
                    "iteration": iterations,
                    "model": agent.model.primary,
                }),
                &context.workspace_id,
                Some(context.correlation_id.clone()),
            ));

            // Call model router
            let req = CompletionRequest {
                model: agent.model.primary.clone(),
                messages: messages.clone(),
                tools: tool_defs.clone(),
                temperature: agent.model.temperature,
                max_tokens: agent.model.max_tokens,
                stream: agent.model.streaming,
            };

            let result = if agent.model.streaming {
                self.stream_completion(req, &context).await
            } else {
                self.non_stream_completion(req).await
            };

            match result {
                Ok((response_msg, usage)) => {
                    // Accumulate usage
                    total_usage.input_tokens += usage.input_tokens;
                    total_usage.output_tokens += usage.output_tokens;
                    total_usage.total_tokens += usage.total_tokens;

                    // Emit LlmCallCompleted
                    self.event_bus.emit(Event::new(
                        EventSource::Agent,
                        EventType::LlmCallCompleted,
                        serde_json::json!({
                            "agent_id": agent.id,
                            "run_id": context.run_id,
                            "iteration": iterations,
                            "usage": {
                                "input_tokens": usage.input_tokens,
                                "output_tokens": usage.output_tokens,
                            },
                        }),
                        &context.workspace_id,
                        Some(context.correlation_id.clone()),
                    ));

                    // Record cost
                    if let Some(ref cost_tracker) = self.cost_tracker {
                        let cost_micro = cost_tracker.price_table().calculate_cost_microdollars(
                            &agent.model.primary,
                            usage.input_tokens,
                            usage.output_tokens,
                        );
                        let _ = cost_tracker.record(crate::cost::CostRecord {
                            workspace_id: context.workspace_id.clone(),
                            agent_id: agent.id.clone(),
                            run_id: context.run_id.clone(),
                            model: agent.model.primary.clone(),
                            provider: agent.model.primary.split('/').next().unwrap_or("unknown").to_string(),
                            input_tokens: usage.input_tokens,
                            output_tokens: usage.output_tokens,
                            cost_microdollars: cost_micro,
                        });

                        // Budget enforcement
                        if let Ok(crate::cost::BudgetStatus::Exceeded) =
                            cost_tracker.check_budget(&agent.id, &agent.budget)
                        {
                            warn!(
                                agent_id = %agent.id,
                                "daily budget exceeded, stopping agent"
                            );
                            state = AgentState::Failed {
                                error: "daily cost budget exceeded".into(),
                            };
                            break;
                        }
                    }

                    // Check response type
                    if let Some(tool_calls) = response_msg.tool_calls() {
                        info!(
                            agent_id = %agent.id,
                            num_calls = tool_calls.len(),
                            "tool calls received"
                        );

                        messages.push(response_msg.clone());

                        // Execute tool calls for real
                        for tc in tool_calls {
                            let tool_result_str =
                                self.execute_tool_call(&tc.name, &tc.arguments, agent, &context).await;
                            messages.push(ChatMessage::tool_result(&tc.id, &tool_result_str));
                        }

                        continue;
                    }

                    if let Some(text) = response_msg.text() {
                        final_response = Some(text.to_string());
                        messages.push(response_msg);
                        state = AgentState::Completed;
                        break;
                    }

                    // Unexpected response
                    final_response = Some(String::new());
                    state = AgentState::Completed;
                    break;
                }
                Err(e) => {
                    error!(
                        agent_id = %agent.id,
                        error = %e,
                        "LLM call failed"
                    );

                    // Try fallback
                    if let Some(fallback) = &agent.model.fallback {
                        warn!(
                            agent_id = %agent.id,
                            fallback = %fallback,
                            "trying fallback model"
                        );

                        let fallback_req = CompletionRequest {
                            model: fallback.clone(),
                            messages: messages.clone(),
                            tools: tool_defs.clone(),
                            temperature: agent.model.temperature,
                            max_tokens: agent.model.max_tokens,
                            stream: false,
                        };

                        match self.non_stream_completion(fallback_req).await {
                            Ok((response_msg, usage)) => {
                                total_usage.input_tokens += usage.input_tokens;
                                total_usage.output_tokens += usage.output_tokens;
                                total_usage.total_tokens += usage.total_tokens;

                                if let Some(text) = response_msg.text() {
                                    final_response = Some(text.to_string());
                                }
                                state = AgentState::Completed;
                                break;
                            }
                            Err(fallback_err) => {
                                state = AgentState::Failed {
                                    error: format!(
                                        "primary: {}; fallback: {}",
                                        e, fallback_err
                                    ),
                                };
                                break;
                            }
                        }
                    }

                    state = AgentState::Failed {
                        error: e.to_string(),
                    };
                    break;
                }
            }
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        // ── Post-run: store session messages ─────────────────────────

        if let Some(ref memory_store) = self.memory_store {
            // Store user message
            let _ = memory_store.store_session_message(NewSessionMessage {
                workspace_id: context.workspace_id.clone(),
                session_id: context.session_id.clone(),
                agent_id: agent.id.clone(),
                role: "user".into(),
                content: input.into(),
                tool_calls: None,
                tool_call_id: None,
                tokens: None,
            });

            // Store assistant response
            if let Some(ref response) = final_response {
                let _ = memory_store.store_session_message(NewSessionMessage {
                    workspace_id: context.workspace_id.clone(),
                    session_id: context.session_id.clone(),
                    agent_id: agent.id.clone(),
                    role: "assistant".into(),
                    content: response.clone(),
                    tool_calls: None,
                    tool_call_id: None,
                    tokens: None,
                });
            }
        }

        // Emit completion/failure event
        let event_type = match &state {
            AgentState::Completed => EventType::AgentCompleted,
            _ => EventType::AgentFailed,
        };

        self.event_bus.emit(Event::new(
            EventSource::Agent,
            event_type,
            serde_json::json!({
                "agent_id": agent.id,
                "run_id": context.run_id,
                "iterations": iterations,
                "tokens_used": total_usage.total_tokens,
                "duration_ms": duration_ms,
            }),
            &context.workspace_id,
            Some(context.correlation_id.clone()),
        ));

        // Emit CostRecorded
        self.event_bus.emit(Event::new(
            EventSource::System,
            EventType::CostRecorded,
            serde_json::json!({
                "agent_id": agent.id,
                "model": agent.model.primary,
                "input_tokens": total_usage.input_tokens,
                "output_tokens": total_usage.output_tokens,
            }),
            &context.workspace_id,
            Some(context.correlation_id.clone()),
        ));

        // Update run record
        self.record_run_end(&context, &state).ok();

        info!(
            agent_id = %agent.id,
            run_id = %context.run_id,
            status = %state,
            iterations,
            duration_ms,
            tokens = total_usage.total_tokens,
            "agent run completed"
        );

        Ok(AgentRunResult {
            run_id: context.run_id,
            status: state,
            response: final_response,
            tokens_used: total_usage,
            iterations,
            duration_ms,
        })
    }

    // ── Memory context building ──────────────────────────────────────

    async fn build_memory_context(
        &self,
        memory_store: &MemoryStoreImpl,
        input: &str,
        context: &RunContext,
        agent: &AgentDefinition,
    ) -> String {
        let result = memory_store
            .retrieve_full(MemoryQuery {
                query_text: input.to_string(),
                workspace_id: context.workspace_id.clone(),
                agent_id: Some(agent.id.clone()),
                memory_types: vec![MemoryType::Semantic, MemoryType::Pinned],
                top_k: 20,
                min_importance: Some(0.3),
            })
            .await;

        match result {
            Ok(retrieval) => {
                let mut lines = Vec::new();
                let mut budget: u32 = agent.memory_policy.max_context_tokens;
                for scored in &retrieval.memories {
                    let tokens = (scored.memory.content.len() / 4) as u32;
                    if tokens > budget {
                        break;
                    }
                    budget -= tokens;
                    lines.push(format!(
                        "[{} | importance: {:.1}] {}",
                        scored.memory.memory_type.as_str(),
                        scored.memory.importance,
                        scored.memory.content
                    ));
                }
                lines.join("\n")
            }
            Err(e) => {
                warn!(error = %e, "failed to retrieve memories");
                String::new()
            }
        }
    }

    // ── Tool execution ───────────────────────────────────────────────

    async fn execute_tool_call(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
        agent: &AgentDefinition,
        context: &RunContext,
    ) -> String {
        let Some(ref tool_registry) = self.tool_registry else {
            return format!("Error: tool '{}' is not available (no tool runtime configured)", tool_name);
        };

        let tool_ctx = ToolContext {
            agent_id: agent.id.clone(),
            workspace_id: context.workspace_id.clone(),
            workspace_path: context.workspace_path.clone(),
            granted_permissions: agent.permissions.clone(),
            correlation_id: context.correlation_id.clone(),
        };

        match tool_registry
            .execute(tool_name, arguments.clone(), &tool_ctx)
            .await
        {
            Ok(ToolOutput::Success { result, .. }) => {
                info!(tool = %tool_name, "tool executed successfully");
                serde_json::to_string(&result).unwrap_or_else(|_| result.to_string())
            }
            Ok(ToolOutput::NeedsApproval { tool_id, tool_args, reason }) => {
                // If we have an approval manager, create an approval request and wait
                if let Some(ref approval_mgr) = self.approval_manager {
                    info!(tool = %tool_name, "requesting approval for tool execution");

                    let req = crate::approval::ApprovalRequest {
                        workspace_id: context.workspace_id.clone(),
                        requester_agent_id: agent.id.clone(),
                        requester_run_id: context.run_id.clone(),
                        tool_id: tool_id.clone(),
                        tool_args: tool_args.clone(),
                        action_description: format!("Execute tool '{}': {}", tool_name, reason),
                        risk_level: crate::approval::RiskLevel::Medium,
                        context_summary: None,
                        expires_in: std::time::Duration::from_secs(agent.execution_policy.timeout_seconds),
                    };

                    match approval_mgr.request_approval(req) {
                        Ok(approval_id) => {
                            let timeout = std::time::Duration::from_secs(
                                agent.execution_policy.timeout_seconds,
                            );
                            match approval_mgr.wait_for_decision(&approval_id, timeout).await {
                                Ok(crate::approval::ApprovalDecision::Approved) => {
                                    info!(tool = %tool_name, "approval granted, executing tool");
                                    match tool_registry
                                        .execute_approved(tool_name, tool_args, &tool_ctx)
                                        .await
                                    {
                                        Ok(ToolOutput::Success { result, .. }) => {
                                            serde_json::to_string(&result)
                                                .unwrap_or_else(|_| result.to_string())
                                        }
                                        Ok(ToolOutput::Error { error, .. }) => {
                                            format!("Error: {}", error)
                                        }
                                        Ok(_) => "Error: unexpected tool output".into(),
                                        Err(e) => format!("Error: {}", e),
                                    }
                                }
                                Ok(crate::approval::ApprovalDecision::Denied { reason }) => {
                                    let msg = reason.unwrap_or_else(|| "no reason given".into());
                                    warn!(tool = %tool_name, reason = %msg, "approval denied");
                                    format!("Tool '{}' was denied by user: {}", tool_name, msg)
                                }
                                Ok(crate::approval::ApprovalDecision::Expired) => {
                                    warn!(tool = %tool_name, "approval expired");
                                    format!("Tool '{}' approval request expired", tool_name)
                                }
                                Ok(crate::approval::ApprovalDecision::Pending) => {
                                    format!("Tool '{}' approval is still pending", tool_name)
                                }
                                Err(e) => {
                                    error!(tool = %tool_name, error = %e, "approval wait failed");
                                    format!("Error waiting for approval: {}", e)
                                }
                            }
                        }
                        Err(e) => {
                            error!(tool = %tool_name, error = %e, "failed to create approval request");
                            format!("Error: could not request approval: {}", e)
                        }
                    }
                } else {
                    warn!(tool = %tool_name, "tool needs approval but no approval manager configured");
                    format!("This tool requires user approval: {}", reason)
                }
            }
            Ok(ToolOutput::Error { error, .. }) => {
                warn!(tool = %tool_name, error = %error, "tool execution error");
                format!("Error: {}", error)
            }
            Err(e) => {
                error!(tool = %tool_name, error = %e, "tool execution failed");
                format!("Error: {}", e)
            }
        }
    }

    // ── Streaming / non-streaming completion ─────────────────────────

    async fn stream_completion(
        &self,
        req: CompletionRequest,
        _context: &RunContext,
    ) -> Result<(ChatMessage, TokenUsage), AgentError> {
        let mut stream = self
            .model_router
            .stream(req)
            .await
            .map_err(|e| AgentError::ModelError(e.to_string()))?;

        let mut text_buffer = String::new();
        let mut tool_calls: Vec<nexmind_model_router::ToolCall> = Vec::new();
        let mut current_tool_id: Option<String> = None;
        let mut current_tool_name: Option<String> = None;
        let mut current_tool_args = String::new();
        let mut usage = TokenUsage::default();

        while let Some(chunk) = stream.next().await {
            match chunk {
                StreamChunk::TextDelta(text) => {
                    text_buffer.push_str(&text);
                }
                StreamChunk::ToolCallStart { id, name } => {
                    if let Some(prev_id) = current_tool_id.take() {
                        tool_calls.push(nexmind_model_router::ToolCall {
                            id: prev_id,
                            name: current_tool_name.take().unwrap_or_default(),
                            arguments: serde_json::from_str(&current_tool_args)
                                .unwrap_or(serde_json::Value::Null),
                        });
                        current_tool_args.clear();
                    }
                    current_tool_id = Some(id);
                    current_tool_name = Some(name);
                }
                StreamChunk::ToolCallArgumentsDelta { delta, .. } => {
                    current_tool_args.push_str(&delta);
                }
                StreamChunk::ToolCallEnd { .. } => {
                    if let Some(id) = current_tool_id.take() {
                        tool_calls.push(nexmind_model_router::ToolCall {
                            id,
                            name: current_tool_name.take().unwrap_or_default(),
                            arguments: serde_json::from_str(&current_tool_args)
                                .unwrap_or(serde_json::Value::Null),
                        });
                        current_tool_args.clear();
                    }
                }
                StreamChunk::Usage(u) => {
                    usage.input_tokens = usage.input_tokens.max(u.input_tokens);
                    usage.output_tokens = usage.output_tokens.max(u.output_tokens);
                    usage.total_tokens = usage.input_tokens + usage.output_tokens;
                }
                StreamChunk::Done => break,
                StreamChunk::Error(e) => {
                    return Err(AgentError::ModelError(e));
                }
            }
        }

        if let Some(id) = current_tool_id.take() {
            tool_calls.push(nexmind_model_router::ToolCall {
                id,
                name: current_tool_name.take().unwrap_or_default(),
                arguments: serde_json::from_str(&current_tool_args)
                    .unwrap_or(serde_json::Value::Null),
            });
        }

        let message = if !tool_calls.is_empty() {
            ChatMessage::assistant_tool_calls(tool_calls)
        } else {
            ChatMessage::assistant_text(&text_buffer)
        };

        Ok((message, usage))
    }

    async fn non_stream_completion(
        &self,
        req: CompletionRequest,
    ) -> Result<(ChatMessage, TokenUsage), AgentError> {
        let resp = self
            .model_router
            .complete(req)
            .await
            .map_err(|e| AgentError::ModelError(e.to_string()))?;

        Ok((resp.message, resp.usage))
    }

    fn record_run_start(&self, context: &RunContext, agent_id: &str) -> Result<(), AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| AgentError::StorageError(e.to_string()))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO agent_runs (id, agent_id, status, state_snapshot, started_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            rusqlite::params![context.run_id, agent_id, "executing", "{}", now],
        )
        .map_err(|e| AgentError::StorageError(e.to_string()))?;
        Ok(())
    }

    fn record_run_end(&self, context: &RunContext, state: &AgentState) -> Result<(), AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| AgentError::StorageError(e.to_string()))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE agent_runs SET status = ?1, updated_at = ?2, completed_at = ?2 WHERE id = ?3",
            rusqlite::params![state.name(), now, context.run_id],
        )
        .map_err(|e| AgentError::StorageError(e.to_string()))?;
        Ok(())
    }
}
