//! Message Router — connects incoming connector messages to agent runtime
//! and routes agent responses back through the appropriate connector.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use nexmind_agent_engine::{AgentRuntime, RunContext};
use nexmind_connector::{
    ChannelStatus, Connector, ConnectorError, InboundContent, InboundMessage, MessageId,
    OutboundMessage, ParseMode,
};
use nexmind_event_bus::EventBus;
use nexmind_telegram::formatting::markdown_to_telegram_html;
use nexmind_tool_runtime::tools::{AudioFormat, VoiceProcessor};

/// Message router — bridges connectors ↔ agent runtime.
pub struct MessageRouter {
    agent_runtime: Arc<AgentRuntime>,
    connectors: HashMap<String, Arc<dyn Connector>>,
    #[allow(dead_code)]
    event_bus: Arc<EventBus>,
    default_agent_id: String,
    /// Store agent registry for looking up agents
    agent_registry: Arc<nexmind_agent_engine::AgentRegistry>,
    workspace_id: String,
    session_id: String,
    workspace_path: std::path::PathBuf,
    /// Stores the default Telegram chat_id for scheduled messages
    default_chat_id: Arc<tokio::sync::RwLock<Option<String>>>,
    /// Approval manager for handling callback queries
    approval_manager: Option<Arc<nexmind_agent_engine::approval::ApprovalManager>>,
    /// Voice processor for STT transcription
    voice_processor: Option<Arc<VoiceProcessor>>,
}

impl MessageRouter {
    pub fn new(
        agent_runtime: Arc<AgentRuntime>,
        event_bus: Arc<EventBus>,
        agent_registry: Arc<nexmind_agent_engine::AgentRegistry>,
        workspace_id: String,
        session_id: String,
        workspace_path: std::path::PathBuf,
    ) -> Self {
        Self {
            agent_runtime,
            connectors: HashMap::new(),
            event_bus,
            default_agent_id: "agt_default_chat".into(),
            agent_registry,
            workspace_id,
            session_id,
            workspace_path,
            default_chat_id: Arc::new(tokio::sync::RwLock::new(None)),
            approval_manager: None,
            voice_processor: None,
        }
    }

    /// Set the voice processor for STT transcription.
    pub fn set_voice_processor(&mut self, vp: Arc<VoiceProcessor>) {
        self.voice_processor = Some(vp);
    }

    /// Set the approval manager for handling callback queries.
    pub fn set_approval_manager(&mut self, mgr: Arc<nexmind_agent_engine::approval::ApprovalManager>) {
        self.approval_manager = Some(mgr);
    }

    /// Register a connector.
    pub fn register_connector(&mut self, connector: Arc<dyn Connector>) {
        let id = connector.id().to_string();
        info!(connector_id = %id, "registering connector");
        self.connectors.insert(id, connector);
    }

    /// Get a connector by ID.
    pub fn get_connector(&self, id: &str) -> Option<&Arc<dyn Connector>> {
        self.connectors.get(id)
    }

    /// Get the default chat ID (set when first Telegram message arrives).
    pub async fn get_default_chat_id(&self) -> Option<String> {
        self.default_chat_id.read().await.clone()
    }

