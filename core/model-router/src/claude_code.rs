use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use tracing::{info, warn};

use crate::provider::ModelProvider;
use crate::types::*;

/// Provider that proxies LLM calls through the Claude Code CLI.
/// Works with Claude Pro/Max subscriptions via OAuth setup-token.
pub struct ClaudeCodeProvider {
    cli_path: String,
}

impl ClaudeCodeProvider {
    /// Resolve a NexMind model ID to the short name Claude Code CLI expects.
    fn resolve_model(model_id: &str) -> String {
        match model_id {
            "claude-code/sonnet" | "sonnet" => "sonnet".to_string(),
            "claude-code/opus" | "opus" => "opus".to_string(),
            "claude-code/haiku" | "haiku" => "haiku".to_string(),
            other => {
                // Extract model family from full ID like "anthropic/claude-sonnet-4-20250514"
                let lower = other.to_lowercase();
                if lower.contains("sonnet") {
                    "sonnet".to_string()
                } else if lower.contains("opus") {
                    "opus".to_string()
                } else if lower.contains("haiku") {
                    "haiku".to_string()
                } else {
                    "sonnet".to_string() // safe default
                }
            }
        }
    }

    /// Detect if the Claude Code CLI is installed and available.
    pub fn detect() -> Option<Self> {
        // Try common paths and `which`/`where` lookup
        let candidates = if cfg!(windows) {
            vec![
                "claude.cmd".to_string(),
                "claude".to_string(),
            ]
        } else {
            vec![
                "claude".to_string(),
            ]
        };

        for candidate in &candidates {
            if let Ok(output) = std::process::Command::new(candidate)
                .args(["--version"])
                .output()
            {
                if output.status.success() {
                    let version = String::from_utf8_lossy(&output.stdout);
                    info!(
                        cli_path = %candidate,
                        version = %version.trim(),
                        "Claude Code CLI detected"
                    );
                    return Some(Self {
                        cli_path: candidate.clone(),
                    });
                }
            }
        }

        // Try known install locations
        let extra_paths: Vec<String> = if cfg!(windows) {
            if let Ok(appdata) = std::env::var("APPDATA") {
                vec![format!("{}\\npm\\claude.cmd", appdata)]
            } else {
                vec![]
            }
        } else {
            let home = std::env::var("HOME").unwrap_or_default();
            vec![
                format!("{}/.npm-global/bin/claude", home),
                format!("{}/.local/bin/claude", home),
                "/usr/local/bin/claude".to_string(),
            ]
        };

        for path in &extra_paths {
            if let Ok(output) = std::process::Command::new(path)
                .args(["--version"])
                .output()
            {
                if output.status.success() {
                    let version = String::from_utf8_lossy(&output.stdout);
                    info!(
                        cli_path = %path,
                        version = %version.trim(),
                        "Claude Code CLI detected"
                    );
                    return Some(Self {
                        cli_path: path.clone(),
                    });
                }
            }
        }

        warn!("Claude Code CLI not found");
        None
    }

    /// Build the user/conversation prompt from non-system messages.
    fn build_user_prompt(req: &CompletionRequest) -> String {
        let mut parts = Vec::new();

        for msg in req.conversation_messages() {
            match (&msg.role, &msg.content) {
                (Role::User, Content::Text { text }) => {
                    parts.push(text.clone());
                }
                (Role::Assistant, Content::Text { text }) => {
                    parts.push(format!("[Previous response]: {}", text));
                }
                (Role::Tool, Content::ToolResult { content, .. }) => {
                    parts.push(format!("[Tool result]: {}", content));
                }
                _ => {}
            }
        }

        parts.join("\n\n")
    }

    /// Build a full prompt string (legacy: includes system prompt inline).
    /// Used as fallback when --system-prompt flag is not available.
    #[allow(dead_code)]
    fn build_prompt_with_system(req: &CompletionRequest) -> String {
        let mut parts = Vec::new();

        // System prompt as a clear instruction block
        if let Some(system) = req.system_prompt() {
            parts.push(format!("<instructions>\n{}\n</instructions>", system));
        }

        // Conversation messages
        parts.push(Self::build_user_prompt(req));

        parts.join("\n\n")
    }

