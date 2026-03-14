use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::config::OpenClawConfig;
use crate::OpenClawError;

/// OpenClaw Gateway HTTP client.
///
/// Communicates with an OpenClaw Gateway instance via its OpenAI-compatible
/// `/v1/chat/completions` endpoint. This is the primary integration path:
/// NexMind sends messages as chat completions, OpenClaw processes them
/// with its full agent pipeline (tools, memory, skills) and returns responses.
pub struct GatewayClient {
    config: OpenClawConfig,
    http: reqwest::Client,
}

// ── OpenAI-compatible request/response types ────────────────────────

/// Chat message in OpenAI format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// OpenAI-compatible chat completion request.
#[derive(Debug, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// OpenAI-compatible chat completion response.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: Option<String>,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

/// A single choice in a chat completion response.
#[derive(Debug, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

/// Token usage info.
#[derive(Debug, Deserialize)]
pub struct Usage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

/// OpenClaw gateway health status.
#[derive(Debug, Deserialize)]
pub struct GatewayHealth {
    pub ok: Option<bool>,
    pub status: Option<String>,
    pub version: Option<String>,
}

/// Simplified request for sending a message to OpenClaw.
#[derive(Debug)]
pub struct SendMessageRequest {
    pub message: String,
    pub agent_id: Option<String>,
    pub session_user: Option<String>,
}

/// Simplified response from OpenClaw.
#[derive(Debug)]
pub struct SendMessageResponse {
    pub reply: String,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}

impl GatewayClient {
    /// Create a new gateway client.
    pub fn new(config: OpenClawConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_default();

        Self { config, http }
    }

    /// Build the chat completions URL.
    fn completions_url(&self) -> String {
        format!(
            "{}/v1/chat/completions",
            self.config.http_url.trim_end_matches('/')
        )
    }

    /// Add auth and agent headers.
    fn add_headers(&self, req: reqwest::RequestBuilder, agent_id: Option<&str>) -> reqwest::RequestBuilder {
        let mut req = req.header("Content-Type", "application/json");

        if let Some(ref token) = self.config.gateway_token {
            req = req.header("Authorization", format!("Bearer {}", token));
        }

        if let Some(agent) = agent_id {
            req = req.header("x-openclaw-agent-id", agent);
        }

        req
    }

    // ── Core API methods ────────────────────────────────────────────

