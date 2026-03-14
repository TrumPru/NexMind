use serde::{Deserialize, Serialize};

/// Agent definition — the canonical agent schema from docs/02.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub description: Option<String>,
    pub system_prompt: String,
    pub model: ModelConfig,
    pub tools: Vec<String>,
    pub memory_policy: MemoryPolicy,
    pub execution_policy: ExecutionPolicy,
    pub budget: BudgetPolicy,
    pub trust_level: TrustLevel,
    pub permissions: Vec<String>,
    pub schedule: Option<Schedule>,
    pub tags: Vec<String>,
    pub workspace_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub primary: String,
    pub fallback: Option<String>,
    pub temperature: f32,
    pub max_tokens: u32,
    pub streaming: bool,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            primary: "anthropic/claude-sonnet-4-20250514".into(),
            fallback: None,
            temperature: 0.7,
            max_tokens: 4096,
            streaming: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPolicy {
    pub session: bool,
    pub semantic: bool,
    pub max_context_tokens: u32,
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self {
            session: true,
            semantic: false,
            max_context_tokens: 4000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    pub max_iterations: u32,
    pub max_tool_calls_per_iteration: u32,
    pub timeout_seconds: u64,
    pub retry: RetryPolicy,
    pub on_failure: FailureAction,
    pub checkpoint_interval: u32,
    /// Enable Think→Plan→Act→Reflect loop for complex tasks.
    #[serde(default = "default_true")]
    pub planning_enabled: bool,
    /// Run reflection every N tool calls to self-correct.
    #[serde(default = "default_reflection_interval")]
    pub reflection_interval: u32,
    /// Automatically extract key facts to semantic memory after each run.
    #[serde(default)]
    pub auto_extract_facts: bool,
    /// Summarize conversation context when it exceeds this token threshold.
    #[serde(default = "default_summarization_threshold")]
    pub context_summarization_threshold: u32,
}

fn default_true() -> bool { true }
fn default_reflection_interval() -> u32 { 3 }
fn default_summarization_threshold() -> u32 { 6000 }

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            max_tool_calls_per_iteration: 5,
            timeout_seconds: 300,
            retry: RetryPolicy::default(),
            on_failure: FailureAction::Fail,
            checkpoint_interval: 0,
            planning_enabled: true,
            reflection_interval: 3,
            auto_extract_facts: true,
            context_summarization_threshold: 6000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub backoff_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            backoff_ms: 1000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FailureAction {
    Suspend,
    Fail,
    RetryWithFallback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetPolicy {
    pub max_tokens_per_run: u64,
    pub max_cost_per_run_usd: f64,
    pub max_cost_per_day_usd: f64,
}

impl Default for BudgetPolicy {
    fn default() -> Self {
        Self {
            max_tokens_per_run: 100_000,
            max_cost_per_run_usd: 1.0,
            max_cost_per_day_usd: 10.0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum TrustLevel {
    Minimal,
    #[default]
    Standard,
    Elevated,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub cron: String,
    pub enabled: bool,
}

impl AgentDefinition {
    /// Create the default conversational agent.
    pub fn default_chat(workspace_id: &str) -> Self {
        Self {
            id: "agt_default_chat".into(),
            name: "NexMind Assistant".into(),
            version: 1,
            description: Some("Default conversational AI assistant".into()),
            system_prompt: r#"You are NexMind, a powerful autonomous AI assistant running locally on the user's machine.

You have access to these capabilities:
- **Memory**: Remember facts about the user and recall them later (memory_read, memory_write)
- **Files**: Read, write, and list files in the workspace (fs_read, fs_write, fs_list)
- **Web**: Fetch data from URLs (http_fetch)
- **Browser**: Full browser automation — navigate, screenshots, extract text/links, click, type (browser_navigate, browser_screenshot, browser_extract_text, browser_extract_links, browser_click, browser_type)
- **Shell**: Execute shell commands for system tasks (shell_exec — requires approval)
- **Code Execution**: Run Python, JavaScript, or Bash scripts in a sandboxed environment for calculations, data analysis, and automation (code_exec)
- **Database**: Query SQLite databases with read-only SQL (db_query)
- **Messaging**: Send messages via Telegram (send_message)
- **Notifications**: Send notifications to the user (notify)
- **Delegation**: Delegate subtasks to specialized agents (delegate_to_agent)
- **Scheduling**: Create, list, and delete scheduled recurring tasks (schedule_task)
- **Goals**: Track long-running goals and multi-step projects across sessions (goal_tracker)
- **Tool Generation**: Create new script-based tools on the fly (generate_tool — requires approval)

Guidelines:
- When browsing, use browser_navigate + browser_extract_text for rich pages (http_fetch only gets raw HTML).
- Remember important information about the user using memory_write.
- Check memory_read first for relevant context before answering questions.
- For complex tasks, break them into steps. Use code_exec for calculations and data processing.
- Delegate specialized subtasks to other agents when appropriate.
- Use schedule_task to set up recurring tasks (e.g., reminders, daily checks).
- Track multi-step projects with goal_tracker so you remember progress across sessions.
- Use shell_exec for system operations like installing packages, running builds, managing processes."#.into(),
            model: ModelConfig::default(),
            tools: vec![
                "memory_read".into(),
                "memory_write".into(),
                "fs_read".into(),
                "fs_write".into(),
                "fs_list".into(),
                "http_fetch".into(),
                "shell_exec".into(),
                "send_message".into(),
                "notify".into(),
                "code_exec".into(),
                "delegate_to_agent".into(),
                "db_query".into(),
                "schedule_task".into(),
                "goal_tracker".into(),
                "generate_tool".into(),
                "browser_navigate".into(),
                "browser_screenshot".into(),
                "browser_extract_text".into(),
                "browser_extract_links".into(),
                "browser_click".into(),
                "browser_type".into(),
            ],
            memory_policy: MemoryPolicy {
                session: true,
                semantic: true,
                max_context_tokens: 4000,
            },
            execution_policy: ExecutionPolicy::default(),
            budget: BudgetPolicy::default(),
            trust_level: TrustLevel::Standard,
            permissions: vec![
                "memory:read:workspace".into(),
                "memory:write:workspace".into(),
                "fs:read".into(),
                "fs:write".into(),
                "network:outbound".into(),
                "shell:exec".into(),
                "message:send".into(),
                "notification:send".into(),
                "code:exec".into(),
                "agent:delegate".into(),
                "db:read".into(),
                "scheduler:manage".into(),
                "goal:manage".into(),
                "tool:generate".into(),
                "browser:navigate".into(),
                "browser:screenshot".into(),
                "browser:read".into(),
                "browser:interact".into(),
            ],
            schedule: None,
            tags: vec!["conversational".into(), "default".into()],
            workspace_id: workspace_id.into(),
        }
    }
}
