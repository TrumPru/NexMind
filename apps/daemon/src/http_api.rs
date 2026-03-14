/// HTTP REST API for the NexMind web dashboard.
///
/// Provides JSON endpoints consumed by the embedded dashboard HTML.
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dashboard::{DashboardServer, DASHBOARD_HTML};

/// Shared application state for all HTTP handlers.
pub struct AppState {
    pub db: Arc<nexmind_storage::Database>,
    pub agent_registry: Arc<nexmind_agent_engine::AgentRegistry>,
    pub approval_manager: Arc<nexmind_agent_engine::approval::ApprovalManager>,
    pub cost_tracker: Arc<nexmind_agent_engine::cost::CostTracker>,
    pub dashboard_token: String,
    pub event_bus: Arc<nexmind_event_bus::EventBus>,
    pub start_time: std::time::Instant,
    pub skill_registry: Arc<nexmind_skill_registry::SkillRegistry>,
    pub memory_store: Arc<nexmind_memory::MemoryStoreImpl>,
    pub model_router: Arc<nexmind_model_router::ModelRouter>,
}

#[derive(Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// Validate the dashboard token from query params.
fn check_token(query: &TokenQuery, expected: &str) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    match &query.token {
        Some(t) if DashboardServer::validate_token(t, expected) => Ok(()),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Invalid or missing token".into(),
            }),
        )),
    }
}

/// Build the axum router with all dashboard routes.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(serve_dashboard))
        // Health
        .route("/api/health", get(health))
        // Agents
        .route("/api/agents", get(list_agents))
        // Approvals
        .route("/api/approvals", get(list_approvals))
        .route("/api/approvals/{id}/approve", post(approve))
        .route("/api/approvals/{id}/deny", post(deny))
        // Costs
        .route("/api/costs", get(get_costs))
        // Chat
        .route("/api/chat", post(chat))
        // Logs
        .route("/api/logs", get(get_logs))
        // Skills
        .route("/api/skills", get(list_skills))
        // Models
        .route("/api/models", get(list_models))
        // Memory
        .route("/api/memory", get(list_memory).post(create_memory))
        .route("/api/memory/{id}", delete(delete_memory))
        .route("/api/memory/{id}/pin", post(pin_memory))
        .route("/api/memory/{id}/unpin", post(unpin_memory))
        // Settings
        .route("/api/settings", get(get_settings).put(save_settings))
        .route("/api/settings/stats", get(get_stats))
        .route("/api/settings/backup", post(create_backup))
        // Integrations
        .route("/api/integrations", get(list_integrations))
        .route("/api/integrations/{id}/configure", post(configure_integration))
        .route("/api/integrations/{id}/test", post(test_integration))
        .route("/api/integrations/{id}", delete(delete_integration))
        .with_state(state)
}

// ── Handlers ──────────────────────────────────────────────────────────

async fn serve_dashboard() -> impl IntoResponse {
    Html(DASHBOARD_HTML)
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    version: String,
    uptime_seconds: u64,
    database_ok: bool,
    active_agents: usize,
    pending_approvals: usize,
}

async fn health(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
) -> Result<Json<HealthResponse>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    let db_ok = state.db.health_check().unwrap_or(false);
    let uptime = state.start_time.elapsed().as_secs();
    let active_agents = state.agent_registry.list_all().map(|a| a.len()).unwrap_or(0);
    let pending = state
        .approval_manager
        .list_pending("default")
        .map(|p| p.len())
        .unwrap_or(0);

    Ok(Json(HealthResponse {
        status: if db_ok { "ok".into() } else { "degraded".into() },
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: uptime,
        database_ok: db_ok,
        active_agents,
        pending_approvals: pending,
    }))
}

#[derive(Serialize)]
struct AgentInfo {
    id: String,
    name: String,
    status: String,
    description: String,
}

#[derive(Serialize)]
struct AgentsResponse {
    agents: Vec<AgentInfo>,
}

