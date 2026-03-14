use std::sync::Arc;

use chrono::Utc;
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

/// Schedule task tool — create, list, and delete scheduled jobs via the scheduler.
pub struct ScheduleTaskTool {
    scheduler: Arc<nexmind_scheduler::SchedulerImpl>,
}

impl ScheduleTaskTool {
    pub fn new(scheduler: Arc<nexmind_scheduler::SchedulerImpl>) -> Self {
        Self { scheduler }
    }
}

#[async_trait::async_trait]
impl Tool for ScheduleTaskTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "schedule_task".into(),
            name: "schedule_task".into(),
            description: "Manage scheduled tasks: create, list, or delete recurring jobs that run on a cron schedule.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "The action to perform",
                        "enum": ["create", "list", "delete"]
                    },
                    "cron": {
                        "type": "string",
                        "description": "Cron expression for the schedule (required for 'create')"
                    },
                    "description": {
                        "type": "string",
                        "description": "Human-readable description of the scheduled task"
                    },
                    "agent_id": {
                        "type": "string",
                        "description": "Agent ID to run when the job fires (defaults to 'agt_default_chat')",
                        "default": "agt_default_chat"
                    },
                    "input": {
                        "type": "string",
                        "description": "The message/task to send to the agent when the job fires"
                    },
                    "job_id": {
                        "type": "string",
                        "description": "Job ID to delete (required for 'delete')"
                    }
                },
                "required": ["action"]
            }),
            required_permissions: vec!["scheduler:manage".into()],
            trust_level: 1,
            idempotent: false,
            timeout_seconds: 10,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        let action = args.get("action").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::ValidationError("'action' is required".into()))?;

        if !["create", "list", "delete"].contains(&action) {
            return Err(ToolError::ValidationError(format!(
                "invalid action '{action}'; expected create, list, or delete"
            )));
        }

        match action {
            "create" => {
                if args.get("cron").and_then(|v| v.as_str()).is_none() {
                    return Err(ToolError::ValidationError(
                        "'cron' is required for 'create' action".into(),
                    ));
                }
                if args.get("description").and_then(|v| v.as_str()).is_none() {
                    return Err(ToolError::ValidationError(
                        "'description' is required for 'create' action".into(),
                    ));
                }
            }
            "delete" => {
                if args.get("job_id").and_then(|v| v.as_str()).is_none() {
                    return Err(ToolError::ValidationError(
                        "'job_id' is required for 'delete' action".into(),
                    ));
                }
            }
            _ => {}
        }

        Ok(())
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let action = args["action"].as_str().unwrap();

        match action {
            "create" => {
                let cron_expr = args["cron"].as_str().unwrap();
                let description = args["description"].as_str().unwrap();
                let agent_id = args
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("agt_default_chat");
                let input = args.get("input").and_then(|v| v.as_str()).map(String::from);

                let job_id = format!("job_{}", ulid::Ulid::new());
                let now = Utc::now().to_rfc3339();

                let job = nexmind_scheduler::ScheduledJob {
                    id: job_id.clone(),
                    name: description.to_string(),
                    trigger: nexmind_scheduler::Trigger::Cron {
                        expression: cron_expr.to_string(),
                        timezone: "UTC".into(),
                    },
                    action: nexmind_scheduler::ScheduledAction::RunAgent {
                        agent_id: agent_id.to_string(),
                        input,
                    },
                    status: nexmind_scheduler::JobStatus::Active,
                    last_run_at: None,
                    next_run_at: None,
                    run_count: 0,
                    error_count: 0,
                    missed_policy: nexmind_scheduler::MissedPolicy::RunOnce,
                    created_at: now.clone(),
                    updated_at: now,
                    workspace_id: "default".into(),
                };

                self.scheduler.register(job).map_err(|e| {
                    ToolError::ExecutionError(format!("failed to register job: {}", e))
                })?;

                tracing::info!(
                    job_id = %job_id,
                    cron = %cron_expr,
                    description = %description,
                    "schedule_task: job created"
                );

                Ok(ToolOutput::Success {
                    result: json!({
                        "job_id": job_id,
                        "description": description,
                        "cron": cron_expr,
                        "agent_id": agent_id,
                        "status": "active",
                        "created": true,
                    }),
                    tokens_used: None,
                })
            }

            "list" => {
                let jobs = self.scheduler.list().map_err(|e| {
                    ToolError::ExecutionError(format!("failed to list jobs: {}", e))
                })?;

                let jobs_json: Vec<Value> = jobs
                    .iter()
                    .map(|j| {
                        json!({
                            "id": j.id,
                            "name": j.name,
                            "status": j.status.to_string(),
                            "last_run_at": j.last_run_at,
                            "next_run_at": j.next_run_at,
                            "run_count": j.run_count,
                            "error_count": j.error_count,
                            "created_at": j.created_at,
                        })
                    })
                    .collect();

                Ok(ToolOutput::Success {
                    result: json!({
                        "jobs": jobs_json,
                        "total": jobs_json.len(),
                    }),
                    tokens_used: None,
                })
            }

            "delete" => {
                let job_id = args["job_id"].as_str().unwrap();

                self.scheduler.delete(job_id).map_err(|e| {
                    ToolError::ExecutionError(format!("failed to delete job: {}", e))
                })?;

                tracing::info!(job_id = %job_id, "schedule_task: job deleted");

                Ok(ToolOutput::Success {
                    result: json!({
                        "job_id": job_id,
                        "deleted": true,
                    }),
                    tokens_used: None,
                })
            }

            _ => Err(ToolError::ValidationError(format!(
                "unknown action: {action}"
            ))),
        }
    }
}
