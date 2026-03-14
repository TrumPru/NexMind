use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::provider::ModelProvider;
use crate::types::*;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
const OAUTH_REFRESH_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const TOKEN_REFRESH_MARGIN_MS: u64 = 60_000; // Refresh 60s before expiry

/// Authentication mode for Anthropic.
#[derive(Debug, Clone)]
pub enum AnthropicAuth {
    ApiKey(String),
    OAuth {
        access_token: String,
        refresh_token: String,
        expires_at: u64, // unix timestamp ms
    },
}

#[derive(Debug, Deserialize)]
struct OAuthRefreshResponse {
    access_token: String,
    expires_in: u64,
}

pub struct AnthropicProvider {
    client: Client,
    auth: Arc<RwLock<AnthropicAuth>>,
    credential_file_path: Option<String>,
}

impl AnthropicProvider {
    /// Create with explicit API key.
    pub fn with_api_key(api_key: String) -> Result<Self, ModelError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| ModelError::ProviderError(e.to_string()))?;

        Ok(Self {
            client,
            auth: Arc::new(RwLock::new(AnthropicAuth::ApiKey(api_key))),
            credential_file_path: None,
        })
    }

    /// Create with OAuth credentials.
    pub fn with_oauth(
        access_token: String,
        refresh_token: String,
        expires_at: u64,
        credential_file_path: Option<String>,
    ) -> Result<Self, ModelError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| ModelError::ProviderError(e.to_string()))?;

        Ok(Self {
            client,
            auth: Arc::new(RwLock::new(AnthropicAuth::OAuth {
                access_token,
                refresh_token,
                expires_at,
            })),
            credential_file_path,
        })
    }

    /// Auto-detect auth: only uses ANTHROPIC_API_KEY env var with a real API key.
    ///
    /// OAuth tokens from ~/.claude/.credentials.json are NOT accepted here because
    /// the Anthropic Messages API rejects OAuth tokens with "OAuth authentication is
    /// currently not supported." OAuth users should use `ClaudeCodeProvider` instead,
    /// which proxies through the Claude Code CLI.
    pub fn from_auto_detect() -> Result<Self, ModelError> {
        if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
            if !api_key.is_empty() && is_real_api_key(&api_key) {
                info!("Anthropic provider: API key (env var)");
                return Self::with_api_key(api_key);
            }
            if !api_key.is_empty() {
                info!(
                    "Anthropic provider: ANTHROPIC_API_KEY is set but does not look like \
                     a real API key (expected sk-ant-api* prefix). Skipping."
                );
            }
        }

        Err(ModelError::AuthError(
            "No Anthropic API key found. Set ANTHROPIC_API_KEY (starts with sk-ant-api).\n\
             For Claude Pro/Max subscriptions, use the Claude Code CLI provider instead."
                .to_string(),
        ))
    }

    /// Ensure the token is valid; refresh if OAuth and expired.
    async fn ensure_valid_token(&self) -> Result<String, ModelError> {
        let auth = self.auth.read().await;
        match &*auth {
            AnthropicAuth::ApiKey(key) => Ok(key.clone()),
            AnthropicAuth::OAuth {
                access_token,
                refresh_token,
                expires_at,
            } => {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;

                if now_ms + TOKEN_REFRESH_MARGIN_MS < *expires_at {
                    return Ok(access_token.clone());
                }

                let refresh_token = refresh_token.clone();
                drop(auth); // Release read lock before acquiring write lock

                self.refresh_oauth_token(&refresh_token).await
            }
        }
    }

    async fn refresh_oauth_token(&self, refresh_token: &str) -> Result<String, ModelError> {
        info!("Refreshing Anthropic OAuth token");

        let resp = self
            .client
            .post(OAUTH_REFRESH_URL)
            .json(&json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
            }))
            .send()
            .await
            .map_err(|e| ModelError::AuthError(format!("Token refresh request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ModelError::AuthError(format!(
                "Token refresh failed ({}): {}",
                status, body
            )));
        }

        let refresh_resp: OAuthRefreshResponse = resp
            .json()
            .await
            .map_err(|e| ModelError::AuthError(format!("Failed to parse refresh response: {}", e)))?;

        let new_expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + refresh_resp.expires_in * 1000;

        let new_token = refresh_resp.access_token.clone();

        // Update stored auth
        let mut auth = self.auth.write().await;
        *auth = AnthropicAuth::OAuth {
            access_token: refresh_resp.access_token,
            refresh_token: refresh_token.to_string(),
            expires_at: new_expires_at,
        };

        // Update credential file if we have a path
        if let Some(path) = &self.credential_file_path {
            if let Ok(contents) = std::fs::read_to_string(path) {
                if let Ok(mut cred_file) = serde_json::from_str::<Value>(&contents) {
                    if let Some(oauth) = cred_file.get_mut("claudeAiOauth") {
                        oauth["accessToken"] = json!(new_token);
                        oauth["expiresAt"] = json!(new_expires_at);
                        if let Err(e) = std::fs::write(path, serde_json::to_string_pretty(&cred_file).unwrap()) {
                            warn!("Failed to update credential file: {}", e);
                        }
                    }
                }
            }
        }

        info!("OAuth token refreshed successfully");
        Ok(new_token)
    }

    /// Build auth headers.
    async fn auth_headers(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, ModelError> {
        let auth = self.auth.read().await;
        match &*auth {
            AnthropicAuth::ApiKey(key) => Ok(builder.header("x-api-key", key)),
            AnthropicAuth::OAuth { .. } => {
                drop(auth);
                let token = self.ensure_valid_token().await?;
                Ok(builder
                    .header("Authorization", format!("Bearer {}", token))
                    .header("anthropic-beta", "interleaved-thinking-2025-05-14"))
            }
        }
    }

    /// Convert our messages to Anthropic API format.
    fn build_request_body(&self, req: &CompletionRequest) -> Value {
        let mut system_prompt = None;
        let mut messages = Vec::new();

        for msg in &req.messages {
            match msg.role {
                Role::System => {
                    if let Content::Text { text } = &msg.content {
                        system_prompt = Some(text.clone());
                    }
                }
                Role::User => {
                    if let Content::Text { text } = &msg.content {
                        messages.push(json!({
                            "role": "user",
                            "content": text,
                        }));
                    }
                }
                Role::Assistant => match &msg.content {
                    Content::Text { text } => {
                        messages.push(json!({
                            "role": "assistant",
                            "content": text,
                        }));
                    }
                    Content::ToolCalls { tool_calls } => {
                        let blocks: Vec<Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                json!({
                                    "type": "tool_use",
                                    "id": tc.id,
                                    "name": tc.name,
                                    "input": tc.arguments,
                                })
                            })
                            .collect();
                        messages.push(json!({
                            "role": "assistant",
                            "content": blocks,
                        }));
                    }
                    _ => {}
                },
                Role::Tool => {
                    if let Content::ToolResult {
                        tool_call_id,
                        content,
                    } = &msg.content
                    {
                        messages.push(json!({
                            "role": "user",
                            "content": [{
                                "type": "tool_result",
                                "tool_use_id": tool_call_id,
                                "content": content,
                            }],
                        }));
                    }
                }
            }
        }

        let model = req
            .model
            .strip_prefix("anthropic/")
            .unwrap_or(&req.model);

        let mut body = json!({
            "model": model,
            "max_tokens": req.max_tokens,
            "temperature": req.temperature,
            "messages": messages,
        });

        if let Some(sp) = system_prompt {
            body["system"] = json!(sp);
        }

        if req.stream {
            body["stream"] = json!(true);
        }

        if let Some(tools) = &req.tools {
            let tool_defs: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })
                })
                .collect();
            body["tools"] = json!(tool_defs);
        }

        body
    }

    /// Parse a non-streaming Anthropic response into CompletionResponse.
    fn parse_response(
        &self,
        body: &Value,
        model: &str,
        latency_ms: u64,
    ) -> Result<CompletionResponse, ModelError> {
        let content_blocks = body["content"]
            .as_array()
            .ok_or_else(|| ModelError::ParseError("missing content array".into()))?;

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in content_blocks {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(t) = block["text"].as_str() {
                        text_parts.push(t.to_string());
                    }
                }
                Some("tool_use") => {
                    tool_calls.push(ToolCall {
                        id: block["id"].as_str().unwrap_or("").to_string(),
                        name: block["name"].as_str().unwrap_or("").to_string(),
                        arguments: block["input"].clone(),
                    });
                }
                _ => {}
            }
        }

        let message = if !tool_calls.is_empty() {
            ChatMessage::assistant_tool_calls(tool_calls)
        } else {
            ChatMessage::assistant_text(&text_parts.join(""))
        };

        let usage = if let Some(u) = body.get("usage") {
            TokenUsage {
                input_tokens: u["input_tokens"].as_u64().unwrap_or(0) as u32,
                output_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: (u["input_tokens"].as_u64().unwrap_or(0)
                    + u["output_tokens"].as_u64().unwrap_or(0)) as u32,
            }
        } else {
            TokenUsage::default()
        };

        Ok(CompletionResponse {
            message,
            usage,
            model: model.to_string(),
            latency_ms,
        })
    }

    /// Parse SSE events from Anthropic streaming response.
    fn parse_sse_line(line: &str) -> Option<(&str, &str)> {
        if let Some(rest) = line.strip_prefix("event: ") {
            Some(("event", rest.trim()))
        } else if let Some(rest) = line.strip_prefix("data: ") {
            Some(("data", rest.trim()))
        } else {
            None
        }
    }
}