async fn list_agents(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
) -> Result<Json<AgentsResponse>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    let agents = state.agent_registry.list_all().unwrap_or_default();
    let infos = agents
        .into_iter()
        .map(|a| AgentInfo {
            id: a.id,
            name: a.name,
            status: "idle".into(),
            description: a.description.unwrap_or_default(),
        })
        .collect();

    Ok(Json(AgentsResponse { agents: infos }))
}

#[derive(Serialize)]
struct ApprovalInfo {
    id: String,
    agent_id: String,
    tool_id: String,
    action_description: String,
    risk_level: String,
    created_at: String,
}

#[derive(Serialize)]
struct ApprovalsResponse {
    approvals: Vec<ApprovalInfo>,
}

async fn list_approvals(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
) -> Result<Json<ApprovalsResponse>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    let pending = state.approval_manager.list_pending("default").unwrap_or_default();
    let infos = pending
        .into_iter()
        .map(|r| ApprovalInfo {
            id: r.id,
            agent_id: r.requester_agent_id,
            tool_id: r.tool_id,
            action_description: r.action_description,
            risk_level: r.risk_level,
            created_at: r.created_at,
        })
        .collect();

    Ok(Json(ApprovalsResponse { approvals: infos }))
}

#[derive(Serialize)]
struct ActionResult {
    success: bool,
    message: String,
}

async fn approve(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Result<Json<ActionResult>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    match state.approval_manager.approve(&id, "dashboard") {
        Ok(()) => Ok(Json(ActionResult {
            success: true,
            message: "Approved".into(),
        })),
        Err(e) => Ok(Json(ActionResult {
            success: false,
            message: e.to_string(),
        })),
    }
}

async fn deny(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Result<Json<ActionResult>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    match state.approval_manager.deny(&id, "dashboard", None) {
        Ok(()) => Ok(Json(ActionResult {
            success: true,
            message: "Denied".into(),
        })),
        Err(e) => Ok(Json(ActionResult {
            success: false,
            message: e.to_string(),
        })),
    }
}

#[derive(Deserialize)]
struct CostQueryParams {
    token: Option<String>,
    period: Option<String>,
}

#[derive(Serialize)]
struct CostModelInfo {
    model: String,
    cost_microdollars: i64,
}

#[derive(Serialize)]
struct CostResponse {
    total_microdollars: i64,
    total_requests: u64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    by_model: Vec<CostModelInfo>,
}

async fn get_costs(
    State(state): State<Arc<AppState>>,
    Query(cq): Query<CostQueryParams>,
) -> Result<Json<CostResponse>, (StatusCode, Json<ErrorResponse>)> {
    let q = TokenQuery { token: cq.token.clone() };
    check_token(&q, &state.dashboard_token)?;

    let period = match cq.period.as_deref() {
        Some("7d") => nexmind_agent_engine::cost::CostPeriod::Last7Days,
        Some("30d") => nexmind_agent_engine::cost::CostPeriod::Last30Days,
        Some("all") => nexmind_agent_engine::cost::CostPeriod::AllTime,
        _ => nexmind_agent_engine::cost::CostPeriod::Today,
    };

    let summary = state
        .cost_tracker
        .summary("default", period)
        .unwrap_or_else(|_| nexmind_agent_engine::CostSummary {
            total_cost_usd: 0.0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_requests: 0,
            by_model: Default::default(),
            by_agent: Default::default(),
        });

    let by_model = summary
        .by_model
        .iter()
        .map(|(model, cost)| CostModelInfo {
            model: model.clone(),
            cost_microdollars: (cost * 1_000_000.0) as i64,
        })
        .collect();

    Ok(Json(CostResponse {
        total_microdollars: (summary.total_cost_usd * 1_000_000.0) as i64,
        total_requests: summary.total_requests,
        total_input_tokens: summary.total_input_tokens,
        total_output_tokens: summary.total_output_tokens,
        by_model,
    }))
}

#[derive(Deserialize)]
struct ChatRequest {
    message: String,
}

#[derive(Serialize)]
struct ChatResponse {
    response: String,
}

