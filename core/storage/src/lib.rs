pub mod backup;
pub mod db;
pub mod migrations;

pub use backup::BackupManager;
pub use db::Database;
pub use migrations::MigrationRunner;

/// Storage-level errors.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("connection pool error: {0}")]
    Pool(String),
    #[error("migration error: {0}")]
    Migration(String),
    #[error("backup error: {0}")]
    Backup(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<r2d2::Error> for StorageError {
    fn from(e: r2d2::Error) -> Self {
        StorageError::Pool(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_in_memory_and_migrate() {
        let db = Database::open_in_memory().expect("failed to open in-memory db");
        db.run_migrations().expect("migrations failed");
    }

    #[test]
    fn test_health_check() {
        let db = Database::open_in_memory().expect("failed to open db");
        db.run_migrations().expect("migrations failed");
        assert!(db.health_check().expect("health check failed"));
    }

    #[test]
    fn test_migrations_tracked() {
        let db = Database::open_in_memory().expect("failed to open db");
        db.run_migrations().expect("migrations failed");

        let conn = db.conn().expect("failed to get connection");
        let runner = MigrationRunner::new();
        let applied = runner.applied(&conn).expect("failed to list migrations");

        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].0, 1);
        assert_eq!(applied[0].1, "001_core_schema");
    }

    #[test]
    fn test_migrations_idempotent() {
        let db = Database::open_in_memory().expect("failed to open db");
        db.run_migrations().expect("first migration run failed");
        db.run_migrations()
            .expect("second migration run should be idempotent");
    }

    #[test]
    fn test_all_tables_exist() {
        let db = Database::open_in_memory().expect("failed to open db");
        db.run_migrations().expect("migrations failed");
        let conn = db.conn().expect("failed to get connection");

        let expected_tables = vec![
            "agents",
            "agent_runs",
            "teams",
            "tasks",
            "task_plans",
            "task_messages",
            "workflows",
            "workflow_runs",
            "workflow_node_states",
            "trigger_bindings",
            "approval_requests",
            "audit_log",
            "cost_records",
        ];

        for table in &expected_tables {
            let count: i32 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{}'",
                        table
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap_or_else(|e| panic!("failed to check table {}: {}", table, e));

            assert_eq!(count, 1, "table '{}' does not exist", table);
        }
    }

    #[test]
    fn test_wal_mode_enabled() {
        let db = Database::open_in_memory().expect("failed to open db");
        // In-memory DBs don't use WAL, but let's test on a temp file
        let tmp = tempfile::NamedTempFile::new().expect("failed to create temp file");
        let db = Database::open(tmp.path()).expect("failed to open file db");
        let conn = db.conn().expect("failed to get connection");

        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("failed to query journal mode");

        assert_eq!(journal_mode, "wal");
    }

    #[test]
    fn test_foreign_keys_enabled() {
        let db = Database::open_in_memory().expect("failed to open db");
        let conn = db.conn().expect("failed to get connection");

        let fk: i32 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("failed to query foreign_keys");

        assert_eq!(fk, 1);
    }

    #[test]
    fn test_backup_creates_valid_db() {
        let tmp_src = tempfile::NamedTempFile::new().expect("failed to create src temp");
        let db = Database::open(tmp_src.path()).expect("failed to open src db");
        db.run_migrations().expect("migrations failed");

        // Insert a test row
        let conn = db.conn().expect("failed to get connection");
        conn.execute(
            "INSERT INTO agents (id, workspace_id, definition, version, status) VALUES ('test_agent', 'ws1', '{}', 1, 'idle')",
            [],
        )
        .expect("failed to insert test agent");

        // Backup
        let tmp_dest = tempfile::NamedTempFile::new().expect("failed to create dest temp");
        let src_conn = db.raw_connection().expect("failed to get raw connection");
        BackupManager::backup(&src_conn, tmp_dest.path()).expect("backup failed");

        // Verify backup
        assert!(BackupManager::verify(tmp_dest.path()).expect("verify failed"));

        // Check data in backup
        let dest_conn = rusqlite::Connection::open(tmp_dest.path()).expect("failed to open backup");
        let count: i32 = dest_conn
            .query_row("SELECT COUNT(*) FROM agents", [], |row| row.get(0))
            .expect("failed to count agents");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_insert_and_query_agents() {
        let db = Database::open_in_memory().expect("failed to open db");
        db.run_migrations().expect("migrations failed");
        let conn = db.conn().expect("failed to get connection");

        conn.execute(
            "INSERT INTO agents (id, workspace_id, definition, version, status) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["agt_001", "ws1", r#"{"name":"test"}"#, 1, "idle"],
        )
        .expect("insert failed");

        let name: String = conn
            .query_row(
                "SELECT json_extract(definition, '$.name') FROM agents WHERE id = 'agt_001'",
                [],
                |row| row.get(0),
            )
            .expect("query failed");

        assert_eq!(name, "test");
    }
}
