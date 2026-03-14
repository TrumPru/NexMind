use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};
use ulid::Ulid;

use nexmind_event_bus::{Event, EventBus, EventSource, EventType};
use nexmind_storage::Database;

/// Error type for agent communication.
#[derive(Debug, thiserror::Error)]
pub enum CommError {
    #[error("agent not registered: {0}")]
    AgentNotRegistered(String),
    #[error("storage error: {0}")]
    StorageError(String),
    #[error("send error: {0}")]
    SendError(String),
    #[error("execution error: {0}")]
    ExecutionError(String),
}

/// Type of message exchanged between agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentMessageType {
    Direct,
    Broadcast,
    TaskDelegation,
    TaskResult,
    FileShare,
}

impl std::fmt::Display for AgentMessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Direct => write!(f, "direct"),
            Self::Broadcast => write!(f, "broadcast"),
            Self::TaskDelegation => write!(f, "task_delegation"),
            Self::TaskResult => write!(f, "task_result"),
            Self::FileShare => write!(f, "file_share"),
        }
    }
}

/// Reference to a shared file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRef {
    pub path: String,
    pub filename: String,
    pub mime_type: Option<String>,
    pub size_bytes: u64,
}

/// A message exchanged between agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: String,
    pub sender_id: String,
    pub recipient_id: Option<String>,
    pub team_id: Option<String>,
    pub task_id: Option<String>,
    pub message_type: AgentMessageType,
    pub content: String,
    pub file_refs: Vec<FileRef>,
    pub correlation_id: Option<String>,
    pub reply_to: Option<String>,
    pub timestamp: String,
}

