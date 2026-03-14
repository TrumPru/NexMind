pub mod engine;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use engine::DagWorkflowEngine;

/// Workflow node types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeType {
    Tool,
    Agent,
    Condition,
    Approval,
    Timer,
    Transform,
}

/// A node in a workflow DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNode {
    pub id: String,
    pub node_type: NodeType,
    pub config: Value,
    pub timeout_seconds: u32,
    pub retry_config: Option<Value>,
}

/// An edge in a workflow DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEdge {
    pub from: String,
    pub to: String,
    pub condition: Option<String>,
}

/// Workflow definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub nodes: Vec<WorkflowNode>,
    pub edges: Vec<WorkflowEdge>,
}

/// Workflow engine error.
#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error("workflow not found: {0}")]
    NotFound(String),
    #[error("cycle detected in DAG")]
    CycleDetected,
    #[error("execution error: {0}")]
    ExecutionError(String),
}

/// Workflow engine trait — executes DAG-based workflows.
#[async_trait::async_trait]
pub trait WorkflowEngine: Send + Sync {
    async fn run(&self, workflow_id: &str) -> Result<Value, WorkflowError>;
}
