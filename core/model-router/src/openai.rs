use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use reqwest::Client;
use serde_json::{json, Value};
use crate::provider::ModelProvider;
use crate::types::*;

const OPENAI_API_URL: &str = "https://api.openai.com/v1/chat/completions";

pub struct OpenAIProvider {
    client: Client,
    api_key: String,
}

impl OpenAIProvider {
    pub fn new(api_key: String) -> Result<Self, ModelError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| ModelError::ProviderError(e.to_string()))?;

        Ok(Self { client, api_key })
    }

    pub fn from_env() -> Result<Self, ModelError> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| ModelError::AuthError("OPENAI_API_KEY not set".into()))?;
        Self::new(api_key)
    }

    fn build_request_body(&self, req: &CompletionRequest) -> Value {
        let model = req.model.strip_prefix("openai/").unwrap_or(&req.model);

        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|msg| match (&msg.role, &msg.content) {
                (Role::System, Content::Text { text }) => json!({
                    "role": "system",
                    "content": text,
                }),
                (Role::User, Content::Text { text }) => json!({
                    "role": "user",
                    "content": text,
                }),
                (Role::Assistant, Content::Text { text }) => json!({
                    "role": "assistant",
                    "content": text,
                }),
                (Role::Assistant, Content::ToolCalls { tool_calls }) => {
                    let tc: Vec<Value> = tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": tc.arguments.to_string(),
                                }
                            })
                        })
                        .collect();
                    json!({
                        "role": "assistant",
                        "tool_calls": tc,
                    })
                }
                (Role::Tool, Content::ToolResult { tool_call_id, content }) => json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": content,
                }),
                _ => json!({"role": "user", "content": ""}),
            })
            .collect();

        let mut body = json!({
            "model": model,
            "messages": messages,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
        });

        if req.stream {
            body["stream"] = json!(true);
            body["stream_options"] = json!({"include_usage": true});
        }

        if let Some(tools) = &req.tools {
            let tool_defs: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect();
            body["tools"] = json!(tool_defs);
        }

        body
    }

    fn parse_response(
        &self,
        body: &Value,
        latency_ms: u64,
    ) -> Result<CompletionResponse, ModelError> {
        let choice = body["choices"]
            .get(0)
            .ok_or_else(|| ModelError::ParseError("no choices in response".into()))?;

        let msg = &choice["message"];
        let message = if let Some(tool_calls) = msg["tool_calls"].as_array() {
            let calls: Vec<ToolCall> = tool_calls
                .iter()
                .map(|tc| {
                    let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                    ToolCall {
                        id: tc["id"].as_str().unwrap_or("").to_string(),
                        name: tc["function"]["name"].as_str().unwrap_or("").to_string(),
                        arguments: serde_json::from_str(args_str).unwrap_or(json!({})),
                    }
                })
                .collect();
            ChatMessage::assistant_tool_calls(calls)
        } else {
            let text = msg["content"].as_str().unwrap_or("");
            ChatMessage::assistant_text(text)
        };

        let usage = if let Some(u) = body.get("usage") {
            TokenUsage {
                input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                output_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: u["total_tokens"].as_u64().unwrap_or(0) as u32,
            }
        } else {
            TokenUsage::default()
        };

        let model_used = body["model"].as_str().unwrap_or("").to_string();

        Ok(CompletionResponse {
            message,
            usage,
            model: model_used,
            latency_ms,
        })
    }
}

#[async_trait]
impl ModelProvider for OpenAIProvider {
    fn id(&self) -> &str {
        "openai"
    }

