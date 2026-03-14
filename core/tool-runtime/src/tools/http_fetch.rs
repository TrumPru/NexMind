use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

pub struct HttpFetchTool;

#[async_trait::async_trait]
impl Tool for HttpFetchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "http_fetch".into(),
            name: "http_fetch".into(),
            description: "Make an HTTP request to an external URL".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to fetch"
                    },
                    "method": {
                        "type": "string",
                        "enum": ["GET", "POST"],
                        "description": "HTTP method (default: GET)"
                    },
                    "headers": {
                        "type": "object",
                        "description": "Optional HTTP headers"
                    },
                    "body": {
                        "type": "string",
                        "description": "Request body (for POST)"
                    }
                },
                "required": ["url"]
            }),
            required_permissions: vec!["network:outbound".into()],
            trust_level: 1,
            idempotent: false,
            timeout_seconds: 30,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("url").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'url' is required".into()));
        }
        Ok(())
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        #[cfg(feature = "http")]
        {
            let url = args["url"].as_str().unwrap();
            let method = args
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("GET");

            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            let mut req = match method.to_uppercase().as_str() {
                "POST" => client.post(url),
                _ => client.get(url),
            };

            // Add headers
            if let Some(headers) = args.get("headers").and_then(|v| v.as_object()) {
                for (key, val) in headers {
                    if let Some(v) = val.as_str() {
                        req = req.header(key.as_str(), v);
                    }
                }
            }

            // Add body for POST
            if method.to_uppercase() == "POST" {
                if let Some(body) = args.get("body").and_then(|v| v.as_str()) {
                    req = req.body(body.to_string());
                }
            }

            let resp = req
                .send()
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            let status = resp.status().as_u16();
            let headers: serde_json::Map<String, Value> = resp
                .headers()
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_string(),
                        Value::String(v.to_str().unwrap_or("").to_string()),
                    )
                })
                .collect();

            let body = resp
                .text()
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            // Truncate body to 50KB
            let body = if body.len() > 51200 {
                format!("{}... [truncated, total {} bytes]", &body[..51200], body.len())
            } else {
                body
            };

            return Ok(ToolOutput::Success {
                result: json!({
                    "status": status,
                    "body": body,
                    "headers": headers,
                }),
                tokens_used: None,
            });
        }

        #[cfg(not(feature = "http"))]
        {
            let _ = args;
            Ok(ToolOutput::Error {
                error: "HTTP support not compiled in".into(),
                retryable: false,
            })
        }
    }
}
