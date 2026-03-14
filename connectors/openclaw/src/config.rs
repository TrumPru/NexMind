use serde::{Deserialize, Serialize};

/// Configuration for connecting to an OpenClaw Gateway instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenClawConfig {
    /// Gateway WebSocket URL (e.g., "ws://127.0.0.1:18789")
    pub gateway_url: String,

    /// Gateway HTTP base URL (e.g., "http://127.0.0.1:18789")
    pub http_url: String,

    /// Gateway authentication token (optional, for remote gateways)
    pub gateway_token: Option<String>,

    /// Default agent ID to use in OpenClaw sessions
    pub default_agent: Option<String>,

    /// Timeout for requests in seconds
    pub timeout_secs: u64,

    /// Maximum retries on transient failures
    pub max_retries: u32,
}

impl Default for OpenClawConfig {
    fn default() -> Self {
        Self {
            gateway_url: "ws://127.0.0.1:18789".into(),
            http_url: "http://127.0.0.1:18789".into(),
            gateway_token: None,
            default_agent: None,
            timeout_secs: 120,
            max_retries: 2,
        }
    }
}

impl OpenClawConfig {
    /// Create config from environment variables.
    ///
    /// - `OPENCLAW_GATEWAY_URL` — WebSocket URL (default: ws://127.0.0.1:18789)
    /// - `OPENCLAW_HTTP_URL` — HTTP URL (default: http://127.0.0.1:18789)
    /// - `OPENCLAW_GATEWAY_TOKEN` — Auth token (optional)
    /// - `OPENCLAW_DEFAULT_AGENT` — Default agent ID (optional)
    /// - `OPENCLAW_TIMEOUT` — Request timeout in seconds (default: 120)
    pub fn from_env() -> Result<Self, String> {
        let gateway_url = std::env::var("OPENCLAW_GATEWAY_URL")
            .unwrap_or_else(|_| "ws://127.0.0.1:18789".into());

        // Derive HTTP URL from WS URL if not explicitly set
        let http_url = std::env::var("OPENCLAW_HTTP_URL").unwrap_or_else(|_| {
            gateway_url
                .replace("ws://", "http://")
                .replace("wss://", "https://")
        });

        let gateway_token = std::env::var("OPENCLAW_GATEWAY_TOKEN").ok();
        let default_agent = std::env::var("OPENCLAW_DEFAULT_AGENT").ok();
        let timeout_secs = std::env::var("OPENCLAW_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120);

        Ok(Self {
            gateway_url,
            http_url,
            gateway_token,
            default_agent,
            timeout_secs,
            max_retries: 2,
        })
    }

    /// Create config for a local OpenClaw instance.
    pub fn local() -> Self {
        Self::default()
    }

    /// Create config for a remote OpenClaw instance (e.g., via Tailscale).
    pub fn remote(url: &str, token: &str) -> Self {
        let http_url = url
            .replace("ws://", "http://")
            .replace("wss://", "https://");

        Self {
            gateway_url: url.into(),
            http_url,
            gateway_token: Some(token.into()),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = OpenClawConfig::default();
        assert_eq!(config.gateway_url, "ws://127.0.0.1:18789");
        assert_eq!(config.http_url, "http://127.0.0.1:18789");
        assert!(config.gateway_token.is_none());
        assert_eq!(config.timeout_secs, 120);
    }

    #[test]
    fn test_local_config() {
        let config = OpenClawConfig::local();
        assert_eq!(config.gateway_url, "ws://127.0.0.1:18789");
    }

    #[test]
    fn test_remote_config() {
        let config = OpenClawConfig::remote("wss://my-mac.tailnet.ts.net:18789", "secret-token");
        assert_eq!(config.gateway_url, "wss://my-mac.tailnet.ts.net:18789");
        assert_eq!(config.http_url, "https://my-mac.tailnet.ts.net:18789");
        assert_eq!(config.gateway_token.unwrap(), "secret-token");
    }
}
