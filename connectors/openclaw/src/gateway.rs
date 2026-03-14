use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::config::OpenClawConfig;
use crate::OpenClawError;

/// OpenClaw Gateway HTTP client.
///
/// Communicates with an OpenClaw Gateway instance to:
/// - Send messages and get responses (synchronous request/response)
/// - Spawn isolated agent sessions
/// - Check gateway health and status
pub struct GatewayClient {
    config: OpenClawConfig,
    http: reqwest::Client,
}

// ── Request / Response types ────────────────────────────────────────

/// Request to send a message to an OpenClaw session.
#[derive(Debug, Serialize)]
pub struct SessionSendRequest {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

/// Response from an OpenClaw session message.
#[derive(Debug, Deserialize)]
pub struct SessionSendResponse {
    pub reply: Option<String>,
    pub session_key: Option<String>,
    pub error: Option<String>,
}

/// Request to spawn an isolated OpenClaw agent session.
#[derive(Debug, Serialize)]
pub struct SpawnRequest {
    pub task: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

/// Response from spawning an OpenClaw session.
#[derive(Debug, Deserialize)]
pub struct SpawnResponse {
    pub session_id: Option<String>,
    pub session_key: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
}

/// OpenClaw gateway health status.
#[derive(Debug, Deserialize)]
pub struct GatewayHealth {
    pub status: Option<String>,
    pub version: Option<String>,
    pub uptime: Option<u64>,
}

/// Session info from listing sessions.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionInfo {
    pub session_key: Option<String>,
    pub label: Option<String>,
    pub agent_id: Option<String>,
    pub status: Option<String>,
    pub last_message: Option<String>,
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

    /// Build a full URL for an API endpoint.
    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{}", self.config.http_url.trim_end_matches('/'), path)
    }

    /// Add auth headers if a gateway token is configured.
    fn auth_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref token) = self.config.gateway_token {
            req.header("Authorization", format!("Bearer {}", token))
        } else {
            req
        }
    }

    // ── Core API methods ────────────────────────────────────────────

    /// Send a message to an OpenClaw session and wait for a response.
    ///
    /// This is the primary way NexMind agents communicate with OpenClaw.
    /// The message is sent to OpenClaw's agent, which processes it and
    /// returns a response.
    pub async fn send_message(
        &self,
        request: SessionSendRequest,
    ) -> Result<SessionSendResponse, OpenClawError> {
        let url = self.url("/sessions/send");
        debug!(url = %url, "sending message to OpenClaw");

        let mut last_error = None;

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let backoff = std::time::Duration::from_millis(500 * 2u64.pow(attempt - 1));
                warn!(attempt, "retrying after {:?}", backoff);
                tokio::time::sleep(backoff).await;
            }

            let req = self.auth_headers(self.http.post(&url)).json(&request);

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        let body = resp
                            .json::<SessionSendResponse>()
                            .await
                            .map_err(|e| OpenClawError::ParseError(e.to_string()))?;

                        if let Some(ref err) = body.error {
                            return Err(OpenClawError::AgentError(err.clone()));
                        }

                        return Ok(body);
                    }

                    let error_body = resp.text().await.unwrap_or_default();

                    if status.as_u16() == 429 {
                        last_error = Some(OpenClawError::RateLimited);
                        continue;
                    }

                    if status.is_server_error() {
                        last_error =
                            Some(OpenClawError::GatewayError(format!("{}: {}", status, error_body)));
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

    /// Spawn an isolated agent session in OpenClaw.
    ///
    /// Used for one-shot tasks: send a task description, get back a result
    /// when the agent finishes. Good for delegating complex work.
    pub async fn spawn_session(
        &self,
        request: SpawnRequest,
    ) -> Result<SpawnResponse, OpenClawError> {
        let url = self.url("/sessions/spawn");
        debug!(url = %url, task = %request.task, "spawning OpenClaw session");

        let req = self.auth_headers(self.http.post(&url)).json(&request);

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

        resp.json::<SpawnResponse>()
            .await
            .map_err(|e| OpenClawError::ParseError(e.to_string()))
    }

    /// Check if the OpenClaw gateway is reachable and healthy.
    pub async fn health_check(&self) -> Result<GatewayHealth, OpenClawError> {
        let url = format!("{}/health", self.config.http_url.trim_end_matches('/'));

        let req = self.auth_headers(self.http.get(&url));

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

    /// List active sessions on the OpenClaw gateway.
    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>, OpenClawError> {
        let url = self.url("/sessions");

        let req = self.auth_headers(self.http.get(&url));

        let resp = req
            .send()
            .await
            .map_err(|e| OpenClawError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let error_body = resp.text().await.unwrap_or_default();
            return Err(OpenClawError::GatewayError(format!(
                "list sessions failed: {}",
                error_body
            )));
        }

        resp.json::<Vec<SessionInfo>>()
            .await
            .map_err(|e| OpenClawError::ParseError(e.to_string()))
    }

    /// Check if the gateway is reachable (simple connectivity test).
    pub async fn is_reachable(&self) -> bool {
        match self.health_check().await {
            Ok(h) => {
                info!(
                    version = ?h.version,
                    status = ?h.status,
                    "OpenClaw gateway reachable"
                );
                true
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
    fn test_url_building() {
        let config = OpenClawConfig {
            http_url: "http://127.0.0.1:18789".into(),
            ..Default::default()
        };
        let client = GatewayClient::new(config);
        assert_eq!(client.url("/sessions/send"), "http://127.0.0.1:18789/api/v1/sessions/send");
    }

    #[test]
    fn test_url_building_trailing_slash() {
        let config = OpenClawConfig {
            http_url: "http://127.0.0.1:18789/".into(),
            ..Default::default()
        };
        let client = GatewayClient::new(config);
        assert_eq!(client.url("/sessions/send"), "http://127.0.0.1:18789/api/v1/sessions/send");
    }

    #[test]
    fn test_send_request_serialization() {
        let req = SessionSendRequest {
            message: "Hello OpenClaw".into(),
            session_key: None,
            label: Some("nexmind-agent".into()),
            agent_id: None,
            timeout_seconds: Some(60),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("Hello OpenClaw"));
        assert!(json.contains("nexmind-agent"));
        assert!(!json.contains("session_key")); // None fields skipped
    }

    #[test]
    fn test_spawn_request_serialization() {
        let req = SpawnRequest {
            task: "Summarize this document".into(),
            agent_id: None,
            model: Some("anthropic/claude-sonnet-4-20250514".into()),
            label: None,
            mode: Some("run".into()),
            timeout_seconds: Some(300),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("Summarize this document"));
        assert!(json.contains("claude-sonnet"));
    }
}
