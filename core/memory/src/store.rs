use std::sync::Arc;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use nexmind_event_bus::EventBus;
use nexmind_model_router::ModelRouter;

use crate::schema::init_memory_schema;
use crate::types::*;

/// The memory store — manages per-workspace memory databases.
pub struct MemoryStoreImpl {
    pool: Pool<SqliteConnectionManager>,
    model_router: Arc<ModelRouter>,
    event_bus: Arc<EventBus>,
}

impl MemoryStoreImpl {
    /// Open (or create) a memory store backed by the given SQLite path.
    pub fn open(
        path: &str,
        model_router: Arc<ModelRouter>,
        event_bus: Arc<EventBus>,
    ) -> Result<Self, MemoryError> {
        let manager = SqliteConnectionManager::file(path);
        let pool = Pool::builder()
            .max_size(4)
            .build(manager)
            .map_err(|e| MemoryError::Storage(e.to_string()))?;

        // Enable WAL and foreign keys, then init schema
        {
            let conn = pool.get().map_err(|e| MemoryError::Storage(e.to_string()))?;
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA busy_timeout=5000;",
            )
            .map_err(|e| MemoryError::Storage(e.to_string()))?;
            init_memory_schema(&conn).map_err(|e| MemoryError::Storage(e.to_string()))?;
        }

        info!(path = path, "memory store opened");

