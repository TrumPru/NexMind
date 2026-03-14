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
    /// The plan generated during the planning phase, if any.
    pub plan: Option<String>,
    /// Reflection notes collected during execution.
    pub reflections: Vec<String>,
    /// Facts automatically extracted from the conversation.
    pub extracted_facts: Vec<String>,
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
    /// Active run cancellation tokens, keyed by run_id.
    active_runs: Arc<std::sync::Mutex<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>>,
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
            active_runs: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
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

    /// Cancel a running agent task by run_id. Returns true if found and cancelled.
    pub fn cancel_run(&self, run_id: &str) -> bool {
        if let Ok(runs) = self.active_runs.lock() {
            if let Some(token) = runs.get(run_id) {
                token.cancel();
                return true;
            }
        }
        false
    }

    /// Run an agent to completion with Think→Plan→Act→Reflect loop.
    pub async fn run(
        &self,
        agent: &AgentDefinition,
        input: &str,
        context: RunContext,
    ) -> Result<AgentRunResult, AgentError> {
        let start = Instant::now();
        let mut total_usage = TokenUsage::default();
        let mut iterations: u32 = 0;
        let mut plan: Option<String> = None;
        let mut reflections: Vec<String> = Vec::new();
        let mut tool_calls_since_reflection: u32 = 0;
        let mut consecutive_errors: u32 = 0;

        // Register cancellation token for this run
        let cancel_token = tokio_util::sync::CancellationToken::new();
        if let Ok(mut runs) = self.active_runs.lock() {
            runs.insert(context.run_id.clone(), cancel_token.clone());
        }

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

            // Load conversation history (with summarization if needed)
            let max_history_tokens = agent.execution_policy.context_summarization_threshold;
            let session_history = memory_store
                .get_session_history(&context.session_id, 50, max_history_tokens)
                .unwrap_or_default();

            if !session_history.is_empty() {
                // If history is large, summarize older messages
                let total_chars: usize = session_history.iter().map(|m| m.content.len()).sum();
                let approx_tokens = (total_chars / 4) as u32;

                if approx_tokens > max_history_tokens && session_history.len() > 6 {
                    // Keep last 4 messages verbatim, summarize the rest
                    let split_point = session_history.len().saturating_sub(4);
                    let old_msgs: Vec<String> = session_history[..split_point]
                        .iter()
                        .map(|m| format!("{}: {}", m.role, &m.content[..m.content.len().min(200)]))
                        .collect();
                    let summary = format!(
                        "[Summary of {} earlier messages: {}]",
                        split_point,
                        old_msgs.join(" | ")
                    );
                    messages.push(ChatMessage::system(&format!(
                        "<conversation_summary>\n{}\n</conversation_summary>",
                        summary
                    )));
                    for msg in &session_history[split_point..] {
                        match msg.role.as_str() {
                            "user" => messages.push(ChatMessage::user(&msg.content)),
                            "assistant" => messages.push(ChatMessage::assistant_text(&msg.content)),
                            _ => {}
                        }
                    }
                } else {
                    for msg in &session_history {
                        match msg.role.as_str() {
                            "user" => messages.push(ChatMessage::user(&msg.content)),
                            "assistant" => messages.push(ChatMessage::assistant_text(&msg.content)),
                            _ => {}
                        }
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

        // ── Planning phase: Think before acting ──────────────────────

        if agent.execution_policy.planning_enabled && tool_defs.is_some() {
            info!(agent_id = %agent.id, "entering planning phase");

            let planning_prompt = format!(
                "Before taking any actions, analyze this request and create a brief plan.\n\
                 Think about:\n\
                 1. What is the user asking for?\n\
                 2. What steps do I need to take?\n\
                 3. Which tools will I need?\n\
                 4. What could go wrong?\n\n\
                 Respond with a concise plan in <plan> tags, then proceed to execute.\n\
                 Example: <plan>1. Search memory for context 2. Use browser to fetch data 3. Summarize results</plan>"
            );

            messages.push(ChatMessage::system(&planning_prompt));

            let plan_req = CompletionRequest {
                model: agent.model.primary.clone(),
                messages: messages.clone(),
                tools: None, // No tools during planning
                temperature: agent.model.temperature,
                max_tokens: 1024,
                stream: false,
            };

            if let Ok((plan_msg, plan_usage)) = self.non_stream_completion(plan_req).await {
                total_usage.input_tokens += plan_usage.input_tokens;
                total_usage.output_tokens += plan_usage.output_tokens;
                total_usage.total_tokens += plan_usage.total_tokens;

                if let Some(plan_text) = plan_msg.text() {
                    // Extract plan from <plan> tags if present
                    let extracted = if let Some(start_idx) = plan_text.find("<plan>") {
                        if let Some(end_idx) = plan_text.find("</plan>") {
                            plan_text[start_idx + 6..end_idx].trim().to_string()
                        } else {
                            plan_text.to_string()
                        }
                    } else {
                        plan_text.to_string()
                    };

                    plan = Some(extracted.clone());
                    info!(agent_id = %agent.id, "plan generated: {}", &extracted[..extracted.len().min(200)]);

                    // Add plan to context for execution
                    messages.push(plan_msg);

                    // Emit planning event
                    self.event_bus.emit(Event::new(
                        EventSource::Agent,
                        EventType::Custom("agent_plan_generated".into()),
                        serde_json::json!({
                            "agent_id": agent.id,
                            "run_id": context.run_id,
                            "plan": &extracted[..extracted.len().min(500)],
                        }),
                        &context.workspace_id,
                        Some(context.correlation_id.clone()),
                    ));
                }
            }
        }

        let mut final_response: Option<String> = None;
        #[allow(unused_assignments)]
        let mut state = AgentState::Executing;

        // ── Execution loop with reflection ───────────────────────────

        loop {
            iterations += 1;

            // Check for cancellation
            if cancel_token.is_cancelled() {
                info!(agent_id = %agent.id, run_id = %context.run_id, "run cancelled");
                state = AgentState::Failed {
                    error: "cancelled by user".into(),
                };
                break;
            }

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

            // ── Reflection checkpoint ────────────────────────────────
            if agent.execution_policy.planning_enabled
                && tool_calls_since_reflection >= agent.execution_policy.reflection_interval
                && tool_calls_since_reflection > 0
            {
                info!(agent_id = %agent.id, "entering reflection phase");
                tool_calls_since_reflection = 0;

                let reflection_prompt = if consecutive_errors > 0 {
                    format!(
                        "<reflection_prompt>\n\
                        {} recent tool call(s) resulted in errors. \
                        Re-evaluate your approach:\n\
                        1. What went wrong?\n\
                        2. Should I try a different approach?\n\
                        3. Is the original plan still valid?\n\
                        Adjust your strategy if needed.\n\
                        </reflection_prompt>",
                        consecutive_errors
                    )
                } else {
                    "<reflection_prompt>\n\
                    Briefly assess your progress:\n\
                    1. Am I making progress toward the goal?\n\
                    2. Should I adjust my approach?\n\
                    Continue with the next step.\n\
                    </reflection_prompt>"
                        .to_string()
                };

                messages.push(ChatMessage::system(&reflection_prompt));

                // Quick reflection call (no tools, short response)
                let refl_req = CompletionRequest {
                    model: agent.model.primary.clone(),
                    messages: messages.clone(),
                    tools: None,
                    temperature: agent.model.temperature,
                    max_tokens: 512,
                    stream: false,
                };

                if let Ok((refl_msg, refl_usage)) = self.non_stream_completion(refl_req).await {
                    total_usage.input_tokens += refl_usage.input_tokens;
                    total_usage.output_tokens += refl_usage.output_tokens;
                    total_usage.total_tokens += refl_usage.total_tokens;

                    if let Some(refl_text) = refl_msg.text() {
                        reflections.push(refl_text.to_string());
                        info!(agent_id = %agent.id, "reflection: {}", &refl_text[..refl_text.len().min(200)]);
                        messages.push(refl_msg);
                    }
                }

                consecutive_errors = 0;
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

                        // Execute tool calls in parallel for better performance
                        let mut tool_results = Vec::with_capacity(tool_calls.len());
                        if tool_calls.len() > 1 {
                            let futs: Vec<_> = tool_calls.iter().map(|tc| {
                                self.execute_tool_call(&tc.name, &tc.arguments, agent, &context)
                            }).collect();
                            tool_results = futures::future::join_all(futs).await;
                        } else {
                            for tc in tool_calls.iter() {
                                let r = self.execute_tool_call(&tc.name, &tc.arguments, agent, &context).await;
                                tool_results.push(r);
                            }
                        }

                        for (tc, tool_result_str) in tool_calls.iter().zip(tool_results) {
                            // Track errors for reflection
                            if tool_result_str.starts_with("Error:") {
                                consecutive_errors += 1;
                            } else {
                                consecutive_errors = 0;
                            }

                            tool_calls_since_reflection += 1;
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

        let mut extracted_facts: Vec<String> = Vec::new();

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

            // ── Auto-extract facts from conversation ─────────────────
            if agent.execution_policy.auto_extract_facts && state == AgentState::Completed {
                extracted_facts = self
                    .extract_facts(input, final_response.as_deref(), agent, &context, memory_store)
                    .await;
            }

            // ── Store conversation summary for long conversations ────
            if state == AgentState::Completed {
                if let Some(ref response) = final_response {
                    let summary = format!(
                        "User asked: {} | Agent responded: {}",
                        &input[..input.len().min(200)],
                        &response[..response.len().min(300)]
                    );
                    let _ = memory_store
                        .store(nexmind_memory::NewMemory {
                            workspace_id: context.workspace_id.clone(),
                            agent_id: Some(agent.id.clone()),
                            memory_type: nexmind_memory::MemoryType::Semantic,
                            content: summary,
                            source: nexmind_memory::MemorySource::System,
                            source_task_id: Some(context.run_id.clone()),
                            access_policy: nexmind_memory::AccessPolicy::Workspace,
                            metadata: Some(serde_json::json!({
                                "type": "conversation_summary",
                                "session_id": context.session_id,
                            })),
                            importance: Some(0.4),
                        })
                        .await;
                }
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
                "has_plan": plan.is_some(),
                "reflections_count": reflections.len(),
                "facts_extracted": extracted_facts.len(),
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
            has_plan = plan.is_some(),
            reflections = reflections.len(),
            facts = extracted_facts.len(),
            "agent run completed"
        );

        // Clean up cancellation token
        if let Ok(mut runs) = self.active_runs.lock() {
            runs.remove(&context.run_id);
        }

        Ok(AgentRunResult {
            run_id: context.run_id,
            status: state,
            response: final_response,
            tokens_used: total_usage,
            iterations,
            duration_ms,
            plan,
            reflections,
            extracted_facts,
        })
    }

    // ── Fact extraction ────────────────────────────────────────────────

    async fn extract_facts(
        &self,
        input: &str,
        response: Option<&str>,
        agent: &AgentDefinition,
        context: &RunContext,
        memory_store: &MemoryStoreImpl,
    ) -> Vec<String> {
        let response_text = response.unwrap_or("");
        if input.len() + response_text.len() < 50 {
            return Vec::new();
        }

        let extraction_prompt = format!(
            "Extract key facts from this conversation that should be remembered for future interactions.\n\
             Focus on: user preferences, personal information, project details, technical decisions, and important context.\n\
             Return each fact on a separate line, prefixed with '- '. Return ONLY facts, no commentary.\n\
             If there are no important facts to remember, respond with 'NONE'.\n\n\
             User: {}\n\
             Assistant: {}",
            &input[..input.len().min(500)],
            &response_text[..response_text.len().min(500)]
        );

        let req = CompletionRequest {
            model: agent.model.primary.clone(),
            messages: vec![
                ChatMessage::system("You extract key facts from conversations. Be concise."),
                ChatMessage::user(&extraction_prompt),
            ],
            tools: None,
            temperature: 0.3,
            max_tokens: 512,
            stream: false,
        };

        let facts = match self.non_stream_completion(req).await {
            Ok((msg, _usage)) => {
                if let Some(text) = msg.text() {
                    if text.contains("NONE") {
                        return Vec::new();
                    }
                    text.lines()
                        .filter(|line| line.starts_with("- ") || line.starts_with("* "))
                        .map(|line| line.trim_start_matches("- ").trim_start_matches("* ").trim().to_string())
                        .filter(|fact| fact.len() >= 10)
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                }
            }
            Err(_) => Vec::new(),
        };

        // Store extracted facts as semantic memories
        for fact in &facts {
            let _ = memory_store
                .store(nexmind_memory::NewMemory {
                    workspace_id: context.workspace_id.clone(),
                    agent_id: Some(agent.id.clone()),
                    memory_type: nexmind_memory::MemoryType::Semantic,
                    content: fact.clone(),
                    source: nexmind_memory::MemorySource::Agent,
                    source_task_id: Some(context.run_id.clone()),
                    access_policy: nexmind_memory::AccessPolicy::Workspace,
                    metadata: Some(serde_json::json!({"type": "auto_extracted_fact"})),
                    importance: Some(0.6),
                })
                .await;
        }

        if !facts.is_empty() {
            info!(
                agent_id = %agent.id,
                count = facts.len(),
                "auto-extracted facts stored"
            );
        }

        facts
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
