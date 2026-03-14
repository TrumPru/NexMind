//! Shared slash-command handling for HTTP API and Telegram connector.

use std::sync::Arc;
use std::time::Instant;

use nexmind_agent_engine::AgentRegistry;
use nexmind_memory::MemoryStoreImpl;
use nexmind_model_router::ModelRouter;

/// Shared context for command execution.
pub struct CommandContext {
    pub model_router: Arc<ModelRouter>,
    pub agent_registry: Arc<AgentRegistry>,
    pub memory_store: Arc<MemoryStoreImpl>,
    pub session_id: Arc<std::sync::Mutex<String>>,
    pub current_agent_id: Arc<std::sync::Mutex<String>>,
    pub start_time: Instant,
}

/// Result of slash-command processing.
pub enum CommandResult {
    /// Command was recognized and produced a response.
    Response(String),
    /// Input is not a command — proceed with normal agent processing.
    NotACommand,
}

/// Parse and execute a slash command. Returns `CommandResult::NotACommand` if
/// the input doesn't start with `/` or is an unknown command (fall through to agent).
pub async fn handle_command(input: &str, ctx: &CommandContext) -> CommandResult {
    let input = input.trim();
    if !input.starts_with('/') {
        return CommandResult::NotACommand;
    }

    let mut parts = input.splitn(3, ' ');
    let cmd = parts.next().unwrap_or("");
    let arg1 = parts.next().unwrap_or("");
    let arg2 = parts.next().unwrap_or("");

    match cmd {
        "/help" => CommandResult::Response(help_text()),
        "/status" => CommandResult::Response(cmd_status(ctx)),
        "/model" => CommandResult::Response(cmd_model(arg1, arg2, ctx)),
        "/restart" => CommandResult::Response(cmd_restart()),
        "/clear" => CommandResult::Response(cmd_clear(ctx)),
        "/agent" => CommandResult::Response(cmd_agent(arg1, arg2, ctx)),
        "/history" => CommandResult::Response(cmd_history(arg1, ctx)),
        "/memory" => CommandResult::Response(cmd_memory(arg1, arg2, ctx).await),
        _ => CommandResult::NotACommand,
    }
}

fn help_text() -> String {
    "\
Commands:
  /help                — Show this help
  /status              — System status (uptime, model, agent)
  /model               — Show current model
  /model list          — List available models
  /model set <id>      — Switch model (e.g. anthropic/claude-sonnet-4-20250514)
  /restart             — Restart the NexMind server
  /clear               — Clear conversation history
  /agent list          — List registered agents
  /agent switch <id>   — Switch active agent
  /history [N]         — Show last N messages (default 10)
  /memory search <q>   — Search semantic memory"
        .into()
}

fn cmd_status(ctx: &CommandContext) -> String {
    let uptime = ctx.start_time.elapsed();
    let hours = uptime.as_secs() / 3600;
    let mins = (uptime.as_secs() % 3600) / 60;
    let secs = uptime.as_secs() % 60;

    let agent_id = ctx.current_agent_id.lock().unwrap().clone();
    let current_model = ctx
        .agent_registry
        .get(&agent_id)
        .map(|a| a.model.primary.clone())
        .unwrap_or_else(|_| "unknown".into());

    let agent_count = ctx
        .agent_registry
        .list_all()
        .map(|a| a.len())
        .unwrap_or(0);

    let model_count = ctx.model_router.models().len();

    format!(
        "NexMind Status\n\
         ─────────────────\n\
         Uptime:    {}h {}m {}s\n\
         Agent:     {}\n\
         Model:     {}\n\
         Agents:    {} registered\n\
         Models:    {} available",
        hours, mins, secs, agent_id, current_model, agent_count, model_count
    )
}

fn cmd_model(sub: &str, arg: &str, ctx: &CommandContext) -> String {
    match sub {
        "" => {
            // Show current model
            let agent_id = ctx.current_agent_id.lock().unwrap().clone();
            match ctx.agent_registry.get(&agent_id) {
                Ok(agent) => format!(
                    "Current model: {}\nFallback: {}",
                    agent.model.primary,
                    agent.model.fallback.as_deref().unwrap_or("none")
                ),
                Err(_) => "Could not read agent config.".into(),
            }
        }
        "list" => {
            let models = ctx.model_router.models();
            if models.is_empty() {
                return "No models registered.".into();
            }
            let mut out = String::from("Available models:\n");
            for m in models {
                out.push_str(&format!(
                    "  {} — {}K ctx, tools: {}\n",
                    m.id,
                    m.context_window / 1000,
                    if m.supports_tools { "yes" } else { "no" }
                ));
            }
            out.trim_end().to_string()
        }
        "set" => {
            if arg.is_empty() {
                return "Usage: /model set <model_id>\nExample: /model set anthropic/claude-sonnet-4-20250514".into();
            }
            // Validate model exists
            let model_exists = ctx.model_router.models().iter().any(|m| m.id == arg);
            if !model_exists {
                return format!(
                    "Unknown model: {}\nUse /model list to see available models.",
                    arg
                );
            }
            // Update agent definition
            let agent_id = ctx.current_agent_id.lock().unwrap().clone();
            match ctx.agent_registry.get(&agent_id) {
                Ok(mut agent) => {
                    let old = agent.model.primary.clone();
                    agent.model.primary = arg.to_string();
                    match ctx.agent_registry.update(agent) {
                        Ok(_) => format!("Model switched: {} → {}", old, arg),
                        Err(e) => format!("Failed to update agent: {}", e),
                    }
                }
                Err(e) => format!("Agent not found: {}", e),
            }
        }
        _ => "Usage: /model [list | set <id>]".into(),
    }
}

