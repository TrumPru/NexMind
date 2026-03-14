use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{error, info};

// ── Types ───────────────────────────────────────────────────────────

/// Scheduled job definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledJob {
    pub id: String,
    pub name: String,
    pub trigger: Trigger,
    pub action: ScheduledAction,
    pub status: JobStatus,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    pub run_count: u64,
    pub error_count: u64,
    pub missed_policy: MissedPolicy,
    pub created_at: String,
    pub updated_at: String,
    pub workspace_id: String,
}

/// Trigger type for a scheduled job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Trigger {
    Cron {
        expression: String,
        timezone: String,
    },
    Manual,
}

/// Action to execute when a job fires.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScheduledAction {
    RunAgent {
        agent_id: String,
        input: Option<String>,
    },
    RunWorkflow {
        workflow_id: String,
        params: Option<serde_json::Value>,
    },
}

/// Job status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JobStatus {
    Active,
    Paused,
    Disabled,
    Error,
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobStatus::Active => write!(f, "active"),
            JobStatus::Paused => write!(f, "paused"),
            JobStatus::Disabled => write!(f, "disabled"),
            JobStatus::Error => write!(f, "error"),
        }
    }
}

impl FromStr for JobStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(JobStatus::Active),
            "paused" => Ok(JobStatus::Paused),
            "disabled" => Ok(JobStatus::Disabled),
            "error" => Ok(JobStatus::Error),
            _ => Err(format!("unknown job status: {}", s)),
        }
    }
}

/// What to do when a cron fire time was missed (daemon was offline).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MissedPolicy {
    RunOnce,
    Skip,
    RunAllMissed,
}

impl std::fmt::Display for MissedPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MissedPolicy::RunOnce => write!(f, "run_once"),
            MissedPolicy::Skip => write!(f, "skip"),
            MissedPolicy::RunAllMissed => write!(f, "run_all_missed"),
        }
    }
}

impl FromStr for MissedPolicy {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "run_once" => Ok(MissedPolicy::RunOnce),
            "skip" => Ok(MissedPolicy::Skip),
            "run_all_missed" => Ok(MissedPolicy::RunAllMissed),
            _ => Err(format!("unknown missed policy: {}", s)),
        }
    }
}

// ── Errors ──────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error("job not found: {0}")]
    NotFound(String),
    #[error("invalid cron expression: {0}")]
    InvalidCron(String),
    #[error("invalid timezone: {0}")]
    InvalidTimezone(String),
    #[error("storage error: {0}")]
    StorageError(String),
    #[error("scheduler error: {0}")]
    InternalError(String),
}

// ── Action Handler Trait ────────────────────────────────────────────

/// Trait for executing scheduled actions (implemented by daemon).
#[async_trait::async_trait]
pub trait SchedulerActionHandler: Send + Sync {
    async fn run_agent(&self, agent_id: &str, input: Option<&str>) -> Result<(), String>;
    async fn run_workflow(
        &self,
        workflow_id: &str,
        params: Option<&serde_json::Value>,
    ) -> Result<(), String>;
}

// ── Cron Utilities ──────────────────────────────────────────────────

/// Compute the next fire time for a cron expression in a given timezone.
pub fn next_fire_time(
    cron_expr: &str,
    timezone_str: &str,
) -> Result<DateTime<Utc>, SchedulerError> {
    let schedule = cron::Schedule::from_str(cron_expr)
        .map_err(|e| SchedulerError::InvalidCron(format!("{}: {}", cron_expr, e)))?;

    let tz: chrono_tz::Tz = timezone_str
        .parse()
        .map_err(|_| SchedulerError::InvalidTimezone(timezone_str.to_string()))?;

    let now_local = Utc::now().with_timezone(&tz);

    schedule
        .after(&now_local)
        .next()
        .map(|dt| dt.with_timezone(&Utc))
        .ok_or_else(|| SchedulerError::InvalidCron("no future fire time found".into()))
}

