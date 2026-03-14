//! Live Telegram Bot API implementation using reqwest.
//! Only compiled when the `live` feature is enabled.

use std::time::Instant;

use tracing::{info, warn, error};

use nexmind_connector::{
    ChannelStatus, ConnectorError, FilePayload, HealthStatus, MessageId, OutboundMessage,
    ParseMode, PlatformExtras, InlineButton,
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
