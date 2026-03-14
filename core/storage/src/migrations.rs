use rusqlite::Connection;
use tracing::info;

use crate::StorageError;

/// A single SQL migration.
struct Migration {
    version: u32,
    name: &'static str,
    sql: &'static str,
}

/// Runs SQL migrations in order, tracking applied migrations in `_migrations` table.
pub struct MigrationRunner {
    migrations: Vec<Migration>,
}

impl MigrationRunner {
    pub fn new() -> Self {
        MigrationRunner {
            migrations: vec![Migration {
                version: 1,
                name: "001_core_schema",
                sql: include_str!("migrations/001_core_schema.sql"),
            }],
        }
    }

    /// Run all pending migrations.
    pub fn run(&self, conn: &Connection) -> Result<(), StorageError> {
        // Create migrations tracking table
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _migrations (
                version  INTEGER PRIMARY KEY,
                name     TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .map_err(|e| StorageError::Migration(e.to_string()))?;

        // Get already-applied versions
        let mut stmt = conn
            .prepare("SELECT version FROM _migrations ORDER BY version")
            .map_err(|e| StorageError::Migration(e.to_string()))?;
        let applied: Vec<u32> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| StorageError::Migration(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        for migration in &self.migrations {
            if applied.contains(&migration.version) {
                continue;
            }

            info!(
                version = migration.version,
                name = migration.name,
                "applying migration"
            );

            conn.execute_batch(migration.sql).map_err(|e| {
                StorageError::Migration(format!(
                    "migration {} ({}) failed: {}",
                    migration.version, migration.name, e
                ))
            })?;

            conn.execute(
                "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
                rusqlite::params![migration.version, migration.name],
            )
            .map_err(|e| StorageError::Migration(e.to_string()))?;

            info!(
                version = migration.version,
                name = migration.name,
                "migration applied"
            );
        }

        Ok(())
    }

    /// List applied migrations.
    pub fn applied(&self, conn: &Connection) -> Result<Vec<(u32, String)>, StorageError> {
        let mut stmt = conn
            .prepare("SELECT version, name FROM _migrations ORDER BY version")
            .map_err(|e| StorageError::Migration(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| StorageError::Migration(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }
}

impl Default for MigrationRunner {
    fn default() -> Self {
        Self::new()
    }
}
