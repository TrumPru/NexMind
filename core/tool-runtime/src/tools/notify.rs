use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

/// Notify tool — creates a notification payload for the notification engine.
///
/// The actual delivery is handled at a higher level by the daemon's
/// `NotificationEngine`; this tool simply validates parameters and returns a
/// structured result that the engine can pick up.
pub struct NotifyTool;

#[async_trait::async_trait]
impl Tool for NotifyTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "notify".into(),
            name: "notify".into(),
            description: "Create a notification to be sent to the user via the notification engine.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Notification title"
                    },
                    "body": {
                        "type": "string",
                        "description": "Notification body text"
                    },
                    "priority": {
                        "type": "string",
                        "description": "Notification priority level",
                        "enum": ["low", "normal", "high", "urgent"],
                        "default": "normal"
                    }
                },
                "required": ["title", "body"]
            }),
            required_permissions: vec!["notification:send".into()],
            trust_level: 1,
            idempotent: false,
            timeout_seconds: 10,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("title").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'title' is required".into()));
        }
        if args.get("body").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'body' is required".into()));
        }
        if let Some(p) = args.get("priority").and_then(|v| v.as_str()) {
            if !["low", "normal", "high", "urgent"].contains(&p) {
                return Err(ToolError::ValidationError(format!(
                    "invalid priority '{p}'; expected low, normal, high, or urgent"
                )));
            }
        }
        Ok(())
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let title = args["title"].as_str().unwrap();
        let body = args["body"].as_str().unwrap();
        let priority = args
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("normal");

        let notification_id = format!("notif_{}", ulid::Ulid::new());

        tracing::info!(
            notification_id = %notification_id,
            title = title,
            priority = priority,
            "notify: notification created"
        );

        Ok(ToolOutput::Success {
            result: json!({
                "notification_id": notification_id,
                "title": title,
                "body": body,
                "priority": priority,
                "queued": true,
            }),
            tokens_used: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 6 (from the task numbering). NotifyTool definition has correct trust level.
    #[test]
    fn test_notify_tool_trust_level() {
        let tool = NotifyTool;
        let def = tool.definition();
        assert_eq!(def.id, "notify");
        assert_eq!(def.trust_level, 1);
    }
}