    fn supported_models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "gpt-4o".into(),
                display_name: "GPT-4o".into(),
                context_window: 128_000,
                supports_tools: true,
                supports_vision: true,
                supports_streaming: true,
                cost_per_1k_input: 0.005,
                cost_per_1k_output: 0.015,
            },
            ModelInfo {
                id: "gpt-4o-mini".into(),
                display_name: "GPT-4o Mini".into(),
                context_window: 128_000,
                supports_tools: true,
                supports_vision: true,
                supports_streaming: true,
                cost_per_1k_input: 0.00015,
                cost_per_1k_output: 0.0006,
            },
        ]
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ModelError> {
        let body = self.build_request_body(&req);
        let start = std::time::Instant::now();

        let resp = self
            .client
            .post(OPENAI_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
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
                401 => Err(ModelError::AuthError(err_body)),
                429 => Err(ModelError::RateLimited { retry_after_ms: None }),
                500 | 503 => Err(ModelError::Overloaded),
                _ => Err(ModelError::ProviderError(format!("{}: {}", status, err_body))),
            };
        }

        let latency_ms = start.elapsed().as_millis() as u64;
        let resp_body: Value = resp
            .json()
            .await
            .map_err(|e| ModelError::ParseError(e.to_string()))?;

        self.parse_response(&resp_body, latency_ms)
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamChunk> + Send>>, ModelError> {
        let mut body = self.build_request_body(&req);
        body["stream"] = json!(true);
        body["stream_options"] = json!({"include_usage": true});

        let resp = self
            .client
            .post(OPENAI_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
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
                401 => Err(ModelError::AuthError(err_body)),
                429 => Err(ModelError::RateLimited { retry_after_ms: None }),
                500 | 503 => Err(ModelError::Overloaded),
                _ => Err(ModelError::ProviderError(format!("{}: {}", status, err_body))),
            };
        }

        let byte_stream = resp.bytes_stream();

        use futures::StreamExt;
        use futures::TryStreamExt;

        let chunk_stream = futures::stream::unfold(
            (byte_stream, String::new()),
            |(mut byte_stream, mut buffer)| async move {
                loop {
                    while let Some(newline_pos) = buffer.find('\n') {
                        let line = buffer[..newline_pos].trim().to_string();
                        buffer = buffer[newline_pos + 1..].to_string();

                        if line.is_empty() {
                            continue;
                        }

                        if let Some(data) = line.strip_prefix("data: ") {
                            let data = data.trim();
                            if data == "[DONE]" {
                                return Some((StreamChunk::Done, (byte_stream, buffer)));
                            }

                            if let Ok(parsed) = serde_json::from_str::<Value>(data) {
                                if let Some(chunk) = parse_openai_sse_chunk(&parsed) {
                                    return Some((chunk, (byte_stream, buffer)));
                                }
                            }
                        }
                    }

                    match byte_stream.try_next().await {
                        Ok(Some(bytes)) => {
                            buffer.push_str(&String::from_utf8_lossy(&bytes));
                        }
                        Ok(None) => {
                            return Some((StreamChunk::Done, (byte_stream, buffer)));
                        }
                        Err(e) => {
                            return Some((
                                StreamChunk::Error(e.to_string()),
                                (byte_stream, buffer),
                            ));
                        }
                    }
                }
            },
        );

        let chunk_stream = chunk_stream.take_while(|chunk| {
            let cont = !matches!(chunk, StreamChunk::Done | StreamChunk::Error(_));
            futures::future::ready(cont)
        });

        let final_stream = chunk_stream.chain(futures::stream::once(async { StreamChunk::Done }));

        Ok(Box::pin(final_stream))
    }

    async fn embed(&self, texts: Vec<String>, model: &str) -> Result<Vec<Vec<f32>>, ModelError> {
        let model = model.strip_prefix("openai/").unwrap_or(model);

        let resp = self
            .client
            .post("https://api.openai.com/v1/embeddings")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&json!({
                "input": texts,
                "model": model,
            }))
            .send()
            .await
            .map_err(|e| ModelError::RequestError(e.to_string()))?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            return Err(ModelError::ProviderError(err));
        }

        let body: Value = resp
            .json()
            .await
            .map_err(|e| ModelError::ParseError(e.to_string()))?;

        let embeddings = body["data"]
            .as_array()
            .ok_or_else(|| ModelError::ParseError("missing data array".into()))?
            .iter()
            .filter_map(|item| {
                item["embedding"]
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|v| v.as_f64().map(|f| f as f32)).collect())
            })
            .collect();

        Ok(embeddings)
    }

    async fn health_check(&self) -> HealthStatus {
        if self.api_key.is_empty() {
            return HealthStatus::Unavailable("API key not configured".into());
        }
        HealthStatus::Healthy
    }
}

