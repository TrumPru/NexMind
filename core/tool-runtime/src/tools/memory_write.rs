use std::sync::Arc;

use serde_json::{json, Value};

use nexmind_memory::{AccessPolicy, MemorySource, MemoryStoreImpl, MemoryType, NewMemory};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

pub struct MemoryWriteTool {
    memory_store: Arc<MemoryStoreImpl>,
}

impl MemoryWriteTool {
    pub fn new(memory_store: Arc<MemoryStoreImpl>) -> Self {
        Self { memory_store }
    }
}

#[async_trait::async_trait]
impl Tool for MemoryWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "memory_write".into(),
            name: "memory_write".into(),
            description: "Store a new memory (user preference, fact, context)".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "Content to remember"
                    },
                    "memory_type": {
                        "type": "string",
                        "enum": ["semantic", "pinned"],
                        "description": "Type of memory (default: semantic)"
                    },
                    "importance": {
                        "type": "number",
                        "description": "Importance score 0.0-1.0 (optional)"
                    }
                },
                "required": ["content"]
            }),
            required_permissions: vec!["memory:write".into()],
            trust_level: 1,
            idempotent: true,
            timeout_seconds: 30,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("content").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'content' is required".into()));
        }
        Ok(())
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let content = args["content"].as_str().unwrap().to_string();
        let memory_type = args
            .get("memory_type")
            .and_then(|v| v.as_str())
            .and_then(MemoryType::from_str)
            .unwrap_or(MemoryType::Semantic);
        let importance = args.get("importance").and_then(|v| v.as_f64());

        let memory_id = self
            .memory_store
            .store(NewMemory {
                workspace_id: ctx.workspace_id.clone(),
                agent_id: Some(ctx.agent_id.clone()),
                memory_type,
                content,
                source: MemorySource::Agent,
                source_task_id: None,
                access_policy: AccessPolicy::Workspace,
                metadata: None,
                importance,
            })
            .await
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        Ok(ToolOutput::Success {
            result: json!({
                "memory_id": memory_id,
                "stored": true,
            }),
            tokens_used: None,
        })
    }
}
