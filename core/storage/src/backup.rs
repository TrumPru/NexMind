use std::path::Path;

use rusqlite::backup::Backup;
use rusqlite::Connection;
use tracing::info;

use crate::StorageError;

/// Wrapper around SQLite's online backup API for hot backups.
pub struct BackupManager;

impl BackupManager {
    /// Perform a hot backup of the source database to the destination path.
    /// Uses the sqlite3_backup API which safely copies a live WAL-mode database.
    pub fn backup(source: &Connection, dest_path: impl AsRef<Path>) -> Result<(), StorageError> {
        let dest_path = dest_path.as_ref();
        info!(dest = %dest_path.display(), "starting database backup");

        let mut dest = Connection::open(dest_path)?;

        let backup =
            Backup::new(source, &mut dest).map_err(|e| StorageError::Backup(e.to_string()))?;

        // Copy in steps of 100 pages, sleeping 10ms between steps
        // to avoid blocking writers too long.
        backup
            .run_to_completion(100, std::time::Duration::from_millis(10), None)
            .map_err(|e| StorageError::Backup(e.to_string()))?;

        info!(dest = %dest_path.display(), "backup completed");
        Ok(())
    }

    /// Verify a backup database is valid by opening it and running integrity check.
    pub fn verify(path: impl AsRef<Path>) -> Result<bool, StorageError> {
        let conn = Connection::open(path.as_ref())?;
        let result: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(result == "ok")
    }
}