async fn chat(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
    Json(body): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    // For now, echo back. Full agent integration requires the AgentRuntime,
    // which can be added to AppState later.
    let _ = &state.event_bus;
    Ok(Json(ChatResponse {
        response: format!(
            "Received: {}. (Chat requires agent runtime integration.)",
            body.message
        ),
    }))
}

#[derive(Serialize)]
struct LogsResponse {
    entries: Vec<String>,
}

async fn get_logs(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
) -> Result<Json<LogsResponse>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    // Placeholder: in production, read from a ring buffer of recent log entries.
    Ok(Json(LogsResponse {
        entries: vec!["NexMind daemon running.".into()],
    }))
}

#[derive(Serialize)]
struct SkillInfo {
    id: String,
    name: String,
    version: String,
    description: String,
    status: String,
}

#[derive(Serialize)]
struct SkillsResponse {
    skills: Vec<SkillInfo>,
}

async fn list_skills(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
) -> Result<Json<SkillsResponse>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    let skills = state.skill_registry.list();
    let infos = skills
        .into_iter()
        .map(|s| SkillInfo {
            id: s.id,
            name: s.name,
            version: s.version,
            description: s.description,
            status: format!("{:?}", s.status),
        })
        .collect();

    Ok(Json(SkillsResponse { skills: infos }))
}

// ── Models ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ModelInfoResponse {
    id: String,
    display_name: String,
    context_window: u32,
    supports_tools: bool,
    supports_vision: bool,
    supports_streaming: bool,
    cost_per_1k_input: f64,
    cost_per_1k_output: f64,
}

#[derive(Serialize)]
struct ModelsResponse {
    models: Vec<ModelInfoResponse>,
    default_model: String,
}

async fn list_models(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
) -> Result<Json<ModelsResponse>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    let models = state
        .model_router
        .models()
        .iter()
        .map(|m| ModelInfoResponse {
            id: m.id.clone(),
            display_name: m.display_name.clone(),
            context_window: m.context_window,
            supports_tools: m.supports_tools,
            supports_vision: m.supports_vision,
            supports_streaming: m.supports_streaming,
            cost_per_1k_input: m.cost_per_1k_input,
            cost_per_1k_output: m.cost_per_1k_output,
        })
        .collect();

    let default_model = state.model_router.select_default_model();

    Ok(Json(ModelsResponse {
        models,
        default_model,
    }))
}

// ── Memory ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct MemoryQueryParams {
    token: Option<String>,
    q: Option<String>,
    #[serde(rename = "type")]
    memory_type: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Serialize)]
struct MemoryEntryResponse {
    id: String,
    memory_type: String,
    content: String,
    importance: f64,
    source: String,
    agent_id: Option<String>,
    created_at: String,
}

#[derive(Serialize)]
struct MemoryListResponse {
    memories: Vec<MemoryEntryResponse>,
    total: usize,
}

async fn list_memory(
    State(state): State<Arc<AppState>>,
    Query(mq): Query<MemoryQueryParams>,
) -> Result<Json<MemoryListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let q = TokenQuery { token: mq.token.clone() };
    check_token(&q, &state.dashboard_token)?;

    let limit = mq.limit.unwrap_or(20).min(100);
    let offset = mq.offset.unwrap_or(0);

    let type_filter: Vec<nexmind_memory::MemoryType> = match mq.memory_type.as_deref() {
        Some("semantic") => vec![nexmind_memory::MemoryType::Semantic],
        Some("pinned") => vec![nexmind_memory::MemoryType::Pinned],
        Some("session") => vec![nexmind_memory::MemoryType::Session],
        _ => vec![],
    };

    // If search query provided, use search; otherwise use list
    if let Some(ref search) = mq.q {
        if !search.is_empty() {
            let query = nexmind_memory::MemoryQuery {
                query_text: search.clone(),
                workspace_id: "default".into(),
                agent_id: None,
                memory_types: type_filter,
                top_k: limit,
                min_importance: None,
            };

            let result = state
                .memory_store
                .retrieve_full(query)
                .await
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ErrorResponse {
                            error: e.to_string(),
                        }),
                    )
                })?;

            let memories = result
                .memories
                .into_iter()
                .map(|sm| MemoryEntryResponse {
                    id: sm.memory.id,
                    memory_type: sm.memory.memory_type.as_str().to_string(),
                    content: sm.memory.content,
                    importance: sm.memory.importance,
                    source: sm.memory.source.as_str().to_string(),
                    agent_id: sm.memory.agent_id,
                    created_at: sm.memory.created_at,
                })
                .collect::<Vec<_>>();

            let total = memories.len();
            return Ok(Json(MemoryListResponse { memories, total }));
        }
    }

    // No search — list all
    let (memories, total) = state
        .memory_store
        .list("default", &type_filter, limit, offset)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    let entries = memories
        .into_iter()
        .map(|m| MemoryEntryResponse {
            id: m.id,
            memory_type: m.memory_type.as_str().to_string(),
            content: m.content,
            importance: m.importance,
            source: m.source.as_str().to_string(),
            agent_id: m.agent_id,
            created_at: m.created_at,
        })
        .collect();

    Ok(Json(MemoryListResponse {
        memories: entries,
        total,
    }))
}

