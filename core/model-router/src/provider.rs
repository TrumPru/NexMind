use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::types::*;

/// Provider-agnostic LLM abstraction.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Provider identifier (e.g., "anthropic", "openai", "ollama").
    fn id(&self) -> &str;

    /// List of models this provider supports.
    fn supported_models(&self) -> Vec<ModelInfo>;

    /// Non-streaming completion.
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ModelError>;

    /// Streaming completion.
    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamChunk> + Send>>, ModelError>;

    /// Generate embeddings.
    async fn embed(&self, texts: Vec<String>, model: &str) -> Result<Vec<Vec<f32>>, ModelError>;

    /// Check provider health.
    async fn health_check(&self) -> HealthStatus;
}
