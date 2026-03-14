use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::info;
use ulid::Ulid;

use nexmind_event_bus::{Event, EventBus, EventSource, EventType};
use nexmind_storage::Database;

/// Risk level for approval requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

impl RiskLevel {
    pub fn as_str(&self) -> &str {
        match self {
            RiskLevel::Low => "low",
            RiskLevel::Medium => "medium",
            RiskLevel::High => "high",
            RiskLevel::Critical => "critical",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "low" => RiskLevel::Low,
            "high" => RiskLevel::High,
            "critical" => RiskLevel::Critical,
            _ => RiskLevel::Medium,
        }
    }

    pub fn emoji(&self) -> &str {
        match self {
            RiskLevel::Low => "\u{1f7e2}",
            RiskLevel::Medium => "\u{1f7e1}",
            RiskLevel::High => "\u{1f7e0}",
            RiskLevel::Critical => "\u{1f534}",
        }
    }
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Request for approval.
pub struct ApprovalRequest {
    pub workspace_id: String,
    pub requester_agent_id: String,
    pub requester_run_id: String,
    pub tool_id: String,
    pub tool_args: serde_json::Value,
    pub action_description: String,
    pub risk_level: RiskLevel,
    pub context_summary: Option<String>,
    pub expires_in: Duration,
}

/// Stored approval record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub id: String,
    pub workspace_id: String,
    pub requester_agent_id: String,
    pub requester_run_id: String,
    pub tool_id: String,
    pub tool_args: serde_json::Value,
    pub action_description: String,
    pub risk_level: String,
    pub status: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<String>,
    pub decision_note: Option<String>,
    pub created_at: String,
    pub expires_at: String,
}

/// Decision on an approval.
#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalDecision {
    Approved,
    Denied { reason: Option<String> },
    Expired,
    Pending,
}

/// Manages approval requests -- create, decide, query, expire.
pub struct ApprovalManager {
    db: Arc<Database>,
    event_bus: Arc<EventBus>,
}

impl ApprovalManager {
    pub fn new(db: Arc<Database>, event_bus: Arc<EventBus>) -> Self {
        Self { db, event_bus }
    }

    /// Create an approval request. Returns the approval_id.
    pub fn request_approval(&self, req: ApprovalRequest) -> Result<String, crate::AgentError> {
        let id = format!("apr_{}", Ulid::new());
        let now = Utc::now();
        let created_at = now.to_rfc3339();
        let expires_at = (now
            + chrono::Duration::from_std(req.expires_in)
                .unwrap_or(chrono::Duration::hours(24)))
        .to_rfc3339();

        let tool_args_str = serde_json::to_string(&req.tool_args).unwrap_or_default();
        let tool_args_hash = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(tool_args_str.as_bytes());
            hex::encode(hasher.finalize())
        };

        let conn = self
            .db
            .conn()
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;
        conn.execute(
            "INSERT INTO approval_requests (id, workspace_id, requester_agent_id, requester_run_id, tool_id, tool_args, tool_args_hash, action_description, risk_level, policy_id, status, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending', ?11, ?12)",
            rusqlite::params![
                id,
                req.workspace_id,
                req.requester_agent_id,
                req.requester_run_id,
                req.tool_id,
                tool_args_str,
                tool_args_hash,
                req.action_description,
                req.risk_level.as_str(),
                "default",
                created_at,
                expires_at,
            ],
        )
        .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;

        // Emit event
        self.event_bus.emit(Event::new(
            EventSource::System,
            EventType::Custom("ApprovalRequested".into()),
            serde_json::json!({
                "approval_id": id,
                "tool_id": req.tool_id,
                "agent_id": req.requester_agent_id,
                "risk_level": req.risk_level.as_str(),
                "action": req.action_description,
            }),
            &req.workspace_id,
            None,
        ));