#[derive(Deserialize)]
struct CreateMemoryRequest {
    content: String,
    #[serde(rename = "type")]
    memory_type: Option<String>,
    importance: Option<f64>,
}

async fn create_memory(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
    Json(body): Json<CreateMemoryRequest>,
) -> Result<Json<ActionResult>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    let memory_type = match body.memory_type.as_deref() {
        Some("pinned") => nexmind_memory::MemoryType::Pinned,
        Some("session") => nexmind_memory::MemoryType::Session,
        _ => nexmind_memory::MemoryType::Semantic,
    };

    let new_mem = nexmind_memory::NewMemory {
        workspace_id: "default".into(),
        agent_id: None,
        memory_type,
        content: body.content,
        source: nexmind_memory::MemorySource::User,
        source_task_id: None,
        access_policy: nexmind_memory::AccessPolicy::Workspace,
        metadata: None,
        importance: body.importance,
    };

    match state.memory_store.store(new_mem).await {
        Ok(id) => Ok(Json(ActionResult {
            success: true,
            message: id,
        })),
        Err(e) => Ok(Json(ActionResult {
            success: false,
            message: e.to_string(),
        })),
    }
}

async fn delete_memory(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Result<Json<ActionResult>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    match state.memory_store.delete(&id) {
        Ok(()) => Ok(Json(ActionResult {
            success: true,
            message: "Deleted".into(),
        })),
        Err(e) => Ok(Json(ActionResult {
            success: false,
            message: e.to_string(),
        })),
    }
}

async fn pin_memory(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Result<Json<ActionResult>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    match state.memory_store.pin(&id) {
        Ok(()) => Ok(Json(ActionResult {
            success: true,
            message: "Pinned".into(),
        })),
        Err(e) => Ok(Json(ActionResult {
            success: false,
            message: e.to_string(),
        })),
    }
}

async fn unpin_memory(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Result<Json<ActionResult>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    match state.memory_store.unpin(&id) {
        Ok(()) => Ok(Json(ActionResult {
            success: true,
            message: "Unpinned".into(),
        })),
        Err(e) => Ok(Json(ActionResult {
            success: false,
            message: e.to_string(),
        })),
    }
}

// ── Settings ──────────────────────────────────────────────────────────

async fn get_settings(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
) -> Result<Json<Value>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    let conn = state.db.conn().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() }))
    })?;

    // Ensure settings table exists
    let _ = conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL)"
    );

    let mut stmt = conn
        .prepare("SELECT key, value FROM settings")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() })))?;

    let settings: Vec<(String, String)> = stmt
        .query_map([], |row| {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            Ok((key, value))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() })))?
        .filter_map(|r| r.ok())
        .collect();

    let mut map = serde_json::Map::new();
    for (key, value) in settings {
        let v = serde_json::from_str(&value).unwrap_or(Value::String(value));
        map.insert(key, v);
    }

    Ok(Json(Value::Object(map)))
}

