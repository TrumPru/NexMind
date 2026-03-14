use tracing::info;

use crate::definition::AgentDefinition;
use crate::AgentError;

/// Agent registry — CRUD for agent definitions in SQLite.
pub struct AgentRegistry {
    db: std::sync::Arc<nexmind_storage::Database>,
}

impl AgentRegistry {
    pub fn new(db: std::sync::Arc<nexmind_storage::Database>) -> Self {
        Self { db }
    }

    /// Create a new agent definition. Returns the agent ID.
    pub fn create(&self, def: &AgentDefinition) -> Result<String, AgentError> {
        let conn = self.db.conn().map_err(|e| AgentError::StorageError(e.to_string()))?;
        let definition_json =
            serde_json::to_string(def).map_err(|e| AgentError::StorageError(e.to_string()))?;

        conn.execute(
            "INSERT OR REPLACE INTO agents (id, workspace_id, definition, version, status) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![def.id, def.workspace_id, definition_json, def.version, "idle"],
        )
        .map_err(|e| AgentError::StorageError(e.to_string()))?;

        info!(agent_id = %def.id, name = %def.name, "agent created");
        Ok(def.id.clone())
    }

    /// Get an agent definition by ID.
    pub fn get(&self, id: &str) -> Result<AgentDefinition, AgentError> {
        let conn = self.db.conn().map_err(|e| AgentError::StorageError(e.to_string()))?;

        let definition_json: String = conn
            .query_row(
                "SELECT definition FROM agents WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AgentError::NotFound(id.to_string()),
                other => AgentError::StorageError(other.to_string()),
            })?;

        serde_json::from_str(&definition_json)
            .map_err(|e| AgentError::StorageError(format!("failed to parse agent definition: {}", e)))
    }

    /// List all agents in a workspace.
    pub fn list(&self, workspace_id: &str) -> Result<Vec<AgentDefinition>, AgentError> {
        let conn = self.db.conn().map_err(|e| AgentError::StorageError(e.to_string()))?;

        let mut stmt = conn
            .prepare("SELECT definition FROM agents WHERE workspace_id = ?1 ORDER BY id")
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        let agents = stmt
            .query_map(rusqlite::params![workspace_id], |row| {
                let json: String = row.get(0)?;
                Ok(json)
            })
            .map_err(|e| AgentError::StorageError(e.to_string()))?
            .filter_map(|r| {
                r.ok()
                    .and_then(|json| serde_json::from_str::<AgentDefinition>(&json).ok())
            })
            .collect();

        Ok(agents)
    }

    /// List all agents across all workspaces.
    pub fn list_all(&self) -> Result<Vec<AgentDefinition>, AgentError> {
        let conn = self.db.conn().map_err(|e| AgentError::StorageError(e.to_string()))?;

        let mut stmt = conn
            .prepare("SELECT definition FROM agents ORDER BY id")
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        let agents = stmt
            .query_map([], |row| {
                let json: String = row.get(0)?;
                Ok(json)
            })
            .map_err(|e| AgentError::StorageError(e.to_string()))?
            .filter_map(|r| {
                r.ok()
                    .and_then(|json| serde_json::from_str::<AgentDefinition>(&json).ok())
            })
            .collect();

        Ok(agents)
    }

    /// Update an agent definition, auto-incrementing the version.
    pub fn update(&self, mut def: AgentDefinition) -> Result<(), AgentError> {
        // Get current version
        let current = self.get(&def.id)?;
        def.version = current.version + 1;

        let conn = self.db.conn().map_err(|e| AgentError::StorageError(e.to_string()))?;
        let definition_json =
            serde_json::to_string(&def).map_err(|e| AgentError::StorageError(e.to_string()))?;

        let rows = conn
            .execute(
                "UPDATE agents SET definition = ?1, version = ?2 WHERE id = ?3",
                rusqlite::params![definition_json, def.version, def.id],
            )
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        if rows == 0 {
            return Err(AgentError::NotFound(def.id));
        }

        info!(agent_id = %def.id, version = def.version, "agent updated");
        Ok(())
    }

    /// Delete an agent definition.
    pub fn delete(&self, id: &str) -> Result<(), AgentError> {
        let conn = self.db.conn().map_err(|e| AgentError::StorageError(e.to_string()))?;

        let rows = conn
            .execute("DELETE FROM agents WHERE id = ?1", rusqlite::params![id])
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        if rows == 0 {
            return Err(AgentError::NotFound(id.to_string()));
        }

        info!(agent_id = %id, "agent deleted");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::AgentDefinition;

    fn setup() -> (std::sync::Arc<nexmind_storage::Database>, AgentRegistry) {
        let db = nexmind_storage::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let db = std::sync::Arc::new(db);
        let registry = AgentRegistry::new(db.clone());
        (db, registry)
    }

    #[test]
    fn test_create_and_get() {
        let (_db, registry) = setup();
        let def = AgentDefinition::default_chat("ws1");

        let id = registry.create(&def).unwrap();
        assert_eq!(id, "agt_default_chat");

        let retrieved = registry.get("agt_default_chat").unwrap();
        assert_eq!(retrieved.name, "NexMind Assistant");
        assert_eq!(retrieved.version, 1);
    }

    #[test]
    fn test_list() {
        let (_db, registry) = setup();
        let def = AgentDefinition::default_chat("ws1");
        registry.create(&def).unwrap();

        let agents = registry.list("ws1").unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "agt_default_chat");

        let empty = registry.list("ws_other").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_update_increments_version() {
        let (_db, registry) = setup();
        let def = AgentDefinition::default_chat("ws1");
        registry.create(&def).unwrap();

        let mut updated = def.clone();
        updated.name = "Updated Assistant".into();
        registry.update(updated).unwrap();

        let retrieved = registry.get("agt_default_chat").unwrap();
        assert_eq!(retrieved.name, "Updated Assistant");
        assert_eq!(retrieved.version, 2);
    }

    #[test]
    fn test_delete() {
        let (_db, registry) = setup();
        let def = AgentDefinition::default_chat("ws1");
        registry.create(&def).unwrap();

        registry.delete("agt_default_chat").unwrap();

        let result = registry.get("agt_default_chat");
        assert!(result.is_err());
    }

    #[test]
    fn test_get_not_found() {
        let (_db, registry) = setup();
        let result = registry.get("nonexistent");
        assert!(matches!(result, Err(AgentError::NotFound(_))));
    }

    #[test]
    fn test_delete_not_found() {
        let (_db, registry) = setup();
        let result = registry.delete("nonexistent");
        assert!(matches!(result, Err(AgentError::NotFound(_))));
    }
}
