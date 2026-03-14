use std::path::Path;
use std::time::Duration;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use tracing::info;

use crate::migrations::MigrationRunner;
use crate::StorageError;

/// Core database struct wrapping an r2d2 connection pool over SQLite.
pub struct Database {
    pool: Pool<SqliteConnectionManager>,
    path: String,
}

impl Database {
    /// Open or create a SQLite database at the given path.
    /// Configures WAL mode, foreign keys, and busy timeout.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        info!(path = %path_str, "opening database");

        let manager = SqliteConnectionManager::file(path.as_ref()).with_init(|conn| {
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                     PRAGMA foreign_keys = ON;
                     PRAGMA busy_timeout = 5000;",
            )?;
            Ok(())
        });

        let pool = Pool::builder()
            .max_size(4)
            .connection_timeout(Duration::from_secs(10))
            .build(manager)?;

        info!("database opened successfully");

        Ok(Database {
            pool,
            path: path_str,
        })
    }

    /// Open an in-memory database (for testing).
    pub fn open_in_memory() -> Result<Self, StorageError> {
        let manager = SqliteConnectionManager::memory().with_init(|conn| {
            conn.execute_batch(
                "PRAGMA foreign_keys = ON;
                     PRAGMA busy_timeout = 5000;",
            )?;
            Ok(())
        });

        let pool = Pool::builder().max_size(1).build(manager)?;

        Ok(Database {
            pool,
            path: ":memory:".to_string(),
        })
    }

    /// Get a connection from the pool.
    pub fn conn(&self) -> Result<r2d2::PooledConnection<SqliteConnectionManager>, StorageError> {
        Ok(self.pool.get()?)
    }

    /// Run all pending migrations.
    pub fn run_migrations(&self) -> Result<(), StorageError> {
        let conn = self.conn()?;
        let runner = MigrationRunner::new();
        runner.run(&conn)?;
        Ok(())
    }

    /// Get the database file path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Get a raw connection for backup operations.
    /// This opens a new direct connection, not from the pool.
    pub fn raw_connection(&self) -> Result<Connection, StorageError> {
        let conn = Connection::open(&self.path)?;
        Ok(conn)
    }

    /// Check if the database is healthy (can execute a simple query).
    pub fn health_check(&self) -> Result<bool, StorageError> {
        let conn = self.conn()?;
        let result: i32 = conn.query_row("SELECT 1", [], |row| row.get(0))?;
        Ok(result == 1)
    }
}
