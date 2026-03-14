//! # nexmind-openclaw
//!
//! OpenClaw external agent connector for NexMind.
//!
//! This crate allows NexMind to connect to an [OpenClaw](https://openclaw.ai)
//! Gateway instance and use it as an external AI agent. OpenClaw provides:
//!
//! - **Full tool access**: file operations, shell commands, web browsing, memory
//! - **Multi-model routing**: Anthropic, OpenAI, local models via a single interface
//! - **Persistent memory**: semantic search across conversation history
//! - **Skill ecosystem**: installable skills from ClawHub
//! - **Multi-platform connectors**: Telegram, Discord, Signal, WhatsApp
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────┐       HTTP/WS        ┌──────────────────┐
//! │   NexMind   │ ──────────────────►   │  OpenClaw Gateway │
//! │   Agent     │ ◄──────────────────   │  (localhost:18789) │
//! │   Engine    │   JSON responses     │                    │
//! └─────────────┘                      │  ┌──────────────┐ │
//!       │                              │  │ Agent Session │ │
//!       │ uses tools:                  │  │ (Claude/GPT)  │ │
//!       │ • openclaw_send              │  └──────────────┘ │
//!       │ • openclaw_delegate          │  ┌──────────────┐ │
//!       │ • openclaw_status            │  │ Tools/Skills  │ │
//!       │                              │  └──────────────┘ │
//!       │                              │  ┌──────────────┐ │
//!       │                              │  │ Memory Store  │ │
//!       │                              │  └──────────────┘ │
//!       ▼                              └──────────────────┘
//! ┌─────────────┐
//! │  NexMind    │
//! │  Dashboard  │
//! └─────────────┘
//! ```
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use nexmind_openclaw::{OpenClawConfig, OpenClawAgent};
//!
//! // Connect to a local OpenClaw instance
//! let config = OpenClawConfig::local();
//! let agent = OpenClawAgent::new(config);
//!
//! // Check if OpenClaw is available
//! if agent.is_available().await {
//!     // Send a message
//!     let response = agent.run("What's the weather?").await?;
//!     println!("OpenClaw says: {}", response);
//!
//!     // Delegate a complex task
//!     let result = agent.delegate_task("Analyze the codebase").await?;
//!     println!("Task result: {}", result);
//! }
//! ```
//!
//! ## As NexMind Tools
//!
//! Register OpenClaw tools in the NexMind tool registry:
//!
//! ```rust,ignore
//! use nexmind_openclaw::{OpenClawAgent, OpenClawConfig};
//! use nexmind_openclaw::tools::*;
//!
//! let agent = Arc::new(OpenClawAgent::new(OpenClawConfig::local()));
//!
//! registry.register(Box::new(OpenClawSendTool::new(agent.clone())));
//! registry.register(Box::new(OpenClawDelegateTool::new(agent.clone())));
//! registry.register(Box::new(OpenClawStatusTool::new(agent)));
//! ```

pub mod agent;
pub mod config;
pub mod gateway;
pub mod tools;

pub use agent::{OpenClawAgent, OpenClawExecutor};
pub use config::OpenClawConfig;
pub use gateway::GatewayClient;
pub use tools::{OpenClawDelegateTool, OpenClawSendTool, OpenClawStatusTool};

/// Errors from OpenClaw connector operations.
#[derive(Debug, thiserror::Error)]
pub enum OpenClawError {
    /// Failed to connect to the OpenClaw gateway.
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    /// Gateway returned an error response.
    #[error("gateway error: {0}")]
    GatewayError(String),

    /// The OpenClaw agent returned an error.
    #[error("agent error: {0}")]
    AgentError(String),

    /// Rate limited by the gateway.
    #[error("rate limited — try again later")]
    RateLimited,

    /// Failed to parse the response.
    #[error("parse error: {0}")]
    ParseError(String),

    /// Request timed out.
    #[error("request timed out")]
    Timeout,
}

impl OpenClawError {
    /// Whether this error is transient and the operation can be retried.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            OpenClawError::ConnectionFailed(_)
                | OpenClawError::RateLimited
                | OpenClawError::Timeout
                | OpenClawError::GatewayError(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_retryable() {
        assert!(OpenClawError::ConnectionFailed("timeout".into()).is_retryable());
        assert!(OpenClawError::RateLimited.is_retryable());
        assert!(OpenClawError::Timeout.is_retryable());
        assert!(OpenClawError::GatewayError("500".into()).is_retryable());
        assert!(!OpenClawError::AgentError("bad input".into()).is_retryable());
        assert!(!OpenClawError::ParseError("invalid json".into()).is_retryable());
    }

    #[test]
    fn test_error_display() {
        let err = OpenClawError::ConnectionFailed("refused".into());
        assert_eq!(err.to_string(), "connection failed: refused");

        let err = OpenClawError::RateLimited;
        assert_eq!(err.to_string(), "rate limited — try again later");
    }
}
