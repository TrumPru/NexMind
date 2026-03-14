use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Memory entry types (P0: Session, Semantic, Pinned).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MemoryType {
    Session,
    Semantic,
    Pinned,
}

impl MemoryType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Session => "session",
            Self::Semantic => "semantic",
            Self::Pinned => "pinned",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "session" => Some(Self::Session),
            "semantic" => Some(Self::Semantic),
            "pinned" => Some(Self::Pinned),
            _ => None,
        }
    }
}

/// Who created the memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MemorySource {
    User,
    Agent,
    System,
}

impl MemorySource {
    pub fn as_str(&self) -> &str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::System => "system",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "agent" => Some(Self::Agent),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

/// Access policy for memory entries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AccessPolicy {
    Private,
    Team,
    Workspace,
}

impl AccessPolicy {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Private => "private",
            Self::Team => "team",
            Self::Workspace => "workspace",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "private" => Some(Self::Private),
            "team" => Some(Self::Team),
            "workspace" => Some(Self::Workspace),
            _ => None,
        }
    }
}

/// A stored memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub workspace_id: String,
    pub agent_id: Option<String>,
    pub memory_type: MemoryType,
    pub content: String,
    pub embedding: Option<Vec<f32>>,
    pub importance: f64,
    pub source: MemorySource,
    pub source_task_id: Option<String>,
    pub access_policy: AccessPolicy,
    pub metadata: Option<Value>,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Input for creating a new memory.
pub struct NewMemory {
    pub workspace_id: String,
    pub agent_id: Option<String>,
    pub memory_type: MemoryType,
    pub content: String,
    pub source: MemorySource,
    pub source_task_id: Option<String>,
    pub access_policy: AccessPolicy,
    pub metadata: Option<Value>,
    pub importance: Option<f64>,
}

/// Session message (conversation turn).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub id: String,
    pub workspace_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub role: String,
    pub content: String,
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<String>,
    pub tokens: Option<i32>,
    pub created_at: String,
}

/// Input for storing a new session message.
pub struct NewSessionMessage {
    pub workspace_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub role: String,
    pub content: String,
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<String>,
    pub tokens: Option<i32>,
}

/// Query parameters for memory retrieval.
pub struct MemoryQuery {
    pub query_text: String,
    pub workspace_id: String,
    pub agent_id: Option<String>,
    pub memory_types: Vec<MemoryType>,
    pub top_k: usize,
    pub min_importance: Option<f64>,
}

/// Result of memory retrieval.
#[derive(Debug, Clone)]
pub struct RetrievalResult {
    pub memories: Vec<ScoredMemory>,
    pub total_tokens: u32,
}

/// A memory with its relevance score.
#[derive(Debug, Clone)]
pub struct ScoredMemory {
    pub memory: Memory,
    pub score: f64,
    pub bm25_score: f64,
    pub vector_score: f64,
    pub rrf_score: f64,
}

/// Memory errors.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("storage error: {0}")]
    Storage(String),
    #[error("embedding error: {0}")]
    Embedding(String),
    #[error("not found: {0}")]
    NotFound(String),
}
