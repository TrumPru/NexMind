use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Chat message role.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Message content — text, tool calls, or tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Content {
    Text { text: String },
    ToolCalls { tool_calls: Vec<ToolCall> },
    ToolResult { tool_call_id: String, content: String },
}

/// A single tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// Normalized chat message (provider-agnostic).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: Content,
}

impl ChatMessage {
    pub fn system(text: &str) -> Self {
        Self {
            role: Role::System,
            content: Content::Text { text: text.to_string() },
        }
    }

    pub fn user(text: &str) -> Self {
        Self {
            role: Role::User,
            content: Content::Text { text: text.to_string() },
        }
    }

    pub fn assistant_text(text: &str) -> Self {
        Self {
            role: Role::Assistant,
            content: Content::Text { text: text.to_string() },
        }
    }

    pub fn assistant_tool_calls(calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: Content::ToolCalls { tool_calls: calls },
        }
    }

    pub fn tool_result(tool_call_id: &str, content: &str) -> Self {
        Self {
            role: Role::Tool,
            content: Content::ToolResult {
                tool_call_id: tool_call_id.to_string(),
                content: content.to_string(),
            },
        }
    }

    /// Extract text content if this is a text message.
    pub fn text(&self) -> Option<&str> {
        match &self.content {
            Content::Text { text } => Some(text),
            _ => None,
        }
    }

    /// Extract tool calls if present.
    pub fn tool_calls(&self) -> Option<&[ToolCall]> {
        match &self.content {
            Content::ToolCalls { tool_calls } => Some(tool_calls),
            _ => None,
        }
    }
}

/// Tool definition for function calling (JSON Schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// What the agent sends to the model router.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Option<Vec<ToolDefinition>>,
    pub temperature: f32,
    pub max_tokens: u32,
    pub stream: bool,
}

impl CompletionRequest {
    /// Extract system prompt from messages (first System role message).
    pub fn system_prompt(&self) -> Option<String> {
        self.messages
            .iter()
            .find(|m| m.role == Role::System)
            .and_then(|m| m.text().map(|t| t.to_string()))
    }

    /// Get all non-system messages.
    pub fn conversation_messages(&self) -> Vec<&ChatMessage> {
        self.messages
            .iter()
            .filter(|m| m.role != Role::System)
            .collect()
    }
}

/// Normalized streaming chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamChunk {
    TextDelta(String),
    ToolCallStart { id: String, name: String },
    ToolCallArgumentsDelta { id: String, delta: String },
    ToolCallEnd { id: String },
    Usage(TokenUsage),
    Done,
    Error(String),
}

/// Token usage for a single LLM call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

/// Full (non-streaming) completion response.
#[derive(Debug, Clone)]
pub struct CompletionResponse {
    pub message: ChatMessage,
    pub usage: TokenUsage,
    pub model: String,
    pub latency_ms: u64,
}

/// Information about a model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub context_window: u32,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_streaming: bool,
    pub cost_per_1k_input: f64,
    pub cost_per_1k_output: f64,
}

/// Health status of a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Degraded(String),
    Unavailable(String),
}

/// Model router errors.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("provider not found: {0}")]
    ProviderNotFound(String),
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("authentication error: {0}")]
    AuthError(String),
    #[error("rate limited (retry after {retry_after_ms:?}ms)")]
    RateLimited { retry_after_ms: Option<u64> },
    #[error("provider timeout")]
    Timeout,
    #[error("provider overloaded")]
    Overloaded,
    #[error("provider error: {0}")]
    ProviderError(String),
    #[error("no fallback configured")]
    NoFallback,
    #[error("request error: {0}")]
    RequestError(String),
    #[error("parse error: {0}")]
    ParseError(String),
}