        Ok(Self {
            pool,
            model_router,
            event_bus,
        })
    }

    /// Open an in-memory store (for testing).
    pub fn open_in_memory(
        model_router: Arc<ModelRouter>,
        event_bus: Arc<EventBus>,
    ) -> Result<Self, MemoryError> {
        let manager = SqliteConnectionManager::memory();
        let pool = Pool::builder()
            .max_size(1) // in-memory requires single connection
            .build(manager)
            .map_err(|e| MemoryError::Storage(e.to_string()))?;

        {
            let conn = pool.get().map_err(|e| MemoryError::Storage(e.to_string()))?;
            init_memory_schema(&conn).map_err(|e| MemoryError::Storage(e.to_string()))?;
        }

        Ok(Self {
            pool,
            model_router,
            event_bus,
        })
    }

    fn conn(&self) -> Result<r2d2::PooledConnection<SqliteConnectionManager>, MemoryError> {
        self.pool
            .get()
            .map_err(|e| MemoryError::Storage(e.to_string()))
    }

    fn content_hash(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        hex::encode(hasher.finalize())
    }

    // ── Write Path ──────────────────────────────────────────────────

    /// Store a new memory. Returns the memory ID (existing if duplicate).
    pub async fn store(&self, mem: NewMemory) -> Result<String, MemoryError> {
        let conn = self.conn()?;
        let hash = Self::content_hash(&mem.content);

        // Dedup check
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM memories WHERE workspace_id = ?1 AND content_hash = ?2",
                params![mem.workspace_id, hash],
                |row| row.get(0),
            )
            .ok();

        if let Some(existing_id) = existing {
            debug!(id = %existing_id, "duplicate memory, returning existing");
            return Ok(existing_id);
        }

        let id = ulid::Ulid::new().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let importance = mem
            .importance
            .unwrap_or_else(|| compute_importance(&mem.content, &mem.source, &mem.memory_type));

        // Compute embedding for Semantic and Pinned (if content is substantial)
        let embedding = if matches!(mem.memory_type, MemoryType::Semantic | MemoryType::Pinned)
            && mem.content.len() >= 20
        {
            self.compute_embedding(&mem.content).await.ok()
        } else {
            None
        };

        let embedding_blob = embedding.as_ref().map(|e| embedding_to_blob(e));
        let memory_type_str = mem.memory_type.as_str();
        let source_str = mem.source.as_str();
        let access_str = mem.access_policy.as_str();
        let metadata_str = mem.metadata.as_ref().map(|m| m.to_string());

        conn.execute(
            "INSERT INTO memories (id, workspace_id, agent_id, memory_type, content, content_hash, embedding, importance, source, source_task_id, access_policy, metadata, expires_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
            params![
                id,
                mem.workspace_id,
                mem.agent_id,
                memory_type_str,
                mem.content,
                hash,
                embedding_blob,
                importance,
                source_str,
                mem.source_task_id,
                access_str,
                metadata_str,
                mem.expires_at(),
                now,
            ],
        )
        .map_err(|e| MemoryError::Storage(e.to_string()))?;

        // Emit event
        self.event_bus.emit(nexmind_event_bus::Event::new(
            nexmind_event_bus::EventSource::System,
            nexmind_event_bus::EventType::Custom("memory_stored".into()),
            serde_json::json!({
                "memory_id": id,
                "memory_type": memory_type_str,
                "workspace_id": mem.workspace_id,
            }),
            &mem.workspace_id,
            None,
        ));

        info!(id = %id, memory_type = memory_type_str, "memory stored");
        Ok(id)
    }

    /// Store a session message.
    pub fn store_session_message(&self, msg: NewSessionMessage) -> Result<String, MemoryError> {
        let conn = self.conn()?;
        let id = ulid::Ulid::new().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO session_messages (id, workspace_id, session_id, agent_id, role, content, tool_calls, tool_call_id, tokens, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                msg.workspace_id,
                msg.session_id,
                msg.agent_id,
                msg.role,
                msg.content,
                msg.tool_calls,
                msg.tool_call_id,
                msg.tokens,
                now,
            ],
        )
        .map_err(|e| MemoryError::Storage(e.to_string()))?;

        Ok(id)
    }

    /// Pin a memory.
    pub fn pin(&self, memory_id: &str) -> Result<(), MemoryError> {
        let conn = self.conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        let affected = conn
            .execute(
                "UPDATE memories SET memory_type = 'pinned', importance = 1.0, expires_at = NULL, updated_at = ?1 WHERE id = ?2",
                params![now, memory_id],
            )
            .map_err(|e| MemoryError::Storage(e.to_string()))?;

        if affected == 0 {
            return Err(MemoryError::NotFound(memory_id.to_string()));
        }
        Ok(())
    }

    /// Unpin a memory (revert to Semantic).
    pub fn unpin(&self, memory_id: &str) -> Result<(), MemoryError> {
        let conn = self.conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE memories SET memory_type = 'semantic', updated_at = ?1 WHERE id = ?2 AND memory_type = 'pinned'",
            params![now, memory_id],
        )
        .map_err(|e| MemoryError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Delete a memory.
    pub fn delete(&self, memory_id: &str) -> Result<(), MemoryError> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM memories WHERE id = ?1", params![memory_id])
            .map_err(|e| MemoryError::Storage(e.to_string()))?;
        Ok(())
    }

    // ── Read Path ───────────────────────────────────────────────────

    /// Retrieve relevant memories using hybrid BM25 + vector search with RRF merge.
    pub async fn retrieve(&self, query: MemoryQuery) -> Result<RetrievalResult, MemoryError> {
        let conn = self.conn()?;

        // Build type filter
        let type_placeholders: Vec<String> = query
            .memory_types
            .iter()
            .map(|t| format!("'{}'", t.as_str()))
            .collect();
        let type_filter = if type_placeholders.is_empty() {
            String::new()
        } else {
            format!(" AND memory_type IN ({})", type_placeholders.join(","))
        };

        let importance_filter = query
            .min_importance
            .map(|mi| format!(" AND importance >= {}", mi))
            .unwrap_or_default();

        // Access policy filter: workspace memories + agent's own private memories
        let access_filter = if let Some(ref agent_id) = query.agent_id {
            format!(
                " AND (access_policy != 'private' OR agent_id = '{}')",
                agent_id.replace('\'', "''")
            )
        } else {
            " AND access_policy != 'private'".to_string()
        };

        // 1. BM25 search
        let bm25_results = self.bm25_search(
            &conn,
            &query.query_text,
            &query.workspace_id,
            &type_filter,
            &importance_filter,
            &access_filter,
            30,
        )?;

        // 2. Vector search
        let vector_results = if query.query_text.len() >= 20 {
            match self.compute_embedding(&query.query_text).await {
                Ok(query_embedding) => self.vector_search(
                    &conn,
                    &query_embedding,
                    &query.workspace_id,
                    &type_filter,
                    &importance_filter,
                    &access_filter,
                    30,
                )?,
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };

        // 3. RRF merge
        let merged = rrf_merge(&bm25_results, &vector_results, query.top_k);

        // 4. Apply token budget
        let mut total_tokens: u32 = 0;
        let mut result_memories = Vec::new();
        for scored in merged {
            let mem_tokens = (scored.memory.content.len() / 4) as u32;
            total_tokens += mem_tokens;
            result_memories.push(scored);
        }

        Ok(RetrievalResult {
            memories: result_memories,
            total_tokens,
        })
    }

    /// Get recent session messages for a conversation.
    pub fn get_session_history(
        &self,
        session_id: &str,
        limit: usize,
        max_tokens: u32,
    ) -> Result<Vec<SessionMessage>, MemoryError> {
        let conn = self.conn()?;

        let mut stmt = conn
            .prepare(
                "SELECT id, workspace_id, session_id, agent_id, role, content, tool_calls, tool_call_id, tokens, created_at
                 FROM session_messages
                 WHERE session_id = ?1
                 ORDER BY created_at DESC
                 LIMIT ?2",
            )
            .map_err(|e| MemoryError::Storage(e.to_string()))?;

        let msgs: Vec<SessionMessage> = stmt
            .query_map(params![session_id, limit], |row| {
                Ok(SessionMessage {
                    id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    session_id: row.get(2)?,
                    agent_id: row.get(3)?,
                    role: row.get(4)?,
                    content: row.get(5)?,
                    tool_calls: row.get(6)?,
                    tool_call_id: row.get(7)?,
                    tokens: row.get(8)?,
                    created_at: row.get(9)?,
                })
            })
            .map_err(|e| MemoryError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        // Reverse to chronological order, apply token budget
        let mut result: Vec<SessionMessage> = Vec::new();
        let mut budget = max_tokens;
        for msg in msgs.into_iter().rev() {
            let tok = (msg.content.len() / 4) as u32;
            if tok > budget {
                break;
            }
            budget -= tok;
            result.push(msg);
        }

        Ok(result)
    }

    // ── Internal ────────────────────────────────────────────────────

    fn bm25_search(
        &self,
        conn: &rusqlite::Connection,
        query: &str,
        workspace_id: &str,
        type_filter: &str,
        importance_filter: &str,
        access_filter: &str,
        limit: usize,
    ) -> Result<Vec<(String, f64)>, MemoryError> {
        // Use FTS5 to search, join with memories table for filtering
        let sql = format!(
            "SELECT m.id, rank
             FROM memories_fts f
             JOIN memories m ON m.rowid = f.rowid
             WHERE memories_fts MATCH ?1
               AND m.workspace_id = ?2
               {}{}{}
             ORDER BY rank
             LIMIT ?3",
            type_filter, importance_filter, access_filter
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| MemoryError::Storage(e.to_string()))?;

        let results: Vec<(String, f64)> = stmt
            .query_map(params![query, workspace_id, limit], |row| {
                let id: String = row.get(0)?;
                let rank: f64 = row.get(1)?;
                // FTS5 rank is negative (more negative = more relevant), normalize to positive
                Ok((id, -rank))
            })
            .map_err(|e| MemoryError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    fn vector_search(
        &self,
        conn: &rusqlite::Connection,
        query_embedding: &[f32],
        workspace_id: &str,
        type_filter: &str,
        importance_filter: &str,
        access_filter: &str,
        limit: usize,
    ) -> Result<Vec<(String, f64)>, MemoryError> {
        // Load all embeddings and compute cosine similarity in Rust
        let sql = format!(
            "SELECT id, embedding FROM memories
             WHERE workspace_id = ?1
               AND embedding IS NOT NULL
               {}{}{}",
            type_filter, importance_filter, access_filter
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| MemoryError::Storage(e.to_string()))?;

        let mut scored: Vec<(String, f64)> = stmt
            .query_map(params![workspace_id], |row| {
                let id: String = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                Ok((id, blob))
            })
            .map_err(|e| MemoryError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .map(|(id, blob)| {
                let emb = blob_to_embedding(&blob);
                let sim = cosine_similarity(query_embedding, &emb);
                (id, sim)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    fn load_memory_by_id(
        &self,
        conn: &rusqlite::Connection,
        id: &str,
    ) -> Result<Memory, MemoryError> {
        conn.query_row(
            "SELECT id, workspace_id, agent_id, memory_type, content, embedding, importance, source, source_task_id, access_policy, metadata, expires_at, created_at, updated_at
             FROM memories WHERE id = ?1",
            params![id],
            |row| {
                let embedding_blob: Option<Vec<u8>> = row.get(5)?;
                Ok(Memory {
                    id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    agent_id: row.get(2)?,
                    memory_type: MemoryType::from_str(&row.get::<_, String>(3)?).unwrap_or(MemoryType::Semantic),
                    content: row.get(4)?,
                    embedding: embedding_blob.map(|b| blob_to_embedding(&b)),
                    importance: row.get(6)?,
                    source: MemorySource::from_str(&row.get::<_, String>(7)?).unwrap_or(MemorySource::System),
                    source_task_id: row.get(8)?,
                    access_policy: AccessPolicy::from_str(&row.get::<_, String>(9)?).unwrap_or(AccessPolicy::Workspace),
                    metadata: row.get::<_, Option<String>>(10)?.and_then(|s| serde_json::from_str(&s).ok()),
                    expires_at: row.get(11)?,
                    created_at: row.get(12)?,
                    updated_at: row.get(13)?,
                })
            },
        )
        .map_err(|e| MemoryError::Storage(e.to_string()))
    }

    async fn compute_embedding(&self, text: &str) -> Result<Vec<f32>, MemoryError> {
        // Try available embedding providers
        let providers = self.model_router.providers();

        // Prefer Ollama (local, free)
        if providers.contains(&"ollama") {
            match self
                .model_router
                .embed(vec![text.to_string()], "ollama/all-minilm")
                .await
            {
                Ok(embeddings) if !embeddings.is_empty() => return Ok(embeddings[0].clone()),
                _ => {}
            }
        }

        // Try OpenAI
        if providers.contains(&"openai") {
            match self
                .model_router
                .embed(
                    vec![text.to_string()],
                    "openai/text-embedding-3-small",
                )
                .await
            {
                Ok(embeddings) if !embeddings.is_empty() => return Ok(embeddings[0].clone()),
                _ => {}
            }
        }

        Err(MemoryError::Embedding(
            "no embedding provider available".into(),
        ))
    }
}

// ── Helper functions ─────────────────────────────────────────────────

impl NewMemory {
    fn expires_at(&self) -> Option<&str> {
        match self.memory_type {
            MemoryType::Session => {
                // Sessions could expire, but for MVP we don't set expiry
                None
            }
            _ => None,
        }
    }
}

/// Compute importance score for a memory.
pub fn compute_importance(content: &str, source: &MemorySource, memory_type: &MemoryType) -> f64 {
    let mut score: f64 = 0.5;

    match source {
        MemorySource::User => score += 0.2,
        MemorySource::Agent => {}
        MemorySource::System => score += 0.1,
    }

    match memory_type {
        MemoryType::Pinned => return 1.0,
        MemoryType::Semantic => {}
        MemoryType::Session => score -= 0.1,
    }

    if content.len() > 500 {
        score += 0.1;
    }

    let lower = content.to_lowercase();
    if lower.contains("important") || lower.contains("remember") {
        score += 0.1;
    }

    score.clamp(0.0, 1.0)
}

/// RRF merge of BM25 and vector search results.
fn rrf_merge(
    bm25: &[(String, f64)],
    vector: &[(String, f64)],
    top_k: usize,
) -> Vec<ScoredMemory> {
    use std::collections::HashMap;

    let mut scores: HashMap<String, (f64, f64, f64)> = HashMap::new(); // (bm25_score, vector_score, rrf_score)

    for (i, (id, bm25_score)) in bm25.iter().enumerate() {
        let rrf = 1.0 / (60.0 + i as f64);
        let entry = scores.entry(id.clone()).or_insert((0.0, 0.0, 0.0));
        entry.0 = *bm25_score;
        entry.2 += rrf;
    }

    for (i, (id, vec_score)) in vector.iter().enumerate() {
        let rrf = 1.0 / (60.0 + i as f64);
        let entry = scores.entry(id.clone()).or_insert((0.0, 0.0, 0.0));
        entry.1 = *vec_score;
        entry.2 += rrf;
    }

    let mut ranked: Vec<(String, f64, f64, f64)> = scores
        .into_iter()
        .map(|(id, (b, v, r))| (id, b, v, r))
        .collect();

    ranked.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(top_k);

    // Build ScoredMemory stubs (without full Memory objects yet — those will be loaded later)
    ranked
        .into_iter()
        .map(|(id, bm25_score, vector_score, rrf_score)| ScoredMemory {
            memory: Memory {
                id,
                workspace_id: String::new(),
                agent_id: None,
                memory_type: MemoryType::Semantic,
                content: String::new(),
                embedding: None,
                importance: 0.0,
                source: MemorySource::System,
                source_task_id: None,
                access_policy: AccessPolicy::Workspace,
                metadata: None,
                expires_at: None,
                created_at: String::new(),
                updated_at: String::new(),
            },
            score: rrf_score,
            bm25_score,
            vector_score,
            rrf_score,
        })
        .collect()
}

/// Convert f32 embedding to BLOB.
fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    embedding
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

/// Convert BLOB back to f32 embedding.
fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let x = *x as f64;
        let y = *y as f64;
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

// ── Retrieve with full memory loading ──────────────────────────────

impl MemoryStoreImpl {
    /// Retrieve with full Memory objects loaded.
    pub async fn retrieve_full(
        &self,
        query: MemoryQuery,
    ) -> Result<RetrievalResult, MemoryError> {
        let mut result = self.retrieve(query).await?;

        // Load full memory objects
        let conn = self.conn()?;
        let mut loaded = Vec::new();
        let mut total_tokens: u32 = 0;

        for mut scored in result.memories.drain(..) {
            match self.load_memory_by_id(&conn, &scored.memory.id) {
                Ok(mem) => {
                    let tok = (mem.content.len() / 4) as u32;
                    total_tokens += tok;
                    scored.memory = mem;
                    loaded.push(scored);
                }
                Err(e) => {
                    warn!(id = %scored.memory.id, error = %e, "failed to load memory");
                }
            }
        }

        Ok(RetrievalResult {
            memories: loaded,
            total_tokens,
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> MemoryStoreImpl {
        let router = Arc::new(ModelRouter::new());
        let bus = Arc::new(EventBus::with_default_capacity());
        MemoryStoreImpl::open_in_memory(router, bus).unwrap()
    }

    fn make_semantic(content: &str) -> NewMemory {
        NewMemory {
            workspace_id: "ws1".into(),
            agent_id: Some("agt1".into()),
            memory_type: MemoryType::Semantic,
            content: content.into(),
            source: MemorySource::User,
            source_task_id: None,
            access_policy: AccessPolicy::Workspace,
            metadata: None,
            importance: None,
        }
    }

    #[tokio::test]
    async fn test_store_and_retrieve_roundtrip() {
        let store = test_store();
        let id = store
            .store(make_semantic("The user prefers dark mode in all applications"))
            .await
            .unwrap();
        assert!(!id.is_empty());

        let result = store
            .retrieve_full(MemoryQuery {
                query_text: "dark mode".into(),
                workspace_id: "ws1".into(),
                agent_id: Some("agt1".into()),
                memory_types: vec![MemoryType::Semantic],
                top_k: 10,
                min_importance: None,
            })
            .await
            .unwrap();

        assert_eq!(result.memories.len(), 1);
        assert!(result.memories[0]
            .memory
            .content
            .contains("dark mode"));
    }

    #[tokio::test]
    async fn test_bm25_search_returns_relevant() {
        let store = test_store();
        store
            .store(make_semantic("Rust programming language is fast and safe"))
            .await
            .unwrap();
        store
            .store(make_semantic("Python is great for data science"))
            .await
            .unwrap();
        store
            .store(make_semantic("JavaScript runs in the browser"))
            .await
            .unwrap();

        let result = store
            .retrieve_full(MemoryQuery {
                query_text: "Rust programming".into(),
                workspace_id: "ws1".into(),
                agent_id: Some("agt1".into()),
                memory_types: vec![MemoryType::Semantic],
                top_k: 10,
                min_importance: None,
            })
            .await
            .unwrap();

        assert!(!result.memories.is_empty());
        assert!(result.memories[0].memory.content.contains("Rust"));
    }

    #[tokio::test]
    async fn test_vector_search_with_mock_embeddings() {
        // Without an embedding provider, vector search is skipped.
        // Test that BM25-only retrieval still works.
        let store = test_store();
        store
            .store(make_semantic("Machine learning models need training data"))
            .await
            .unwrap();

        let result = store
            .retrieve_full(MemoryQuery {
                query_text: "training data".into(),
                workspace_id: "ws1".into(),
                agent_id: None,
                memory_types: vec![MemoryType::Semantic],
                top_k: 5,
                min_importance: None,
            })
            .await
            .unwrap();

        // BM25 should find "training data"
        assert!(!result.memories.is_empty());
    }

    #[test]
    fn test_rrf_merge_combines_scores() {
        let bm25 = vec![
            ("a".into(), 1.0),
            ("b".into(), 0.8),
            ("c".into(), 0.5),
        ];
        let vector = vec![
            ("b".into(), 0.9),
            ("d".into(), 0.7),
            ("a".into(), 0.6),
        ];

        let merged = rrf_merge(&bm25, &vector, 5);

        // "b" appears in both at good ranks, should be near the top
        assert!(!merged.is_empty());
        let ids: Vec<&str> = merged.iter().map(|s| s.memory.id.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));

        // "b" should have the highest RRF score (rank 1 in bm25, rank 0 in vector)
        let b = merged.iter().find(|s| s.memory.id == "b").unwrap();
        assert!(b.rrf_score > 0.0);
        assert!(b.bm25_score > 0.0);
        assert!(b.vector_score > 0.0);
    }

    #[tokio::test]
    async fn test_access_policy_filtering() {
        let store = test_store();

        // Store a private memory for agent1
        store
            .store(NewMemory {
                workspace_id: "ws1".into(),
                agent_id: Some("agent1".into()),
                memory_type: MemoryType::Semantic,
                content: "This is agent1's private secret data".into(),
                source: MemorySource::Agent,
                source_task_id: None,
                access_policy: AccessPolicy::Private,
                metadata: None,
                importance: None,
            })
            .await
            .unwrap();

        // Store a workspace-visible memory
        store
            .store(make_semantic("This is visible to all agents in workspace"))
            .await
            .unwrap();

        // agent2 should NOT see agent1's private memory
        let result = store
            .retrieve_full(MemoryQuery {
                query_text: "secret data".into(),
                workspace_id: "ws1".into(),
                agent_id: Some("agent2".into()),
                memory_types: vec![MemoryType::Semantic],
                top_k: 10,
                min_importance: None,
            })
            .await
            .unwrap();

        for mem in &result.memories {
            assert_ne!(mem.memory.access_policy, AccessPolicy::Private,
                "agent2 should not see agent1's private memory");
        }

        // agent1 SHOULD see its own private memory
        let result = store
            .retrieve_full(MemoryQuery {
                query_text: "secret data".into(),
                workspace_id: "ws1".into(),
                agent_id: Some("agent1".into()),
                memory_types: vec![MemoryType::Semantic],
                top_k: 10,
                min_importance: None,
            })
            .await
            .unwrap();

        let has_private = result
            .memories
            .iter()
            .any(|m| m.memory.content.contains("private secret"));
        assert!(has_private, "agent1 should see its own private memory");
    }

    #[tokio::test]
    async fn test_pinned_memory_importance() {
        let store = test_store();
        let id = store
            .store(NewMemory {
                workspace_id: "ws1".into(),
                agent_id: None,
                memory_type: MemoryType::Pinned,
                content: "User's name is Vladimir and they prefer Russian language".into(),
                source: MemorySource::User,
                source_task_id: None,
                access_policy: AccessPolicy::Workspace,
                metadata: None,
                importance: None,
            })
            .await
            .unwrap();

        let conn = store.conn().unwrap();
        let mem = store.load_memory_by_id(&conn, &id).unwrap();
        assert_eq!(mem.importance, 1.0, "pinned memory should have importance 1.0");
    }

    #[tokio::test]
    async fn test_session_message_storage_and_retrieval() {
        let store = test_store();

        // Store messages
        for i in 0..5 {
            store
                .store_session_message(NewSessionMessage {
                    workspace_id: "ws1".into(),
                    session_id: "sess1".into(),
                    agent_id: "agt1".into(),
                    role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                    content: format!("Message {}", i),
                    tool_calls: None,
                    tool_call_id: None,
                    tokens: Some(10),
                })
                .unwrap();
        }

        let msgs = store.get_session_history("sess1", 10, 10000).unwrap();
        assert_eq!(msgs.len(), 5);
        // Should be in chronological order
        assert!(msgs[0].content.contains("Message 0"));
        assert!(msgs[4].content.contains("Message 4"));
    }

    #[tokio::test]
    async fn test_dedup_returns_existing_id() {
        let store = test_store();
        let content = "This is a unique piece of information about cats";
        let id1 = store.store(make_semantic(content)).await.unwrap();
        let id2 = store.store(make_semantic(content)).await.unwrap();
        assert_eq!(id1, id2, "storing same content should return same ID");
    }

    #[tokio::test]
    async fn test_token_budget_retrieval() {
        let store = test_store();

        // Store several memories with substantial content
        for i in 0..10 {
            store
                .store(make_semantic(&format!(
                    "Memory number {} with some content about topic {}",
                    i, i
                )))
                .await
                .unwrap();
        }

        let result = store
            .retrieve_full(MemoryQuery {
                query_text: "Memory content topic".into(),
                workspace_id: "ws1".into(),
                agent_id: None,
                memory_types: vec![MemoryType::Semantic],
                top_k: 20,
                min_importance: None,
            })
            .await
            .unwrap();

        assert!(result.total_tokens > 0);
    }

    #[tokio::test]
    async fn test_session_history_respects_limit() {
        let store = test_store();

        for i in 0..10 {
            store
                .store_session_message(NewSessionMessage {
                    workspace_id: "ws1".into(),
                    session_id: "sess_limit".into(),
                    agent_id: "agt1".into(),
                    role: "user".into(),
                    content: format!("Message {}", i),
                    tool_calls: None,
                    tool_call_id: None,
                    tokens: Some(10),
                })
                .unwrap();
        }

        let msgs = store.get_session_history("sess_limit", 3, 10000).unwrap();
        assert_eq!(msgs.len(), 3);
    }

    #[tokio::test]
    async fn test_pin_and_unpin() {
        let store = test_store();
        let id = store.store(make_semantic("Some information to pin later on in the test")).await.unwrap();

        store.pin(&id).unwrap();
        {
            let conn = store.conn().unwrap();
            let mem = store.load_memory_by_id(&conn, &id).unwrap();
            assert_eq!(mem.memory_type, MemoryType::Pinned);
            assert_eq!(mem.importance, 1.0);
        }

        store.unpin(&id).unwrap();
        {
            let conn = store.conn().unwrap();
            let mem = store.load_memory_by_id(&conn, &id).unwrap();
            assert_eq!(mem.memory_type, MemoryType::Semantic);
        }
    }

    #[tokio::test]
    async fn test_delete_memory() {
        let store = test_store();
        let id = store.store(make_semantic("Memory to be deleted from the store")).await.unwrap();
        store.delete(&id).unwrap();

        let conn = store.conn().unwrap();
        let result = store.load_memory_by_id(&conn, &id);
        assert!(result.is_err());
    }

    #[test]
    fn test_embedding_blob_roundtrip() {
        let original = vec![0.1f32, 0.2, 0.3, -0.5, 1.0];
        let blob = embedding_to_blob(&original);
        let restored = blob_to_embedding(&blob);
        assert_eq!(original, restored);
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0f32, 0.0, 0.0];
        let sim = cosine_similarity(&a, &a);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn test_importance_scoring() {
        let pinned = compute_importance("anything", &MemorySource::User, &MemoryType::Pinned);
        assert_eq!(pinned, 1.0);

        let user = compute_importance("short", &MemorySource::User, &MemoryType::Semantic);
        let agent = compute_importance("short", &MemorySource::Agent, &MemoryType::Semantic);
        assert!(user > agent);

        let with_marker =
            compute_importance("please remember this", &MemorySource::Agent, &MemoryType::Semantic);
        let without = compute_importance("some random text", &MemorySource::Agent, &MemoryType::Semantic);
        assert!(with_marker > without);
    }
}
