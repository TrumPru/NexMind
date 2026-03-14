pub mod registry;
pub mod tools;

pub use registry::{ToolContext, ToolOutput, ToolRegistry};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tool definition exposed to agents via JSON Schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub required_permissions: Vec<String>,
    pub trust_level: u8,
    pub idempotent: bool,
    pub timeout_seconds: u32,
}

/// Result of a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub output: Value,
    pub error: Option<String>,
    pub duration_ms: u64,
}

/// Tool runtime error.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("validation error: {0}")]
    ValidationError(String),
    #[error("execution error: {0}")]
    ExecutionError(String),
    #[error("timeout")]
    Timeout,
    #[error("needs approval: {0}")]
    NeedsApproval(String),
}

/// The trait all tools implement.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError>;
    fn validate_args(&self, args: &Value) -> Result<(), ToolError>;
}
