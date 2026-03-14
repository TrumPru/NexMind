use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

// ── Core Types ──────────────────────────────────────────────────────

/// Connector capabilities — what a connector supports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorCapabilities {
    pub text_messages: bool,
    pub file_send: bool,
    pub file_receive: bool,
    pub typing_indicator: bool,
    pub inline_buttons: bool,
    pub rich_formatting: bool,
    pub voice_messages: bool,
    pub images: bool,
}

impl Default for ConnectorCapabilities {
    fn default() -> Self {
        Self {
            text_messages: true,
            file_send: false,
            file_receive: false,
            typing_indicator: false,
            inline_buttons: false,
            rich_formatting: false,
            voice_messages: false,
            images: false,
        }
    }
}

/// Outbound message to send through a connector.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub chat_id: String,
    pub text: String,
    pub parse_mode: Option<ParseMode>,
    pub reply_to: Option<String>,
    pub platform_extras: Option<PlatformExtras>,
}

/// Platform-specific extras for outbound messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlatformExtras {
    Telegram {
        inline_keyboard: Option<Vec<Vec<InlineButton>>>,
    },
    Discord {
        embed: Option<Value>,
    },
}

/// Inline button for interactive keyboards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineButton {
    pub text: String,
    pub callback_data: String,
}

/// Inbound message received from an external platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    pub id: String,
    pub connector_id: String,
    pub chat_id: String,
    pub sender_id: String,
    pub sender_name: Option<String>,
    pub content: InboundContent,
    pub timestamp: String,
    pub raw: Option<Value>,
}

/// Types of inbound message content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InboundContent {
    Text(String),
    Photo {
        file_id: String,
        caption: Option<String>,
    },
    Voice {
        file_id: String,
        duration_secs: u32,
    },
    Document {
        file_id: String,
        file_name: Option<String>,
        mime_type: Option<String>,
    },
    CallbackQuery {
        data: String,
        message_id: String,
    },
    Command {
        command: String,
        args: String,
    },
}

/// File payload for sending files.
#[derive(Debug, Clone)]
pub struct FilePayload {
    pub data: Vec<u8>,
    pub file_name: String,
    pub mime_type: String,
}

/// Channel status indicators.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ChannelStatus {
    Typing,
    UploadingDocument,
    UploadingPhoto,
}

/// Message parse mode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum ParseMode {
    Markdown,
    Html,
    Plain,
}

/// Message ID alias.
pub type MessageId = String;

/// Health status returned by connectors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub connected: bool,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

// ── Connector Error ─────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    #[error("Authentication failed: {0}")]
    AuthFailed(String),
    #[error("Message send failed: {0}")]
    SendFailed(String),
    #[error("Connection lost: {0}")]
    ConnectionLost(String),
    #[error("Rate limited, retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("Not connected")]
    NotConnected,
    #[error("Unsupported operation: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Other(String),
}

// ── Connector Trait ─────────────────────────────────────────────────

/// The core Connector trait — bridges external messaging services.
#[async_trait::async_trait]
pub trait Connector: Send + Sync {
    /// Unique connector identifier (e.g., "telegram", "discord").
    fn id(&self) -> &str;

    /// What this connector supports.
    fn capabilities(&self) -> ConnectorCapabilities;

    /// Connect / start receiving messages.
    async fn connect(&self) -> Result<(), ConnectorError>;

    /// Disconnect gracefully.
    async fn disconnect(&self) -> Result<(), ConnectorError>;

    /// Send a text message.
    async fn send_message(&self, msg: OutboundMessage) -> Result<MessageId, ConnectorError>;

    /// Send a file/media.
    async fn send_file(
        &self,
        file: FilePayload,
        chat_id: &str,
        caption: Option<&str>,
    ) -> Result<MessageId, ConnectorError>;

    /// Send typing/processing indicator.
    async fn send_status(
        &self,
        chat_id: &str,
        status: ChannelStatus,
    ) -> Result<(), ConnectorError>;

    /// Download a file by its platform-specific ID (e.g. Telegram file_id).
    async fn download_file(&self, _file_id: &str) -> Result<Vec<u8>, ConnectorError> {
        Err(ConnectorError::Unsupported("download_file not implemented for this connector".into()))
    }

    /// Subscribe to incoming messages.
    fn subscribe(&self) -> broadcast::Receiver<InboundMessage>;

    /// Health check.
    async fn health_check(&self) -> HealthStatus;
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inbound_message_serialization() {
        let msg = InboundMessage {
            id: "msg_001".into(),
            connector_id: "telegram".into(),
            chat_id: "12345".into(),
            sender_id: "67890".into(),
            sender_name: Some("Test User".into()),
            content: InboundContent::Text("Hello, NexMind!".into()),
            timestamp: "2025-01-01T08:00:00Z".into(),
            raw: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: InboundMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "msg_001");
        assert_eq!(deserialized.connector_id, "telegram");
        match &deserialized.content {
            InboundContent::Text(t) => assert_eq!(t, "Hello, NexMind!"),
            _ => panic!("expected Text content"),
        }
    }