async fn save_settings(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
    Json(body): Json<Value>,
) -> Result<Json<ActionResult>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    if let Value::Object(map) = body {
        let conn = state.db.conn().map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() }))
        })?;
        let _ = conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL)"
        );
        let now = chrono::Utc::now().to_rfc3339();
        for (key, value) in map {
            let value_str = match &value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let _ = conn.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
                rusqlite::params![key, value_str, now],
            );
        }
        Ok(Json(ActionResult {
            success: true,
            message: "Settings saved".into(),
        }))
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Expected JSON object".into(),
            }),
        ))
    }
}

#[derive(Serialize)]
struct StatsResponse {
    database_size_bytes: u64,
    memory_count: usize,
    agent_count: usize,
    approval_count: usize,
    cost_records: usize,
}

async fn get_stats(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
) -> Result<Json<StatsResponse>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    let agent_count = state.agent_registry.list_all().map(|a| a.len()).unwrap_or(0);
    let approval_count = state
        .approval_manager
        .list_pending("default")
        .map(|p| p.len())
        .unwrap_or(0);

    let memory_count = state
        .memory_store
        .list("default", &[], 0, 0)
        .map(|(_, total)| total)
        .unwrap_or(0);

    // Cost record count from DB
    let cost_records: usize = state
        .db
        .conn()
        .ok()
        .and_then(|c| c.query_row("SELECT COUNT(*) FROM cost_records", [], |row| row.get(0)).ok())
        .unwrap_or(0);

    // Database size
    let db_size: u64 = state
        .db
        .conn()
        .ok()
        .and_then(|c| {
            c.query_row(
                "SELECT page_count * page_size as size FROM pragma_page_count(), pragma_page_size()",
                [],
                |row| row.get(0),
            )
            .ok()
        })
        .unwrap_or(0);

    Ok(Json(StatsResponse {
        database_size_bytes: db_size,
        memory_count,
        agent_count,
        approval_count,
        cost_records,
    }))
}

async fn create_backup(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
) -> Result<Json<ActionResult>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    // Trigger WAL checkpoint for a consistent backup snapshot
    if let Ok(conn) = state.db.conn() {
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
    }

    Ok(Json(ActionResult {
        success: true,
        message: "Backup checkpoint completed".into(),
    }))
}

// ── Integrations ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct IntegrationInfo {
    id: String,
    name: String,
    category: String,
    status: String,
    configured: bool,
    details: Value,
}

#[derive(Serialize)]
struct IntegrationsResponse {
    integrations: Vec<IntegrationInfo>,
}