fn cmd_restart() -> String {
    // Spawn the new process then exit
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return format!("Cannot determine executable path: {}", e),
    };
    let args: Vec<String> = std::env::args().skip(1).collect();

    match std::process::Command::new(&exe).args(&args).spawn() {
        Ok(_) => {
            // Give a short delay for the response to be sent before exiting
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(500));
                std::process::exit(0);
            });
            "Restarting NexMind server...".into()
        }
        Err(e) => format!("Failed to restart: {}", e),
    }
}

fn cmd_clear(ctx: &CommandContext) -> String {
    let session_id = ctx.session_id.lock().unwrap().clone();
    match ctx.memory_store.clear_session(&session_id) {
        Ok(_) => "Conversation history cleared.".into(),
        Err(e) => format!("Failed to clear history: {}", e),
    }
}

fn cmd_agent(sub: &str, arg: &str, ctx: &CommandContext) -> String {
    match sub {
        "list" | "" => {
            match ctx.agent_registry.list_all() {
                Ok(agents) => {
                    if agents.is_empty() {
                        return "No agents registered.".into();
                    }
                    let current = ctx.current_agent_id.lock().unwrap().clone();
                    let mut out = String::from("Agents:\n");
                    for a in &agents {
                        let marker = if a.id == current { " ◀ active" } else { "" };
                        out.push_str(&format!(
                            "  {} — {}{}\n",
                            a.id,
                            a.description.as_deref().unwrap_or(&a.name),
                            marker
                        ));
                    }
                    out.trim_end().to_string()
                }
                Err(e) => format!("Error listing agents: {}", e),
            }
        }
        "switch" => {
            if arg.is_empty() {
                return "Usage: /agent switch <agent_id>".into();
            }
            // Verify agent exists
            match ctx.agent_registry.get(arg) {
                Ok(agent) => {
                    let mut current = ctx.current_agent_id.lock().unwrap();
                    let old = current.clone();
                    *current = arg.to_string();
                    format!("Switched agent: {} → {} ({})", old, arg, agent.name)
                }
                Err(_) => format!(
                    "Agent '{}' not found. Use /agent list to see available agents.",
                    arg
                ),
            }
        }
        _ => "Usage: /agent [list | switch <id>]".into(),
    }
}

fn cmd_history(count_str: &str, ctx: &CommandContext) -> String {
    let count: usize = count_str.parse().unwrap_or(10);
    let session_id = ctx.session_id.lock().unwrap().clone();

    match ctx
        .memory_store
        .get_session_history(&session_id, count, u32::MAX)
    {
        Ok(messages) => {
            if messages.is_empty() {
                return "No messages in current session.".into();
            }
            let mut out = format!("Last {} messages:\n", messages.len());
            for msg in &messages {
                let role = &msg.role;
                let preview: String = msg.content.chars().take(120).collect();
                let ellipsis = if msg.content.len() > 120 { "..." } else { "" };
                out.push_str(&format!("  [{}] {}{}\n", role, preview, ellipsis));
            }
            out.trim_end().to_string()
        }
        Err(e) => format!("Error reading history: {}", e),
    }
}

async fn cmd_memory(sub: &str, query: &str, ctx: &CommandContext) -> String {
    match sub {
        "search" => {
            if query.is_empty() {
                return "Usage: /memory search <query>".into();
            }
            let mem_query = nexmind_memory::MemoryQuery {
                query_text: query.to_string(),
                workspace_id: "default".into(),
                agent_id: None,
                memory_types: vec![],
                top_k: 10,
                min_importance: None,
            };
            match ctx.memory_store.retrieve(mem_query).await {
                Ok(result) => {
                    if result.memories.is_empty() {
                        return format!("No memories found for '{}'.", query);
                    }
                    let mut out = format!("Found {} memories:\n", result.memories.len());
                    for sm in &result.memories {
                        let preview: String = sm.memory.content.chars().take(100).collect();
                        let ellipsis = if sm.memory.content.len() > 100 {
                            "..."
                        } else {
                            ""
                        };
                        out.push_str(&format!(
                            "  [score: {:.2}] {}{}\n",
                            sm.score, preview, ellipsis
                        ));
                    }
                    out.trim_end().to_string()
                }
                Err(e) => format!("Memory search error: {}", e),
            }
        }
        "" => "Usage: /memory search <query>".into(),
        _ => format!("Unknown subcommand: /memory {}. Use /memory search <query>.", sub),
    }
}