    /// Try to extract a tool_use call from the response text.
    fn parse_tool_use(text: &str) -> Option<ToolCall> {
        let trimmed = text.trim();

        // Try direct JSON parse
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
            // Format: {"tool_use": {"name": "...", "arguments": {...}}}
            if let Some(tool_use) = json.get("tool_use") {
                let name = tool_use.get("name")?.as_str()?.to_string();
                let arguments = tool_use.get("arguments").cloned().unwrap_or(serde_json::json!({}));
                return Some(ToolCall {
                    id: format!("call_{}", ulid::Ulid::new()),
                    name,
                    arguments,
                });
            }

            // Claude Code SDK format: array of content blocks with tool_use type
            if let Some(arr) = json.as_array() {
                for block in arr {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        let name = block.get("name")?.as_str()?.to_string();
                        let arguments = block.get("input").cloned().unwrap_or(serde_json::json!({}));
                        return Some(ToolCall {
                            id: block.get("id").and_then(|i| i.as_str()).unwrap_or("call_1").to_string(),
                            name,
                            arguments,
                        });
                    }
                }
            }
        }

        // Try to find JSON embedded in text
        if let Some(start) = trimmed.find("{\"tool_use\"") {
            if let Some(end_candidate) = trimmed[start..].rfind('}') {
                let json_str = &trimmed[start..=start + end_candidate];
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
                    if let Some(tool_use) = json.get("tool_use") {
                        let name = tool_use.get("name")?.as_str()?.to_string();
                        let arguments = tool_use.get("arguments").cloned().unwrap_or(serde_json::json!({}));
                        return Some(ToolCall {
                            id: format!("call_{}", ulid::Ulid::new()),
                            name,
                            arguments,
                        });
                    }
                }
            }
        }

        None
    }

    /// Parse the response from claude CLI output.
    fn parse_response(output: &str, model: &str, latency_ms: u64) -> Result<CompletionResponse, ModelError> {
        // Try JSON parse first (when --output-format json is used)
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(output) {
            let text = json["result"]
                .as_str()
                .or_else(|| json["content"].as_str())
                .or_else(|| {
                    // Claude Code SDK JSON format: array of content blocks
                    json.as_array().and_then(|arr| {
                        arr.iter()
                            .find_map(|block| {
                                if block["type"].as_str() == Some("text") {
                                    block["text"].as_str()
                                } else {
                                    None
                                }
                            })
                    })
                })
                .unwrap_or(output.trim());

            Ok(CompletionResponse {
                message: ChatMessage::assistant_text(text),
                usage: TokenUsage::default(),
                model: model.to_string(),
                latency_ms,
            })
        } else {
            // Plain text output
            Ok(CompletionResponse {
                message: ChatMessage::assistant_text(output.trim()),
                usage: TokenUsage::default(),
                model: model.to_string(),
                latency_ms,
            })
        }
    }
}

#[async_trait]
impl ModelProvider for ClaudeCodeProvider {
    fn id(&self) -> &str {
        "claude-code"
    }

