//! Built-in agent templates (S1 morning briefing, etc.)

use crate::definition::*;

/// Create the morning briefing agent template.
pub fn morning_briefing_template(workspace_id: &str) -> AgentDefinition {
    AgentDefinition {
        id: "agt_morning_briefing".into(),
        name: "Morning Briefing".into(),
        version: 1,
        description: Some(
            "Fetches weather, news headlines, and sends a digest to Telegram".into(),
        ),
        system_prompt: r#"You are a morning briefing assistant. Your job is to:

1. Use http_fetch to get the current weather for the user's city
2. Use http_fetch to get top news headlines
3. Compose a concise morning briefing message
4. Use send_message to deliver it to the user via Telegram

Format the briefing with sections:
🌤 Weather: [city, temperature, conditions]
📰 Headlines: [top 3-5 headlines with one-line summaries]
📅 Date: [today's date]

Keep it under 1500 characters. Use Telegram HTML formatting.
If any fetch fails, skip that section and note it was unavailable.

For weather, use: http_fetch with url "https://wttr.in/Moscow?format=j1"
For news, use: http_fetch with url "https://hacker-news.firebaseio.com/v0/topstories.json?print=pretty&limitToFirst=5&orderBy=%22$key%22"

After composing the briefing, use send_message with channel "telegram" to deliver it."#
            .into(),
        model: ModelConfig {
            primary: "anthropic/claude-haiku-4-5-20251001".into(),
            fallback: Some("openai/gpt-4o-mini".into()),
            temperature: 0.3,
            max_tokens: 2048,
            streaming: true,
        },
        tools: vec![
            "http_fetch".into(),
            "send_message".into(),
            "memory_read".into(),
        ],
        memory_policy: MemoryPolicy {
            session: false,
            semantic: true,
            max_context_tokens: 2000,
        },
        execution_policy: ExecutionPolicy {
            max_iterations: 10,
            max_tool_calls_per_iteration: 5,
            timeout_seconds: 120,
            retry: RetryPolicy::default(),
            on_failure: FailureAction::Fail,
            checkpoint_interval: 0,
        },
        budget: BudgetPolicy {
            max_tokens_per_run: 10000,
            max_cost_per_run_usd: 0.05,
            max_cost_per_day_usd: 0.10,
        },
        trust_level: TrustLevel::Standard,
        permissions: vec![
            "network:outbound".into(),
            "connector:telegram:send".into(),
            "memory:read:workspace".into(),
        ],
        schedule: None,
        tags: vec!["scheduler-agent".into(), "morning-briefing".into()],
        workspace_id: workspace_id.into(),
    }
}

/// List available agent template names.
pub fn available_templates() -> Vec<(&'static str, &'static str)> {
    vec![(
        "morning-briefing",
        "Morning Briefing — fetches weather & news, sends to Telegram on schedule",
    )]
}