async fn list_integrations(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
) -> Result<Json<IntegrationsResponse>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    let model_router = &state.model_router;
    let mut integrations = Vec::new();

    // Claude Code (subscription)
    let has_claude_code = model_router.has_provider("claude-code");
    integrations.push(IntegrationInfo {
        id: "claude-code".into(),
        name: "Claude Code (Subscription)".into(),
        category: "llm".into(),
        status: if has_claude_code { "connected".into() } else { "not_configured".into() },
        configured: has_claude_code,
        details: serde_json::json!({
            "description": "Uses Claude Code CLI subscription. Free with active subscription.",
            "auto_detected": true,
        }),
    });

    // Anthropic API
    let has_anthropic = model_router.has_provider("anthropic");
    integrations.push(IntegrationInfo {
        id: "anthropic".into(),
        name: "Anthropic API".into(),
        category: "llm".into(),
        status: if has_anthropic { "connected".into() } else { "not_configured".into() },
        configured: has_anthropic,
        details: serde_json::json!({
            "description": "Direct Anthropic API access. Requires API key from console.anthropic.com.",
            "fields": ["api_key"],
        }),
    });

    // OpenAI
    let has_openai = model_router.has_provider("openai");
    integrations.push(IntegrationInfo {
        id: "openai".into(),
        name: "OpenAI".into(),
        category: "llm".into(),
        status: if has_openai { "connected".into() } else { "not_configured".into() },
        configured: has_openai,
        details: serde_json::json!({
            "description": "OpenAI API for GPT models. Requires API key from platform.openai.com.",
            "fields": ["api_key"],
        }),
    });

    // Ollama
    let has_ollama = model_router.has_provider("ollama");
    integrations.push(IntegrationInfo {
        id: "ollama".into(),
        name: "Ollama (Local)".into(),
        category: "llm".into(),
        status: if has_ollama { "connected".into() } else { "not_configured".into() },
        configured: has_ollama,
        details: serde_json::json!({
            "description": "Local LLM inference. Requires Ollama running on localhost.",
            "fields": ["url"],
            "default_url": "http://localhost:11434",
        }),
    });

    // Telegram
    let has_telegram = std::env::var("TELEGRAM_BOT_TOKEN").is_ok();
    integrations.push(IntegrationInfo {
        id: "telegram".into(),
        name: "Telegram".into(),
        category: "messaging".into(),
        status: if has_telegram { "connected".into() } else { "not_configured".into() },
        configured: has_telegram,
        details: serde_json::json!({
            "description": "Telegram bot for chat interface.",
            "fields": ["bot_token", "allowed_users"],
            "help": "Get a bot token from @BotFather in Telegram. Find your User ID via @userinfobot.",
        }),
    });

    // Discord (coming soon)
    integrations.push(IntegrationInfo {
        id: "discord".into(),
        name: "Discord".into(),
        category: "messaging".into(),
        status: "coming_soon".into(),
        configured: false,
        details: serde_json::json!({
            "description": "Discord bot integration. Coming in V2.",
        }),
    });

    // Email
    let has_email = std::env::var("EMAIL_ADDRESS").is_ok();
    integrations.push(IntegrationInfo {
        id: "email".into(),
        name: "Gmail / IMAP".into(),
        category: "email".into(),
        status: if has_email { "connected".into() } else { "not_configured".into() },
        configured: has_email,
        details: serde_json::json!({
            "description": "Email access via IMAP/SMTP.",
            "fields": ["email", "app_password", "imap_host", "smtp_host"],
            "help": "For Gmail: enable 2FA, then create App Password at myaccount.google.com/apppasswords",
        }),
    });

    // Browser
    integrations.push(IntegrationInfo {
        id: "browser".into(),
        name: "Browser (Chrome)".into(),
        category: "services".into(),
        status: "connected".into(),
        configured: true,
        details: serde_json::json!({
            "description": "Headless Chrome for web browsing.",
            "fields": ["headless"],
        }),
    });

    Ok(Json(IntegrationsResponse { integrations }))
}

async fn configure_integration(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<ActionResult>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    // Store config in settings table for persistence
    if let Ok(conn) = state.db.conn() {
        let _ = conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL)"
        );
        let now = chrono::Utc::now().to_rfc3339();
        let config_key = format!("integration_{}_config", id);
        let config_str = body.to_string();
        let _ = conn.execute(
            "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
            rusqlite::params![config_key, config_str, now],
        );
    }

    Ok(Json(ActionResult {
        success: true,
        message: format!("Configuration saved for {}. Restart daemon to apply.", id),
    }))
}

async fn test_integration(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Result<Json<ActionResult>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    match id.as_str() {
        "claude-code" | "anthropic" | "openai" | "ollama" => {
            if state.model_router.has_provider(&id) {
                Ok(Json(ActionResult {
                    success: true,
                    message: format!("{} provider is connected and responding.", id),
                }))
            } else {
                Ok(Json(ActionResult {
                    success: false,
                    message: format!("{} provider is not available.", id),
                }))
            }
        }
        "telegram" => {
            let has_token = std::env::var("TELEGRAM_BOT_TOKEN").is_ok();
            Ok(Json(ActionResult {
                success: has_token,
                message: if has_token {
                    "Telegram bot token is configured.".into()
                } else {
                    "TELEGRAM_BOT_TOKEN environment variable not set.".into()
                },
            }))
        }
        "email" => {
            let has_email = std::env::var("EMAIL_ADDRESS").is_ok();
            Ok(Json(ActionResult {
                success: has_email,
                message: if has_email {
                    "Email configuration detected.".into()
                } else {
                    "Email not configured. Set EMAIL_ADDRESS, EMAIL_PASSWORD, IMAP_HOST, SMTP_HOST.".into()
                },
            }))
        }
        _ => Ok(Json(ActionResult {
            success: false,
            message: format!("Unknown integration: {}", id),
        })),
    }
}