/// Check if a string looks like a real Anthropic API key (not an OAuth token).
pub fn is_real_api_key(key: &str) -> bool {
    key.starts_with("sk-ant-api")
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn supported_models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "claude-sonnet-4-20250514".into(),
                display_name: "Claude Sonnet 4".into(),
                context_window: 200_000,
                supports_tools: true,
                supports_vision: true,
                supports_streaming: true,
                cost_per_1k_input: 0.003,
                cost_per_1k_output: 0.015,
            },
            ModelInfo {
                id: "claude-opus-4-20250514".into(),
                display_name: "Claude Opus 4".into(),
                context_window: 200_000,
                supports_tools: true,
                supports_vision: true,
                supports_streaming: true,
                cost_per_1k_input: 0.015,
                cost_per_1k_output: 0.075,
            },
            ModelInfo {
                id: "claude-haiku-3-5-20241022".into(),
                display_name: "Claude 3.5 Haiku".into(),
                context_window: 200_000,
                supports_tools: true,
                supports_vision: true,
                supports_streaming: true,
                cost_per_1k_input: 0.0008,
                cost_per_1k_output: 0.004,
            },
        ]
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ModelError> {
        let body = self.build_request_body(&req);
        let start = std::time::Instant::now();

        let request = self.client.post(ANTHROPIC_API_URL)
            .header("content-type", "application/json")
            .header("anthropic-version", ANTHROPIC_API_VERSION);

        let request = self.auth_headers(request).await?;

        let resp = request
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ModelError::Timeout
                } else {
                    ModelError::RequestError(e.to_string())
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return match status.as_u16() {
                401 => Err(ModelError::AuthError(format!("Unauthorized: {}", err_body))),
                429 => Err(ModelError::RateLimited { retry_after_ms: None }),
                529 | 500 => Err(ModelError::Overloaded),
                _ => Err(ModelError::ProviderError(format!("{}: {}", status, err_body))),
            };
        }

        let latency_ms = start.elapsed().as_millis() as u64;
        let resp_body: Value = resp
            .json()
            .await
            .map_err(|e| ModelError::ParseError(e.to_string()))?;

        let model_used = resp_body["model"]
            .as_str()
            .unwrap_or(&req.model)
            .to_string();

        self.parse_response(&resp_body, &model_used, latency_ms)
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamChunk> + Send>>, ModelError> {
        let mut body = self.build_request_body(&req);
        body["stream"] = json!(true);

        let request = self.client.post(ANTHROPIC_API_URL)
            .header("content-type", "application/json")
            .header("anthropic-version", ANTHROPIC_API_VERSION);

        let request = self.auth_headers(request).await?;

        let resp = request
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ModelError::Timeout
                } else {
                    ModelError::RequestError(e.to_string())
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return match status.as_u16() {
                401 => Err(ModelError::AuthError(format!("Unauthorized: {}", err_body))),
                429 => Err(ModelError::RateLimited { retry_after_ms: None }),
                529 | 500 => Err(ModelError::Overloaded),
                _ => Err(ModelError::ProviderError(format!("{}: {}", status, err_body))),
            };
        }

        let byte_stream = resp.bytes_stream();

        use futures::StreamExt;

        let chunk_stream = futures::stream::unfold(
            (byte_stream, String::new(), None::<String>),
            |(mut byte_stream, mut buffer, mut current_event)| async move {
                use futures::TryStreamExt;

                loop {
                    // Process buffered lines first
                    while let Some(newline_pos) = buffer.find('\n') {
                        let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                        buffer = buffer[newline_pos + 1..].to_string();

                        if line.is_empty() {
                            continue;
                        }

                        if let Some(("event", event_name)) = AnthropicProvider::parse_sse_line(&line) {
                            current_event = Some(event_name.to_string());
                            continue;
                        }

                        if let Some(("data", data)) = AnthropicProvider::parse_sse_line(&line) {
                            let event_name = current_event.take();
                            if let Ok(parsed) = serde_json::from_str::<Value>(data) {
                                if let Some(chunk) =
                                    parse_anthropic_sse_event(event_name.as_deref(), &parsed)
                                {
                                    return Some((chunk, (byte_stream, buffer, current_event)));
                                }
                            }
                        }
                    }

                    // Read more bytes
                    match byte_stream.try_next().await {
                        Ok(Some(bytes)) => {
                            buffer.push_str(&String::from_utf8_lossy(&bytes));
                        }
                        Ok(None) => {
                            // Stream ended
                            return Some((StreamChunk::Done, (byte_stream, buffer, current_event)));
                        }
                        Err(e) => {
                            return Some((
                                StreamChunk::Error(e.to_string()),
                                (byte_stream, buffer, current_event),
                            ));
                        }
                    }
                }
            },
        );

        // Stop after Done or Error
        let chunk_stream = chunk_stream.take_while(|chunk| {
            let cont = !matches!(chunk, StreamChunk::Done | StreamChunk::Error(_));
            futures::future::ready(cont)
        });

        // Append a Done at the end
        let final_stream = chunk_stream.chain(futures::stream::once(async { StreamChunk::Done }));

        Ok(Box::pin(final_stream))
    }

    async fn embed(&self, _texts: Vec<String>, _model: &str) -> Result<Vec<Vec<f32>>, ModelError> {
        Err(ModelError::ProviderError(
            "Anthropic does not support embeddings".into(),
        ))
    }

    async fn health_check(&self) -> HealthStatus {
        match self.ensure_valid_token().await {
            Ok(_) => HealthStatus::Healthy,
            Err(e) => HealthStatus::Unavailable(e.to_string()),
        }
    }
}

