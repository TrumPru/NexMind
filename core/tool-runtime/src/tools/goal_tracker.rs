use std::sync::Arc;

use chrono::Utc;
use serde_json::{json, Value};
use ulid::Ulid;

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

/// Goal tracking tool — lets the AI agent track long-running goals across sessions.
pub struct GoalTrackerTool {
    db: Arc<nexmind_storage::Database>,
}

impl GoalTrackerTool {
    pub fn new(db: Arc<nexmind_storage::Database>) -> Self {
        Self { db }
    }

    /// Ensure the `goals` table exists.
    fn ensure_table(&self) -> Result<(), ToolError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| ToolError::ExecutionError(format!("Failed to get db connection: {}", e)))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS goals (
                id TEXT PRIMARY KEY,
                title TEXT,
                description TEXT,
                status TEXT,
                progress_notes TEXT,
                created_at TEXT,
                updated_at TEXT,
                workspace_id TEXT
            );",
        )
        .map_err(|e| ToolError::ExecutionError(format!("Failed to create goals table: {}", e)))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Tool for GoalTrackerTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "goal_tracker".into(),
            name: "goal_tracker".into(),
            description: "Track long-running goals across sessions. Supports creating, updating, listing, and completing goals.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "update", "list", "complete"],
                        "description": "The action to perform"
                    },
                    "goal_id": {
                        "type": "string",
                        "description": "The goal ID (required for update/complete)"
                    },
                    "title": {
                        "type": "string",
                        "description": "Goal title (required for create)"
                    },
                    "description": {
                        "type": "string",
                        "description": "Goal description (for create)"
                    },
                    "status": {
                        "type": "string",
                        "enum": ["not_started", "in_progress", "blocked", "completed"],
                        "description": "Goal status (for update)"
                    },
                    "progress_notes": {
                        "type": "string",
                        "description": "Progress note to append (for update)"
                    }
                },
                "required": ["action"]
            }),
            required_permissions: vec!["goal:manage".into()],
            trust_level: 0,
            idempotent: false,
            timeout_seconds: 10,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::ValidationError("'action' is required".into()))?;

        match action {
            "create" => {
                if args.get("title").and_then(|v| v.as_str()).is_none() {
                    return Err(ToolError::ValidationError(
                        "'title' is required for create action".into(),
                    ));
                }
            }
            "update" | "complete" => {
                if args.get("goal_id").and_then(|v| v.as_str()).is_none() {
                    return Err(ToolError::ValidationError(
                        "'goal_id' is required for update/complete action".into(),
                    ));
                }
            }
            "list" => {}
            _ => {
                return Err(ToolError::ValidationError(format!(
                    "Invalid action '{}'. Must be one of: create, update, list, complete",
                    action
                )));
            }
        }

        Ok(())
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        self.ensure_table()?;

        let action = args["action"].as_str().unwrap();

        match action {
            "create" => {
                let id = Ulid::new().to_string();
                let title = args["title"].as_str().unwrap();
                let description = args
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let now = Utc::now().to_rfc3339();

                let conn = self.db.conn().map_err(|e| {
                    ToolError::ExecutionError(format!("Failed to get db connection: {}", e))
                })?;
                conn.execute(
                    "INSERT INTO goals (id, title, description, status, progress_notes, created_at, updated_at, workspace_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        id,
                        title,
                        description,
                        "not_started",
                        "[]",
                        now,
                        now,
                        ctx.workspace_id,
                    ],
                )
                .map_err(|e| ToolError::ExecutionError(format!("Failed to insert goal: {}", e)))?;

                Ok(ToolOutput::Success {
                    result: json!({
                        "action": "create",
                        "goal": {
                            "id": id,
                            "title": title,
                            "description": description,
                            "status": "not_started",
                            "created_at": now,
                        }
                    }),
                    tokens_used: None,
                })
            }

            "update" => {
                let goal_id = args["goal_id"].as_str().unwrap();
                let new_status = args.get("status").and_then(|v| v.as_str());
                let progress_note = args.get("progress_notes").and_then(|v| v.as_str());
                let now = Utc::now().to_rfc3339();

                let conn = self.db.conn().map_err(|e| {
                    ToolError::ExecutionError(format!("Failed to get db connection: {}", e))
                })?;

                // Fetch current goal
                let (current_status, current_notes): (String, String) = conn
                    .query_row(
                        "SELECT status, progress_notes FROM goals WHERE id = ?1 AND workspace_id = ?2",
                        rusqlite::params![goal_id, ctx.workspace_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => {
                            ToolError::ExecutionError(format!("Goal not found: {}", goal_id))
                        }
                        other => ToolError::ExecutionError(format!("Query error: {}", other)),
                    })?;

                let status = new_status.unwrap_or(&current_status);

                // Append progress note if provided
                let notes = if let Some(note) = progress_note {
                    let mut parsed: Vec<Value> =
                        serde_json::from_str(&current_notes).unwrap_or_default();
                    parsed.push(json!({
                        "note": note,
                        "timestamp": now,
                    }));
                    serde_json::to_string(&parsed).unwrap_or_else(|_| "[]".into())
                } else {
                    current_notes
                };

                conn.execute(
                    "UPDATE goals SET status = ?1, progress_notes = ?2, updated_at = ?3 WHERE id = ?4 AND workspace_id = ?5",
                    rusqlite::params![status, notes, now, goal_id, ctx.workspace_id],
                )
                .map_err(|e| ToolError::ExecutionError(format!("Failed to update goal: {}", e)))?;

                Ok(ToolOutput::Success {
                    result: json!({
                        "action": "update",
                        "goal_id": goal_id,
                        "status": status,
                        "updated_at": now,
                    }),
                    tokens_used: None,
                })
            }

            "list" => {
                let conn = self.db.conn().map_err(|e| {
                    ToolError::ExecutionError(format!("Failed to get db connection: {}", e))
                })?;

                let mut stmt = conn
                    .prepare(
                        "SELECT id, title, description, status, progress_notes, created_at, updated_at
                         FROM goals
                         WHERE workspace_id = ?1 AND status != 'completed'
                         ORDER BY updated_at DESC",
                    )
                    .map_err(|e| ToolError::ExecutionError(format!("Query error: {}", e)))?;

                let goals: Vec<Value> = stmt
                    .query_map(rusqlite::params![ctx.workspace_id], |row| {
                        let notes_str: String = row.get(4)?;
                        let notes: Value =
                            serde_json::from_str(&notes_str).unwrap_or(Value::Array(vec![]));
                        Ok(json!({
                            "id": row.get::<_, String>(0)?,
                            "title": row.get::<_, String>(1)?,
                            "description": row.get::<_, String>(2)?,
                            "status": row.get::<_, String>(3)?,
                            "progress_notes": notes,
                            "created_at": row.get::<_, String>(5)?,
                            "updated_at": row.get::<_, String>(6)?,
                        }))
                    })
                    .map_err(|e| ToolError::ExecutionError(format!("Query error: {}", e)))?
                    .filter_map(|r| r.ok())
                    .collect();

                Ok(ToolOutput::Success {
                    result: json!({
                        "action": "list",
                        "goals": goals,
                        "count": goals.len(),
                    }),
                    tokens_used: None,
                })
            }

            "complete" => {
                let goal_id = args["goal_id"].as_str().unwrap();
                let now = Utc::now().to_rfc3339();

                let conn = self.db.conn().map_err(|e| {
                    ToolError::ExecutionError(format!("Failed to get db connection: {}", e))
                })?;

                let rows_affected = conn
                    .execute(
                        "UPDATE goals SET status = 'completed', updated_at = ?1 WHERE id = ?2 AND workspace_id = ?3",
                        rusqlite::params![now, goal_id, ctx.workspace_id],
                    )
                    .map_err(|e| {
                        ToolError::ExecutionError(format!("Failed to complete goal: {}", e))
                    })?;

                if rows_affected == 0 {
                    return Ok(ToolOutput::Error {
                        error: format!("Goal not found: {}", goal_id),
                        retryable: false,
                    });
                }

                Ok(ToolOutput::Success {
                    result: json!({
                        "action": "complete",
                        "goal_id": goal_id,
                        "status": "completed",
                        "updated_at": now,
                    }),
                    tokens_used: None,
                })
            }

            _ => Ok(ToolOutput::Error {
                error: format!("Unknown action: {}", action),
                retryable: false,
            }),
        }
    }
}