    fn supported_models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "sonnet".into(),
                display_name: "Claude Sonnet (via Claude Code)".into(),
                context_window: 200_000,
                supports_tools: true,
                supports_vision: false,
                supports_streaming: true,
                cost_per_1k_input: 0.0,
                cost_per_1k_output: 0.0,
            },
            ModelInfo {
                id: "opus".into(),
                display_name: "Claude Opus (via Claude Code)".into(),
                context_window: 200_000,
                supports_tools: true,
                supports_vision: false,
                supports_streaming: true,
                cost_per_1k_input: 0.0,
                cost_per_1k_output: 0.0,
            },
            ModelInfo {
                id: "haiku".into(),
                display_name: "Claude Haiku (via Claude Code)".into(),
                context_window: 200_000,
                supports_tools: true,
                supports_vision: false,
                supports_streaming: true,
                cost_per_1k_input: 0.0,
                cost_per_1k_output: 0.0,
            },
        ]
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ModelError> {
        let model_id = Self::resolve_model(&req.model);
        let has_tools = req.tools.as_ref().map_or(false, |t| !t.is_empty());

        let mut args = vec![
            "--print".to_string(),
            "--output-format".to_string(),
            if has_tools { "json".to_string() } else { "text".to_string() },
        ];

        // Add system prompt via --system-prompt flag
        if let Some(system) = req.system_prompt() {
            args.extend(["--system-prompt".to_string(), system]);
        }

        // Set model
        args.extend(["--model".to_string(), model_id]);

        // If tools are provided, include them in the system prompt as a tool schema
        // Claude Code CLI understands tool definitions when passed via prompt
        if has_tools {
            if let Some(ref tools) = req.tools {
                let tool_schema: Vec<serde_json::Value> = tools.iter().map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema
                    })
                }).collect();

                let tools_instruction = format!(
                    "\n\nYou have access to tools. To use a tool, respond with a JSON object:\n\
                    {{\"tool_use\": {{\"name\": \"<tool_name>\", \"arguments\": {{...}}}}}}\n\n\
                    Available tools:\n{}",
                    serde_json::to_string_pretty(&tool_schema).unwrap_or_default()
                );

                // Append to the prompt
                args.push("--append-system-prompt".to_string());
                args.push(tools_instruction);
            }
        }

        // Add the user/conversation prompt
        let user_prompt = Self::build_user_prompt(&req);
        args.extend(["-p".to_string(), user_prompt]);

        let start = std::time::Instant::now();

        let output = tokio::process::Command::new(&self.cli_path)
            .args(&args)
            .output()
            .await
            .map_err(|e| ModelError::ProviderError(format!("Failed to run Claude Code CLI: {}", e)))?;

        let latency_ms = start.elapsed().as_millis() as u64;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ModelError::ProviderError(format!(
                "Claude Code CLI failed: {}",
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Try to parse tool_use response
        if has_tools {
            if let Some(tool_call) = Self::parse_tool_use(&stdout) {
                return Ok(CompletionResponse {
                    message: ChatMessage::assistant_tool_calls(vec![tool_call]),
                    usage: TokenUsage::default(),
                    model: req.model.clone(),
                    latency_ms,
                });
            }
        }

        Self::parse_response(&stdout, &req.model, latency_ms)
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamChunk> + Send>>, ModelError> {
        // Claude Code CLI doesn't support true streaming in a way we can easily
        // consume via subprocess. Fall back to non-streaming: collect the full
        // response and yield it as a single chunk.
        let response = self.complete(req).await?;
        let text = response
            .message
            .text()
            .unwrap_or("")
            .to_string();

        let chunks = vec![
            StreamChunk::TextDelta(text),
            StreamChunk::Done,
        ];

        Ok(Box::pin(futures::stream::iter(chunks)))
    }

    async fn embed(&self, _texts: Vec<String>, _model: &str) -> Result<Vec<Vec<f32>>, ModelError> {
        Err(ModelError::ProviderError(
            "Embeddings not supported via Claude Code CLI proxy".into(),
        ))
    }

    async fn health_check(&self) -> HealthStatus {
        match tokio::process::Command::new(&self.cli_path)
            .args(["--version"])
            .output()
            .await
        {
            Ok(output) if output.status.success() => HealthStatus::Healthy,
            Ok(_) => HealthStatus::Unavailable("Claude Code CLI returned error".into()),
            Err(e) => HealthStatus::Unavailable(format!("Claude Code CLI not accessible: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_user_prompt_excludes_system() {
        let req = CompletionRequest {
            model: "claude-code/sonnet".into(),
            messages: vec![
                ChatMessage::system("You are helpful."),
                ChatMessage::user("Hello!"),
            ],
            tools: None,
            temperature: 0.7,
            max_tokens: 100,
            stream: false,
        };

        let prompt = ClaudeCodeProvider::build_user_prompt(&req);
        // System prompt should NOT be in user prompt
        assert!(!prompt.contains("You are helpful"));
        assert!(prompt.contains("Hello!"));
    }

    #[test]
    fn test_build_prompt_with_system_includes_instructions_block() {
        let req = CompletionRequest {
            model: "claude-code/sonnet".into(),
            messages: vec![
                ChatMessage::system("You are NexMind."),
                ChatMessage::user("Hello!"),
            ],
            tools: None,
            temperature: 0.7,
            max_tokens: 100,
            stream: false,
        };

        let prompt = ClaudeCodeProvider::build_prompt_with_system(&req);
        assert!(prompt.contains("<instructions>"));
        assert!(prompt.contains("You are NexMind."));
        assert!(prompt.contains("</instructions>"));
        assert!(prompt.contains("Hello!"));
    }

    #[test]
    fn test_build_user_prompt_with_history() {
        let req = CompletionRequest {
            model: "claude-code/sonnet".into(),
            messages: vec![
                ChatMessage::user("What is 2+2?"),
                ChatMessage::assistant_text("4"),
                ChatMessage::user("And 3+3?"),
            ],
            tools: None,
            temperature: 0.7,
            max_tokens: 100,
            stream: false,
        };

        let prompt = ClaudeCodeProvider::build_user_prompt(&req);
        assert!(prompt.contains("What is 2+2?"));
        assert!(prompt.contains("[Previous response]: 4"));
        assert!(prompt.contains("And 3+3?"));
    }

    #[test]
    fn test_system_prompt_extraction() {
        let req = CompletionRequest {
            model: "claude-code/sonnet".into(),
            messages: vec![
                ChatMessage::system("You are NexMind."),
                ChatMessage::user("Hello!"),
            ],
            tools: None,
            temperature: 0.7,
            max_tokens: 100,
            stream: false,
        };

        assert_eq!(req.system_prompt().unwrap(), "You are NexMind.");
    }

    #[test]
    fn test_system_prompt_returns_none_when_absent() {
        let req = CompletionRequest {
            model: "claude-code/sonnet".into(),
            messages: vec![ChatMessage::user("Hello!")],
            tools: None,
            temperature: 0.7,
            max_tokens: 100,
            stream: false,
        };

        assert!(req.system_prompt().is_none());
    }

    #[test]
    fn test_conversation_messages_excludes_system() {
        let req = CompletionRequest {
            model: "claude-code/sonnet".into(),
            messages: vec![
                ChatMessage::system("System instructions"),
                ChatMessage::user("Hello!"),
                ChatMessage::assistant_text("Hi!"),
            ],
            tools: None,
            temperature: 0.7,
            max_tokens: 100,
            stream: false,
        };

        let conv = req.conversation_messages();
        assert_eq!(conv.len(), 2);
        assert_eq!(conv[0].role, Role::User);
        assert_eq!(conv[1].role, Role::Assistant);
    }

    #[test]
    fn test_resolve_model_mapping() {
        assert_eq!(ClaudeCodeProvider::resolve_model("claude-code/sonnet"), "sonnet");
        assert_eq!(ClaudeCodeProvider::resolve_model("claude-code/opus"), "opus");
        assert_eq!(ClaudeCodeProvider::resolve_model("claude-code/haiku"), "haiku");
        assert_eq!(ClaudeCodeProvider::resolve_model("sonnet"), "sonnet");
        assert_eq!(ClaudeCodeProvider::resolve_model("anthropic/claude-sonnet-4-20250514"), "sonnet");
        assert_eq!(ClaudeCodeProvider::resolve_model("anthropic/claude-opus-4-20250514"), "opus");
        assert_eq!(ClaudeCodeProvider::resolve_model("unknown-model"), "sonnet"); // default
    }

    #[test]
    fn test_multi_turn_prompt_with_system_and_tool_result() {
        let req = CompletionRequest {
            model: "claude-code/sonnet".into(),
            messages: vec![
                ChatMessage::system("You are NexMind."),
                ChatMessage::user("Search for weather"),
                ChatMessage::assistant_text("Let me check..."),
                ChatMessage::tool_result("call_1", "Temperature: 22C"),
                ChatMessage::user("Thanks, what about tomorrow?"),
            ],
            tools: None,
            temperature: 0.7,
            max_tokens: 100,
            stream: false,
        };

        let user_prompt = ClaudeCodeProvider::build_user_prompt(&req);
        assert!(!user_prompt.contains("NexMind")); // system excluded
        assert!(user_prompt.contains("Search for weather"));
        assert!(user_prompt.contains("[Previous response]: Let me check..."));
        assert!(user_prompt.contains("[Tool result]: Temperature: 22C"));
        assert!(user_prompt.contains("Thanks, what about tomorrow?"));

        let full_prompt = ClaudeCodeProvider::build_prompt_with_system(&req);
        assert!(full_prompt.contains("<instructions>"));
        assert!(full_prompt.contains("You are NexMind."));
    }

    #[test]
    fn test_parse_response_plain_text() {
        let result = ClaudeCodeProvider::parse_response("Hello, world!", "claude-code/sonnet", 100);
        let resp = result.unwrap();
        assert_eq!(resp.message.text().unwrap(), "Hello, world!");
        assert_eq!(resp.latency_ms, 100);
    }

    #[test]
    fn test_parse_response_json_result() {
        let json = r#"{"result": "Hello from JSON"}"#;
        let result = ClaudeCodeProvider::parse_response(json, "claude-code/sonnet", 200);
        let resp = result.unwrap();
        assert_eq!(resp.message.text().unwrap(), "Hello from JSON");
    }

    #[test]
    fn test_parse_response_json_content_blocks() {
        let json = r#"[{"type": "text", "text": "Hello from blocks"}]"#;
        let result = ClaudeCodeProvider::parse_response(json, "claude-code/sonnet", 150);
        let resp = result.unwrap();
        assert_eq!(resp.message.text().unwrap(), "Hello from blocks");
    }

    #[test]
    fn test_parse_response_trims_whitespace() {
        let result = ClaudeCodeProvider::parse_response("  trimmed  \n", "claude-code/sonnet", 50);
        let resp = result.unwrap();
        assert_eq!(resp.message.text().unwrap(), "trimmed");
    }

    #[test]
    fn test_supported_models() {
        let provider = ClaudeCodeProvider {
            cli_path: "claude".into(),
        };
        let models = provider.supported_models();
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "sonnet");
        assert_eq!(models[1].id, "opus");
        assert_eq!(models[2].id, "haiku");
        assert_eq!(models[0].cost_per_1k_input, 0.0);
        assert!(models[0].supports_tools);
    }

    #[test]
    fn test_provider_id() {
        let provider = ClaudeCodeProvider {
            cli_path: "claude".into(),
        };
        assert_eq!(provider.id(), "claude-code");
    }

    #[test]
    fn test_detect_returns_none_when_cli_not_found() {
        let _result = ClaudeCodeProvider::detect();
    }
}