fn parse_anthropic_sse_event(event_name: Option<&str>, data: &Value) -> Option<StreamChunk> {
    match event_name {
        Some("content_block_start") => {
            let block = &data["content_block"];
            match block["type"].as_str() {
                Some("tool_use") => Some(StreamChunk::ToolCallStart {
                    id: block["id"].as_str().unwrap_or("").to_string(),
                    name: block["name"].as_str().unwrap_or("").to_string(),
                }),
                _ => None,
            }
        }
        Some("content_block_delta") => {
            let delta = &data["delta"];
            match delta["type"].as_str() {
                Some("text_delta") => {
                    delta["text"].as_str().map(|t| StreamChunk::TextDelta(t.to_string()))
                }
                Some("input_json_delta") => {
                    delta["partial_json"].as_str().map(|d| {
                        // Need to figure out which tool call this belongs to
                        let index = data["index"].as_u64().unwrap_or(0);
                        StreamChunk::ToolCallArgumentsDelta {
                            id: format!("idx_{}", index),
                            delta: d.to_string(),
                        }
                    })
                }
                _ => None,
            }
        }
        Some("content_block_stop") => {
            let index = data["index"].as_u64().unwrap_or(0);
            Some(StreamChunk::ToolCallEnd {
                id: format!("idx_{}", index),
            })
        }
        Some("message_delta") => {
            data.get("usage").map(|usage| StreamChunk::Usage(TokenUsage {
                input_tokens: 0,
                output_tokens: usage["output_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: usage["output_tokens"].as_u64().unwrap_or(0) as u32,
            }))
        }
        Some("message_start") => {
            data["message"].get("usage").map(|usage| StreamChunk::Usage(TokenUsage {
                input_tokens: usage["input_tokens"].as_u64().unwrap_or(0) as u32,
                output_tokens: 0,
                total_tokens: usage["input_tokens"].as_u64().unwrap_or(0) as u32,
            }))
        }
        Some("message_stop") => Some(StreamChunk::Done),
        Some("error") => {
            let msg = data["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            Some(StreamChunk::Error(msg.to_string()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_request_body_api_key() {
        let provider = AnthropicProvider::with_api_key("sk-test-key".into()).unwrap();
        let req = CompletionRequest {
            model: "anthropic/claude-sonnet-4-20250514".into(),
            messages: vec![
                ChatMessage::system("You are helpful."),
                ChatMessage::user("Hello"),
            ],
            tools: None,
            temperature: 0.7,
            max_tokens: 1024,
            stream: false,
        };

        let body = provider.build_request_body(&req);
        assert_eq!(body["model"], "claude-sonnet-4-20250514");
        assert_eq!(body["system"], "You are helpful.");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn test_build_request_body_with_tools() {
        let provider = AnthropicProvider::with_api_key("sk-test".into()).unwrap();
        let req = CompletionRequest {
            model: "anthropic/claude-sonnet-4-20250514".into(),
            messages: vec![ChatMessage::user("What's the weather?")],
            tools: Some(vec![ToolDefinition {
                name: "get_weather".into(),
                description: "Get weather".into(),
                input_schema: serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            }]),
            temperature: 0.5,
            max_tokens: 1024,
            stream: false,
        };

        let body = provider.build_request_body(&req);
        assert!(body["tools"].is_array());
        assert_eq!(body["tools"][0]["name"], "get_weather");
    }

    #[test]
    fn test_parse_response_text() {
        let provider = AnthropicProvider::with_api_key("sk-test".into()).unwrap();
        let resp = json!({
            "content": [{"type": "text", "text": "Hello!"}],
            "usage": {"input_tokens": 10, "output_tokens": 5},
            "model": "claude-sonnet-4-20250514",
        });

        let parsed = provider.parse_response(&resp, "claude-sonnet-4-20250514", 100).unwrap();
        assert_eq!(parsed.message.text().unwrap(), "Hello!");
        assert_eq!(parsed.usage.input_tokens, 10);
        assert_eq!(parsed.usage.output_tokens, 5);
    }

    #[test]
    fn test_parse_response_tool_calls() {
        let provider = AnthropicProvider::with_api_key("sk-test".into()).unwrap();
        let resp = json!({
            "content": [{
                "type": "tool_use",
                "id": "tc_1",
                "name": "get_weather",
                "input": {"city": "London"}
            }],
            "usage": {"input_tokens": 15, "output_tokens": 20},
            "model": "claude-sonnet-4-20250514",
        });

        let parsed = provider.parse_response(&resp, "claude-sonnet-4-20250514", 200).unwrap();
        let calls = parsed.message.tool_calls().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(calls[0].arguments["city"], "London");
    }

    #[test]
    fn test_tool_call_normalization_roundtrip() {
        let tc = ToolCall {
            id: "tc_1".into(),
            name: "search".into(),
            arguments: json!({"query": "test"}),
        };

        let msg = ChatMessage::assistant_tool_calls(vec![tc]);
        let calls = msg.tool_calls().unwrap();
        assert_eq!(calls[0].name, "search");

        // Tool result
        let result = ChatMessage::tool_result("tc_1", "Found 5 results");
        if let Content::ToolResult { tool_call_id, content } = &result.content {
            assert_eq!(tool_call_id, "tc_1");
            assert_eq!(content, "Found 5 results");
        } else {
            panic!("Expected ToolResult content");
        }
    }

    #[test]
    fn test_parse_anthropic_sse_text_delta() {
        let data = json!({"delta": {"type": "text_delta", "text": "Hello"}});
        let chunk = parse_anthropic_sse_event(Some("content_block_delta"), &data);
        assert!(matches!(chunk, Some(StreamChunk::TextDelta(ref t)) if t == "Hello"));
    }

    #[test]
    fn test_parse_anthropic_sse_tool_call_start() {
        let data = json!({"content_block": {"type": "tool_use", "id": "tc_1", "name": "search"}});
        let chunk = parse_anthropic_sse_event(Some("content_block_start"), &data);
        assert!(matches!(chunk, Some(StreamChunk::ToolCallStart { ref id, ref name }) if id == "tc_1" && name == "search"));
    }

    #[test]
    fn test_parse_anthropic_sse_message_stop() {
        let data = json!({});
        let chunk = parse_anthropic_sse_event(Some("message_stop"), &data);
        assert!(matches!(chunk, Some(StreamChunk::Done)));
    }

    #[test]
    fn test_parse_anthropic_sse_usage() {
        let data = json!({"message": {"usage": {"input_tokens": 100}}});
        let chunk = parse_anthropic_sse_event(Some("message_start"), &data);
        assert!(matches!(chunk, Some(StreamChunk::Usage(ref u)) if u.input_tokens == 100));
    }

    /// Tests that touch ANTHROPIC_API_KEY env var are combined into one test
    /// to avoid race conditions from parallel test execution.
    #[tokio::test]
    async fn test_auto_detect_env_var_scenarios() {
        let orig = std::env::var("ANTHROPIC_API_KEY").ok();

        // Scenario 1: No credentials at all
        std::env::remove_var("ANTHROPIC_API_KEY");
        let _result = AnthropicProvider::from_auto_detect();
        // Just verify it doesn't panic (may succeed if Claude Code is installed)

        // Scenario 2: Real API key → should succeed
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-api03-test-key-for-testing");
        let provider = AnthropicProvider::from_auto_detect().unwrap();
        assert_eq!(provider.id(), "anthropic");

        // Scenario 3: Non-API-key string → should fail
        std::env::set_var("ANTHROPIC_API_KEY", "oauth-token-not-a-real-key");
        let result = AnthropicProvider::from_auto_detect();
        assert!(result.is_err());

        // Restore
        match orig {
            Some(key) => std::env::set_var("ANTHROPIC_API_KEY", key),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
    }

    #[test]
    fn test_is_real_api_key() {
        assert!(is_real_api_key("sk-ant-api03-abc123"));
        assert!(is_real_api_key("sk-ant-api01-xyz"));
        assert!(!is_real_api_key("sk-ant-admin-key"));
        assert!(!is_real_api_key("oauth-token"));
        assert!(!is_real_api_key(""));
        assert!(!is_real_api_key("sk-proj-openai-key"));
    }
}
