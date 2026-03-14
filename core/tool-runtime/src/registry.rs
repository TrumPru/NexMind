use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tracing::info;

use nexmind_event_bus::{Event, EventBus, EventSource, EventType};
use nexmind_security::{parse_permission, check_permission, AuditLogger, Permission, PermissionVerdict};

use crate::{Tool, ToolDefinition, ToolError};

/// Context passed to tool execution.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub agent_id: String,
    pub workspace_id: String,
    pub workspace_path: PathBuf,
    pub granted_permissions: Vec<String>,
    pub correlation_id: String,
}

/// Result of tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolOutput {
    Success {
        result: serde_json::Value,
        tokens_used: Option<u32>,
    },
    Error {
        error: String,
        retryable: bool,
    },
    NeedsApproval {
        tool_id: String,
        tool_args: serde_json::Value,
        reason: String,
    },
}

/// Tool registry and executor with permission checking and audit logging.
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
    event_bus: Arc<EventBus>,
    audit: Arc<AuditLogger>,
}

impl ToolRegistry {
    pub fn new(event_bus: Arc<EventBus>, audit: Arc<AuditLogger>) -> Self {
        Self {
            tools: HashMap::new(),
            event_bus,
            audit,
        }
    }

    /// Register a built-in tool.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let def = tool.definition();
        info!(tool_id = %def.id, "registered tool");
        self.tools.insert(def.id.clone(), tool);
    }

    /// Get tool definitions filtered by agent's permissions.
    pub fn get_available_tools(&self, agent_permissions: &[String]) -> Vec<ToolDefinition> {
        let parsed_perms: Vec<Permission> = agent_permissions
            .iter()
            .filter_map(|p| parse_permission(p).ok())
            .collect();

        self.tools
            .values()
            .filter(|tool| {
                let def = tool.definition();
                // Include tool if agent has at least one of its required permissions
                // or the tool has no required permissions
                if def.required_permissions.is_empty() {
                    return true;
                }
                def.required_permissions.iter().all(|rp| {
                    if let Ok(req) = parse_permission(rp) {
                        matches!(check_permission(&parsed_perms, &req), PermissionVerdict::Allowed)
                    } else {
                        false
                    }
                })
            })
            .map(|t| t.definition())
            .collect()
    }

    /// Get all registered tool definitions (unfiltered).
    pub fn list_all_tools(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    /// Execute an already-approved tool call, bypassing the trust_level check.
    /// Permission checking and audit logging still apply.
    pub async fn execute_approved(
        &self,
        tool_name: &str,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = Instant::now();

        // 1. Find tool
        let tool = self
            .tools
            .get(tool_name)
            .ok_or_else(|| ToolError::NotFound(tool_name.to_string()))?;

        let def = tool.definition();

        // 2. Validate args
        tool.validate_args(&args)?;

        // 3. Check permissions (still enforced)
        let parsed_perms: Vec<Permission> = ctx
            .granted_permissions
            .iter()
            .filter_map(|p| parse_permission(p).ok())
            .collect();

        for req_perm_str in &def.required_permissions {
            if let Ok(req_perm) = parse_permission(req_perm_str) {
                match check_permission(&parsed_perms, &req_perm) {
                    PermissionVerdict::Allowed => {}
                    PermissionVerdict::Denied { required, .. } => {
                        let _ = self.audit.log_event(
                            &ctx.workspace_id,
                            "agent",
                            &ctx.agent_id,
                            "tool_permission_denied",
                            Some("tool"),
                            Some(tool_name),
                            "denied",
                            Some(&format!("missing permission: {}", required)),
                            Some(&ctx.correlation_id),
                            "system",
                            None,
                        );
                        return Ok(ToolOutput::Error {
                            error: format!(
                                "Permission denied: agent lacks '{}' permission required by tool '{}'",
                                req_perm_str, tool_name
                            ),
                            retryable: false,
                        });
                    }
                }
            }
        }

        // 4. Skip trust level check (this tool was approved)

        // 5. Execute with timeout
        let timeout = std::time::Duration::from_secs(def.timeout_seconds as u64);
        let result = tokio::time::timeout(timeout, tool.execute(args.clone(), ctx)).await;

        let duration_ms = start.elapsed().as_millis() as u64;

        let output = match result {
            Ok(Ok(output)) => {
                let _ = self.audit.log_event(
                    &ctx.workspace_id,
                    "agent",
                    &ctx.agent_id,
                    "tool_executed",
                    Some("tool"),
                    Some(tool_name),
                    "success",
                    None,
                    Some(&ctx.correlation_id),
                    "system",
                    Some(&serde_json::json!({"duration_ms": duration_ms, "approved": true}).to_string()),
                );
                output
            }
            Ok(Err(e)) => {
                let _ = self.audit.log_event(
                    &ctx.workspace_id,
                    "agent",
                    &ctx.agent_id,
                    "tool_executed",
                    Some("tool"),
                    Some(tool_name),
                    "error",
                    Some(&e.to_string()),
                    Some(&ctx.correlation_id),
                    "system",
                    None,
                );
                ToolOutput::Error {
                    error: e.to_string(),
                    retryable: !matches!(e, ToolError::ValidationError(_) | ToolError::PermissionDenied(_)),
                }
            }
            Err(_) => {
                let _ = self.audit.log_event(
                    &ctx.workspace_id,
                    "agent",
                    &ctx.agent_id,
                    "tool_executed",
                    Some("tool"),
                    Some(tool_name),
                    "timeout",
                    Some("tool execution timed out"),
                    Some(&ctx.correlation_id),
                    "system",
                    None,
                );
                ToolOutput::Error {
                    error: format!("Tool '{}' timed out after {}s", tool_name, def.timeout_seconds),
                    retryable: true,
                }
            }
        };

        // 6. Emit event
        self.event_bus.emit(Event::new(
            EventSource::Tool,
            EventType::ToolExecuted,
            serde_json::json!({
                "tool_id": tool_name,
                "agent_id": ctx.agent_id,
                "duration_ms": duration_ms,
                "success": matches!(output, ToolOutput::Success { .. }),
                "approved": true,
            }),
            &ctx.workspace_id,
            Some(ctx.correlation_id.clone()),
        ));

        Ok(output)
    }

    /// Execute a tool call with permission checking and audit logging.
    pub async fn execute(
        &self,
        tool_name: &str,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = Instant::now();

        // 1. Find tool
        let tool = self
            .tools
            .get(tool_name)
            .ok_or_else(|| ToolError::NotFound(tool_name.to_string()))?;

        let def = tool.definition();

        // 2. Validate args
        tool.validate_args(&args)?;

        // 3. Check permissions
        let parsed_perms: Vec<Permission> = ctx
            .granted_permissions
            .iter()
            .filter_map(|p| parse_permission(p).ok())
            .collect();

        for req_perm_str in &def.required_permissions {
            if let Ok(req_perm) = parse_permission(req_perm_str) {
                match check_permission(&parsed_perms, &req_perm) {
                    PermissionVerdict::Allowed => {}
                    PermissionVerdict::Denied { required, .. } => {
                        // Audit the denial
                        let _ = self.audit.log_event(
                            &ctx.workspace_id,
                            "agent",
                            &ctx.agent_id,
                            "tool_permission_denied",
                            Some("tool"),
                            Some(tool_name),
                            "denied",
                            Some(&format!("missing permission: {}", required)),
                            Some(&ctx.correlation_id),
                            "system",
                            None,
                        );

                        return Ok(ToolOutput::Error {
                            error: format!(
                                "Permission denied: agent lacks '{}' permission required by tool '{}'",
                                req_perm_str, tool_name
                            ),
                            retryable: false,
                        });
                    }
                }
            }
        }

        // 4. Check trust level — auto-approve 0 and 1, block ≥ 2
        if def.trust_level >= 2 {
            let _ = self.audit.log_event(
                &ctx.workspace_id,
                "agent",
                &ctx.agent_id,
                "tool_needs_approval",
                Some("tool"),
                Some(tool_name),
                "needs_approval",
                None,
                Some(&ctx.correlation_id),
                "system",
                None,
            );

            return Ok(ToolOutput::NeedsApproval {
                tool_id: def.id.clone(),
                tool_args: args,
                reason: format!(
                    "Tool '{}' has trust_level {} and requires user approval before execution",
                    tool_name, def.trust_level
                ),
            });
        }

        // 5. Execute with timeout
        let timeout = std::time::Duration::from_secs(def.timeout_seconds as u64);
        let result = tokio::time::timeout(timeout, tool.execute(args.clone(), ctx)).await;

        let duration_ms = start.elapsed().as_millis() as u64;

        let output = match result {
            Ok(Ok(output)) => {
                // 6. Audit log success
                let _ = self.audit.log_event(
                    &ctx.workspace_id,
                    "agent",
                    &ctx.agent_id,
                    "tool_executed",
                    Some("tool"),
                    Some(tool_name),
                    "success",
                    None,
                    Some(&ctx.correlation_id),
                    "system",
                    Some(&serde_json::json!({"duration_ms": duration_ms}).to_string()),
                );

                output
            }
            Ok(Err(e)) => {
                // 6. Audit log failure
                let _ = self.audit.log_event(
                    &ctx.workspace_id,
                    "agent",
                    &ctx.agent_id,
                    "tool_executed",
                    Some("tool"),
                    Some(tool_name),
                    "error",
                    Some(&e.to_string()),
                    Some(&ctx.correlation_id),
                    "system",
                    None,
                );

                ToolOutput::Error {
                    error: e.to_string(),
                    retryable: !matches!(e, ToolError::ValidationError(_) | ToolError::PermissionDenied(_)),
                }
            }
            Err(_) => {
                // Timeout
                let _ = self.audit.log_event(
                    &ctx.workspace_id,
                    "agent",
                    &ctx.agent_id,
                    "tool_executed",
                    Some("tool"),
                    Some(tool_name),
                    "timeout",
                    Some("tool execution timed out"),
                    Some(&ctx.correlation_id),
                    "system",
                    None,
                );

                ToolOutput::Error {
                    error: format!("Tool '{}' timed out after {}s", tool_name, def.timeout_seconds),
                    retryable: true,
                }
            }
        };

        // 7. Emit event
        self.event_bus.emit(Event::new(
            EventSource::Tool,
            EventType::ToolExecuted,
            serde_json::json!({
                "tool_id": tool_name,
                "agent_id": ctx.agent_id,
                "duration_ms": duration_ms,
                "success": matches!(output, ToolOutput::Success { .. }),
            }),
            &ctx.workspace_id,
            Some(ctx.correlation_id.clone()),
        ));

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::*;
    use nexmind_event_bus::EventBus;
    use nexmind_memory::MemoryStoreImpl;
    use nexmind_model_router::ModelRouter;
    use std::sync::Arc;

    fn test_audit() -> Arc<AuditLogger> {
        let db = nexmind_storage::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        Arc::new(AuditLogger::new(Arc::new(db), [0x42u8; 32]))
    }

    fn test_registry() -> ToolRegistry {
        let bus = Arc::new(EventBus::with_default_capacity());
        let audit = test_audit();
        let mut registry = ToolRegistry::new(bus, audit);

        // Register basic tools (no memory-dependent ones for basic tests)
        registry.register(Box::new(FsReadTool));
        registry.register(Box::new(FsWriteTool));
        registry.register(Box::new(FsListTool));
        registry.register(Box::new(ShellExecTool));
        registry.register(Box::new(SendMessageTool));

        registry
    }

    fn test_registry_with_memory() -> (ToolRegistry, Arc<MemoryStoreImpl>) {
        let bus = Arc::new(EventBus::with_default_capacity());
        let audit = test_audit();
        let router = Arc::new(ModelRouter::new());
        let memory = Arc::new(
            MemoryStoreImpl::open_in_memory(router, bus.clone()).unwrap(),
        );
        let mut registry = ToolRegistry::new(bus, audit);

        registry.register(Box::new(FsReadTool));
        registry.register(Box::new(FsWriteTool));
        registry.register(Box::new(FsListTool));
        registry.register(Box::new(ShellExecTool));
        registry.register(Box::new(SendMessageTool));
        registry.register(Box::new(MemoryReadTool::new(memory.clone())));
        registry.register(Box::new(MemoryWriteTool::new(memory.clone())));
        registry.register(Box::new(HttpFetchTool));

        (registry, memory)
    }

    fn test_ctx(workspace_path: &std::path::Path) -> ToolContext {
        ToolContext {
            agent_id: "agt_test".into(),
            workspace_id: "ws_test".into(),
            workspace_path: workspace_path.to_path_buf(),
            granted_permissions: vec![
                "fs:read".into(),
                "fs:write".into(),
                "memory:read".into(),
                "memory:write".into(),
                "network:outbound".into(),
            ],
            correlation_id: "corr_test".into(),
        }
    }

    #[test]
    fn test_register_and_discover_tools() {
        let registry = test_registry();
        let tools = registry.list_all_tools();
        assert!(tools.len() >= 5);

        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"fs_read"));
        assert!(names.contains(&"fs_write"));
        assert!(names.contains(&"shell_exec"));
    }

    #[test]
    fn test_filter_tools_by_permissions() {
        let registry = test_registry();

        // Agent with only fs:read should see fs_read, fs_list, send_message but not fs_write
        let available = registry.get_available_tools(&["fs:read".to_string()]);
        let names: Vec<&str> = available.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"fs_read"));
        assert!(names.contains(&"fs_list"));
        assert!(!names.contains(&"fs_write"));
    }

    #[test]
    fn test_permission_denied_for_missing_perm() {
        let registry = test_registry();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            agent_id: "agt_test".into(),
            workspace_id: "ws_test".into(),
            workspace_path: tmp.path().to_path_buf(),
            granted_permissions: vec![], // No permissions!
            correlation_id: "corr_test".into(),
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(registry.execute(
            "fs_write",
            serde_json::json!({"path": "test.txt", "content": "hello"}),
            &ctx,
        ));

        match result.unwrap() {
            ToolOutput::Error { error, .. } => {
                assert!(error.contains("Permission denied"));
            }
            other => panic!("expected error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_fs_read_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("test.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let registry = test_registry();
        let ctx = test_ctx(tmp.path());

        let result = registry
            .execute(
                "fs_read",
                serde_json::json!({"path": file_path.display().to_string()}),
                &ctx,
            )
            .await
            .unwrap();

        match result {
            ToolOutput::Success { result, .. } => {
                assert_eq!(result["content"], "hello world");
            }
            other => panic!("expected success, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_fs_read_rejects_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = test_registry();
        let ctx = test_ctx(tmp.path());

        let result = registry
            .execute(
                "fs_read",
                serde_json::json!({"path": "/nonexistent/file.txt"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(matches!(result, ToolOutput::Error { .. }));
    }

    #[tokio::test]
    async fn test_fs_write_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = test_registry();
        let ctx = test_ctx(tmp.path());

        let result = registry
            .execute(
                "fs_write",
                serde_json::json!({"path": "subdir/output.txt", "content": "created!"}),
                &ctx,
            )
            .await
            .unwrap();

        match result {
            ToolOutput::Success { result, .. } => {
                assert!(result["size_bytes"].as_u64().unwrap() > 0);
            }
            other => panic!("expected success, got {:?}", other),
        }

        let content = std::fs::read_to_string(tmp.path().join("subdir/output.txt")).unwrap();
        assert_eq!(content, "created!");
    }

    #[tokio::test]
    async fn test_fs_write_rejects_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = test_registry();
        let ctx = test_ctx(tmp.path());

        // Use a platform-appropriate absolute path
        let abs_path = if cfg!(windows) {
            "C:\\Windows\\test.txt"
        } else {
            "/etc/passwd"
        };

        let result = registry
            .execute(
                "fs_write",
                serde_json::json!({"path": abs_path, "content": "nope"}),
                &ctx,
            )
            .await
            .unwrap();

        match result {
            ToolOutput::Error { error, .. } => {
                assert!(error.contains("Absolute paths"));
            }
            other => panic!("expected error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_shell_exec_returns_needs_approval() {
        let registry = test_registry();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            agent_id: "agt_test".into(),
            workspace_id: "ws_test".into(),
            workspace_path: tmp.path().to_path_buf(),
            granted_permissions: vec!["shell:exec".into()],
            correlation_id: "corr_test".into(),
        };

        let result = registry
            .execute(
                "shell_exec",
                serde_json::json!({"command": "ls"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(
            matches!(result, ToolOutput::NeedsApproval { .. }),
            "shell_exec should return NeedsApproval"
        );
    }

    #[tokio::test]
    async fn test_memory_read_write_roundtrip() {
        let (registry, _memory) = test_registry_with_memory();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());

        // Write
        let write_result = registry
            .execute(
                "memory_write",
                serde_json::json!({
                    "content": "User prefers Russian language for communication",
                    "memory_type": "semantic",
                    "importance": 0.8
                }),
                &ctx,
            )
            .await
            .unwrap();

        match &write_result {
            ToolOutput::Success { result, .. } => {
                assert!(result["stored"].as_bool().unwrap());
            }
            other => panic!("expected success, got {:?}", other),
        }

        // Read
        let read_result = registry
            .execute(
                "memory_read",
                serde_json::json!({"query": "Russian language", "top_k": 5}),
                &ctx,
            )
            .await
            .unwrap();

        match read_result {
            ToolOutput::Success { result, .. } => {
                let memories = result["memories"].as_array().unwrap();
                assert!(!memories.is_empty());
                assert!(memories[0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("Russian"));
            }
            other => panic!("expected success, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_audit_logging_on_tool_execution() {
        let db = nexmind_storage::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let db = Arc::new(db);
        let audit = Arc::new(AuditLogger::new(db.clone(), [0x42u8; 32]));
        let bus = Arc::new(EventBus::with_default_capacity());

        let mut registry = ToolRegistry::new(bus, audit.clone());
        registry.register(Box::new(SendMessageTool));

        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = test_ctx(tmp.path());
        ctx.granted_permissions.push("connector:telegram:send".into());

        registry
            .execute(
                "send_message",
                serde_json::json!({"channel": "telegram", "text": "hello"}),
                &ctx,
            )
            .await
            .unwrap();

        // Check that audit log has an entry
        let rows = audit.get_rows(10).unwrap();
        assert!(!rows.is_empty(), "audit log should have entries after tool execution");

        let tool_exec = rows.iter().find(|r| r.action == "tool_executed");
        assert!(tool_exec.is_some(), "should have tool_executed audit entry");
    }

    #[tokio::test]
    async fn test_fs_list_directory() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "aaa").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "bbb").unwrap();
        std::fs::create_dir(tmp.path().join("subdir")).unwrap();

        let registry = test_registry();
        let ctx = test_ctx(tmp.path());

        let result = registry
            .execute(
                "fs_list",
                serde_json::json!({"path": tmp.path().display().to_string()}),
                &ctx,
            )
            .await
            .unwrap();

        match result {
            ToolOutput::Success { result, .. } => {
                let entries = result["entries"].as_array().unwrap();
                assert_eq!(entries.len(), 3);
            }
            other => panic!("expected success, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_tool_not_found() {
        let registry = test_registry();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());

        let result = registry
            .execute("nonexistent_tool", serde_json::json!({}), &ctx)
            .await;

        assert!(matches!(result, Err(ToolError::NotFound(_))));
    }
}