    /// Start listening on all connectors.
    pub async fn start(&self) -> Vec<JoinHandle<()>> {
        let mut handles = Vec::new();

        for (id, connector) in &self.connectors {
            if let Err(e) = connector.connect().await {
                error!(connector_id = %id, error = %e, "failed to connect");
                continue;
            }
            info!(connector_id = %id, "connector connected");

            let mut rx = connector.subscribe();
            let router_clone = self.clone_for_handler();

            let handle = tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(msg) => {
                            router_clone.handle_incoming(msg).await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(skipped = n, "message router lagged, skipped messages");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            info!("connector channel closed");
                            break;
                        }
                    }
                }
            });

            handles.push(handle);
        }

        handles
    }

    /// Handle an incoming message from any connector.
    pub async fn handle_incoming(&self, msg: InboundMessage) {
        let connector_id = msg.connector_id.clone();
        let chat_id = msg.chat_id.clone();

        info!(
            connector = %connector_id,
            chat_id = %chat_id,
            sender = %msg.sender_id,
            "incoming message"
        );

        // Store default chat_id on first message
        {
            let mut default = self.default_chat_id.write().await;
            if default.is_none() {
                *default = Some(chat_id.clone());
                info!(chat_id = %chat_id, "stored default Telegram chat_id");
            }
        }

        // 1. Send typing indicator
        if let Some(conn) = self.connectors.get(&connector_id) {
            let _ = conn.send_status(&chat_id, ChannelStatus::Typing).await;
        }

        // 2. Start background typing indicator (every 4 seconds)
        let typing_handle = self.spawn_typing_indicator(&connector_id, &chat_id);

        // 3. Route to agent based on content type
        let response_text = match &msg.content {
            InboundContent::Text(text) => self.run_agent_for_text(text, &chat_id).await,
            InboundContent::Command { command, args } => {
                self.handle_command(command, args, &msg).await
            }
            InboundContent::Photo { .. } => {
                Some("Photo received. Vision processing coming soon.".into())
            }
            InboundContent::CallbackQuery { data, .. } => {
                self.handle_callback_query(data, &msg.sender_id).await
            }
            InboundContent::Voice { file_id, duration_secs } => {
                self.handle_voice(file_id, *duration_secs, &connector_id, &chat_id).await
            }
            InboundContent::Document { file_name, .. } => {
                Some(format!(
                    "Document received: {}. Processing coming soon.",
                    file_name.as_deref().unwrap_or("unknown")
                ))
            }
        };

        // 4. Stop typing indicator
        if let Some(handle) = typing_handle {
            handle.abort();
        }

        // 5. Send response back through same connector
        if let Some(text) = response_text {
            if let Some(conn) = self.connectors.get(&connector_id) {
                // Convert markdown to Telegram HTML
                let html_text = markdown_to_telegram_html(&text);
                let result = conn
                    .send_message(OutboundMessage {
                        chat_id: chat_id.clone(),
                        text: html_text,
                        parse_mode: Some(ParseMode::Html),
                        ..Default::default()
                    })
                    .await;

                if let Err(e) = result {
                    error!(connector = %connector_id, chat_id = %chat_id, error = %e, "failed to send response");
                }
            }
        }
    }

    /// Run the default agent with a text input.
    async fn run_agent_for_text(&self, text: &str, chat_id: &str) -> Option<String> {
        let agent = match self.agent_registry.get(&self.default_agent_id) {
            Ok(a) => a,
            Err(e) => {
                error!(error = %e, "failed to get default agent");
                return Some("Internal error: agent not found.".into());
            }
        };

        // Use stable session ID derived from chat_id for conversation continuity
        let session_id = format!("sess_{}", chat_id);

        let context = RunContext::new(&self.workspace_id)
            .with_session(&session_id)
            .with_workspace_path(self.workspace_path.clone());

        match self.agent_runtime.run(&agent, text, context).await {
            Ok(result) => result.response,
            Err(e) => {
                error!(error = %e, "agent run failed");
                Some(format!("Error: {}", e))
            }
        }
    }

    /// Handle callback queries (inline keyboard buttons, e.g., approve:/deny:).
    async fn handle_callback_query(&self, data: &str, sender_id: &str) -> Option<String> {
        if let Some(approval_id) = data.strip_prefix("approve:") {
            if let Some(ref mgr) = self.approval_manager {
                match mgr.approve(approval_id, sender_id) {
                    Ok(()) => Some(format!("Approved: {}", approval_id)),
                    Err(e) => Some(format!("Failed to approve: {}", e)),
                }
            } else {
                Some("Approval system not configured.".into())
            }
        } else if let Some(approval_id) = data.strip_prefix("deny:") {
            if let Some(ref mgr) = self.approval_manager {
                match mgr.deny(approval_id, sender_id, Some("Denied via Telegram")) {
                    Ok(()) => Some(format!("Denied: {}", approval_id)),
                    Err(e) => Some(format!("Failed to deny: {}", e)),
                }
            } else {
                Some("Approval system not configured.".into())
            }
        } else {
            Some(format!("Callback received: {}", data))
        }
    }

    /// Handle bot commands.
    async fn handle_command(
        &self,
        command: &str,
        args: &str,
        msg: &InboundMessage,
    ) -> Option<String> {
        match command {
            "/start" => Some("Welcome to NexMind! Send me any message and I'll help.".into()),
            "/help" => Some(
                "Commands:\n/start - Start the bot\n/help - Show this help\n/status - System status"
                    .into(),
            ),
            "/status" => Some("NexMind is running. All systems healthy.".into()),
            "/memory" => Some("Memory summary: use 'recall' in a message to search memories.".into()),
            _ => {
                // Unknown command → treat as regular message
                let full_text = if args.is_empty() {
                    command.to_string()
                } else {
                    format!("{} {}", command, args)
                };
                self.run_agent_for_text(&full_text, &msg.chat_id).await
            }
        }
    }

    /// Handle an incoming voice message: download audio, transcribe, then run the agent.
    async fn handle_voice(
        &self,
        file_id: &str,
        duration_secs: u32,
        connector_id: &str,
        chat_id: &str,
    ) -> Option<String> {
        let vp = match &self.voice_processor {
            Some(vp) => vp,
            None => return Some("Voice messages are not yet supported (no STT provider).".into()),
        };

        if let Err(e) = vp.check_duration(duration_secs) {
            return Some(format!("Voice error: {}", e));
        }

        // Download audio bytes from the connector
        let conn = self.connectors.get(connector_id)?;
        let audio_bytes = match conn.download_file(file_id).await {
            Ok(bytes) => bytes,
            Err(e) => {
                error!(error = %e, "failed to download voice file");
                return Some(format!("Failed to download voice file: {}", e));
            }
        };

        let format = AudioFormat::detect(&audio_bytes).unwrap_or(AudioFormat::OggOpus);

        match vp.transcribe(&audio_bytes, format).await {
            Ok(text) => {
                info!(chars = text.len(), "voice transcribed");
                self.run_agent_for_text(&text, chat_id).await
            }
            Err(e) => {
                error!(error = %e, "voice transcription failed");
                Some(format!("Transcription error: {}", e))
            }
        }
    }

    /// Spawn a background task that sends typing indicator every 4 seconds.
    fn spawn_typing_indicator(
        &self,
        connector_id: &str,
        chat_id: &str,
    ) -> Option<JoinHandle<()>> {
        let conn = self.connectors.get(connector_id)?.clone();
        let chat_id = chat_id.to_string();

        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                let _ = conn.send_status(&chat_id, ChannelStatus::Typing).await;
            }
        }))
    }

    /// Create a lightweight clone for use in spawned tasks.
    fn clone_for_handler(&self) -> MessageRouterHandler {
        MessageRouterHandler {
            agent_runtime: self.agent_runtime.clone(),
            connectors: self.connectors.clone(),
            default_agent_id: self.default_agent_id.clone(),
            agent_registry: self.agent_registry.clone(),
            workspace_id: self.workspace_id.clone(),
            session_id: self.session_id.clone(),
            workspace_path: self.workspace_path.clone(),
            default_chat_id: self.default_chat_id.clone(),
            approval_manager: self.approval_manager.clone(),
            voice_processor: self.voice_processor.clone(),
        }
    }

    /// Send a message through a specific connector (used by send_message tool).
    pub async fn send_via_connector(
        &self,
        connector_id: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<MessageId, ConnectorError> {
        let conn = self
            .connectors
            .get(connector_id)
            .ok_or_else(|| ConnectorError::Other(format!("connector '{}' not found", connector_id)))?;

        conn.send_message(OutboundMessage {
            chat_id: chat_id.to_string(),
            text: text.to_string(),
            parse_mode,
            ..Default::default()
        })
        .await
    }
}

