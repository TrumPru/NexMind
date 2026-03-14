/// HTTP REST API for the NexMind web dashboard.
///
/// Provides JSON endpoints consumed by the embedded dashboard HTML.
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

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
        .route("/api/health", get(health))
        .route("/api/agents", get(list_agents))
        .route("/api/approvals", get(list_approvals))
        .route("/api/approvals/{id}/approve", post(approve))
        .route("/api/approvals/{id}/deny", post(deny))
        .route("/api/costs", get(get_costs))
        .route("/api/chat", post(chat))
        .route("/api/logs", get(get_logs))
        .route("/api/skills", get(list_skills))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dashboard_html_serves() {
        // Verify the HTML is non-empty and contains key markers
        assert!(!DASHBOARD_HTML.is_empty());
        assert!(DASHBOARD_HTML.contains("<!DOCTYPE html>"));
        assert!(DASHBOARD_HTML.contains("NexMind Dashboard"));
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
}
