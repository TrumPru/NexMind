//! Live Telegram Bot API implementation using reqwest.
//! Only compiled when the `live` feature is enabled.

use std::time::Instant;

use tracing::{info, warn, error};

use nexmind_connector::{
    ChannelStatus, ConnectorError, FilePayload, HealthStatus, MessageId, OutboundMessage,
    ParseMode, PlatformExtras,
};

const TELEGRAM_API_BASE: &str = "https://api.telegram.org/bot";

/// Send a text message via the Telegram Bot API.
pub async fn send_message_live(
    bot_token: &str,
    msg: OutboundMessage,
) -> Result<MessageId, ConnectorError> {
    let url = format!("{}{}/sendMessage", TELEGRAM_API_BASE, bot_token);
    let client = reqwest::Client::new();

    let mut body = serde_json::json!({
        "chat_id": msg.chat_id,
        "text": msg.text,
    });

    // Set parse mode
    if let Some(pm) = &msg.parse_mode {
        let mode_str = match pm {
            ParseMode::Markdown => "MarkdownV2",
            ParseMode::Html => "HTML",
            ParseMode::Plain => "",
        };
        if !mode_str.is_empty() {
            body["parse_mode"] = serde_json::Value::String(mode_str.to_string());
        }
    }

    // Set reply_to
    if let Some(reply_to) = &msg.reply_to {
        if let Ok(msg_id) = reply_to.parse::<i64>() {
            body["reply_to_message_id"] = serde_json::json!(msg_id);
        }
    }

    // Set inline keyboard
    if let Some(PlatformExtras::Telegram {
        inline_keyboard: Some(ref keyboard),
    }) = msg.platform_extras
    {
        let kb: Vec<Vec<serde_json::Value>> = keyboard
            .iter()
            .map(|row| {
                row.iter()
                    .map(|btn| {
                        serde_json::json!({
                            "text": btn.text,
                            "callback_data": btn.callback_data,
                        })
                    })
                    .collect()
            })
            .collect();
        body["reply_markup"] = serde_json::json!({
            "inline_keyboard": kb,
        });
    }

    // Retry on rate limit
    for attempt in 0..3 {
        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ConnectorError::SendFailed(e.to_string()))?;

        let status = resp.status();
        let resp_body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ConnectorError::SendFailed(e.to_string()))?;

        if status.as_u16() == 429 {
            let retry_after = resp_body["parameters"]["retry_after"]
                .as_u64()
                .unwrap_or(5);
            warn!(retry_after, attempt, "Telegram rate limited");
            tokio::time::sleep(std::time::Duration::from_secs(retry_after)).await;
            continue;
        }

        if !resp_body["ok"].as_bool().unwrap_or(false) {
            let description = resp_body["description"]
                .as_str()
                .unwrap_or("unknown error");
            return Err(ConnectorError::SendFailed(description.to_string()));
        }

        let message_id = resp_body["result"]["message_id"]
            .as_i64()
            .unwrap_or(0)
            .to_string();

        return Ok(message_id);
    }

    Err(ConnectorError::SendFailed(
        "Max retries exceeded due to rate limiting".into(),
    ))
}

/// Send a file via the Telegram Bot API.
pub async fn send_file_live(
    bot_token: &str,
    file: FilePayload,
    chat_id: &str,
    caption: Option<&str>,
) -> Result<MessageId, ConnectorError> {
    let is_image = file.mime_type.starts_with("image/");
    let endpoint = if is_image { "sendPhoto" } else { "sendDocument" };
    let url = format!("{}{}/{}", TELEGRAM_API_BASE, bot_token, endpoint);

    let client = reqwest::Client::new();

    let field_name = if is_image { "photo" } else { "document" };
    let part = reqwest::multipart::Part::bytes(file.data)
        .file_name(file.file_name)
        .mime_str(&file.mime_type)
        .map_err(|e| ConnectorError::SendFailed(e.to_string()))?;

    let mut form = reqwest::multipart::Form::new()
        .text("chat_id", chat_id.to_string())
        .part(field_name, part);

    if let Some(cap) = caption {
        form = form.text("caption", cap.to_string());
    }

    let resp = client
        .post(&url)
        .multipart(form)
        .send()
        .await
        .map_err(|e| ConnectorError::SendFailed(e.to_string()))?;

    let resp_body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| ConnectorError::SendFailed(e.to_string()))?;

    if !resp_body["ok"].as_bool().unwrap_or(false) {
        let desc = resp_body["description"]
            .as_str()
            .unwrap_or("unknown error");
        return Err(ConnectorError::SendFailed(desc.to_string()));
    }

    let message_id = resp_body["result"]["message_id"]
        .as_i64()
        .unwrap_or(0)
        .to_string();

    Ok(message_id)
}

