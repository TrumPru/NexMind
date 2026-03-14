use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use tracing::{info, warn};

use crate::provider::ModelProvider;
use crate::types::*;

/// Central model router that dispatches requests to the right provider.
pub struct ModelRouter {
    providers: HashMap<String, Arc<dyn ModelProvider>>,
    model_registry: Vec<ModelInfo>,
}

impl Default for ModelRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelRouter {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            model_registry: Vec::new(),
        }
    }

    /// Register a provider.
    pub fn register_provider(&mut self, provider: Arc<dyn ModelProvider>) {
        let id = provider.id().to_string();
        let models = provider.supported_models();
        info!(provider = %id, models = models.len(), "registered provider");
        self.model_registry.extend(models);
        self.providers.insert(id, provider);
    }

    /// List all registered providers.
    pub fn providers(&self) -> Vec<&str> {
        self.providers.keys().map(|s| s.as_str()).collect()
    }

    /// List all known models across providers.
    pub fn models(&self) -> &[ModelInfo] {
        &self.model_registry
    }

    /// Parse "provider/model" string.
    fn parse_model(model: &str) -> (&str, &str) {
        match model.split_once('/') {
            Some((provider, model)) => (provider, model),
            None => ("", model),
        }
    }

    /// Get the provider for a model string.
    fn get_provider(&self, model: &str) -> Result<&Arc<dyn ModelProvider>, ModelError> {
        let (provider_id, _) = Self::parse_model(model);
        self.providers
            .get(provider_id)
            .ok_or_else(|| ModelError::ProviderNotFound(provider_id.to_string()))
    }

    /// Route a completion request to the right provider.
    pub async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ModelError> {
        let provider = self.get_provider(&req.model)?;
        provider.complete(req).await
    }

    /// Route a streaming request to the right provider.
    pub async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamChunk> + Send>>, ModelError> {
        let provider = self.get_provider(&req.model)?;
        provider.stream(req).await
    }

    /// Route an embedding request.
    pub async fn embed(
        &self,
        texts: Vec<String>,
        model: &str,
    ) -> Result<Vec<Vec<f32>>, ModelError> {
        let provider = self.get_provider(model)?;
        provider.embed(texts, model).await
    }

    /// Completion with fallback: if primary fails, try fallback model.
    pub async fn complete_with_fallback(
        &self,
        primary: CompletionRequest,
        fallback_model: &str,
    ) -> Result<CompletionResponse, ModelError> {
        match self.complete(primary.clone()).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                warn!(
                    error = %e,
                    fallback = fallback_model,
                    "primary model failed, trying fallback"
                );

                let fallback_req = CompletionRequest {
                    model: fallback_model.to_string(),
                    ..primary
                };

                self.complete(fallback_req).await
            }
        }
    }

    /// Check health of all providers.
    pub async fn health_check_all(&self) -> HashMap<String, HealthStatus> {
        let mut results = HashMap::new();
        for (id, provider) in &self.providers {
            let status = provider.health_check().await;
            results.insert(id.clone(), status);
        }
        results
    }

    /// Check if any provider is available.
    pub fn has_providers(&self) -> bool {
        !self.providers.is_empty()
    }

    /// Check if a specific provider is registered.
    pub fn has_provider(&self, provider_id: &str) -> bool {
        self.providers.contains_key(provider_id)
    }

    /// List all registered provider IDs.
    pub fn available_providers(&self) -> Vec<&str> {
        self.providers.keys().map(|s| s.as_str()).collect()
    }

    /// Select the best available model based on provider priority.
    ///
    /// Priority (anthropic only registers with a real `sk-ant-api*` key):
    /// 1. anthropic/* — direct API with real API key (fastest, full tool support)
    /// 2. claude-code/* — subscription via CLI proxy (free, no tool support)
    /// 3. openai/* — direct API with OpenAI key
    /// 4. ollama/* — local models (free, requires Ollama running)
    pub fn select_default_model(&self) -> String {
        let priorities = [
            ("anthropic", "anthropic/claude-sonnet-4-20250514"),
            ("claude-code", "claude-code/sonnet"),
            ("openai", "openai/gpt-4o"),
            ("ollama", "ollama/llama3.2"),
        ];

        for (provider_id, model) in &priorities {
            if self.has_provider(provider_id) {
                return model.to_string();
            }
        }

        // Shouldn't reach here if at least one provider is registered
        "anthropic/claude-sonnet-4-20250514".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Mock provider for testing
    struct MockProvider {
        provider_id: String,
        fail_count: AtomicU32,
        fail_limit: u32,
    }

    impl MockProvider {
        fn new(id: &str) -> Self {
            Self {
                provider_id: id.to_string(),
                fail_count: AtomicU32::new(0),
                fail_limit: 0,
            }
        }

        fn failing(id: &str, fail_times: u32) -> Self {
            Self {
                provider_id: id.to_string(),
                fail_count: AtomicU32::new(0),
                fail_limit: fail_times,
            }
        }
    }

    #[async_trait]
    impl ModelProvider for MockProvider {
        fn id(&self) -> &str {
            &self.provider_id
        }

        fn supported_models(&self) -> Vec<ModelInfo> {
            vec![ModelInfo {
                id: "test-model".into(),
                display_name: "Test".into(),
                context_window: 4096,
                supports_tools: true,
                supports_vision: false,
                supports_streaming: true,
                cost_per_1k_input: 0.001,
                cost_per_1k_output: 0.002,
            }]
        }

        async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ModelError> {
            let count = self.fail_count.fetch_add(1, Ordering::SeqCst);
            if count < self.fail_limit {
                return Err(ModelError::Overloaded);
            }
            Ok(CompletionResponse {
                message: ChatMessage::assistant_text("mock response"),
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    total_tokens: 15,
                },
                model: req.model,
                latency_ms: 50,
            })
        }

        async fn stream(
            &self,
            _req: CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = StreamChunk> + Send>>, ModelError> {
            let chunks = vec![
                StreamChunk::TextDelta("hello ".into()),
                StreamChunk::TextDelta("world".into()),
                StreamChunk::Done,
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }

        async fn embed(
            &self,
            texts: Vec<String>,
            _model: &str,
        ) -> Result<Vec<Vec<f32>>, ModelError> {
            Ok(texts.iter().map(|_| vec![0.1, 0.2, 0.3]).collect())
        }

        async fn health_check(&self) -> HealthStatus {
            HealthStatus::Healthy
        }
    }

    #[tokio::test]
    async fn test_router_register_and_complete() {
        let mut router = ModelRouter::new();
        router.register_provider(Arc::new(MockProvider::new("mock")));

        let req = CompletionRequest {
            model: "mock/test-model".into(),
            messages: vec![ChatMessage::user("hello")],
            tools: None,
            temperature: 0.7,
            max_tokens: 100,
            stream: false,
        };

        let resp = router.complete(req).await.unwrap();
        assert_eq!(resp.message.text().unwrap(), "mock response");
    }

    #[tokio::test]
    async fn test_router_provider_not_found() {
        let router = ModelRouter::new();
        let req = CompletionRequest {
            model: "nonexistent/model".into(),
            messages: vec![ChatMessage::user("hello")],
            tools: None,
            temperature: 0.7,
            max_tokens: 100,
            stream: false,
        };

        let err = router.complete(req).await.unwrap_err();
        assert!(matches!(err, ModelError::ProviderNotFound(_)));
    }

    #[tokio::test]
    async fn test_router_fallback() {
        let mut router = ModelRouter::new();
        router.register_provider(Arc::new(MockProvider::failing("primary", 1)));
        router.register_provider(Arc::new(MockProvider::new("fallback")));

        let req = CompletionRequest {
            model: "primary/test-model".into(),
            messages: vec![ChatMessage::user("hello")],
            tools: None,
            temperature: 0.7,
            max_tokens: 100,
            stream: false,
        };

        let resp = router
            .complete_with_fallback(req, "fallback/test-model")
            .await
            .unwrap();

        assert_eq!(resp.message.text().unwrap(), "mock response");
    }

    #[tokio::test]
    async fn test_router_stream() {
        use futures::StreamExt;

        let mut router = ModelRouter::new();
        router.register_provider(Arc::new(MockProvider::new("mock")));

        let req = CompletionRequest {
            model: "mock/test-model".into(),
            messages: vec![ChatMessage::user("hello")],
            tools: None,
            temperature: 0.7,
            max_tokens: 100,
            stream: true,
        };

        let mut stream = router.stream(req).await.unwrap();
        let mut text = String::new();

        while let Some(chunk) = stream.next().await {
            match chunk {
                StreamChunk::TextDelta(t) => text.push_str(&t),
                StreamChunk::Done => break,
                _ => {}
            }
        }

        assert_eq!(text, "hello world");
    }

    #[tokio::test]
    async fn test_router_health_check_all() {
        let mut router = ModelRouter::new();
        router.register_provider(Arc::new(MockProvider::new("mock1")));
        router.register_provider(Arc::new(MockProvider::new("mock2")));

        let health = router.health_check_all().await;
        assert_eq!(health.len(), 2);
        assert!(matches!(health["mock1"], HealthStatus::Healthy));
    }

    #[test]
    fn test_parse_model_string() {
        assert_eq!(ModelRouter::parse_model("anthropic/claude-sonnet-4-20250514"), ("anthropic", "claude-sonnet-4-20250514"));
        assert_eq!(ModelRouter::parse_model("openai/gpt-4o"), ("openai", "gpt-4o"));
        assert_eq!(ModelRouter::parse_model("just-model"), ("", "just-model"));
    }

    #[test]
    fn test_has_provider() {
        let mut router = ModelRouter::new();
        assert!(!router.has_provider("mock"));
        router.register_provider(Arc::new(MockProvider::new("mock")));
        assert!(router.has_provider("mock"));
        assert!(!router.has_provider("nonexistent"));
    }

    #[test]
    fn test_available_providers() {
        let mut router = ModelRouter::new();
        assert!(router.available_providers().is_empty());
        router.register_provider(Arc::new(MockProvider::new("alpha")));
        router.register_provider(Arc::new(MockProvider::new("beta")));
        let providers = router.available_providers();
        assert_eq!(providers.len(), 2);
        assert!(providers.contains(&"alpha"));
        assert!(providers.contains(&"beta"));
    }

    #[test]
    fn test_select_default_model_anthropic_priority() {
        let mut router = ModelRouter::new();
        router.register_provider(Arc::new(MockProvider::new("anthropic")));
        router.register_provider(Arc::new(MockProvider::new("ollama")));
        assert_eq!(
            router.select_default_model(),
            "anthropic/claude-sonnet-4-20250514"
        );
    }

    #[test]
    fn test_select_default_model_claude_code_fallback() {
        let mut router = ModelRouter::new();
        router.register_provider(Arc::new(MockProvider::new("claude-code")));
        router.register_provider(Arc::new(MockProvider::new("ollama")));
        assert_eq!(router.select_default_model(), "claude-code/sonnet");
    }

    #[test]
    fn test_select_default_model_openai_fallback() {
        let mut router = ModelRouter::new();
        router.register_provider(Arc::new(MockProvider::new("openai")));
        assert_eq!(router.select_default_model(), "openai/gpt-4o");
    }

    #[test]
    fn test_select_default_model_ollama_fallback() {
        let mut router = ModelRouter::new();
        router.register_provider(Arc::new(MockProvider::new("ollama")));
        assert_eq!(router.select_default_model(), "ollama/llama3.2");
    }

    #[test]
    fn test_select_default_model_no_providers() {
        let router = ModelRouter::new();
        // Returns hardcoded default when no providers
        assert_eq!(
            router.select_default_model(),
            "anthropic/claude-sonnet-4-20250514"
        );
    }
}
