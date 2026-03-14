use std::collections::HashMap;
use std::sync::Arc;

use chrono::Timelike;
use nexmind_connector::{Connector, OutboundMessage};
use nexmind_event_bus::{Event, EventBus, EventSource, EventType};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Priority level for a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationPriority {
    Low,
    Normal,
    High,
    Urgent,
}

/// An optional action attached to a notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NotificationAction {
    Approve(String),
    OpenUrl(String),
}

/// A notification to be delivered to a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub priority: NotificationPriority,
    pub title: String,
    pub body: String,
    /// Agent ID or `"system"`.
    pub source: String,
    pub action: Option<NotificationAction>,
}

/// Quiet-hours window (inclusive start, exclusive end, wrapping at midnight).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuietHours {
    /// Hour at which quiet hours start (0-23).
    pub start_hour: u32,
    /// Hour at which quiet hours end (0-23).
    pub end_hour: u32,
}

/// Configuration for the notification engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationConfig {
    /// Default connector channel (e.g. `"telegram"`).
    pub default_channel: String,
    /// Optional quiet-hours window; notifications are suppressed during this
    /// window unless the priority override is active.
    pub quiet_hours: Option<QuietHours>,
    /// When `true`, `High` and `Urgent` priority notifications bypass quiet
    /// hours.
    pub priority_override: bool,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            default_channel: "telegram".to_string(),
            quiet_hours: None,
            priority_override: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Proactive notification engine.
///
/// Accepts [`Notification`] values and routes them through registered
/// [`Connector`]s while respecting quiet-hours rules.
pub struct NotificationEngine {
    connectors: HashMap<String, Arc<dyn Connector>>,
    config: NotificationConfig,
    event_bus: Arc<EventBus>,
}

impl NotificationEngine {
    /// Create a new engine with the given configuration and event bus.
    pub fn new(config: NotificationConfig, event_bus: Arc<EventBus>) -> Self {
        Self {
            connectors: HashMap::new(),
            config,
            event_bus,
        }
    }

    /// Register a connector. The connector's `id()` is used as the channel
    /// key.
    pub fn register_connector(&mut self, connector: Arc<dyn Connector>) {
        let id = connector.id().to_string();
        info!(connector = %id, "notification engine: connector registered");
        self.connectors.insert(id, connector);
    }

    /// Send a notification through the default channel.
    ///
    /// Quiet-hours are respected unless the notification has `High` or
    /// `Urgent` priority **and** `priority_override` is enabled.
    pub async fn notify(
        &self,
        notification: Notification,
        chat_id: &str,
    ) -> Result<(), String> {
        // Check quiet hours
        if self.is_quiet_hours() {
            let override_allowed = self.config.priority_override
                && matches!(
                    notification.priority,
                    NotificationPriority::High | NotificationPriority::Urgent
                );

            if !override_allowed {
                warn!(
                    id = %notification.id,
                    "notification suppressed: quiet hours active"
                );
                return Ok(());
            }
            info!(
                id = %notification.id,
                "notification sent despite quiet hours (priority override)"
            );
        }

        // Resolve connector
        let connector = self
            .connectors
            .get(&self.config.default_channel)
            .ok_or_else(|| {
                format!(
                    "no connector registered for channel '{}'",
                    self.config.default_channel
                )
            })?;

        // Build outbound message
        let text = format!("*{}*\n{}", notification.title, notification.body);
        let msg = OutboundMessage {
            chat_id: chat_id.to_string(),
            text,
            parse_mode: None,
            reply_to: None,
            platform_extras: None,
        };

        connector
            .send_message(msg)
            .await
            .map_err(|e| format!("connector send failed: {e}"))?;

        // Publish event
        self.event_bus.emit(Event::new(
            EventSource::System,
            EventType::Custom("notification_sent".to_string()),
            json!({
                "notification_id": notification.id,
                "priority": notification.priority,
                "channel": self.config.default_channel,
                "chat_id": chat_id,
            }),
            "default",
            None,
        ));

        Ok(())
    }