    #[test]
    fn test_outbound_message_serialization() {
        let msg = OutboundMessage {
            chat_id: "12345".into(),
            text: "Hello!".into(),
            parse_mode: Some(ParseMode::Html),
            reply_to: None,
            platform_extras: Some(PlatformExtras::Telegram {
                inline_keyboard: Some(vec![vec![InlineButton {
                    text: "Yes".into(),
                    callback_data: "approve".into(),
                }]]),
            }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: OutboundMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.chat_id, "12345");
        assert_eq!(deserialized.parse_mode, Some(ParseMode::Html));
    }

    #[test]
    fn test_inbound_content_variants() {
        // Photo
        let content = InboundContent::Photo {
            file_id: "file_123".into(),
            caption: Some("A photo".into()),
        };
        let json = serde_json::to_string(&content).unwrap();
        let de: InboundContent = serde_json::from_str(&json).unwrap();
        match de {
            InboundContent::Photo { file_id, caption } => {
                assert_eq!(file_id, "file_123");
                assert_eq!(caption.unwrap(), "A photo");
            }
            _ => panic!("expected Photo"),
        }

        // Voice
        let content = InboundContent::Voice {
            file_id: "voice_1".into(),
            duration_secs: 15,
        };
        let json = serde_json::to_string(&content).unwrap();
        let de: InboundContent = serde_json::from_str(&json).unwrap();
        match de {
            InboundContent::Voice {
                file_id,
                duration_secs,
            } => {
                assert_eq!(file_id, "voice_1");
                assert_eq!(duration_secs, 15);
            }
            _ => panic!("expected Voice"),
        }

        // Command
        let content = InboundContent::Command {
            command: "/start".into(),
            args: "".into(),
        };
        let json = serde_json::to_string(&content).unwrap();
        let de: InboundContent = serde_json::from_str(&json).unwrap();
        match de {
            InboundContent::Command { command, args } => {
                assert_eq!(command, "/start");
                assert_eq!(args, "");
            }
            _ => panic!("expected Command"),
        }

        // CallbackQuery
        let content = InboundContent::CallbackQuery {
            data: "approve_123".into(),
            message_id: "msg_456".into(),
        };
        let json = serde_json::to_string(&content).unwrap();
        let de: InboundContent = serde_json::from_str(&json).unwrap();
        match de {
            InboundContent::CallbackQuery { data, message_id } => {
                assert_eq!(data, "approve_123");
                assert_eq!(message_id, "msg_456");
            }
            _ => panic!("expected CallbackQuery"),
        }

        // Document
        let content = InboundContent::Document {
            file_id: "doc_1".into(),
            file_name: Some("report.pdf".into()),
            mime_type: Some("application/pdf".into()),
        };
        let json = serde_json::to_string(&content).unwrap();
        let de: InboundContent = serde_json::from_str(&json).unwrap();
        match de {
            InboundContent::Document {
                file_id,
                file_name,
                mime_type,
            } => {
                assert_eq!(file_id, "doc_1");
                assert_eq!(file_name.unwrap(), "report.pdf");
                assert_eq!(mime_type.unwrap(), "application/pdf");
            }
            _ => panic!("expected Document"),
        }
    }

    #[test]
    fn test_connector_capabilities_default() {
        let caps = ConnectorCapabilities::default();
        assert!(caps.text_messages);
        assert!(!caps.file_send);
        assert!(!caps.typing_indicator);
    }

    #[test]
    fn test_inline_button_serialization() {
        let buttons = vec![
            vec![
                InlineButton {
                    text: "Approve".into(),
                    callback_data: "approve".into(),
                },
                InlineButton {
                    text: "Deny".into(),
                    callback_data: "deny".into(),
                },
            ],
        ];
        let json = serde_json::to_string(&buttons).unwrap();
        let de: Vec<Vec<InlineButton>> = serde_json::from_str(&json).unwrap();
        assert_eq!(de.len(), 1);
        assert_eq!(de[0].len(), 2);
        assert_eq!(de[0][0].text, "Approve");
    }

    #[test]
    fn test_connector_error_display() {
        let err = ConnectorError::AuthFailed("invalid token".into());
        assert_eq!(err.to_string(), "Authentication failed: invalid token");

        let err = ConnectorError::RateLimited { retry_after_secs: 30 };
        assert_eq!(err.to_string(), "Rate limited, retry after 30s");

        let err = ConnectorError::NotConnected;
        assert_eq!(err.to_string(), "Not connected");
    }

    #[test]
    fn test_health_status_serialization() {
        let status = HealthStatus {
            connected: true,
            latency_ms: Some(42),
            error: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        let de: HealthStatus = serde_json::from_str(&json).unwrap();
        assert!(de.connected);
        assert_eq!(de.latency_ms, Some(42));
    }
}