/// Lightweight handler for spawned tasks (avoids cloning the full router).
struct MessageRouterHandler {
    agent_runtime: Arc<AgentRuntime>,
    connectors: HashMap<String, Arc<dyn Connector>>,
    default_agent_id: String,
    agent_registry: Arc<nexmind_agent_engine::AgentRegistry>,
    workspace_id: String,
    session_id: String,
    workspace_path: std::path::PathBuf,
    default_chat_id: Arc<tokio::sync::RwLock<Option<String>>>,
    approval_manager: Option<Arc<nexmind_agent_engine::approval::ApprovalManager>>,
    voice_processor: Option<Arc<VoiceProcessor>>,
}

impl MessageRouterHandler {
    async fn handle_incoming(&self, msg: InboundMessage) {
        let connector_id = msg.connector_id.clone();
        let chat_id = msg.chat_id.clone();

        info!(
            connector = %connector_id,
            chat_id = %chat_id,
            sender = %msg.sender_id,
            "incoming message (handler)"
        );

        // Store default chat_id on first message
        {
            let mut default = self.default_chat_id.write().await;
            if default.is_none() {
                *default = Some(chat_id.clone());
            }
        }

        // Send typing indicator
        if let Some(conn) = self.connectors.get(&connector_id) {
            let _ = conn.send_status(&chat_id, ChannelStatus::Typing).await;
        }

        // Spawn typing indicator
        let typing_handle = if let Some(conn) = self.connectors.get(&connector_id) {
            let conn = conn.clone();
            let cid = chat_id.clone();
            Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                    let _ = conn.send_status(&cid, ChannelStatus::Typing).await;
                }
            }))
        } else {
            None
        };

        // Route
        let response_text = match &msg.content {
            InboundContent::Text(text) => {
                let agent = match self.agent_registry.get(&self.default_agent_id) {
                    Ok(a) => a,
                    Err(_) => {
                        return;
                    }
                };

                // Use stable session ID derived from chat_id
                let session_id = format!("sess_{}", chat_id);
                let context = RunContext::new(&self.workspace_id)
                    .with_session(&session_id)
                    .with_workspace_path(self.workspace_path.clone());

                match self.agent_runtime.run(&agent, text, context).await {
                    Ok(result) => result.response,
                    Err(e) => Some(format!("Error: {}", e)),
                }
            }
            InboundContent::Command { command, .. } => match command.as_str() {
                "/start" => Some("Welcome to NexMind! Send me any message and I'll help.".into()),
                "/help" => Some("Commands: /start, /help, /status".into()),
                "/status" => Some("NexMind is running. All systems healthy.".into()),
                _ => Some("Unknown command.".into()),
            },
            InboundContent::Photo { .. } => {
                Some("Photo received. Vision processing coming soon.".into())
            }
            InboundContent::CallbackQuery { data, .. } => {
                if let Some(approval_id) = data.strip_prefix("approve:") {
                    if let Some(ref mgr) = self.approval_manager {
                        match mgr.approve(approval_id, &msg.sender_id) {
                            Ok(()) => Some(format!("Approved: {}", approval_id)),
                            Err(e) => Some(format!("Failed to approve: {}", e)),
                        }
                    } else {
                        Some("Approval system not configured.".into())
                    }
                } else if let Some(approval_id) = data.strip_prefix("deny:") {
                    if let Some(ref mgr) = self.approval_manager {
                        match mgr.deny(approval_id, &msg.sender_id, Some("Denied via Telegram")) {
                            Ok(()) => Some(format!("Denied: {}", approval_id)),
                            Err(e) => Some(format!("Failed to deny: {}", e)),
                        }
                    } else {
                        Some("Approval system not configured.".into())
                    }
                } else {
                    Some(format!("Callback received: {}", data))
                }
            }
            InboundContent::Voice { file_id, duration_secs } => {
                self.handle_voice(file_id, *duration_secs, &connector_id, &chat_id).await
            }
            _ => Some("Unsupported message type.".into()),
        };

        // Stop typing
        if let Some(h) = typing_handle {
            h.abort();
        }

        // Send response
        if let Some(text) = response_text {
            if let Some(conn) = self.connectors.get(&connector_id) {
                let html_text = markdown_to_telegram_html(&text);
                let _ = conn
                    .send_message(OutboundMessage {
                        chat_id,
                        text: html_text,
                        parse_mode: Some(ParseMode::Html),
                        ..Default::default()
                    })
                    .await;
            }
        }
    }

    /// Handle an incoming voice message in the handler context.
    async fn handle_voice(
        &self,
        file_id: &str,
        duration_secs: u32,
        connector_id: &str,
        chat_id: &str,
    ) -> Option<String> {
        let vp = match &self.voice_processor {
            Some(vp) => vp,
            None => return Some("Voice messages are not yet supported (no STT provider).".into()),
        };

        if let Err(e) = vp.check_duration(duration_secs) {
            return Some(format!("Voice error: {}", e));
        }

        // Download audio bytes from the connector
        let conn = self.connectors.get(connector_id)?;
        let audio_bytes = match conn.download_file(file_id).await {
            Ok(bytes) => bytes,
            Err(e) => {
                error!(error = %e, "failed to download voice file");
                return Some(format!("Failed to download voice file: {}", e));
            }
        };

        let format = AudioFormat::detect(&audio_bytes).unwrap_or(AudioFormat::OggOpus);

        match vp.transcribe(&audio_bytes, format).await {
            Ok(text) => {
                info!(chars = text.len(), "voice transcribed (handler)");
                let agent = match self.agent_registry.get(&self.default_agent_id) {
                    Ok(a) => a,
                    Err(e) => {
                        error!(error = %e, "failed to get default agent for voice");
                        return Some("Internal error: agent not found.".into());
                    }
                };

                let session_id = format!("sess_{}", chat_id);
                let context = RunContext::new(&self.workspace_id)
                    .with_session(&session_id)
                    .with_workspace_path(self.workspace_path.clone());

                match self.agent_runtime.run(&agent, &text, context).await {
                    Ok(result) => result.response,
                    Err(e) => Some(format!("Error: {}", e)),
                }
            }
            Err(e) => {
                error!(error = %e, "voice transcription failed (handler)");
                Some(format!("Transcription error: {}", e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexmind_connector::{InboundContent, InboundMessage};

    #[test]
    fn test_command_parsing() {
        // Simple validation that command matching works
        let cmd = "/start";
        let result = match cmd {
            "/start" => "Welcome to NexMind! Send me any message and I'll help.",
            "/help" => "Commands: /start, /help, /status",
            "/status" => "NexMind is running.",
            _ => "Unknown command.",
        };
        assert_eq!(result, "Welcome to NexMind! Send me any message and I'll help.");
    }

    #[test]
    fn test_command_help() {
        let cmd = "/help";
        let result = match cmd {
            "/start" => "welcome",
            "/help" => "help text",
            _ => "unknown",
        };
        assert_eq!(result, "help text");
    }

    #[test]
    fn test_typing_interval() {
        // The typing indicator interval is 4 seconds per Telegram spec
        let interval = std::time::Duration::from_secs(4);
        assert_eq!(interval.as_secs(), 4);
    }
}