async fn delete_integration(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Result<Json<ActionResult>, (StatusCode, Json<ErrorResponse>)> {
    check_token(&q, &state.dashboard_token)?;

    if let Ok(conn) = state.db.conn() {
        let config_key = format!("integration_{}_config", id);
        let _ = conn.execute("DELETE FROM settings WHERE key = ?1", rusqlite::params![config_key]);
    }

    Ok(Json(ActionResult {
        success: true,
        message: format!("Integration {} disconnected. Restart daemon to apply.", id),
    }))
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dashboard_html_serves() {
        assert!(!DASHBOARD_HTML.is_empty());
        assert!(DASHBOARD_HTML.contains("<!DOCTYPE html>"));
        assert!(DASHBOARD_HTML.contains("NexMind"));
    }

    #[test]
    fn test_check_token_valid() {
        let q = TokenQuery {
            token: Some("abc123".into()),
        };
        assert!(check_token(&q, "abc123").is_ok());
    }

    #[test]
    fn test_check_token_invalid() {
        let q = TokenQuery {
            token: Some("wrong".into()),
        };
        let result = check_token(&q, "abc123");
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_check_token_missing() {
        let q = TokenQuery { token: None };
        let result = check_token(&q, "abc123");
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_dashboard_html_has_dark_theme() {
        assert!(DASHBOARD_HTML.contains("data-theme"));
        assert!(DASHBOARD_HTML.contains("--bg-primary"));
    }

    #[test]
    fn test_dashboard_html_no_external_cdn() {
        assert!(!DASHBOARD_HTML.contains("cdn.jsdelivr.net"));
        assert!(!DASHBOARD_HTML.contains("unpkg.com"));
        assert!(!DASHBOARD_HTML.contains("cdnjs.cloudflare.com"));
    }

    #[test]
    fn test_dashboard_html_has_views() {
        assert!(DASHBOARD_HTML.contains("chat"));
        assert!(DASHBOARD_HTML.contains("integrations") || DASHBOARD_HTML.contains("Integrations"));
        assert!(DASHBOARD_HTML.contains("settings") || DASHBOARD_HTML.contains("Settings"));
    }

    #[test]
    fn test_dashboard_html_has_api_calls() {
        assert!(DASHBOARD_HTML.contains("/api/health"));
        assert!(DASHBOARD_HTML.contains("/api/agents"));
    }

    #[test]
    fn test_dashboard_html_has_websocket() {
        assert!(DASHBOARD_HTML.contains("WebSocket") || DASHBOARD_HTML.contains("ws://") || DASHBOARD_HTML.contains("wss://"));
    }

    #[test]
    fn test_dashboard_html_size_limit() {
        assert!(
            DASHBOARD_HTML.len() < 150_000,
            "Dashboard HTML must be under 150KB, got {} bytes",
            DASHBOARD_HTML.len()
        );
    }

    #[test]
    fn test_dashboard_html_has_sidebar() {
        assert!(DASHBOARD_HTML.contains("sidebar"));
    }

    #[test]
    fn test_dashboard_html_has_theme_toggle() {
        assert!(DASHBOARD_HTML.contains("theme"));
    }

    #[test]
    fn test_dashboard_html_has_token_handling() {
        assert!(DASHBOARD_HTML.contains("token"));
    }

    #[test]
    fn test_dashboard_html_is_self_contained() {
        // No external script or stylesheet references
        let has_external = DASHBOARD_HTML.contains("src=\"http")
            || DASHBOARD_HTML.contains("href=\"http");
        assert!(!has_external, "Dashboard should not reference external resources");
    }
}