impl AgentMessage {
    /// Create a new direct message.
    pub fn direct(sender_id: &str, recipient_id: &str, content: &str) -> Self {
        Self {
            id: format!("msg_{}", Ulid::new()),
            sender_id: sender_id.to_string(),
            recipient_id: Some(recipient_id.to_string()),
            team_id: None,
            task_id: None,
            message_type: AgentMessageType::Direct,
            content: content.to_string(),
            file_refs: Vec::new(),
            correlation_id: None,
            reply_to: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Create a broadcast message.
    pub fn broadcast(sender_id: &str, team_id: &str, content: &str) -> Self {
        Self {
            id: format!("msg_{}", Ulid::new()),
            sender_id: sender_id.to_string(),
            recipient_id: None,
            team_id: Some(team_id.to_string()),
            task_id: None,
            message_type: AgentMessageType::Broadcast,
            content: content.to_string(),
            file_refs: Vec::new(),
            correlation_id: None,
            reply_to: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Create a task delegation message.
    pub fn delegation(sender_id: &str, recipient_id: &str, task: &str) -> Self {
        Self {
            id: format!("msg_{}", Ulid::new()),
            sender_id: sender_id.to_string(),
            recipient_id: Some(recipient_id.to_string()),
            team_id: None,
            task_id: None,
            message_type: AgentMessageType::TaskDelegation,
            content: task.to_string(),
            file_refs: Vec::new(),
            correlation_id: None,
            reply_to: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Create a file share message.
    pub fn file_share(sender_id: &str, recipient_id: &str, message: &str, file_ref: FileRef) -> Self {
        Self {
            id: format!("msg_{}", Ulid::new()),
            sender_id: sender_id.to_string(),
            recipient_id: Some(recipient_id.to_string()),
            team_id: None,
            task_id: None,
            message_type: AgentMessageType::FileShare,
            content: message.to_string(),
            file_refs: vec![file_ref],
            correlation_id: None,
            reply_to: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    pub fn with_team(mut self, team_id: &str) -> Self {
        self.team_id = Some(team_id.to_string());
        self
    }

    pub fn with_task(mut self, task_id: &str) -> Self {
        self.task_id = Some(task_id.to_string());
        self
    }

    pub fn with_correlation(mut self, correlation_id: &str) -> Self {
        self.correlation_id = Some(correlation_id.to_string());
        self
    }

    pub fn with_reply_to(mut self, reply_to: &str) -> Self {
        self.reply_to = Some(reply_to.to_string());
        self
    }
}

/// Per-agent mailbox that receives messages from other agents.
pub struct AgentMailbox {
    agent_id: String,
    receiver: mpsc::UnboundedReceiver<AgentMessage>,
}

impl AgentMailbox {
    /// Get the agent ID this mailbox belongs to.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Wait for the next message.
    pub async fn recv(&mut self) -> Option<AgentMessage> {
        self.receiver.recv().await
    }

    /// Try to receive all pending messages without blocking.
    pub fn try_recv_all(&mut self) -> Vec<AgentMessage> {
        let mut messages = Vec::new();
        while let Ok(msg) = self.receiver.try_recv() {
            messages.push(msg);
        }
        messages
    }
}

/// Team member info exposed for agent_list_team tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMemberInfo {
    pub agent_id: String,
    pub name: String,
    pub role: String,
    pub description: Option<String>,
}

/// Central router that manages all agent mailboxes and routes messages.
pub struct MailboxRouter {
    senders: RwLock<HashMap<String, mpsc::UnboundedSender<AgentMessage>>>,
    team_members: RwLock<HashMap<String, Vec<TeamMemberInfo>>>,
    db: Arc<Database>,
    event_bus: Arc<EventBus>,
}

impl MailboxRouter {
    pub fn new(db: Arc<Database>, event_bus: Arc<EventBus>) -> Self {
        Self {
            senders: RwLock::new(HashMap::new()),
            team_members: RwLock::new(HashMap::new()),
            db,
            event_bus,
        }
    }

    /// Register an agent and return its mailbox.
    pub async fn register_agent(&self, agent_id: &str) -> AgentMailbox {
        let (tx, rx) = mpsc::unbounded_channel();
        self.senders.write().await.insert(agent_id.to_string(), tx);
        debug!(agent_id = %agent_id, "agent registered in mailbox router");
        AgentMailbox {
            agent_id: agent_id.to_string(),
            receiver: rx,
        }
    }

    /// Unregister an agent, dropping its sender.
    pub async fn unregister_agent(&self, agent_id: &str) {
        self.senders.write().await.remove(agent_id);
        debug!(agent_id = %agent_id, "agent unregistered from mailbox router");
    }

    /// Register team members for a team.
    pub async fn register_team(&self, team_id: &str, members: Vec<TeamMemberInfo>) {
        info!(team_id = %team_id, members = members.len(), "team registered in mailbox router");
        self.team_members.write().await.insert(team_id.to_string(), members);
    }

    /// Unregister a team.
    pub async fn unregister_team(&self, team_id: &str) {
        self.team_members.write().await.remove(team_id);
    }

    /// Get team members.
    pub async fn get_team_members(&self, team_id: &str) -> Vec<TeamMemberInfo> {
        self.team_members
            .read()
            .await
            .get(team_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Send a message to a specific agent.
    pub async fn send(&self, msg: AgentMessage) -> Result<(), CommError> {
        // Persist to database
        self.persist_message(&msg)?;

        // Route to recipient
        if let Some(recipient_id) = &msg.recipient_id {
            let senders = self.senders.read().await;
            if let Some(sender) = senders.get(recipient_id) {
                sender.send(msg.clone()).map_err(|e| {
                    CommError::SendError(format!("failed to send to {}: {}", recipient_id, e))
                })?;
            } else {
                debug!(
                    recipient = %recipient_id,
                    "recipient not registered, message persisted only"
                );
            }
        }

        // Emit event
        self.event_bus.emit(Event::new(
            EventSource::Agent,
            EventType::Custom("AgentMessageSent".into()),
            serde_json::json!({
                "message_id": msg.id,
                "sender_id": msg.sender_id,
                "recipient_id": msg.recipient_id,
                "message_type": msg.message_type.to_string(),
                "team_id": msg.team_id,
            }),
            "default",
            msg.correlation_id.clone(),
        ));

        Ok(())
    }

    /// Broadcast a message to all members of a team.
    pub async fn broadcast(&self, msg: AgentMessage, team_id: &str) -> Result<(), CommError> {
        self.persist_message(&msg)?;

        let members = self.team_members.read().await;
        let team = members.get(team_id);

        if let Some(team_members) = team {
            let senders = self.senders.read().await;
            for member in team_members {
                if member.agent_id == msg.sender_id {
                    continue; // don't send to self
                }
                if let Some(sender) = senders.get(&member.agent_id) {
                    let mut msg_clone = msg.clone();
                    msg_clone.recipient_id = Some(member.agent_id.clone());
                    if let Err(e) = sender.send(msg_clone) {
                        warn!(
                            agent_id = %member.agent_id,
                            error = %e,
                            "failed to deliver broadcast message"
                        );
                    }
                }
            }
        }

        self.event_bus.emit(Event::new(
            EventSource::Agent,
            EventType::Custom("AgentBroadcastSent".into()),
            serde_json::json!({
                "message_id": msg.id,
                "sender_id": msg.sender_id,
                "team_id": team_id,
            }),
            "default",
            msg.correlation_id.clone(),
        ));

        Ok(())
    }

    /// Get message history for a task.
    pub fn get_history(&self, task_id: &str, limit: usize) -> Result<Vec<AgentMessage>, CommError> {
        let conn = self.db.conn().map_err(|e| CommError::StorageError(e.to_string()))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, from_agent, to_agent, msg_type, content, artifacts, team_id, created_at \
                 FROM task_messages WHERE task_id = ?1 ORDER BY created_at ASC LIMIT ?2",
            )
            .map_err(|e| CommError::StorageError(e.to_string()))?;

        let messages = stmt
            .query_map(rusqlite::params![task_id, limit as i64], |row| {
                let id: String = row.get(0)?;
                let from_agent: String = row.get(1)?;
                let to_agent: Option<String> = row.get(2)?;
                let msg_type: String = row.get(3)?;
                let content: String = row.get(4)?;
                let artifacts: Option<String> = row.get(5)?;
                let team_id: Option<String> = row.get(6)?;
                let created_at: String = row.get(7)?;

                let message_type = match msg_type.as_str() {
                    "direct" => AgentMessageType::Direct,
                    "broadcast" => AgentMessageType::Broadcast,
                    "task_delegation" => AgentMessageType::TaskDelegation,
                    "task_result" => AgentMessageType::TaskResult,
                    "file_share" => AgentMessageType::FileShare,
                    _ => AgentMessageType::Direct,
                };

                let file_refs: Vec<FileRef> = artifacts
                    .and_then(|a| serde_json::from_str(&a).ok())
                    .unwrap_or_default();

                Ok(AgentMessage {
                    id,
                    sender_id: from_agent,
                    recipient_id: to_agent,
                    team_id,
                    task_id: Some(task_id.to_string()),
                    message_type,
                    content,
                    file_refs,
                    correlation_id: None,
                    reply_to: None,
                    timestamp: created_at,
                })
            })
            .map_err(|e| CommError::StorageError(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(messages)
    }

    /// Get messages for a specific agent (from DB).
    pub fn get_agent_messages(
        &self,
        agent_id: &str,
        team_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AgentMessage>, CommError> {
        let conn = self.db.conn().map_err(|e| CommError::StorageError(e.to_string()))?;

        let (sql, params_vec): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(tid) = team_id {
            (
                "SELECT id, from_agent, to_agent, msg_type, content, artifacts, team_id, task_id, created_at \
                 FROM task_messages WHERE (to_agent = ?1 OR (visibility = 'team' AND team_id = ?2)) \
                 ORDER BY created_at DESC LIMIT ?3",
                vec![
                    Box::new(agent_id.to_string()),
                    Box::new(tid.to_string()),
                    Box::new(limit as i64),
                ],
            )
        } else {
            (
                "SELECT id, from_agent, to_agent, msg_type, content, artifacts, team_id, task_id, created_at \
                 FROM task_messages WHERE to_agent = ?1 \
                 ORDER BY created_at DESC LIMIT ?2",
                vec![
                    Box::new(agent_id.to_string()),
                    Box::new(limit as i64),
                ],
            )
        };

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();

        let mut stmt = conn.prepare(sql).map_err(|e| CommError::StorageError(e.to_string()))?;

        let messages = stmt
            .query_map(params_refs.as_slice(), |row| {
                let id: String = row.get(0)?;
                let from_agent: String = row.get(1)?;
                let to_agent: Option<String> = row.get(2)?;
                let msg_type: String = row.get(3)?;
                let content: String = row.get(4)?;
                let artifacts: Option<String> = row.get(5)?;
                let team_id: Option<String> = row.get(6)?;
                let task_id: Option<String> = row.get(7)?;
                let created_at: String = row.get(8)?;

                let message_type = match msg_type.as_str() {
                    "direct" => AgentMessageType::Direct,
                    "broadcast" => AgentMessageType::Broadcast,
                    "task_delegation" => AgentMessageType::TaskDelegation,
                    "task_result" => AgentMessageType::TaskResult,
                    "file_share" => AgentMessageType::FileShare,
                    _ => AgentMessageType::Direct,
                };

                let file_refs: Vec<FileRef> = artifacts
                    .and_then(|a| serde_json::from_str(&a).ok())
                    .unwrap_or_default();

                Ok(AgentMessage {
                    id,
                    sender_id: from_agent,
                    recipient_id: to_agent,
                    team_id,
                    task_id,
                    message_type,
                    content,
                    file_refs,
                    correlation_id: None,
                    reply_to: None,
                    timestamp: created_at,
                })
            })
            .map_err(|e| CommError::StorageError(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(messages)
    }

    /// Persist a message to the task_messages table.
    fn persist_message(&self, msg: &AgentMessage) -> Result<(), CommError> {
        let conn = self.db.conn().map_err(|e| CommError::StorageError(e.to_string()))?;

        let task_id = msg.task_id.as_deref().unwrap_or("default");
        let artifacts = if msg.file_refs.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&msg.file_refs).unwrap_or_default())
        };

        conn.execute(
            "INSERT INTO task_messages (id, task_id, team_id, from_agent, to_agent, msg_type, content, artifacts, visibility, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                msg.id,
                task_id,
                msg.team_id,
                msg.sender_id,
                msg.recipient_id,
                msg.message_type.to_string(),
                msg.content,
                artifacts,
                if msg.recipient_id.is_some() { "private" } else { "team" },
                msg.timestamp,
            ],
        )
        .map_err(|e| CommError::StorageError(e.to_string()))?;

        Ok(())
    }
}

/// Trait for executing agents — used by delegate tool to avoid circular deps.
#[async_trait::async_trait]
pub trait AgentExecutor: Send + Sync {
    async fn execute_agent(
        &self,
        agent_id: &str,
        input: &str,
        workspace_id: &str,
        team_id: Option<&str>,
    ) -> Result<String, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, Arc<EventBus>) {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        // Insert a dummy task so FK constraints on task_messages are satisfied
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO tasks (id, workspace_id, status) VALUES ('default', 'default', 'pending')",
            [],
        ).unwrap();
        let db = Arc::new(db);
        let event_bus = Arc::new(EventBus::with_default_capacity());
        (db, event_bus)
    }

    #[test]
    fn test_agent_message_direct() {
        let msg = AgentMessage::direct("agent_a", "agent_b", "Hello!");
        assert!(msg.id.starts_with("msg_"));
        assert_eq!(msg.sender_id, "agent_a");
        assert_eq!(msg.recipient_id, Some("agent_b".into()));
        assert_eq!(msg.message_type, AgentMessageType::Direct);
        assert_eq!(msg.content, "Hello!");
    }

    #[test]
    fn test_agent_message_broadcast() {
        let msg = AgentMessage::broadcast("agent_a", "team_1", "Status update");
        assert!(msg.recipient_id.is_none());
        assert_eq!(msg.team_id, Some("team_1".into()));
        assert_eq!(msg.message_type, AgentMessageType::Broadcast);
    }

    #[test]
    fn test_agent_message_with_file() {
        let file_ref = FileRef {
            path: "shared/team_1/report.pdf".into(),
            filename: "report.pdf".into(),
            mime_type: Some("application/pdf".into()),
            size_bytes: 1024,
        };
        let msg = AgentMessage::file_share("agent_a", "agent_b", "Here's the report", file_ref);
        assert_eq!(msg.message_type, AgentMessageType::FileShare);
        assert_eq!(msg.file_refs.len(), 1);
        assert_eq!(msg.file_refs[0].filename, "report.pdf");
    }

    #[test]
    fn test_agent_message_builders() {
        let msg = AgentMessage::direct("a", "b", "test")
            .with_team("team_1")
            .with_task("task_1")
            .with_correlation("corr_1")
            .with_reply_to("msg_prev");

        assert_eq!(msg.team_id, Some("team_1".into()));
        assert_eq!(msg.task_id, Some("task_1".into()));
        assert_eq!(msg.correlation_id, Some("corr_1".into()));
        assert_eq!(msg.reply_to, Some("msg_prev".into()));
    }

    #[tokio::test]
    async fn test_mailbox_register_and_send() {
        let (db, event_bus) = setup();
        let router = MailboxRouter::new(db, event_bus);

        let mut mailbox_b = router.register_agent("agent_b").await;
        let _mailbox_a = router.register_agent("agent_a").await;

        let msg = AgentMessage::direct("agent_a", "agent_b", "Hello from A!");
        router.send(msg).await.unwrap();

        let received = mailbox_b.recv().await.unwrap();
        assert_eq!(received.sender_id, "agent_a");
        assert_eq!(received.content, "Hello from A!");
    }

    #[tokio::test]
    async fn test_mailbox_try_recv_all() {
        let (db, event_bus) = setup();
        let router = MailboxRouter::new(db, event_bus);

        let mut mailbox = router.register_agent("agent_b").await;
        let _mailbox_a = router.register_agent("agent_a").await;

        for i in 0..3 {
            let msg = AgentMessage::direct("agent_a", "agent_b", &format!("msg {}", i));
            router.send(msg).await.unwrap();
        }

        // Small delay to let messages arrive
        tokio::task::yield_now().await;

        let messages = mailbox.try_recv_all();
        assert_eq!(messages.len(), 3);
    }

    #[tokio::test]
    async fn test_broadcast() {
        let (db, event_bus) = setup();
        let router = MailboxRouter::new(db, event_bus);

        let _mailbox_a = router.register_agent("agent_a").await;
        let mut mailbox_b = router.register_agent("agent_b").await;
        let mut mailbox_c = router.register_agent("agent_c").await;

        router
            .register_team(
                "team_1",
                vec![
                    TeamMemberInfo {
                        agent_id: "agent_a".into(),
                        name: "Agent A".into(),
                        role: "sender".into(),
                        description: None,
                    },
                    TeamMemberInfo {
                        agent_id: "agent_b".into(),
                        name: "Agent B".into(),
                        role: "worker".into(),
                        description: None,
                    },
                    TeamMemberInfo {
                        agent_id: "agent_c".into(),
                        name: "Agent C".into(),
                        role: "worker".into(),
                        description: None,
                    },
                ],
            )
            .await;

        let msg = AgentMessage::broadcast("agent_a", "team_1", "Team update!");
        router.broadcast(msg, "team_1").await.unwrap();

        let msg_b = mailbox_b.recv().await.unwrap();
        assert_eq!(msg_b.content, "Team update!");

        let msg_c = mailbox_c.recv().await.unwrap();
        assert_eq!(msg_c.content, "Team update!");
    }

    #[tokio::test]
    async fn test_persist_and_get_history() {
        let (db, event_bus) = setup();
        // Insert a task for FK constraint
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO tasks (id, workspace_id, status) VALUES ('task_1', 'default', 'pending')",
            [],
        ).unwrap();
        drop(conn);

        let router = MailboxRouter::new(db, event_bus);

        let _mailbox_a = router.register_agent("agent_a").await;
        let _mailbox_b = router.register_agent("agent_b").await;

        let msg1 = AgentMessage::direct("agent_a", "agent_b", "First message")
            .with_task("task_1");
        let msg2 = AgentMessage::direct("agent_b", "agent_a", "Reply")
            .with_task("task_1");

        router.send(msg1).await.unwrap();
        router.send(msg2).await.unwrap();

        let history = router.get_history("task_1", 10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content, "First message");
        assert_eq!(history[1].content, "Reply");
    }

    #[tokio::test]
    async fn test_unregister_agent() {
        let (db, event_bus) = setup();
        let router = MailboxRouter::new(db, event_bus);

        let _mailbox = router.register_agent("agent_a").await;
        router.unregister_agent("agent_a").await;

        // Sending to unregistered agent should still persist but not deliver
        let msg = AgentMessage::direct("agent_b", "agent_a", "Hello?");
        let result = router.send(msg).await;
        assert!(result.is_ok()); // persisted, no delivery
    }

    #[test]
    fn test_message_type_display() {
        assert_eq!(AgentMessageType::Direct.to_string(), "direct");
        assert_eq!(AgentMessageType::Broadcast.to_string(), "broadcast");
        assert_eq!(AgentMessageType::TaskDelegation.to_string(), "task_delegation");
        assert_eq!(AgentMessageType::TaskResult.to_string(), "task_result");
        assert_eq!(AgentMessageType::FileShare.to_string(), "file_share");
    }
}
