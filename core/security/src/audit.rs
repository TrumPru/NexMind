use hmac::{Hmac, Mac};
use sha2::Sha256;
use tracing::info;

use nexmind_storage::Database;

type HmacSha256 = Hmac<Sha256>;

/// Row from the audit_log table.
#[derive(Debug, Clone)]
pub struct AuditRow {
    pub id: String,
    pub timestamp: String,
    pub workspace_id: String,
    pub actor_type: String,
    pub actor_id: String,
    pub action: String,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub outcome: String,
    pub error_message: Option<String>,
    pub correlation_id: Option<String>,
    pub channel: String,
    pub metadata: Option<String>,
    pub prev_hmac: Option<String>,
    pub row_hmac: String,
}

/// Error from audit chain integrity verification.
#[derive(Debug)]
pub struct AuditIntegrityError {
    pub row_id: String,
    pub expected_hmac: String,
    pub actual_hmac: String,
}

/// Compute the HMAC for an audit row.
pub fn compute_row_hmac(
    hmac_key: &[u8; 32],
    id: &str,
    timestamp: &str,
    actor_id: &str,
    action: &str,
    outcome: &str,
    prev_hmac: Option<&str>,
) -> String {
    let mut mac = HmacSha256::new_from_slice(hmac_key).expect("HMAC key length is always 32 bytes");
    mac.update(id.as_bytes());
    mac.update(b"|");
    mac.update(timestamp.as_bytes());
    mac.update(b"|");
    mac.update(actor_id.as_bytes());
    mac.update(b"|");
    mac.update(action.as_bytes());
    mac.update(b"|");
    mac.update(outcome.as_bytes());
    mac.update(b"|");
    mac.update(prev_hmac.unwrap_or("GENESIS").as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Verify chain integrity for a range of audit rows.
pub fn verify_audit_chain(hmac_key: &[u8; 32], rows: &[AuditRow]) -> Option<AuditIntegrityError> {
    for (i, row) in rows.iter().enumerate() {
        let expected_prev = if i == 0 {
            None
        } else {
            Some(rows[i - 1].row_hmac.as_str())
        };
        let computed = compute_row_hmac(
            hmac_key,
            &row.id,
            &row.timestamp,
            &row.actor_id,
            &row.action,
            &row.outcome,
            expected_prev,
        );
        if computed != row.row_hmac {
            return Some(AuditIntegrityError {
                row_id: row.id.clone(),
                expected_hmac: computed,
                actual_hmac: row.row_hmac.clone(),
            });
        }
    }
    None
}

/// Writes audit events to the audit_log table with HMAC chain integrity.
pub struct AuditLogger {
    db: std::sync::Arc<Database>,
    hmac_key: [u8; 32],
}

impl AuditLogger {
    pub fn new(db: std::sync::Arc<Database>, hmac_key: [u8; 32]) -> Self {
        AuditLogger { db, hmac_key }
    }

    /// Log an audit event.
    #[allow(clippy::too_many_arguments)]
    pub fn log_event(
        &self,
        workspace_id: &str,
        actor_type: &str,
        actor_id: &str,
        action: &str,
        resource_type: Option<&str>,
        resource_id: Option<&str>,
        outcome: &str,
        error_message: Option<&str>,
        correlation_id: Option<&str>,
        channel: &str,
        metadata: Option<&str>,
    ) -> Result<String, nexmind_storage::StorageError> {
        let conn = self.db.conn()?;

        // Get previous HMAC
        let prev_hmac: Option<String> = conn
            .query_row(
                "SELECT row_hmac FROM audit_log ORDER BY rowid DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok();

        let id = ulid::Ulid::new().to_string();
        let timestamp = chrono::Utc::now().to_rfc3339();

        let row_hmac = compute_row_hmac(
            &self.hmac_key,
            &id,
            &timestamp,
            actor_id,
            action,
            outcome,
            prev_hmac.as_deref(),
        );

        conn.execute(
            "INSERT INTO audit_log (id, timestamp, workspace_id, actor_type, actor_id, action, resource_type, resource_id, outcome, error_message, correlation_id, channel, metadata, prev_hmac, row_hmac) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                id, timestamp, workspace_id,
                actor_type, actor_id, action,
                resource_type, resource_id,
                outcome, error_message,
                correlation_id, channel, metadata,
                prev_hmac, row_hmac
            ],
        )?;

        info!(
            audit_id = %id,
            action = action,
            outcome = outcome,
            "audit event logged"
        );

        Ok(id)
    }

    /// Retrieve audit rows for chain verification.
    pub fn get_rows(&self, limit: usize) -> Result<Vec<AuditRow>, nexmind_storage::StorageError> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, workspace_id, actor_type, actor_id, action, resource_type, resource_id, outcome, error_message, correlation_id, channel, metadata, prev_hmac, row_hmac FROM audit_log ORDER BY rowid ASC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![limit], |row| {
                Ok(AuditRow {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    workspace_id: row.get(2)?,
                    actor_type: row.get(3)?,
                    actor_id: row.get(4)?,
                    action: row.get(5)?,
                    resource_type: row.get(6)?,
                    resource_id: row.get(7)?,
                    outcome: row.get(8)?,
                    error_message: row.get(9)?,
                    correlation_id: row.get(10)?,
                    channel: row.get(11)?,
                    metadata: row.get(12)?,
                    prev_hmac: row.get(13)?,
                    row_hmac: row.get(14)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Get the HMAC key (for verification).
    pub fn hmac_key(&self) -> &[u8; 32] {
        &self.hmac_key
    }
}
