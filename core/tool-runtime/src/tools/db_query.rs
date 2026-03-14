use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

/// Database query tool — execute read-only SQL queries.
pub struct DbQueryTool;

#[async_trait::async_trait]
impl Tool for DbQueryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "db_query".into(),
            name: "db_query".into(),
            description: "Execute a read-only SQL query against a SQLite database. Only SELECT statements are allowed.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "database": {
                        "type": "string",
                        "description": "Path to the SQLite database file (relative to workspace)"
                    },
                    "query": {
                        "type": "string",
                        "description": "SQL SELECT query to execute"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of rows to return (default: 100)",
                        "default": 100
                    }
                },
                "required": ["database", "query"]
            }),
            required_permissions: vec!["db:read".into()],
            trust_level: 1,
            idempotent: true,
            timeout_seconds: 30,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("database").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::ValidationError("'database' is required".into()));
        }
        let query = args.get("query").and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::ValidationError("'query' is required".into()))?;

        // Security: only allow SELECT statements
        let normalized = query.trim().to_uppercase();
        if !normalized.starts_with("SELECT") && !normalized.starts_with("PRAGMA") && !normalized.starts_with("EXPLAIN") {
            return Err(ToolError::ValidationError(
                "Only SELECT, PRAGMA, and EXPLAIN queries are allowed".into(),
            ));
        }

        // Block dangerous patterns
        let blocked = ["DROP", "DELETE", "INSERT", "UPDATE", "ALTER", "CREATE", "ATTACH"];
        for keyword in &blocked {
            if normalized.contains(keyword) {
                return Err(ToolError::ValidationError(format!(
                    "Query contains blocked keyword: {}",
                    keyword
                )));
            }
        }

        Ok(())
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let db_path = args["database"].as_str().unwrap();
        let query = args["query"].as_str().unwrap();
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;

        // Resolve path relative to workspace
        let full_path = ctx.workspace_path.join(db_path);
        if !full_path.exists() {
            return Ok(ToolOutput::Error {
                error: format!("Database file not found: {}", db_path),
                retryable: false,
            });
        }

        // Open database in read-only mode
        let conn = rusqlite::Connection::open_with_flags(
            &full_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| ToolError::ExecutionError(format!("Failed to open database: {}", e)))?;

        // Set a statement timeout
        conn.execute_batch("PRAGMA busy_timeout = 5000;")
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        // Execute query
        let mut stmt = conn
            .prepare(query)
            .map_err(|e| ToolError::ExecutionError(format!("SQL error: {}", e)))?;

        let column_names: Vec<String> = stmt
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let column_count = column_names.len();

        let rows: Vec<Vec<Value>> = stmt
            .query_map([], |row| {
                let mut values = Vec::with_capacity(column_count);
                for i in 0..column_count {
                    let val: Value = match row.get_ref(i) {
                        Ok(rusqlite::types::ValueRef::Null) => Value::Null,
                        Ok(rusqlite::types::ValueRef::Integer(n)) => json!(n),
                        Ok(rusqlite::types::ValueRef::Real(f)) => json!(f),
                        Ok(rusqlite::types::ValueRef::Text(s)) => {
                            let s = String::from_utf8_lossy(s);
                            json!(s.as_ref())
                        }
                        Ok(rusqlite::types::ValueRef::Blob(b)) => {
                            json!(format!("<blob {} bytes>", b.len()))
                        }
                        Err(_) => Value::Null,
                    };
                    values.push(val);
                }
                Ok(values)
            })
            .map_err(|e| ToolError::ExecutionError(format!("Query execution error: {}", e)))?
            .filter_map(|r| r.ok())
            .take(limit)
            .collect();

        let row_count = rows.len();

        // Convert to JSON objects
        let results: Vec<Value> = rows
            .into_iter()
            .map(|row| {
                let mut obj = serde_json::Map::new();
                for (i, val) in row.into_iter().enumerate() {
                    if let Some(name) = column_names.get(i) {
                        obj.insert(name.clone(), val);
                    }
                }
                Value::Object(obj)
            })
            .collect();

        Ok(ToolOutput::Success {
            result: json!({
                "columns": column_names,
                "rows": results,
                "row_count": row_count,
                "query": query,
            }),
            tokens_used: None,
        })
    }
}
