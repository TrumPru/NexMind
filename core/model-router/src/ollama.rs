use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use reqwest::Client;
use serde_json::{json, Value};
use crate::provider::ModelProvider;
use crate::types::*;

pub struct OllamaProvider {
    client: Client,
    base_url: String,
}

impl OllamaProvider {
    pub fn new(base_url: &str) -> Result<Self, ModelError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| ModelError::ProviderError(e.to_string()))?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    pub fn default_local() -> Result<Self, ModelError> {
        Self::new("http://localhost:11434")
    }

    fn build_request_body(&self, req: &CompletionRequest) -> Value {
        let model = req.model.strip_prefix("ollama/").unwrap_or(&req.model);

        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|msg| match (&msg.role, &msg.content) {
                (Role::System, Content::Text { text }) => json!({"role": "system", "content": text}),
                (Role::User, Content::Text { text }) => json!({"role": "user", "content": text}),
                (Role::Assistant, Content::Text { text }) => json!({"role": "assistant", "content": text}),
                (Role::Tool, Content::ToolResult { content, .. }) => json!({"role": "tool", "content": content}),
                _ => json!({"role": "user", "content": ""}),
            })
            .collect();

        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": req.stream,
            "options": {
                "temperature": req.temperature,
                "num_predict": req.max_tokens,
            }
        });

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
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    fn id(&self) -> &str {
        "ollama"
    }

    fn supported_models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "llama3.2".into(),
                display_name: "Llama 3.2".into(),
                context_window: 128_000,
                supports_tools: true,
                supports_vision: false,
                supports_streaming: true,
                cost_per_1k_input: 0.0,
                cost_per_1k_output: 0.0,
            },
            ModelInfo {
                id: "qwen2.5-coder".into(),
                display_name: "Qwen 2.5 Coder".into(),
                context_window: 32_000,
                supports_tools: true,
                supports_vision: false,
                supports_streaming: true,
                cost_per_1k_input: 0.0,
                cost_per_1k_output: 0.0,
            },
        ]
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ModelError> {
        let mut body = self.build_request_body(&req);
        body["stream"] = json!(false);

        let url = format!("{}/api/chat", self.base_url);
        let start = std::time::Instant::now();

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ModelError::Timeout
                } else if e.is_connect() {
                    ModelError::ProviderError("Ollama not running at localhost:11434".into())
                } else {
                    ModelError::RequestError(e.to_string())
                }
            })?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            return Err(ModelError::ProviderError(err));
        }

        let latency_ms = start.elapsed().as_millis() as u64;
        let resp_body: Value = resp
            .json()
            .await
            .map_err(|e| ModelError::ParseError(e.to_string()))?;

        let content = resp_body["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();

        let usage = TokenUsage {
            input_tokens: resp_body["prompt_eval_count"].as_u64().unwrap_or(0) as u32,
            output_tokens: resp_body["eval_count"].as_u64().unwrap_or(0) as u32,
            total_tokens: (resp_body["prompt_eval_count"].as_u64().unwrap_or(0)
                + resp_body["eval_count"].as_u64().unwrap_or(0)) as u32,
        };

        let model_used = resp_body["model"]
            .as_str()
            .unwrap_or(&req.model)
            .to_string();

        Ok(CompletionResponse {
            message: ChatMessage::assistant_text(&content),
            usage,
            model: model_used,
            latency_ms,
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamChunk> + Send>>, ModelError> {
        let mut body = self.build_request_body(&req);
        body["stream"] = json!(true);

        let url = format!("{}/api/chat", self.base_url);

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ModelError::Timeout
                } else if e.is_connect() {
                    ModelError::ProviderError("Ollama not running".into())
                } else {
                    ModelError::RequestError(e.to_string())
                }
            })?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            return Err(ModelError::ProviderError(err));
        }

        let byte_stream = resp.bytes_stream();

        use futures::StreamExt;
        use futures::TryStreamExt;

        // Ollama uses newline-delimited JSON, not SSE
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

                        if let Ok(parsed) = serde_json::from_str::<Value>(&line) {
                            if parsed["done"].as_bool() == Some(true) {
                                // Final message with usage info
                                let usage = TokenUsage {
                                    input_tokens: parsed["prompt_eval_count"]
                                        .as_u64()
                                        .unwrap_or(0)
                                        as u32,
                                    output_tokens: parsed["eval_count"].as_u64().unwrap_or(0)
                                        as u32,
                                    total_tokens: 0,
                                };
                                return Some((StreamChunk::Usage(usage), (byte_stream, buffer)));
                            }

                            if let Some(content) = parsed["message"]["content"].as_str() {
                                if !content.is_empty() {
                                    return Some((
                                        StreamChunk::TextDelta(content.to_string()),
                                        (byte_stream, buffer),
                                    ));
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
        let model = model.strip_prefix("ollama/").unwrap_or(model);
        let mut results = Vec::new();

        for text in &texts {
            let url = format!("{}/api/embeddings", self.base_url);
            let resp = self
                .client
                .post(&url)
                .json(&json!({
                    "model": model,
                    "prompt": text,
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

            let embedding: Vec<f32> = body["embedding"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();

            results.push(embedding);
        }

        Ok(results)
    }

    async fn health_check(&self) -> HealthStatus {
        let url = format!("{}/api/tags", self.base_url);
        match self.client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => HealthStatus::Healthy,
            Ok(resp) => HealthStatus::Degraded(format!("HTTP {}", resp.status())),
            Err(_) => HealthStatus::Unavailable("Ollama not running".into()),
        }
    }
}