    /// Send a message to OpenClaw and get a response.
    ///
    /// Uses the OpenAI-compatible `/v1/chat/completions` endpoint.
    /// OpenClaw processes the message through its full agent pipeline
    /// (tools, memory, skills) and returns the result.
    pub async fn send_message(
        &self,
        request: SendMessageRequest,
    ) -> Result<SendMessageResponse, OpenClawError> {
        let url = self.completions_url();
        debug!(url = %url, "sending message to OpenClaw");

        let agent_id = request
            .agent_id
            .as_deref()
            .or(self.config.default_agent.as_deref());

        let body = ChatCompletionRequest {
            model: "openclaw".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: request.message,
            }],
            stream: Some(false),
            user: request.session_user,
        };

        let mut last_error = None;

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let backoff = std::time::Duration::from_millis(500 * 2u64.pow(attempt - 1));
                warn!(attempt, "retrying after {:?}", backoff);
                tokio::time::sleep(backoff).await;
            }

            let req = self.add_headers(self.http.post(&url), agent_id).json(&body);

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();

                    if status.is_success() {
                        let completion: ChatCompletionResponse = resp
                            .json()
                            .await
                            .map_err(|e| OpenClawError::ParseError(e.to_string()))?;

                        let reply = completion
                            .choices
                            .first()
                            .map(|c| c.message.content.clone())
                            .unwrap_or_default();

                        let finish_reason = completion
                            .choices
                            .first()
                            .and_then(|c| c.finish_reason.clone());

                        return Ok(SendMessageResponse {
                            reply,
                            finish_reason,
                            usage: completion.usage,
                        });
                    }

                    let error_body = resp.text().await.unwrap_or_default();

                    if status.as_u16() == 429 {
                        last_error = Some(OpenClawError::RateLimited);
                        continue;
                    }

                    if status.as_u16() == 401 || status.as_u16() == 403 {
                        return Err(OpenClawError::GatewayError(format!(
                            "Authentication failed ({}): {}",
                            status, error_body
                        )));
                    }

                    if status.is_server_error() {
                        last_error = Some(OpenClawError::GatewayError(format!(
                            "{}: {}",
                            status, error_body
                        )));
                        continue;
                    }

                    return Err(OpenClawError::GatewayError(format!(
                        "{}: {}",
                        status, error_body
                    )));
                }
                Err(e) => {
                    if e.is_connect() || e.is_timeout() {
                        last_error = Some(OpenClawError::ConnectionFailed(e.to_string()));
                        continue;
                    }
                    return Err(OpenClawError::ConnectionFailed(e.to_string()));
                }
            }
        }

        Err(last_error.unwrap_or(OpenClawError::ConnectionFailed("max retries exceeded".into())))
    }

    /// Send a multi-turn conversation to OpenClaw.
    ///
    /// Allows sending full conversation history for context-aware responses.
    pub async fn send_conversation(
        &self,
        messages: Vec<ChatMessage>,
        agent_id: Option<&str>,
        session_user: Option<&str>,
    ) -> Result<SendMessageResponse, OpenClawError> {
        let url = self.completions_url();

        let agent = agent_id.or(self.config.default_agent.as_deref());

        let body = ChatCompletionRequest {
            model: "openclaw".into(),
            messages,
            stream: Some(false),
            user: session_user.map(|s| s.into()),
        };

        let req = self.add_headers(self.http.post(&url), agent).json(&body);

        let resp = req
            .send()
            .await
            .map_err(|e| OpenClawError::ConnectionFailed(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let error_body = resp.text().await.unwrap_or_default();
            return Err(OpenClawError::GatewayError(format!(
                "{}: {}",
                status, error_body
            )));
        }

        let completion: ChatCompletionResponse = resp
            .json()
            .await
            .map_err(|e| OpenClawError::ParseError(e.to_string()))?;

        let reply = completion
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default();

        let finish_reason = completion
            .choices
            .first()
            .and_then(|c| c.finish_reason.clone());

        Ok(SendMessageResponse {
            reply,
            finish_reason,
            usage: completion.usage,
        })
    }

    /// Check if the OpenClaw gateway is reachable and healthy.
    pub async fn health_check(&self) -> Result<GatewayHealth, OpenClawError> {
        let url = format!("{}/health", self.config.http_url.trim_end_matches('/'));

        let req = self.http.get(&url);

        match req.send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    resp.json::<GatewayHealth>()
                        .await
                        .map_err(|e| OpenClawError::ParseError(e.to_string()))
                } else {
                    Err(OpenClawError::GatewayError(format!(
                        "health check returned {}",
                        resp.status()
                    )))
                }
            }
            Err(e) => Err(OpenClawError::ConnectionFailed(e.to_string())),
        }
    }

    /// Check if the gateway is reachable (simple connectivity test).
    pub async fn is_reachable(&self) -> bool {
        match self.health_check().await {
            Ok(h) => {
                info!(
                    status = ?h.status,
                    "OpenClaw gateway reachable"
                );
                h.ok.unwrap_or(false) || h.status.as_deref() == Some("live")
            }
            Err(e) => {
                error!(error = %e, "OpenClaw gateway unreachable");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_completions_url() {
        let config = OpenClawConfig {
            http_url: "http://127.0.0.1:18789".into(),
            ..Default::default()
        };
        let client = GatewayClient::new(config);
        assert_eq!(
            client.completions_url(),
            "http://127.0.0.1:18789/v1/chat/completions"
        );
    }

    #[test]
    fn test_completions_url_trailing_slash() {
        let config = OpenClawConfig {
            http_url: "http://127.0.0.1:18789/".into(),
            ..Default::default()
        };
        let client = GatewayClient::new(config);
        assert_eq!(
            client.completions_url(),
            "http://127.0.0.1:18789/v1/chat/completions"
        );
    }

    #[test]
    fn test_chat_completion_request_serialization() {
        let req = ChatCompletionRequest {
            model: "openclaw".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "Hello OpenClaw".into(),
            }],
            stream: Some(false),
            user: Some("nexmind-test".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("Hello OpenClaw"));
        assert!(json.contains("\"model\":\"openclaw\""));
        assert!(json.contains("nexmind-test"));
    }

    #[test]
    fn test_chat_completion_response_deserialization() {
        let json = r#"{
            "id": "chatcmpl_test",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello from OpenClaw!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        }"#;

        let resp: ChatCompletionResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.content, "Hello from OpenClaw!");
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(resp.usage.unwrap().total_tokens, Some(15));
    }

    #[test]
    fn test_health_response_deserialization() {
        let json = r#"{"ok": true, "status": "live"}"#;
        let health: GatewayHealth = serde_json::from_str(json).unwrap();
        assert_eq!(health.ok, Some(true));
        assert_eq!(health.status.as_deref(), Some("live"));
    }
}
