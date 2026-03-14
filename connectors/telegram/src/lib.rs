use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::broadcast;
use tracing::info;

use nexmind_connector::{
    ChannelStatus, Connector, ConnectorCapabilities, ConnectorError, FilePayload, HealthStatus,
    InboundMessage, MessageId, OutboundMessage,
};

pub mod formatting;

#[cfg(feature = "live")]
pub mod live;

// ── Configuration ───────────────────────────────────────────────────

/// Telegram connector configuration.
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub allowed_user_ids: Vec<i64>,
    pub mode: TelegramMode,
}

/// Telegram transport mode.
#[derive(Debug, Clone)]
pub enum TelegramMode {
    LongPoll,
    Webhook { url: String, port: u16 },
}

impl TelegramConfig {
    /// Create config from environment variables.
    /// TELEGRAM_BOT_TOKEN (required), TELEGRAM_ALLOWED_USERS (comma-separated, optional).
    pub fn from_env() -> Result<Self, ConnectorError> {
        let bot_token = std::env::var("TELEGRAM_BOT_TOKEN")
            .map_err(|_| ConnectorError::AuthFailed("TELEGRAM_BOT_TOKEN not set".into()))?;

        let allowed_user_ids = std::env::var("TELEGRAM_ALLOWED_USERS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .filter_map(|s| s.trim().parse::<i64>().ok())
            .collect();

        Ok(Self {
            bot_token,
            allowed_user_ids,
            mode: TelegramMode::LongPoll,
        })
    }
}

// ── Telegram Connector ──────────────────────────────────────────────

/// Telegram connector — bridges NexMind to Telegram Bot API.
///
/// Uses long-polling mode for MVP. The `live` feature enables actual
/// Telegram API calls via `teloxide`. Without it, the connector operates
/// in stub mode for testing.
pub struct TelegramConnector {
    config: TelegramConfig,
    incoming_tx: broadcast::Sender<InboundMessage>,
    running: Arc<AtomicBool>,
}

impl TelegramConnector {
    pub fn new(config: TelegramConfig) -> Self {
        let (incoming_tx, _) = broadcast::channel(256);
        Self {
            config,
            incoming_tx,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Check if a user ID is in the allowlist.
    /// If the allowlist is empty, all users are allowed.
    pub fn is_user_allowed(&self, user_id: i64) -> bool {
        if self.config.allowed_user_ids.is_empty() {
            return true;
        }
        self.config.allowed_user_ids.contains(&user_id)
    }

    /// Get the broadcast sender for injecting messages (used for testing/integration).
    pub fn sender(&self) -> broadcast::Sender<InboundMessage> {
        self.incoming_tx.clone()
    }

    /// Get the config reference.
    pub fn config(&self) -> &TelegramConfig {
        &self.config
    }

    /// Check if the connector is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl Connector for TelegramConnector {
    fn id(&self) -> &str {
        "telegram"
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            text_messages: true,
            file_send: true,
            file_receive: true,
            typing_indicator: true,
            inline_buttons: true,
            rich_formatting: true,
            voice_messages: true,
            images: true,
        }
    }

    async fn connect(&self) -> Result<(), ConnectorError> {
        if self.running.load(Ordering::Relaxed) {
            return Ok(());
        }

        info!("Telegram connector starting (long-poll mode)");
        self.running.store(true, Ordering::Relaxed);

        #[cfg(feature = "live")]
        {
            // Start long-polling in background
            live::start_polling(
                self.config.bot_token.clone(),
                self.config.allowed_user_ids.clone(),
                self.incoming_tx.clone(),
                self.running.clone(),
            );
        }

        Ok(())
    }

    async fn disconnect(&self) -> Result<(), ConnectorError> {
        info!("Telegram connector stopping");
        self.running.store(false, Ordering::Relaxed);
        Ok(())
    }

    async fn send_message(&self, msg: OutboundMessage) -> Result<MessageId, ConnectorError> {
        if !self.running.load(Ordering::Relaxed) {
            return Err(ConnectorError::NotConnected);
        }

        // In live mode, this delegates to the Telegram Bot API.
        // In stub mode, we just log and return a fake ID.
        #[cfg(feature = "live")]
        {
            return live::send_message_live(&self.config.bot_token, msg).await;
        }

        #[cfg(not(feature = "live"))]
        {
            info!(
                chat_id = %msg.chat_id,
                text_len = msg.text.len(),
                parse_mode = ?msg.parse_mode,
                "telegram send_message (stub)"
            );
            Ok(format!("tg_stub_{}", ulid::Ulid::new()))
        }
    }

    async fn send_file(
        &self,
        file: FilePayload,
        chat_id: &str,
        caption: Option<&str>,
    ) -> Result<MessageId, ConnectorError> {
        if !self.running.load(Ordering::Relaxed) {
            return Err(ConnectorError::NotConnected);
        }

        #[cfg(feature = "live")]
        {
            return live::send_file_live(&self.config.bot_token, file, chat_id, caption).await;
        }

        #[cfg(not(feature = "live"))]
        {
            info!(
                chat_id = %chat_id,
                file_name = %file.file_name,
                mime_type = %file.mime_type,
                size = file.data.len(),
                caption = ?caption,
                "telegram send_file (stub)"
            );
            Ok(format!("tg_file_stub_{}", ulid::Ulid::new()))
        }
    }

    async fn send_status(
        &self,
        chat_id: &str,
        status: ChannelStatus,
    ) -> Result<(), ConnectorError> {
        if !self.running.load(Ordering::Relaxed) {
            return Err(ConnectorError::NotConnected);
        }

        #[cfg(feature = "live")]
        {
            return live::send_chat_action_live(&self.config.bot_token, chat_id, status).await;
        }

        #[cfg(not(feature = "live"))]
        {
            info!(
                chat_id = %chat_id,
                status = ?status,
                "telegram send_status (stub)"
            );
            Ok(())
        }
    }

    async fn download_file(&self, file_id: &str) -> Result<Vec<u8>, ConnectorError> {
        if !self.running.load(Ordering::Relaxed) {
            return Err(ConnectorError::NotConnected);
        }

        #[cfg(feature = "live")]
        {
            return live::download_file(&self.config.bot_token, file_id).await;
        }

        #[cfg(not(feature = "live"))]
        {
            info!(file_id = %file_id, "telegram download_file (stub)");
            // Return minimal OGG header for stub mode
            Ok(b"OggS\x00\x02\x00\x00\x00\x00\x00\x00\x00\x00".to_vec())
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<InboundMessage> {
        self.incoming_tx.subscribe()
    }

    async fn health_check(&self) -> HealthStatus {
        #[cfg(feature = "live")]
        {
            return live::health_check_live(&self.config.bot_token).await;
        }

        #[cfg(not(feature = "live"))]
        {
            HealthStatus {
                connected: self.running.load(Ordering::Relaxed),
                latency_ms: Some(0),
                error: None,
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexmind_connector::{InboundContent, ParseMode};

    fn test_config() -> TelegramConfig {
        TelegramConfig {
            bot_token: "test_token_123".into(),
            allowed_user_ids: vec![111, 222],
            mode: TelegramMode::LongPoll,
        }
    }

    #[test]
    fn test_user_allowlist() {
        let conn = TelegramConnector::new(test_config());
        assert!(conn.is_user_allowed(111));
        assert!(conn.is_user_allowed(222));
        assert!(!conn.is_user_allowed(333));
    }

    #[test]
    fn test_empty_allowlist_allows_all() {
        let config = TelegramConfig {
            bot_token: "test".into(),
            allowed_user_ids: vec![],
            mode: TelegramMode::LongPoll,
        };
        let conn = TelegramConnector::new(config);
        assert!(conn.is_user_allowed(999));
    }

    #[test]
    fn test_connector_id() {
        let conn = TelegramConnector::new(test_config());
        assert_eq!(conn.id(), "telegram");
    }

    #[test]
    fn test_capabilities() {
        let conn = TelegramConnector::new(test_config());
        let caps = conn.capabilities();
        assert!(caps.text_messages);
        assert!(caps.file_send);
        assert!(caps.file_receive);
        assert!(caps.typing_indicator);
        assert!(caps.inline_buttons);
        assert!(caps.rich_formatting);
        assert!(caps.images);
    }

    #[tokio::test]
    async fn test_connect_disconnect() {
        let conn = TelegramConnector::new(test_config());
        assert!(!conn.is_running());

        conn.connect().await.unwrap();
        assert!(conn.is_running());

        conn.disconnect().await.unwrap();
        assert!(!conn.is_running());
    }

    #[tokio::test]
    async fn test_send_message_stub() {
        let conn = TelegramConnector::new(test_config());
        conn.connect().await.unwrap();

        let msg = OutboundMessage {
            chat_id: "12345".into(),
            text: "Hello from test!".into(),
            parse_mode: Some(ParseMode::Html),
            ..Default::default()
        };
        let result = conn.send_message(msg).await;
        assert!(result.is_ok());
        let msg_id = result.unwrap();
        assert!(msg_id.starts_with("tg_stub_"));
    }

    #[tokio::test]
    async fn test_send_message_not_connected() {
        let conn = TelegramConnector::new(test_config());
        // Don't connect

        let msg = OutboundMessage {
            chat_id: "12345".into(),
            text: "Should fail".into(),
            ..Default::default()
        };
        let result = conn.send_message(msg).await;
        assert!(matches!(result, Err(ConnectorError::NotConnected)));
    }

    #[tokio::test]
    async fn test_send_file_stub() {
        let conn = TelegramConnector::new(test_config());
        conn.connect().await.unwrap();

        let file = FilePayload {
            data: vec![1, 2, 3, 4],
            file_name: "test.pdf".into(),
            mime_type: "application/pdf".into(),
        };
        let result = conn.send_file(file, "12345", Some("Test file")).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_status_stub() {
        let conn = TelegramConnector::new(test_config());
        conn.connect().await.unwrap();

        let result = conn.send_status("12345", ChannelStatus::Typing).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_health_check_stub() {
        let conn = TelegramConnector::new(test_config());
        conn.connect().await.unwrap();

        let health = conn.health_check().await;
        assert!(health.connected);
        assert!(health.error.is_none());
    }

    #[tokio::test]
    async fn test_subscribe_receives_messages() {
        let conn = TelegramConnector::new(test_config());
        conn.connect().await.unwrap();

        let mut rx = conn.subscribe();

        // Inject a message via the sender
        let msg = InboundMessage {
            id: "msg_1".into(),
            connector_id: "telegram".into(),
            chat_id: "12345".into(),
            sender_id: "111".into(),
            sender_name: Some("Test User".into()),
            content: InboundContent::Text("Hello!".into()),
            timestamp: "2025-01-01T08:00:00Z".into(),
            raw: None,
        };
        conn.sender().send(msg).unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.id, "msg_1");
        match &received.content {
            InboundContent::Text(t) => assert_eq!(t, "Hello!"),
            _ => panic!("expected text"),
        }
    }

    #[tokio::test]
    async fn test_inline_keyboard_in_outbound() {
        let conn = TelegramConnector::new(test_config());
        conn.connect().await.unwrap();

        let msg = OutboundMessage {
            chat_id: "12345".into(),
            text: "Approve this action?".into(),
            parse_mode: Some(ParseMode::Html),
            reply_to: None,
            platform_extras: Some(nexmind_connector::PlatformExtras::Telegram {
                inline_keyboard: Some(vec![vec![
                    nexmind_connector::InlineButton {
                        text: "Approve".into(),
                        callback_data: "approve_123".into(),
                    },
                    nexmind_connector::InlineButton {
                        text: "Deny".into(),
                        callback_data: "deny_123".into(),
                    },
                ]]),
            }),
        };

        let result = conn.send_message(msg).await;
        assert!(result.is_ok());
    }
}