/// Send a chat action (typing indicator) via the Telegram Bot API.
pub async fn send_chat_action_live(
    bot_token: &str,
    chat_id: &str,
    status: ChannelStatus,
) -> Result<(), ConnectorError> {
    let action = match status {
        ChannelStatus::Typing => "typing",
        ChannelStatus::UploadingDocument => "upload_document",
        ChannelStatus::UploadingPhoto => "upload_photo",
    };

    let url = format!("{}{}/sendChatAction", TELEGRAM_API_BASE, bot_token);
    let client = reqwest::Client::new();

    let body = serde_json::json!({
        "chat_id": chat_id,
        "action": action,
    });

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| ConnectorError::SendFailed(e.to_string()))?;

    if !resp.status().is_success() {
        warn!(chat_id, action, "failed to send chat action");
    }

    Ok(())
}

/// Health check — calls getMe to validate the bot token.
pub async fn health_check_live(bot_token: &str) -> HealthStatus {
    let url = format!("{}{}/getMe", TELEGRAM_API_BASE, bot_token);
    let client = reqwest::Client::new();
    let start = Instant::now();

    match client.get(&url).send().await {
        Ok(resp) => {
            let latency = start.elapsed().as_millis() as u64;
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            let ok = body["ok"].as_bool().unwrap_or(false);

            if ok {
                HealthStatus {
                    connected: true,
                    latency_ms: Some(latency),
                    error: None,
                }
            } else {
                let desc = body["description"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                HealthStatus {
                    connected: false,
                    latency_ms: Some(latency),
                    error: Some(desc),
                }
            }
        }
        Err(e) => HealthStatus {
            connected: false,
            latency_ms: None,
            error: Some(e.to_string()),
        },
    }
}

/// Start long-polling for incoming messages.
/// Spawns a background task that polls getUpdates and sends parsed messages
/// through the broadcast channel.
pub fn start_polling(
    bot_token: String,
    allowed_user_ids: Vec<i64>,
    sender: tokio::sync::broadcast::Sender<nexmind_connector::InboundMessage>,
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut offset: i64 = 0;

        info!("Telegram long-polling started");

        while running.load(std::sync::atomic::Ordering::Relaxed) {
            let url = format!("{}{}/getUpdates", TELEGRAM_API_BASE, bot_token);

            let body = serde_json::json!({
                "offset": offset,
                "timeout": 30,
                "allowed_updates": ["message", "callback_query"]
            });

            match client.post(&url).json(&body).send().await {
                Ok(resp) => {
                    let body: serde_json::Value = match resp.json().await {
                        Ok(b) => b,
                        Err(e) => {
                            error!("Failed to parse getUpdates response: {}", e);
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            continue;
                        }
                    };

                    if !body["ok"].as_bool().unwrap_or(false) {
                        error!("getUpdates error: {:?}", body["description"]);
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }

                    let updates = match body["result"].as_array() {
                        Some(arr) => arr,
                        None => continue,
                    };

                    for update in updates {
                        let update_id = update["update_id"].as_i64().unwrap_or(0);
                        offset = update_id + 1;

                        // Parse message or callback_query
                        if let Some(inbound) = parse_update(update, &allowed_user_ids) {
                            let _ = sender.send(inbound);
                        }
                    }
                }
                Err(e) => {
                    error!("getUpdates request failed: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }

        info!("Telegram long-polling stopped");
    })
}

/// Parse a single Telegram Update into an InboundMessage.
fn parse_update(
    update: &serde_json::Value,
    allowed_user_ids: &[i64],
) -> Option<nexmind_connector::InboundMessage> {
    use nexmind_connector::{InboundContent, InboundMessage};

    // Handle callback_query
    if let Some(cb) = update.get("callback_query") {
        let from = cb.get("from")?;
        let sender_id = from["id"].as_i64()?;

        if !allowed_user_ids.is_empty() && !allowed_user_ids.contains(&sender_id) {
            warn!(sender_id, "callback from non-allowed user, ignoring");
            return None;
        }

        let data = cb["data"].as_str().unwrap_or("").to_string();
        let message_id = cb["message"]["message_id"].as_i64().unwrap_or(0).to_string();
        let chat_id = cb["message"]["chat"]["id"].as_i64()?.to_string();

        return Some(InboundMessage {
            id: cb["id"].as_str().unwrap_or("").to_string(),
            connector_id: "telegram".into(),
            chat_id,
            sender_id: sender_id.to_string(),
            sender_name: from["first_name"].as_str().map(|s| s.to_string()),
            content: InboundContent::CallbackQuery { data, message_id },
            timestamp: chrono::Utc::now().to_rfc3339(),
            raw: Some(cb.clone()),
        });
    }

    // Handle message
    let msg = update.get("message")?;
    let from = msg.get("from")?;
    let sender_id = from["id"].as_i64()?;

    if !allowed_user_ids.is_empty() && !allowed_user_ids.contains(&sender_id) {
        warn!(sender_id, "message from non-allowed user, ignoring");
        return None;
    }

    let chat_id = msg["chat"]["id"].as_i64()?.to_string();
    let message_id = msg["message_id"].as_i64().unwrap_or(0).to_string();
    let sender_name = from["first_name"].as_str().map(|s| s.to_string());

    // Determine content type
    let content = if let Some(text) = msg["text"].as_str() {
        // Check if it's a bot command
        if text.starts_with('/') {
            let parts: Vec<&str> = text.splitn(2, ' ').collect();
            let command = parts[0].split('@').next().unwrap_or(parts[0]).to_string();
            let args = parts.get(1).unwrap_or(&"").to_string();
            InboundContent::Command { command, args }
        } else {
            InboundContent::Text(text.to_string())
        }
    } else if msg.get("photo").is_some() {
        // Get the largest photo (last in array)
        let photos = msg["photo"].as_array()?;
        let largest = photos.last()?;
        let file_id = largest["file_id"].as_str()?.to_string();
        let caption = msg["caption"].as_str().map(|s| s.to_string());
        InboundContent::Photo { file_id, caption }
    } else if let Some(voice) = msg.get("voice") {
        let file_id = voice["file_id"].as_str()?.to_string();
        let duration_secs = voice["duration"].as_u64().unwrap_or(0) as u32;
        InboundContent::Voice { file_id, duration_secs }
    } else if let Some(doc) = msg.get("document") {
        let file_id = doc["file_id"].as_str()?.to_string();
        let file_name = doc["file_name"].as_str().map(|s| s.to_string());
        let mime_type = doc["mime_type"].as_str().map(|s| s.to_string());
        InboundContent::Document { file_id, file_name, mime_type }
    } else {
        // Unsupported message type
        return None;
    };

    Some(InboundMessage {
        id: message_id,
        connector_id: "telegram".into(),
        chat_id,
        sender_id: sender_id.to_string(),
        sender_name,
        content,
        timestamp: chrono::Utc::now().to_rfc3339(),
        raw: Some(msg.clone()),
    })
}

/// Download a file from Telegram by file_id.
pub async fn download_file(bot_token: &str, file_id: &str) -> Result<Vec<u8>, ConnectorError> {
    let client = reqwest::Client::new();

    // Step 1: getFile to get file_path
    let url = format!("{}{}/getFile", TELEGRAM_API_BASE, bot_token);
    let resp = client
        .post(&url)
        .json(&serde_json::json!({"file_id": file_id}))
        .send()
        .await
        .map_err(|e| ConnectorError::Other(e.to_string()))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| ConnectorError::Other(e.to_string()))?;

    let file_path = body["result"]["file_path"]
        .as_str()
        .ok_or_else(|| ConnectorError::Other("no file_path in response".into()))?;

    // Step 2: Download the file
    let download_url = format!(
        "https://api.telegram.org/file/bot{}/{}",
        bot_token, file_path
    );
    let data = client
        .get(&download_url)
        .send()
        .await
        .map_err(|e| ConnectorError::Other(e.to_string()))?
        .bytes()
        .await
        .map_err(|e| ConnectorError::Other(e.to_string()))?;

    Ok(data.to_vec())
}
