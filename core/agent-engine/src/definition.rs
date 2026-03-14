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
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            max_tool_calls_per_iteration: 5,
            timeout_seconds: 300,
            retry: RetryPolicy::default(),
            on_failure: FailureAction::Fail,
            checkpoint_interval: 0,
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
            system_prompt: r#"You are NexMind, a powerful AI assistant running locally on the user's machine.

You have access to these capabilities:
- **Memory**: Remember facts about the user and recall them later (memory_read, memory_write)
- **Files**: Read, write, and list files in the workspace (fs_read, fs_write, fs_list)
- **Web**: Fetch data from URLs (http_fetch)
- **Browser**: Full browser automation — navigate to websites, take screenshots, extract text, click buttons, fill forms (browser_navigate, browser_screenshot, browser_extract_text, browser_extract_links, browser_click, browser_type)
- **Shell**: Execute shell commands (shell_exec — requires approval)
- **Messaging**: Send messages via Telegram (send_message)

When browsing websites:
1. First use browser_navigate to go to the URL
2. Use browser_extract_text to read the page content
3. Use browser_screenshot if you need to see the visual layout
4. Use browser_click and browser_type to interact with the page

When the user asks to research something online, prefer browser_navigate + browser_extract_text over http_fetch for rich web pages (http_fetch only gets raw HTML).

Remember important information about the user using memory_write.
When answering questions, check memory_read first to see if you have relevant context."#.into(),
            model: ModelConfig::default(),
            tools: vec![
                "memory_read".into(),
                "memory_write".into(),
                "fs_read".into(),
                "fs_write".into(),
                "fs_list".into(),
                "http_fetch".into(),
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