/// Validate a cron expression.
pub fn validate_cron(cron_expr: &str) -> Result<(), SchedulerError> {
    cron::Schedule::from_str(cron_expr)
        .map_err(|e| SchedulerError::InvalidCron(format!("{}: {}", cron_expr, e)))?;
    Ok(())
}

/// Validate a timezone string.
pub fn validate_timezone(tz: &str) -> Result<(), SchedulerError> {
    tz.parse::<chrono_tz::Tz>()
        .map_err(|_| SchedulerError::InvalidTimezone(tz.to_string()))?;
    Ok(())
}

// ── Scheduler Implementation ────────────────────────────────────────

/// The scheduler — manages cron jobs in SQLite and spawns tokio timers.
pub struct SchedulerImpl {
    db: Arc<nexmind_storage::Database>,
    running_jobs: Arc<RwLock<HashMap<String, JoinHandle<()>>>>,
}

impl SchedulerImpl {
    pub fn new(db: Arc<nexmind_storage::Database>) -> Self {
        // Ensure scheduled_jobs table exists
        if let Ok(conn) = db.conn() {
            let _ = conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS scheduled_jobs (
                    id              TEXT PRIMARY KEY,
                    name            TEXT NOT NULL,
                    trigger_type    TEXT NOT NULL,
                    trigger_config  TEXT NOT NULL,
                    action_type     TEXT NOT NULL,
                    action_config   TEXT NOT NULL,
                    status          TEXT DEFAULT 'active',
                    missed_policy   TEXT DEFAULT 'run_once',
                    last_run_at     TEXT,
                    next_run_at     TEXT,
                    run_count       INTEGER DEFAULT 0,
                    error_count     INTEGER DEFAULT 0,
                    workspace_id    TEXT NOT NULL DEFAULT 'default',
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_jobs_status ON scheduled_jobs(status);
                CREATE INDEX IF NOT EXISTS idx_jobs_next_run ON scheduled_jobs(next_run_at);",
            );
        }

        Self {
            db,
            running_jobs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start the scheduler — loads all active jobs from DB, starts timers.
    pub async fn start(
        &self,
        action_handler: Arc<dyn SchedulerActionHandler>,
    ) -> Result<(), SchedulerError> {
        let jobs = self.list()?;

        // Handle missed triggers
        self.handle_missed_triggers(&jobs, action_handler.clone())
            .await?;

        // Schedule all active cron jobs
        for job in &jobs {
            if job.status != JobStatus::Active {
                continue;
            }
            if let Trigger::Cron {
                expression,
                timezone,
            } = &job.trigger
            {
                self.schedule_cron_job(&job.id, expression, timezone, action_handler.clone())
                    .await?;
            }
        }

        info!(count = jobs.len(), "scheduler loaded jobs");
        Ok(())
    }

    /// Register a new scheduled job.
    pub fn register(&self, job: ScheduledJob) -> Result<String, SchedulerError> {
        // Validate cron if applicable
        if let Trigger::Cron {
            ref expression,
            ref timezone,
        } = job.trigger
        {
            validate_cron(expression)?;
            validate_timezone(timezone)?;
        }

        let conn = self
            .db
            .conn()
            .map_err(|e| SchedulerError::StorageError(e.to_string()))?;

        let trigger_type = match &job.trigger {
            Trigger::Cron { .. } => "cron",
            Trigger::Manual => "manual",
        };
        let trigger_config =
            serde_json::to_string(&job.trigger).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
        let action_type = match &job.action {
            ScheduledAction::RunAgent { .. } => "run_agent",
            ScheduledAction::RunWorkflow { .. } => "run_workflow",
        };
        let action_config =
            serde_json::to_string(&job.action).map_err(|e| SchedulerError::StorageError(e.to_string()))?;

        // Compute next_run_at for cron jobs
        let next_run = if let Trigger::Cron {
            ref expression,
            ref timezone,
        } = job.trigger
        {
            next_fire_time(expression, timezone)
                .ok()
                .map(|dt| dt.to_rfc3339())
        } else {
            None
        };

        conn.execute(
            "INSERT INTO scheduled_jobs (id, name, trigger_type, trigger_config, action_type, action_config, status, missed_policy, next_run_at, workspace_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
            rusqlite::params![
                job.id,
                job.name,
                trigger_type,
                trigger_config,
                action_type,
                action_config,
                job.status.to_string(),
                job.missed_policy.to_string(),
                next_run,
                job.workspace_id,
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| SchedulerError::StorageError(e.to_string()))?;

        info!(job_id = %job.id, name = %job.name, "scheduled job registered");
        Ok(job.id)
    }

    /// Get a job by ID.
    pub fn get(&self, job_id: &str) -> Result<ScheduledJob, SchedulerError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| SchedulerError::StorageError(e.to_string()))?;

        conn.query_row(
            "SELECT id, name, trigger_type, trigger_config, action_type, action_config, status, missed_policy, last_run_at, next_run_at, run_count, error_count, workspace_id, created_at, updated_at
             FROM scheduled_jobs WHERE id = ?1",
            rusqlite::params![job_id],
            |row| {
                Ok(row_to_job(row))
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => SchedulerError::NotFound(job_id.to_string()),
            other => SchedulerError::StorageError(other.to_string()),
        })?
    }

    /// List all scheduled jobs.
    pub fn list(&self) -> Result<Vec<ScheduledJob>, SchedulerError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| SchedulerError::StorageError(e.to_string()))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, name, trigger_type, trigger_config, action_type, action_config, status, missed_policy, last_run_at, next_run_at, run_count, error_count, workspace_id, created_at, updated_at
                 FROM scheduled_jobs ORDER BY created_at",
            )
            .map_err(|e| SchedulerError::StorageError(e.to_string()))?;

        let jobs = stmt
            .query_map([], |row| Ok(row_to_job(row)))
            .map_err(|e| SchedulerError::StorageError(e.to_string()))?
            .filter_map(|r| r.ok().and_then(|j| j.ok()))
            .collect();

        Ok(jobs)
    }

    /// Pause a job.
    pub fn pause(&self, job_id: &str) -> Result<(), SchedulerError> {
        self.update_status(job_id, JobStatus::Paused)?;

        // Cancel the running timer
        let rt = tokio::runtime::Handle::try_current();
        if let Ok(handle) = rt {
            let jobs = self.running_jobs.clone();
            let jid = job_id.to_string();
            handle.spawn(async move {
                let mut guard = jobs.write().await;
                if let Some(h) = guard.remove(&jid) {
                    h.abort();
                }
            });
        }

        Ok(())
    }

    /// Resume a paused job.
    pub fn resume(&self, job_id: &str) -> Result<(), SchedulerError> {
        self.update_status(job_id, JobStatus::Active)?;
        Ok(())
    }

    /// Delete a job.
    pub fn delete(&self, job_id: &str) -> Result<(), SchedulerError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| SchedulerError::StorageError(e.to_string()))?;

        let rows = conn
            .execute(
                "DELETE FROM scheduled_jobs WHERE id = ?1",
                rusqlite::params![job_id],
            )
            .map_err(|e| SchedulerError::StorageError(e.to_string()))?;

        if rows == 0 {
            return Err(SchedulerError::NotFound(job_id.to_string()));
        }

        // Cancel running timer
        let rt = tokio::runtime::Handle::try_current();
        if let Ok(handle) = rt {
            let jobs = self.running_jobs.clone();
            let jid = job_id.to_string();
            handle.spawn(async move {
                let mut guard = jobs.write().await;
                if let Some(h) = guard.remove(&jid) {
                    h.abort();
                }
            });
        }

        info!(job_id = %job_id, "scheduled job deleted");
        Ok(())
    }

    /// Manually trigger a job (run now).
    pub async fn trigger(
        &self,
        job_id: &str,
        action_handler: Arc<dyn SchedulerActionHandler>,
    ) -> Result<(), SchedulerError> {
        let job = self.get(job_id)?;
        self.execute_action(&job, action_handler).await
    }

    // ── Internal ────────────────────────────────────────────────────

    fn update_status(&self, job_id: &str, status: JobStatus) -> Result<(), SchedulerError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| SchedulerError::StorageError(e.to_string()))?;

        let rows = conn
            .execute(
                "UPDATE scheduled_jobs SET status = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![status.to_string(), Utc::now().to_rfc3339(), job_id],
            )
            .map_err(|e| SchedulerError::StorageError(e.to_string()))?;

        if rows == 0 {
            return Err(SchedulerError::NotFound(job_id.to_string()));
        }

        Ok(())
    }

    fn record_run(&self, job_id: &str, success: bool) -> Result<(), SchedulerError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| SchedulerError::StorageError(e.to_string()))?;

        let now = Utc::now().to_rfc3339();

        if success {
            conn.execute(
                "UPDATE scheduled_jobs SET last_run_at = ?1, run_count = run_count + 1, updated_at = ?1 WHERE id = ?2",
                rusqlite::params![now, job_id],
            )
            .map_err(|e| SchedulerError::StorageError(e.to_string()))?;
        } else {
            conn.execute(
                "UPDATE scheduled_jobs SET last_run_at = ?1, error_count = error_count + 1, updated_at = ?1 WHERE id = ?2",
                rusqlite::params![now, job_id],
            )
            .map_err(|e| SchedulerError::StorageError(e.to_string()))?;
        }

        // Update next_run_at for cron jobs
        let job = self.get(job_id)?;
        if let Trigger::Cron {
            ref expression,
            ref timezone,
        } = job.trigger
        {
            if let Ok(next) = next_fire_time(expression, timezone) {
                conn.execute(
                    "UPDATE scheduled_jobs SET next_run_at = ?1 WHERE id = ?2",
                    rusqlite::params![next.to_rfc3339(), job_id],
                )
                .map_err(|e| SchedulerError::StorageError(e.to_string()))?;
            }
        }

        Ok(())
    }

    async fn execute_action(
        &self,
        job: &ScheduledJob,
        handler: Arc<dyn SchedulerActionHandler>,
    ) -> Result<(), SchedulerError> {
        info!(job_id = %job.id, name = %job.name, "executing scheduled action");

        let result = match &job.action {
            ScheduledAction::RunAgent { agent_id, input } => {
                handler
                    .run_agent(agent_id, input.as_deref())
                    .await
            }
            ScheduledAction::RunWorkflow { workflow_id, params } => {
                handler
                    .run_workflow(workflow_id, params.as_ref())
                    .await
            }
        };

        match result {
            Ok(()) => {
                self.record_run(&job.id, true)?;
                info!(job_id = %job.id, "scheduled action completed successfully");
                Ok(())
            }
            Err(e) => {
                self.record_run(&job.id, false)?;
                error!(job_id = %job.id, error = %e, "scheduled action failed");
                Err(SchedulerError::InternalError(e))
            }
        }
    }

    async fn schedule_cron_job(
        &self,
        job_id: &str,
        cron_expr: &str,
        timezone: &str,
        handler: Arc<dyn SchedulerActionHandler>,
    ) -> Result<(), SchedulerError> {
        let next = next_fire_time(cron_expr, timezone)?;
        let delay = next
            .signed_duration_since(Utc::now())
            .to_std()
            .unwrap_or(std::time::Duration::from_secs(1));

        let job_id_key = job_id.to_string();
        let job_id_inner = job_id.to_string();
        let cron_expr = cron_expr.to_string();
        let timezone = timezone.to_string();
        let db = self.db.clone();
        let running_jobs = self.running_jobs.clone();

        let handle = tokio::spawn(async move {
            let mut current_delay = delay;

            loop {
                tokio::time::sleep(current_delay).await;

                // Re-read job to check if still active
                let sched = SchedulerImpl {
                    db: db.clone(),
                    running_jobs: running_jobs.clone(),
                };

                let job = match sched.get(&job_id_inner) {
                    Ok(j) => j,
                    Err(_) => break,
                };

                if job.status != JobStatus::Active {
                    break;
                }

                // Execute
                if let Err(e) = sched.execute_action(&job, handler.clone()).await {
                    error!(job_id = %job_id_inner, error = %e, "cron job execution failed");
                }

                // Compute next fire time
                match next_fire_time(&cron_expr, &timezone) {
                    Ok(next) => {
                        current_delay = next
                            .signed_duration_since(Utc::now())
                            .to_std()
                            .unwrap_or(std::time::Duration::from_secs(60));
                    }
                    Err(e) => {
                        error!(error = %e, "failed to compute next fire time");
                        break;
                    }
                }
            }
        });

        let mut jobs = self.running_jobs.write().await;
        jobs.insert(job_id_key, handle);

        Ok(())
    }

    async fn handle_missed_triggers(
        &self,
        jobs: &[ScheduledJob],
        handler: Arc<dyn SchedulerActionHandler>,
    ) -> Result<(), SchedulerError> {
        let now = Utc::now();

        for job in jobs {
            if job.status != JobStatus::Active {
                continue;
            }

            if let Some(ref next_run_str) = job.next_run_at {
                if let Ok(next_run) = DateTime::parse_from_rfc3339(next_run_str) {
                    let next_run_utc = next_run.with_timezone(&Utc);
                    if next_run_utc < now {
                        // This job missed its fire time
                        match job.missed_policy {
                            MissedPolicy::RunOnce => {
                                info!(
                                    job_id = %job.id,
                                    missed_at = %next_run_str,
                                    "executing missed job (run_once policy)"
                                );
                                let _ = self.execute_action(job, handler.clone()).await;
                            }
                            MissedPolicy::Skip => {
                                info!(
                                    job_id = %job.id,
                                    missed_at = %next_run_str,
                                    "skipping missed job (skip policy)"
                                );
                                // Update next_run_at to next future time
                                if let Trigger::Cron {
                                    ref expression,
                                    ref timezone,
                                } = job.trigger
                                {
                                    if let Ok(next) = next_fire_time(expression, timezone) {
                                        let conn = self.db.conn().map_err(|e| {
                                            SchedulerError::StorageError(e.to_string())
                                        })?;
                                        let _ = conn.execute(
                                            "UPDATE scheduled_jobs SET next_run_at = ?1 WHERE id = ?2",
                                            rusqlite::params![next.to_rfc3339(), job.id],
                                        );
                                    }
                                }
                            }
                            MissedPolicy::RunAllMissed => {
                                // Run once (not all — too risky per spec)
                                info!(
                                    job_id = %job.id,
                                    missed_at = %next_run_str,
                                    "executing missed job once (run_all_missed policy, limited to one)"
                                );
                                let _ = self.execute_action(job, handler.clone()).await;
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

// ── Row conversion helper ───────────────────────────────────────────

fn row_to_job(row: &rusqlite::Row) -> Result<ScheduledJob, SchedulerError> {
    let id: String = row.get(0).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let name: String = row.get(1).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let _trigger_type: String = row.get(2).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let trigger_config: String = row.get(3).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let _action_type: String = row.get(4).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let action_config: String = row.get(5).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let status_str: String = row.get(6).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let missed_policy_str: String = row.get(7).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let last_run_at: Option<String> = row.get(8).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let next_run_at: Option<String> = row.get(9).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let run_count: i64 = row.get(10).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let error_count: i64 = row.get(11).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let workspace_id: String = row.get(12).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let created_at: String = row.get(13).map_err(|e| SchedulerError::StorageError(e.to_string()))?;
    let updated_at: String = row.get(14).map_err(|e| SchedulerError::StorageError(e.to_string()))?;

    let trigger: Trigger = serde_json::from_str(&trigger_config)
        .map_err(|e| SchedulerError::StorageError(format!("invalid trigger_config: {}", e)))?;
    let action: ScheduledAction = serde_json::from_str(&action_config)
        .map_err(|e| SchedulerError::StorageError(format!("invalid action_config: {}", e)))?;
    let status = JobStatus::from_str(&status_str)
        .map_err(SchedulerError::StorageError)?;
    let missed_policy = MissedPolicy::from_str(&missed_policy_str)
        .map_err(SchedulerError::StorageError)?;

    Ok(ScheduledJob {
        id,
        name,
        trigger,
        action,
        status,
        last_run_at,
        next_run_at,
        run_count: run_count as u64,
        error_count: error_count as u64,
        missed_policy,
        created_at,
        updated_at,
        workspace_id,
    })
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_scheduler() -> SchedulerImpl {
        let db = nexmind_storage::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        SchedulerImpl::new(Arc::new(db))
    }

    fn test_job(id: &str, name: &str) -> ScheduledJob {
        ScheduledJob {
            id: id.into(),
            name: name.into(),
            trigger: Trigger::Cron {
                expression: "0 0 8 * * *".into(),
                timezone: "UTC".into(),
            },
            action: ScheduledAction::RunAgent {
                agent_id: "agt_test".into(),
                input: None,
            },
            status: JobStatus::Active,
            last_run_at: None,
            next_run_at: None,
            run_count: 0,
            error_count: 0,
            missed_policy: MissedPolicy::RunOnce,
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
            workspace_id: "default".into(),
        }
    }

    #[test]
    fn test_cron_parsing_valid() {
        assert!(validate_cron("0 0 8 * * *").is_ok());
        assert!(validate_cron("0 30 9 * * 1-5").is_ok());
        assert!(validate_cron("0 0 0 1 * *").is_ok());
    }

    #[test]
    fn test_cron_parsing_invalid() {
        assert!(validate_cron("invalid").is_err());
        assert!(validate_cron("").is_err());
    }

    #[test]
    fn test_timezone_validation() {
        assert!(validate_timezone("UTC").is_ok());
        assert!(validate_timezone("Europe/Moscow").is_ok());
        assert!(validate_timezone("America/New_York").is_ok());
        assert!(validate_timezone("Invalid/Zone").is_err());
    }

    #[test]
    fn test_next_fire_time_utc() {
        // "Every day at 08:00" should have a next fire time
        let result = next_fire_time("0 0 8 * * *", "UTC");
        assert!(result.is_ok());
        let dt = result.unwrap();
        assert!(dt > Utc::now());
    }

    #[test]
    fn test_next_fire_time_moscow() {
        let result = next_fire_time("0 0 8 * * *", "Europe/Moscow");
        assert!(result.is_ok());
        let dt = result.unwrap();
        assert!(dt > Utc::now());
    }

    #[test]
    fn test_timezone_affects_utc_time() {
        // Same cron in different timezones should produce different UTC times
        let utc_time = next_fire_time("0 0 8 * * *", "UTC").unwrap();
        let moscow_time = next_fire_time("0 0 8 * * *", "Europe/Moscow").unwrap();
        // Moscow is UTC+3, so the UTC fire time for Moscow should be 3 hours earlier
        // (unless they happen to land on different days)
        assert_ne!(utc_time, moscow_time);
    }

    #[test]
    fn test_job_crud_create_and_list() {
        let sched = test_scheduler();

        let job = test_job("job_001", "Test Job 1");
        sched.register(job).unwrap();

        let jobs = sched.list().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "job_001");
        assert_eq!(jobs[0].name, "Test Job 1");
    }

    #[test]
    fn test_job_crud_get() {
        let sched = test_scheduler();
        let job = test_job("job_002", "Test Job 2");
        sched.register(job).unwrap();

        let retrieved = sched.get("job_002").unwrap();
        assert_eq!(retrieved.name, "Test Job 2");
        assert_eq!(retrieved.status, JobStatus::Active);
    }

    #[test]
    fn test_job_crud_pause_resume() {
        let sched = test_scheduler();
        let job = test_job("job_003", "Pausable Job");
        sched.register(job).unwrap();

        sched.pause("job_003").unwrap();
        let paused = sched.get("job_003").unwrap();
        assert_eq!(paused.status, JobStatus::Paused);

        sched.resume("job_003").unwrap();
        let resumed = sched.get("job_003").unwrap();
        assert_eq!(resumed.status, JobStatus::Active);
    }

    #[test]
    fn test_job_crud_delete() {
        let sched = test_scheduler();
        let job = test_job("job_004", "Deletable Job");
        sched.register(job).unwrap();

        sched.delete("job_004").unwrap();
        let result = sched.get("job_004");
        assert!(matches!(result, Err(SchedulerError::NotFound(_))));
    }

    #[test]
    fn test_job_not_found() {
        let sched = test_scheduler();
        let result = sched.get("nonexistent");
        assert!(matches!(result, Err(SchedulerError::NotFound(_))));
    }

    #[test]
    fn test_job_persistence() {
        let db = Arc::new(nexmind_storage::Database::open_in_memory().unwrap());
        db.run_migrations().unwrap();

        // Create scheduler, register job
        let sched = SchedulerImpl::new(db.clone());
        let job = test_job("job_005", "Persistent Job");
        sched.register(job).unwrap();

        // Create new scheduler with same DB — job should still be there
        let sched2 = SchedulerImpl::new(db);
        let jobs = sched2.list().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "job_005");
    }

    #[test]
    fn test_missed_trigger_skip_policy() {
        let sched = test_scheduler();

        // Create a job with Skip policy
        let mut job = test_job("job_006", "Skip Policy Job");
        job.missed_policy = MissedPolicy::Skip;
        sched.register(job).unwrap();

        let retrieved = sched.get("job_006").unwrap();
        assert!(matches!(retrieved.missed_policy, MissedPolicy::Skip));
        assert_eq!(retrieved.status, JobStatus::Active);
        // next_run_at should be computed from cron expression
        assert!(retrieved.next_run_at.is_some());
    }

    #[test]
    fn test_manual_trigger_job() {
        let sched = test_scheduler();
        let mut job = test_job("job_007", "Manual Job");
        job.trigger = Trigger::Manual;
        sched.register(job).unwrap();

        let retrieved = sched.get("job_007").unwrap();
        assert!(matches!(retrieved.trigger, Trigger::Manual));
    }

    #[test]
    fn test_delete_not_found() {
        let sched = test_scheduler();
        let result = sched.delete("nonexistent");
        assert!(matches!(result, Err(SchedulerError::NotFound(_))));
    }

    #[test]
    fn test_next_run_at_computed_on_register() {
        let sched = test_scheduler();
        let job = test_job("job_008", "Auto Next Run");
        sched.register(job).unwrap();

        let retrieved = sched.get("job_008").unwrap();
        assert!(retrieved.next_run_at.is_some(), "next_run_at should be computed on register");
    }

    #[test]
    fn test_job_status_display() {
        assert_eq!(JobStatus::Active.to_string(), "active");
        assert_eq!(JobStatus::Paused.to_string(), "paused");
        assert_eq!(JobStatus::Disabled.to_string(), "disabled");
        assert_eq!(JobStatus::Error.to_string(), "error");
    }

    #[test]
    fn test_missed_policy_display() {
        assert_eq!(MissedPolicy::RunOnce.to_string(), "run_once");
        assert_eq!(MissedPolicy::Skip.to_string(), "skip");
        assert_eq!(MissedPolicy::RunAllMissed.to_string(), "run_all_missed");
    }
}