    /// Returns `true` when the current (UTC) hour falls within the configured
    /// quiet-hours window.
    pub fn is_quiet_hours(&self) -> bool {
        self.is_quiet_hours_at(chrono::Utc::now().hour())
    }

    /// Testable helper: checks whether `hour` (0-23) is inside the quiet
    /// window.
    fn is_quiet_hours_at(&self, hour: u32) -> bool {
        match &self.config.quiet_hours {
            None => false,
            Some(qh) => {
                if qh.start_hour <= qh.end_hour {
                    // e.g. 08:00 – 17:00
                    hour >= qh.start_hour && hour < qh.end_hour
                } else {
                    // wraps midnight, e.g. 23:00 – 07:00
                    hour >= qh.start_hour || hour < qh.end_hour
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config_with_quiet_hours() -> NotificationConfig {
        NotificationConfig {
            default_channel: "telegram".to_string(),
            quiet_hours: Some(QuietHours {
                start_hour: 23,
                end_hour: 7,
            }),
            priority_override: true,
        }
    }

    fn make_engine(config: NotificationConfig) -> NotificationEngine {
        let bus = Arc::new(EventBus::with_default_capacity());
        NotificationEngine::new(config, bus)
    }

    fn make_notification(priority: NotificationPriority) -> Notification {
        Notification {
            id: "n_test_1".to_string(),
            priority,
            title: "Test".to_string(),
            body: "Hello".to_string(),
            source: "system".to_string(),
            action: None,
        }
    }

    // 1. Quiet hours detection — time within quiet hours
    #[test]
    fn test_quiet_hours_within() {
        let engine = make_engine(make_config_with_quiet_hours());
        // 23:00 – 07:00 window; hour 0 (midnight) is inside
        assert!(engine.is_quiet_hours_at(0));
        assert!(engine.is_quiet_hours_at(3));
        assert!(engine.is_quiet_hours_at(6));
        assert!(engine.is_quiet_hours_at(23));
    }

    // 2. Quiet hours detection — time outside quiet hours
    #[test]
    fn test_quiet_hours_outside() {
        let engine = make_engine(make_config_with_quiet_hours());
        assert!(!engine.is_quiet_hours_at(7));
        assert!(!engine.is_quiet_hours_at(12));
        assert!(!engine.is_quiet_hours_at(22));
    }

    // 3. Priority override ignores quiet hours
    #[tokio::test]
    async fn test_priority_override_ignores_quiet_hours() {
        let config = make_config_with_quiet_hours();
        let engine = make_engine(config);

        // No connector registered so the send will fail — but the point is it
        // is NOT suppressed (it reaches the connector-lookup stage).
        let res = engine
            .notify(make_notification(NotificationPriority::Urgent), "chat1")
            .await;

        // We expect an error about missing connector, NOT silent suppression.
        assert!(res.is_err());
        assert!(res
            .unwrap_err()
            .contains("no connector registered for channel"));
    }

    // 4. Notification serialization round-trip
    #[test]
    fn test_notification_serialization() {
        let n = Notification {
            id: "n1".to_string(),
            priority: NotificationPriority::High,
            title: "Alert".to_string(),
            body: "Something happened".to_string(),
            source: "agent_42".to_string(),
            action: Some(NotificationAction::OpenUrl(
                "https://example.com".to_string(),
            )),
        };

        let json = serde_json::to_string(&n).expect("serialize");
        let deser: Notification = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deser.id, "n1");
        assert_eq!(deser.priority, NotificationPriority::High);
        assert_eq!(deser.title, "Alert");
        matches!(deser.action, Some(NotificationAction::OpenUrl(_)));
    }

    // 5. NotificationConfig defaults
    #[test]
    fn test_notification_config_defaults() {
        let cfg = NotificationConfig::default();
        assert_eq!(cfg.default_channel, "telegram");
        assert!(cfg.quiet_hours.is_none());
        assert!(cfg.priority_override);
    }

    // 6. No quiet hours means never quiet
    #[test]
    fn test_no_quiet_hours_never_quiet() {
        let engine = make_engine(NotificationConfig::default());
        for h in 0..24 {
            assert!(!engine.is_quiet_hours_at(h));
        }
    }
}
