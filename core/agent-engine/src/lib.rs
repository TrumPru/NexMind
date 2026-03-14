pub mod approval;
pub mod cost;
pub mod definition;
pub mod registry;
pub mod runtime;
pub mod state;
pub mod team;
pub mod templates;
pub mod notification;

// Re-export agent communication types from nexmind-agent-comm
pub use nexmind_agent_comm::{
    AgentExecutor, AgentMessage, AgentMessageType, CommError, FileRef,
    MailboxRouter, TeamMemberInfo, AgentMailbox,
};

pub use notification::{
    Notification, NotificationAction, NotificationConfig, NotificationEngine,
    NotificationPriority, QuietHours,
};
pub use approval::{ApprovalDecision, ApprovalManager, ApprovalRecord, ApprovalRequest, RiskLevel};
pub use cost::{BudgetStatus, CostPeriod, CostRecord, CostSummary, CostTracker, PriceTable};
pub use definition::*;
pub use registry::AgentRegistry;
pub use runtime::{AgentRunResult, AgentRuntime, RunContext};
pub use state::AgentState;
pub use team::{
    OrchestrationPattern, SharedContextConfig, TeamDefinition, TeamFailurePolicy, TeamMember,
    TeamOrchestrator, TeamRegistry, TeamRunResult,
};

/// Agent engine error.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent not found: {0}")]
    NotFound(String),
    #[error("budget exceeded")]
    BudgetExceeded,
    #[error("max iterations reached")]
    MaxIterations,
    #[error("execution error: {0}")]
    ExecutionError(String),
    #[error("storage error: {0}")]
    StorageError(String),
    #[error("model error: {0}")]
    ModelError(String),
    #[error("invalid state transition: {0} -> {1}")]
    InvalidTransition(String, String),
}