        info!(approval_id = %id, tool = %req.tool_id, "approval requested");
        Ok(id)
    }

    /// Check the status of an approval.
    pub fn check_status(&self, approval_id: &str) -> Result<ApprovalDecision, crate::AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;
        let status: String = conn
            .query_row(
                "SELECT status FROM approval_requests WHERE id = ?1",
                rusqlite::params![approval_id],
                |row| row.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    crate::AgentError::NotFound(approval_id.to_string())
                }
                other => crate::AgentError::StorageError(other.to_string()),
            })?;

        Ok(match status.as_str() {
            "approved" => ApprovalDecision::Approved,
            "denied" => {
                let note: Option<String> = conn
                    .query_row(
                        "SELECT decision_note FROM approval_requests WHERE id = ?1",
                        rusqlite::params![approval_id],
                        |row| row.get(0),
                    )
                    .ok();
                ApprovalDecision::Denied { reason: note }
            }
            "expired" => ApprovalDecision::Expired,
            _ => ApprovalDecision::Pending,
        })
    }

    /// Approve a request.
    pub fn approve(
        &self,
        approval_id: &str,
        decided_by: &str,
    ) -> Result<(), crate::AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;
        let now = Utc::now().to_rfc3339();

        // Get workspace_id using the same connection
        let workspace_id: String = conn
            .query_row(
                "SELECT workspace_id FROM approval_requests WHERE id = ?1",
                rusqlite::params![approval_id],
                |row| row.get(0),
            )
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;

        let rows = conn
            .execute(
                "UPDATE approval_requests SET status = 'approved', decided_by = ?1, decided_at = ?2 WHERE id = ?3 AND status = 'pending'",
                rusqlite::params![decided_by, now, approval_id],
            )
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;

        if rows == 0 {
            return Err(crate::AgentError::NotFound(format!(
                "pending approval not found: {}",
                approval_id
            )));
        }

        // Drop connection before emitting event
        drop(conn);

        self.event_bus.emit(Event::new(
            EventSource::System,
            EventType::Custom("ApprovalDecided".into()),
            serde_json::json!({
                "approval_id": approval_id,
                "decision": "approved",
                "decided_by": decided_by,
            }),
            &workspace_id,
            None,
        ));

        info!(approval_id = %approval_id, decided_by = %decided_by, "approval approved");
        Ok(())
    }

    /// Deny a request.
    pub fn deny(
        &self,
        approval_id: &str,
        decided_by: &str,
        reason: Option<&str>,
    ) -> Result<(), crate::AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;
        let now = Utc::now().to_rfc3339();

        // Get workspace_id using the same connection
        let workspace_id: String = conn
            .query_row(
                "SELECT workspace_id FROM approval_requests WHERE id = ?1",
                rusqlite::params![approval_id],
                |row| row.get(0),
            )
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;

        let rows = conn
            .execute(
                "UPDATE approval_requests SET status = 'denied', decided_by = ?1, decided_at = ?2, decision_note = ?3 WHERE id = ?4 AND status = 'pending'",
                rusqlite::params![decided_by, now, reason, approval_id],
            )
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;

        if rows == 0 {
            return Err(crate::AgentError::NotFound(format!(
                "pending approval not found: {}",
                approval_id
            )));
        }

        drop(conn);

        self.event_bus.emit(Event::new(
            EventSource::System,
            EventType::Custom("ApprovalDecided".into()),
            serde_json::json!({
                "approval_id": approval_id,
                "decision": "denied",
                "decided_by": decided_by,
                "reason": reason,
            }),
            &workspace_id,
            None,
        ));

        info!(approval_id = %approval_id, decided_by = %decided_by, "approval denied");
        Ok(())
    }

    /// List pending approvals for a workspace.
    pub fn list_pending(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<ApprovalRecord>, crate::AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, workspace_id, requester_agent_id, requester_run_id, tool_id, tool_args, action_description, risk_level, status, decided_by, decided_at, decision_note, created_at, expires_at FROM approval_requests WHERE workspace_id = ?1 AND status = 'pending' ORDER BY created_at DESC",
            )
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;

        let records = stmt
            .query_map(rusqlite::params![workspace_id], |row| {
                let tool_args_str: String = row.get(5)?;
                Ok(ApprovalRecord {
                    id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    requester_agent_id: row.get(2)?,
                    requester_run_id: row.get(3)?,
                    tool_id: row.get(4)?,
                    tool_args: serde_json::from_str(&tool_args_str)
                        .unwrap_or(serde_json::Value::Null),
                    action_description: row.get(6)?,
                    risk_level: row.get(7)?,
                    status: row.get(8)?,
                    decided_by: row.get(9)?,
                    decided_at: row.get(10)?,
                    decision_note: row.get(11)?,
                    created_at: row.get(12)?,
                    expires_at: row.get(13)?,
                })
            })
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(records)
    }

    /// Get a specific approval record.
    pub fn get(&self, approval_id: &str) -> Result<ApprovalRecord, crate::AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;
        conn.query_row(
            "SELECT id, workspace_id, requester_agent_id, requester_run_id, tool_id, tool_args, action_description, risk_level, status, decided_by, decided_at, decision_note, created_at, expires_at FROM approval_requests WHERE id = ?1",
            rusqlite::params![approval_id],
            |row| {
                let tool_args_str: String = row.get(5)?;
                Ok(ApprovalRecord {
                    id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    requester_agent_id: row.get(2)?,
                    requester_run_id: row.get(3)?,
                    tool_id: row.get(4)?,
                    tool_args: serde_json::from_str(&tool_args_str)
                        .unwrap_or(serde_json::Value::Null),
                    action_description: row.get(6)?,
                    risk_level: row.get(7)?,
                    status: row.get(8)?,
                    decided_by: row.get(9)?,
                    decided_at: row.get(10)?,
                    decision_note: row.get(11)?,
                    created_at: row.get(12)?,
                    expires_at: row.get(13)?,
                })
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                crate::AgentError::NotFound(approval_id.to_string())
            }
            other => crate::AgentError::StorageError(other.to_string()),
        })
    }

    /// Expire stale approvals. Returns count of expired approvals.
    pub fn expire_stale(&self) -> Result<u32, crate::AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        let rows = conn
            .execute(
                "UPDATE approval_requests SET status = 'expired' WHERE status = 'pending' AND expires_at < ?1",
                rusqlite::params![now],
            )
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;

        if rows > 0 {
            info!(count = rows, "expired stale approvals");
        }
        Ok(rows as u32)
    }

    /// Wait for an approval decision using polling with sleep (not busy-wait).
    pub async fn wait_for_decision(
        &self,
        approval_id: &str,
        timeout: Duration,
    ) -> Result<ApprovalDecision, crate::AgentError> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(500);

        loop {
            // Check for expiry first
            self.expire_stale()?;

            let decision = self.check_status(approval_id)?;
            match decision {
                ApprovalDecision::Pending => {
                    if start.elapsed() >= timeout {
                        // Force expire this specific approval
                        let conn = self
                            .db
                            .conn()
                            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;
                        let _ = conn.execute(
                            "UPDATE approval_requests SET status = 'expired' WHERE id = ?1 AND status = 'pending'",
                            rusqlite::params![approval_id],
                        );
                        return Ok(ApprovalDecision::Expired);
                    }
                    tokio::time::sleep(poll_interval).await;
                }
                other => return Ok(other),
            }
        }
    }

    #[allow(dead_code)]
    fn get_workspace_id(&self, approval_id: &str) -> Result<String, crate::AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| crate::AgentError::StorageError(e.to_string()))?;
        conn.query_row(
            "SELECT workspace_id FROM approval_requests WHERE id = ?1",
            rusqlite::params![approval_id],
            |row| row.get(0),
        )
        .map_err(|e| crate::AgentError::StorageError(e.to_string()))
    }

    /// Build a Telegram notification message for an approval request.
    pub fn format_telegram_message(record: &ApprovalRecord) -> (String, Vec<Vec<nexmind_connector::InlineButton>>) {
        let risk = RiskLevel::from_str(&record.risk_level);
        let text = format!(
            "\u{26a0}\u{fe0f} <b>Approval Required</b>\n\n\
             Agent: <code>{}</code>\n\
             Action: <b>{}</b>\n\
             Tool: <code>{}</code>\n\
             Risk: {} {}\n\n\
             ID: <code>{}</code>\n\
             Expires: {}",
            record.requester_agent_id,
            record.action_description,
            record.tool_id,
            risk.emoji(),
            risk.as_str().to_uppercase(),
            record.id,
            record.expires_at,
        );

        let keyboard = vec![vec![
            nexmind_connector::InlineButton {
                text: "\u{2705} Approve".into(),
                callback_data: format!("approve:{}", record.id),
            },
            nexmind_connector::InlineButton {
                text: "\u{274c} Deny".into(),
                callback_data: format!("deny:{}", record.id),
            },
        ]];

        (text, keyboard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexmind_event_bus::EventBus;

    fn setup() -> (Arc<Database>, Arc<EventBus>, ApprovalManager) {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let db = Arc::new(db);
        let bus = Arc::new(EventBus::with_default_capacity());
        let manager = ApprovalManager::new(db.clone(), bus.clone());
        (db, bus, manager)
    }

    fn make_request() -> ApprovalRequest {
        ApprovalRequest {
            workspace_id: "ws_test".into(),
            requester_agent_id: "agt_test".into(),
            requester_run_id: "run_test".into(),
            tool_id: "shell_exec".into(),
            tool_args: serde_json::json!({"command": "ls -la"}),
            action_description: "Execute shell command: ls -la".into(),
            risk_level: RiskLevel::Medium,
            context_summary: None,
            expires_in: Duration::from_secs(3600),
        }
    }

    #[test]
    fn test_create_approval_and_check_pending() {
        let (_db, _bus, manager) = setup();
        let req = make_request();

        let id = manager.request_approval(req).unwrap();
        assert!(id.starts_with("apr_"));

        let decision = manager.check_status(&id).unwrap();
        assert_eq!(decision, ApprovalDecision::Pending);
    }

    #[test]
    fn test_approve() {
        let (_db, _bus, manager) = setup();
        let id = manager.request_approval(make_request()).unwrap();

        manager.approve(&id, "user:cli").unwrap();

        let decision = manager.check_status(&id).unwrap();
        assert_eq!(decision, ApprovalDecision::Approved);
    }

    #[test]
    fn test_deny() {
        let (_db, _bus, manager) = setup();
        let id = manager.request_approval(make_request()).unwrap();

        manager.deny(&id, "user:telegram", Some("too risky")).unwrap();

        let decision = manager.check_status(&id).unwrap();
        match decision {
            ApprovalDecision::Denied { reason } => {
                assert_eq!(reason, Some("too risky".to_string()));
            }
            other => panic!("expected Denied, got {:?}", other),
        }
    }

    #[test]
    fn test_list_pending() {
        let (_db, _bus, manager) = setup();

        // Create 3 approvals
        manager.request_approval(make_request()).unwrap();
        manager.request_approval(make_request()).unwrap();
        let id3 = manager.request_approval(make_request()).unwrap();

        // Approve one
        manager.approve(&id3, "user:cli").unwrap();

        let pending = manager.list_pending("ws_test").unwrap();
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn test_get_record() {
        let (_db, _bus, manager) = setup();
        let id = manager.request_approval(make_request()).unwrap();

        let record = manager.get(&id).unwrap();
        assert_eq!(record.tool_id, "shell_exec");
        assert_eq!(record.risk_level, "medium");
        assert_eq!(record.status, "pending");
    }

    #[test]
    fn test_expire_stale() {
        let (_db, _bus, manager) = setup();

        // Create an approval that expires immediately
        let mut req = make_request();
        req.expires_in = Duration::from_secs(0); // Already expired
        let id = manager.request_approval(req).unwrap();

        // Wait a tiny bit to ensure expiry
        std::thread::sleep(std::time::Duration::from_millis(10));

        let count = manager.expire_stale().unwrap();
        assert!(count >= 1);

        let decision = manager.check_status(&id).unwrap();
        assert_eq!(decision, ApprovalDecision::Expired);
    }

    #[tokio::test]
    async fn test_wait_for_decision_approved() {
        let (db, bus, manager) = setup();
        let id = manager.request_approval(make_request()).unwrap();

        let approval_id = id.clone();
        let db2 = db.clone();
        let bus2 = bus.clone();

        // Approve in background after a small delay
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mgr = ApprovalManager::new(db2, bus2);
            mgr.approve(&approval_id, "user:test").unwrap();
        });

        let decision = manager
            .wait_for_decision(&id, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(decision, ApprovalDecision::Approved);
    }

    #[tokio::test]
    async fn test_wait_for_decision_timeout() {
        let (_db, _bus, manager) = setup();
        let id = manager.request_approval(make_request()).unwrap();

        let decision = manager
            .wait_for_decision(&id, Duration::from_millis(200))
            .await
            .unwrap();
        assert_eq!(decision, ApprovalDecision::Expired);
    }

    #[test]
    fn test_approve_nonexistent() {
        let (_db, _bus, manager) = setup();
        let result = manager.approve("apr_nonexistent", "user:cli");
        assert!(result.is_err());
    }

    #[test]
    fn test_deny_already_approved() {
        let (_db, _bus, manager) = setup();
        let id = manager.request_approval(make_request()).unwrap();
        manager.approve(&id, "user:cli").unwrap();

        // Can't deny an already-approved request
        let result = manager.deny(&id, "user:cli", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_risk_level_display() {
        assert_eq!(RiskLevel::Low.as_str(), "low");
        assert_eq!(RiskLevel::Medium.as_str(), "medium");
        assert_eq!(RiskLevel::High.as_str(), "high");
        assert_eq!(RiskLevel::Critical.as_str(), "critical");
    }

    #[test]
    fn test_telegram_message_format() {
        let (_db, _bus, manager) = setup();
        let id = manager.request_approval(make_request()).unwrap();
        let record = manager.get(&id).unwrap();

        let (text, keyboard) = ApprovalManager::format_telegram_message(&record);
        assert!(text.contains("Approval Required"));
        assert!(text.contains("shell_exec"));
        assert!(text.contains("agt_test"));
        assert_eq!(keyboard.len(), 1);
        assert_eq!(keyboard[0].len(), 2);
        assert!(keyboard[0][0].callback_data.starts_with("approve:"));
        assert!(keyboard[0][1].callback_data.starts_with("deny:"));
    }
}
