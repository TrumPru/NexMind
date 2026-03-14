use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

/// Send message tool — routes messages through connectors (Telegram, etc.)
/// or logs them when no connector is available.
pub struct SendMessageTool;

#[async_trait::async_trait]
impl Tool for SendMessageTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "send_message".into(),
            name: "send_message".into(),
            description: "Send a message via a connector (e.g., Telegram). If no chat_id is provided, sends to the default chat.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "channel": {
                        "type": "string",
                        "description": "Channel to send via (e.g., 'telegram')"
                    },
                    "text": {
                        "type": "string",
                        "description": "Message text to send"
                    },
                    "chat_id": {
                        "type": "string",
                        "description": "Chat/recipient ID. If not provided, sends to the default chat."
                    },
                    "parse_mode": {
                        "type": "string",
                        "description": "Message formatting: 'html', 'markdown', or 'plain'",
                        "enum": ["html", "markdown", "plain"]
                    }
                },
                "required": ["channel", "text"]
            }),
            required_permissions: vec!["connector:telegram:send".into()],
            trust_level: 1,
            idempotent: false,
            timeout_seconds: 30,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("channel").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'channel' is required".into()));
        }
        if args.get("text").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'text' is required".into()));
        }
        Ok(())
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let channel = args["channel"].as_str().unwrap();
        let text = args["text"].as_str().unwrap();
        let chat_id = args.get("chat_id").and_then(|v| v.as_str()).unwrap_or("default");
        let parse_mode = args
            .get("parse_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("html");

        // Log the send attempt. In the daemon, the actual send happens via the
        // MessageRouter which intercepts the tool result and forwards through
        // the appropriate connector. For now, we return a success with the
        // parameters so the router can act on it.
        tracing::info!(
            channel = channel,
            chat_id = chat_id,
            text_len = text.len(),
            parse_mode = parse_mode,
            "send_message: queued for delivery"
        );

        let message_id = format!("msg_{}", ulid::Ulid::new());

        Ok(ToolOutput::Success {
            result: json!({
                "sent": true,
                "channel": channel,
                "chat_id": chat_id,
                "message_id": message_id,
                "text": text,
                "parse_mode": parse_mode,
            }),
            tokens_used: None,
        })
    }
}
