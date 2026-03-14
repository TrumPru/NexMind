use rusqlite::Connection;

/// Initialize the memory database schema for a workspace.
pub fn init_memory_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS memories (
            id              TEXT PRIMARY KEY,
            workspace_id    TEXT NOT NULL,
            agent_id        TEXT,
            memory_type     TEXT NOT NULL,
            content         TEXT NOT NULL,
            content_hash    TEXT NOT NULL,
            embedding       BLOB,
            importance      REAL DEFAULT 0.5,
            source          TEXT NOT NULL,
            source_task_id  TEXT,
            access_policy   TEXT DEFAULT 'workspace',
            metadata        TEXT,
            expires_at      TEXT,
            access_count    INTEGER DEFAULT 0,
            last_accessed_at TEXT,
            created_at      TEXT NOT NULL,
            updated_at      TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_memories_workspace ON memories(workspace_id, memory_type);
        CREATE INDEX IF NOT EXISTS idx_memories_agent ON memories(agent_id, memory_type);
        CREATE INDEX IF NOT EXISTS idx_memories_importance ON memories(importance DESC);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_hash ON memories(workspace_id, content_hash);

        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            content,
            content='memories',
            content_rowid='rowid'
        );

        CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
            INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, content) VALUES('delete', old.rowid, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, content) VALUES('delete', old.rowid, old.content);
            INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content);
        END;

        CREATE TABLE IF NOT EXISTS session_messages (
            id              TEXT PRIMARY KEY,
            workspace_id    TEXT NOT NULL,
            session_id      TEXT NOT NULL,
            agent_id        TEXT NOT NULL,
            role            TEXT NOT NULL,
            content         TEXT NOT NULL,
            tool_calls      TEXT,
            tool_call_id    TEXT,
            tokens          INTEGER,
            created_at      TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_session_msgs ON session_messages(session_id, created_at);

        CREATE TABLE IF NOT EXISTS memory_links (
            from_id     TEXT NOT NULL,
            to_id       TEXT NOT NULL,
            relation    TEXT NOT NULL,
            created_at  TEXT NOT NULL,
            PRIMARY KEY (from_id, to_id)
        );
        ",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_creation() {
        let conn = Connection::open_in_memory().unwrap();
        init_memory_schema(&conn).unwrap();

        // Verify tables exist
        for table in &["memories", "session_messages", "memory_links"] {
            let count: i32 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{}'",
                        table
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table '{}' should exist", table);
        }
    }

    #[test]
    fn test_schema_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_memory_schema(&conn).unwrap();
        init_memory_schema(&conn).unwrap(); // Should not fail
    }
}
