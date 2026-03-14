use std::sync::Arc;

use serde_json::{json, Value};

use nexmind_memory::{MemoryQuery, MemoryStoreImpl, MemoryType};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

pub struct MemoryReadTool {
    memory_store: Arc<MemoryStoreImpl>,
}

impl MemoryReadTool {
    pub fn new(memory_store: Arc<MemoryStoreImpl>) -> Self {
        Self { memory_store }
    }
}

#[async_trait::async_trait]
impl Tool for MemoryReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "memory_read".into(),
            name: "memory_read".into(),
            description: "Search and retrieve relevant memories".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query for relevant memories"
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Maximum number of results (default: 5)"
                    }
                },
                "required": ["query"]
            }),
            required_permissions: vec!["memory:read".into()],
            trust_level: 0,
            idempotent: true,
            timeout_seconds: 30,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("query").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'query' is required".into()));
        }
        Ok(())
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let query_text = args["query"].as_str().unwrap().to_string();
        let top_k = args
            .get("top_k")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        let result = self
            .memory_store
            .retrieve_full(MemoryQuery {
                query_text,
                workspace_id: ctx.workspace_id.clone(),
                agent_id: Some(ctx.agent_id.clone()),
                memory_types: vec![MemoryType::Semantic, MemoryType::Pinned],
                top_k,
                min_importance: Some(0.3),
            })
            .await
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        let memories: Vec<Value> = result
            .memories
            .iter()
            .map(|sm| {
                json!({
                    "content": sm.memory.content,
                    "importance": sm.memory.importance,
                    "type": sm.memory.memory_type.as_str(),
                    "score": sm.score,
                })
            })
            .collect();

        Ok(ToolOutput::Success {
            result: json!({ "memories": memories }),
            tokens_used: None,
        })
    }
}
