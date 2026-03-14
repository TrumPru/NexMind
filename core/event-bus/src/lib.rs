use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;
use tracing::debug;
use ulid::Ulid;

/// Event severity level, determines persistence behavior.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventLevel {
    Debug,
    Info,
    Warn,
    Error,
    Critical,
}

/// Source of an event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EventSource {
    Agent,
    Tool,
    Workflow,
    Connector,
    System,
}

/// Event types emitted across the system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EventType {
    AgentStarted,
    AgentCompleted,
    AgentFailed,
    ToolExecuted,
    LlmCallStarted,
    LlmCallCompleted,
    CostRecorded,
    Error,
    Custom(String),
}

/// Structured event emitted by any subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub timestamp: String,
    pub source: EventSource,
    pub event_type: EventType,
    pub payload: Value,
    pub correlation_id: Option<String>,
    pub workspace_id: String,
    pub level: EventLevel,
}

impl Event {
    /// Create a new event with auto-generated ID and timestamp.
    pub fn new(
        source: EventSource,
        event_type: EventType,
        payload: Value,
        workspace_id: &str,
        correlation_id: Option<String>,
    ) -> Self {
        Self {
            id: Ulid::new().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            source,
            event_type,
            payload,
            correlation_id,
            workspace_id: workspace_id.to_string(),
            level: EventLevel::Info,
        }
    }

    /// Create with a specific level.
    pub fn with_level(mut self, level: EventLevel) -> Self {
        self.level = level;
        self
    }
}

/// In-process event bus using tokio broadcast channel.
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

impl EventBus {
    /// Create a new EventBus with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Create with default capacity of 1024.
    pub fn with_default_capacity() -> Self {
        Self::new(1024)
    }

    /// Emit an event to all subscribers (non-blocking).
    pub fn emit(&self, event: Event) {
        match self.sender.send(event) {
            Ok(n) => debug!(subscribers = n, "event emitted"),
            Err(_) => {
                debug!("event emitted with no subscribers");
            }
        }
    }

    /// Subscribe to all events. Returns a broadcast receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }

    /// Number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_event(event_type: EventType) -> Event {
        Event::new(
            EventSource::System,
            event_type,
            json!({"test": true}),
            "ws_test",
            None,
        )
    }

    #[tokio::test]
    async fn test_emit_and_receive() {
        let bus = EventBus::with_default_capacity();
        let mut rx = bus.subscribe();

        let event = make_event(EventType::AgentStarted);
        bus.emit(event);

        let received = rx.recv().await.expect("should receive event");
        assert_eq!(received.workspace_id, "ws_test");
        assert_eq!(received.event_type, EventType::AgentStarted);
    }

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let bus = EventBus::with_default_capacity();
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        assert_eq!(bus.subscriber_count(), 2);

        bus.emit(make_event(EventType::LlmCallStarted));

        let e1 = rx1.recv().await.expect("sub1 should receive");
        let e2 = rx2.recv().await.expect("sub2 should receive");

        assert_eq!(e1.event_type, EventType::LlmCallStarted);
        assert_eq!(e2.event_type, EventType::LlmCallStarted);
    }

    #[tokio::test]
    async fn test_dropped_subscriber_does_not_block() {
        let bus = EventBus::with_default_capacity();
        let rx = bus.subscribe();
        drop(rx);

        // Should not panic or block
        bus.emit(make_event(EventType::CostRecorded));
        bus.emit(make_event(EventType::AgentCompleted));
    }

    #[tokio::test]
    async fn test_event_has_ulid_and_timestamp() {
        let bus = EventBus::with_default_capacity();
        let mut rx = bus.subscribe();

        bus.emit(make_event(EventType::ToolExecuted));

        let received = rx.recv().await.unwrap();
        assert!(!received.id.is_empty());
        assert!(!received.timestamp.is_empty());
    }

    #[tokio::test]
    async fn test_correlation_id() {
        let bus = EventBus::with_default_capacity();
        let mut rx = bus.subscribe();

        let mut event = make_event(EventType::AgentStarted);
        event.correlation_id = Some("corr_123".to_string());
        bus.emit(event);

        let received = rx.recv().await.unwrap();
        assert_eq!(received.correlation_id, Some("corr_123".to_string()));
    }

    #[tokio::test]
    async fn test_event_with_level() {
        let event = make_event(EventType::Error).with_level(EventLevel::Critical);
        assert_eq!(event.level, EventLevel::Critical);
    }
}