fn parse_openai_sse_chunk(data: &Value) -> Option<StreamChunk> {
    // Check for usage (sent at the end in stream_options mode)
    if let Some(usage) = data.get("usage") {
        if !usage.is_null() {
            return Some(StreamChunk::Usage(TokenUsage {
                input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: usage["total_tokens"].as_u64().unwrap_or(0) as u32,
            }));
        }
    }

    let choice = data["choices"].get(0)?;
    let delta = &choice["delta"];

    // Tool calls
    if let Some(tool_calls) = delta["tool_calls"].as_array() {
        for tc in tool_calls {
            let index = tc["index"].as_u64().unwrap_or(0);
            let id = tc["id"].as_str().unwrap_or("").to_string();

            if let Some(func) = tc.get("function") {
                if let Some(name) = func["name"].as_str() {
                    return Some(StreamChunk::ToolCallStart {
                        id: if id.is_empty() {
                            format!("tc_{}", index)
                        } else {
                            id
                        },
                        name: name.to_string(),
                    });
                }
                if let Some(args) = func["arguments"].as_str() {
                    if !args.is_empty() {
                        return Some(StreamChunk::ToolCallArgumentsDelta {
                            id: if id.is_empty() {
                                format!("tc_{}", index)
                            } else {
                                id
                            },
                            delta: args.to_string(),
                        });
                    }
                }
            }
        }
    }

    // Text delta
    if let Some(content) = delta["content"].as_str() {
        if !content.is_empty() {
            return Some(StreamChunk::TextDelta(content.to_string()));
        }
    }

    // Finish reason
    if let Some(reason) = choice["finish_reason"].as_str() {
        if reason == "stop" || reason == "tool_calls" {
            return Some(StreamChunk::Done);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_openai_request_body() {
        let provider = OpenAIProvider::new("sk-test".into()).unwrap();
        let req = CompletionRequest {
            model: "openai/gpt-4o".into(),
            messages: vec![
                ChatMessage::system("Be helpful"),
                ChatMessage::user("Hi"),
            ],
            tools: None,
            temperature: 0.7,
            max_tokens: 1024,
            stream: false,
        };

        let body = provider.build_request_body(&req);
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_parse_openai_response() {
        let provider = OpenAIProvider::new("sk-test".into()).unwrap();
        let resp = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            },
            "model": "gpt-4o-2024-05-13"
        });

        let parsed = provider.parse_response(&resp, 100).unwrap();
        assert_eq!(parsed.message.text().unwrap(), "Hello!");
        assert_eq!(parsed.usage.total_tokens, 15);
    }

    #[test]
    fn test_parse_openai_tool_calls() {
        let provider = OpenAIProvider::new("sk-test".into()).unwrap();
        let resp = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"London\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 15, "total_tokens": 25},
            "model": "gpt-4o"
        });

        let parsed = provider.parse_response(&resp, 50).unwrap();
        let calls = parsed.message.tool_calls().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
    }

    #[test]
    fn test_parse_openai_sse_text_delta() {
        let data = json!({
            "choices": [{"delta": {"content": "Hello"}, "index": 0}]
        });
        let chunk = parse_openai_sse_chunk(&data);
        assert!(matches!(chunk, Some(StreamChunk::TextDelta(ref t)) if t == "Hello"));
    }

    #[test]
    fn test_parse_openai_sse_done() {
        let data = json!({
            "choices": [{"delta": {}, "finish_reason": "stop", "index": 0}]
        });
        let chunk = parse_openai_sse_chunk(&data);
        assert!(matches!(chunk, Some(StreamChunk::Done)));
    }

    #[test]
    fn test_openai_tool_call_normalization() {
        // OpenAI uses "function" format, we normalize to ToolCall
        let provider = OpenAIProvider::new("sk-test".into()).unwrap();

        // Build request with tools
        let req = CompletionRequest {
            model: "openai/gpt-4o".into(),
            messages: vec![ChatMessage::user("weather")],
            tools: Some(vec![ToolDefinition {
                name: "get_weather".into(),
                description: "Get weather".into(),
                input_schema: json!({"type": "object"}),
            }]),
            temperature: 0.5,
            max_tokens: 1024,
            stream: false,
        };

        let body = provider.build_request_body(&req);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
    }
}
