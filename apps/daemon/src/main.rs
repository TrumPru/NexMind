use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use tokio::net::TcpListener;
use tonic::{Request, Response, Status};
use tracing::{error, info};

pub mod proto {
    tonic::include_proto!("nexmind");
}

pub mod dashboard;
pub mod http_api;
pub mod router;

use proto::nex_mind_server::{NexMind, NexMindServer};
use proto::*;

#[derive(Parser)]
#[command(name = "nexmind-daemon", about = "NexMind daemon process")]
struct Args {
    /// Path to the Unix domain socket (or TCP port for Windows)
    #[arg(long, default_value = "127.0.0.1:19384")]
    socket_path: String,

    /// Data directory for databases and state
    #[arg(long, default_value = "./data")]
    data_dir: String,

    /// Workspace directory for file operations
    #[arg(long, default_value = "./data/workspace")]
    workspace_dir: String,
}

struct NexMindService {
    db: Arc<nexmind_storage::Database>,
    agent_registry: Arc<nexmind_agent_engine::AgentRegistry>,
    agent_runtime: Arc<nexmind_agent_engine::AgentRuntime>,
    #[allow(dead_code)]
    event_bus: Arc<nexmind_event_bus::EventBus>,
    start_time: Instant,
    session_id: String,
    workspace_path: std::path::PathBuf,
    #[allow(dead_code)]
    message_router: Arc<tokio::sync::RwLock<router::MessageRouter>>,
    #[allow(dead_code)]
    scheduler: Arc<nexmind_scheduler::SchedulerImpl>,
    approval_manager: Arc<nexmind_agent_engine::approval::ApprovalManager>,
    cost_tracker: Arc<nexmind_agent_engine::cost::CostTracker>,
}

#[tonic::async_trait]
impl NexMind for NexMindService {
    type SendMessageStream = tokio_stream::wrappers::ReceiverStream<Result<ChatEvent, Status>>;
    type SubscribeTaskUpdatesStream =
        tokio_stream::wrappers::ReceiverStream<Result<TaskEvent, Status>>;
    type RunWorkflowStream = tokio_stream::wrappers::ReceiverStream<Result<WorkflowEvent, Status>>;

    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<Response<Self::SendMessageStream>, Status> {
        let req = request.into_inner();
        let agent_id = if req.agent_id.is_empty() {
            "agt_default_chat".to_string()
        } else {
            req.agent_id
        };
        let workspace_id = if req.workspace_id.is_empty() {
            "default".to_string()
        } else {
            req.workspace_id
        };

        let agent = self
            .agent_registry
            .get(&agent_id)
            .map_err(|e| Status::not_found(e.to_string()))?;

        let context = nexmind_agent_engine::RunContext::new(&workspace_id)
            .with_session(&self.session_id)
            .with_workspace_path(self.workspace_path.clone());
        let run_id = context.run_id.clone();

        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let runtime = self.agent_runtime.clone();

        tokio::spawn(async move {
            match runtime.run(&agent, &req.text, context).await {
                Ok(result) => {
                    if let Some(response_text) = &result.response {
                        let _ = tx
                            .send(Ok(ChatEvent {
                                event_type: "text_delta".into(),
                                data: serde_json::json!({"text": response_text}).to_string(),
                                agent_id: agent_id.clone(),
                                run_id: run_id.clone(),
                            }))
                            .await;
                    }
                    let _ = tx
                        .send(Ok(ChatEvent {
                            event_type: "done".into(),
                            data: serde_json::json!({
                                "status": result.status.name(),
                                "iterations": result.iterations,
                                "tokens_used": result.tokens_used.total_tokens,
                                "duration_ms": result.duration_ms,
                            })
                            .to_string(),
                            agent_id: agent_id.clone(),
                            run_id,
                        }))
                        .await;
                }
                Err(e) => {
                    let _ = tx
                        .send(Ok(ChatEvent {
                            event_type: "error".into(),
                            data: serde_json::json!({"error": e.to_string()}).to_string(),
                            agent_id,
                            run_id,
                        }))
                        .await;
                }
            }
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    async fn list_agents(
        &self,
        request: Request<ListAgentsRequest>,
    ) -> Result<Response<AgentList>, Status> {
        let req = request.into_inner();
        let workspace_id = if req.workspace_id.is_empty() {
            "default".to_string()
        } else {
            req.workspace_id
        };

        let agents = self
            .agent_registry
            .list(&workspace_id)
            .map_err(|e| Status::internal(e.to_string()))?;

        let summaries = agents
            .iter()
            .map(|a| AgentSummary {
                id: a.id.clone(),
                name: a.name.clone(),
                status: "idle".into(),
                description: a.description.clone().unwrap_or_default(),
                version: a.version as i32,
            })
            .collect();

        Ok(Response::new(AgentList { agents: summaries }))
    }

    async fn get_agent_status(
        &self,
        request: Request<AgentId>,
    ) -> Result<Response<AgentStatus>, Status> {
        let id = request.into_inner().id;

        let _ = self
            .agent_registry
            .get(&id)
            .map_err(|e| Status::not_found(e.to_string()))?;

        Ok(Response::new(AgentStatus {
            id,
            status: "idle".into(),
            current_run_id: String::new(),
            total_runs: 0,
            cost_microdollars_today: 0,
        }))
    }

    async fn create_agent(
        &self,
        _request: Request<AgentDefinition>,
    ) -> Result<Response<AgentId>, Status> {
        Err(Status::unimplemented("not yet implemented"))
    }

    async fn list_tasks(
        &self,
        _request: Request<TaskFilter>,
    ) -> Result<Response<TaskList>, Status> {
        Ok(Response::new(TaskList { tasks: vec![] }))
    }

    async fn subscribe_task_updates(
        &self,
        _request: Request<TaskId>,
    ) -> Result<Response<Self::SubscribeTaskUpdatesStream>, Status> {
        let (_tx, rx) = tokio::sync::mpsc::channel(32);
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    async fn cancel_task(
        &self,
        _request: Request<TaskId>,
    ) -> Result<Response<CancelResult>, Status> {
        Err(Status::unimplemented("not yet implemented"))
    }

    async fn list_pending_approvals(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ApprovalList>, Status> {
        let pending = self
            .approval_manager
            .list_pending("default")
            .map_err(|e| Status::internal(e.to_string()))?;

        let approvals = pending
            .iter()
            .map(|r| proto::ApprovalSummary {
                id: r.id.clone(),
                agent_id: r.requester_agent_id.clone(),
                tool_id: r.tool_id.clone(),
                action_description: r.action_description.clone(),
                risk_level: r.risk_level.clone(),
                created_at: r.created_at.clone(),
                expires_at: r.expires_at.clone(),
            })
            .collect();

        Ok(Response::new(ApprovalList { approvals }))
    }

    async fn approve(
        &self,
        request: Request<ApprovalDecision>,
    ) -> Result<Response<ApprovalResult>, Status> {
        let req = request.into_inner();
        let result = if req.approved {
            self.approval_manager
                .approve(&req.approval_id, &req.decided_by)
        } else {
            self.approval_manager
                .deny(&req.approval_id, &req.decided_by, req.note.as_deref())
        };

        match result {
            Ok(()) => Ok(Response::new(ApprovalResult {
                success: true,
                message: if req.approved {
                    "Approved".into()
                } else {
                    "Denied".into()
                },
            })),
            Err(e) => Ok(Response::new(ApprovalResult {
                success: false,
                message: e.to_string(),
            })),
        }
    }

    async fn query_memory(
        &self,
        _request: Request<MemoryQuery>,
    ) -> Result<Response<MemoryResults>, Status> {
        Ok(Response::new(MemoryResults { entries: vec![] }))
    }

    async fn run_workflow(
        &self,
        _request: Request<WorkflowId>,
    ) -> Result<Response<Self::RunWorkflowStream>, Status> {
        let (_tx, rx) = tokio::sync::mpsc::channel(32);
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    async fn get_health(&self, _request: Request<Empty>) -> Result<Response<HealthStatus>, Status> {
        let db_ok = self.db.health_check().unwrap_or(false);
        let uptime = self.start_time.elapsed().as_secs() as i64;

        let active_agents = self
            .agent_registry
            .list_all()
            .map(|a| a.len() as i32)
            .unwrap_or(0);

        Ok(Response::new(HealthStatus {
            status: if db_ok { "ok".into() } else { "degraded".into() },
            database_ok: db_ok,
            uptime_seconds: uptime,
            version: env!("CARGO_PKG_VERSION").to_string(),
            active_agents,
            pending_approvals: self
                .approval_manager
                .list_pending("default")
                .map(|p| p.len() as i32)
                .unwrap_or(0),
        }))
    }

    async fn get_cost_summary(
        &self,
        request: Request<CostQuery>,
    ) -> Result<Response<CostSummary>, Status> {
        let req = request.into_inner();
        let period = match req.period.as_str() {
            "7d" => nexmind_agent_engine::cost::CostPeriod::Last7Days,
            "30d" => nexmind_agent_engine::cost::CostPeriod::Last30Days,
            "all" => nexmind_agent_engine::cost::CostPeriod::AllTime,
            _ => nexmind_agent_engine::cost::CostPeriod::Today,
        };

        let workspace_id = if req.workspace_id.is_empty() {
            "default"
        } else {
            &req.workspace_id
        };

        let summary = self
            .cost_tracker
            .summary(workspace_id, period)
            .map_err(|e| Status::internal(e.to_string()))?;

        let by_model = summary
            .by_model
            .iter()
            .map(|(model, cost)| CostByModel {
                model: model.clone(),
                provider: String::new(),
                cost_microdollars: (cost * 1_000_000.0) as i64,
                requests: 0,
            })
            .collect();

        Ok(Response::new(CostSummary {
            total_microdollars: (summary.total_cost_usd * 1_000_000.0) as i64,
            total_requests: summary.total_requests as i32,
            total_input_tokens: summary.total_input_tokens as i64,
            total_output_tokens: summary.total_output_tokens as i64,
            by_model,
        }))
    }
}

/// Initialize the model router with auto-detected providers.
///
/// Provider detection priority (try all, register what works):
/// 1. ANTHROPIC_API_KEY env var → AnthropicProvider (direct API, fastest)
/// 2. Claude Code CLI installed + authenticated → ClaudeCodeProvider (subscription, free)
/// 3. OPENAI_API_KEY env var → OpenAIProvider (direct API)
/// 4. Ollama running on localhost → OllamaProvider (local, free)
fn init_model_router() -> nexmind_model_router::ModelRouter {
    let mut router = nexmind_model_router::ModelRouter::new();
    let mut providers_found: Vec<&str> = Vec::new();

    // 1. Try Anthropic API key (direct API, highest priority)
    match nexmind_model_router::AnthropicProvider::from_auto_detect() {
        Ok(provider) => {
            router.register_provider(Arc::new(provider));
            providers_found.push("Anthropic (API key)");
        }
        Err(e) => {
            info!("Anthropic provider not available: {}", e);
        }
    }

    // 2. Try Claude Code CLI (subscription-based, free)
    if let Some(claude_code) = nexmind_model_router::ClaudeCodeProvider::detect() {
        router.register_provider(Arc::new(claude_code));
        providers_found.push("Claude Code (subscription)");
    } else {
        info!("Claude Code provider: CLI not installed or not authenticated");
    }

    // 3. Try OpenAI (optional)
    match nexmind_model_router::OpenAIProvider::from_env() {
        Ok(provider) => {
            router.register_provider(Arc::new(provider));
            providers_found.push("OpenAI (API key)");
        }
        Err(_) => {
            info!("OpenAI provider: not configured (OPENAI_API_KEY not set)");
        }
    }

    // 4. Try Ollama (optional, local)
    match nexmind_model_router::OllamaProvider::default_local() {
        Ok(provider) => {
            router.register_provider(Arc::new(provider));
            providers_found.push("Ollama (local)");
        }
        Err(e) => {
            info!("Ollama provider: not available: {}", e);
        }
    }

    if providers_found.is_empty() {
        error!(
            "No LLM providers available!\n\
             \n\
             Set up at least one:\n\
             1. Install Claude Code CLI: npm install -g @anthropic-ai/claude-code\n\
                Then authenticate: claude login\n\
             2. Set ANTHROPIC_API_KEY (from console.anthropic.com)\n\
             3. Set OPENAI_API_KEY (from platform.openai.com)\n\
             4. Install Ollama (ollama.com) and run: ollama pull llama3.2"
        );
    } else {
        info!("Available providers: {}", providers_found.join(", "));
    }

    router
}

/// Initialize the tool registry with all built-in tools.
fn init_tool_registry(
    event_bus: Arc<nexmind_event_bus::EventBus>,
    audit: Arc<nexmind_security::AuditLogger>,
    memory_store: Arc<nexmind_memory::MemoryStoreImpl>,
    workspace_path: &std::path::Path,
) -> nexmind_tool_runtime::ToolRegistry {
    use nexmind_tool_runtime::tools::*;

    let mut registry = nexmind_tool_runtime::ToolRegistry::new(event_bus, audit);

    registry.register(Box::new(MemoryReadTool::new(memory_store.clone())));
    registry.register(Box::new(MemoryWriteTool::new(memory_store)));
    registry.register(Box::new(FsReadTool));
    registry.register(Box::new(FsWriteTool));
    registry.register(Box::new(FsListTool));
    registry.register(Box::new(HttpFetchTool));
    registry.register(Box::new(ShellExecTool));
    registry.register(Box::new(SendMessageTool));

    // Email tools (if configured via env vars)
    if let Some(email_config) = EmailConfig::from_env() {
        let email_connector: SharedEmailConnector =
            std::sync::Arc::new(tokio::sync::Mutex::new(EmailConnector::new(email_config)));
        registry.register(Box::new(EmailFetchTool::new(email_connector.clone())));
        registry.register(Box::new(EmailReadTool::new(email_connector.clone())));
        registry.register(Box::new(EmailSearchTool::new(email_connector.clone())));
        registry.register(Box::new(EmailSendTool::new(email_connector)));
        info!("email tools registered");
    }

    // Browser tools (lazy-started — Chrome only launches on first browser tool call)
    {
        let browser_config = BrowserConfig {
            headless: true,
            screenshot_dir: workspace_path.join("screenshots"),
            allowed_domains: None,    // No whitelist — all domains allowed
            blocked_domains: vec![],  // No blocklist — full access
            ..Default::default()
        };
        let browser_manager: SharedBrowserManager =
            std::sync::Arc::new(tokio::sync::Mutex::new(BrowserManager::new(browser_config)));

        registry.register(Box::new(BrowserNavigateTool::new(browser_manager.clone())));
        registry.register(Box::new(BrowserScreenshotTool::new(browser_manager.clone())));
        registry.register(Box::new(BrowserExtractTextTool::new(browser_manager.clone())));
        registry.register(Box::new(BrowserExtractLinksTool::new(browser_manager.clone())));
        registry.register(Box::new(BrowserClickTool::new(browser_manager.clone())));
        registry.register(Box::new(BrowserTypeTool::new(browser_manager)));
    }

    // OpenClaw external agent tools (if gateway is configured via env vars)
    if let Ok(openclaw_config) = nexmind_openclaw::OpenClawConfig::from_env() {
        // Only register if explicitly configured (OPENCLAW_GATEWAY_URL is set)
        if std::env::var("OPENCLAW_GATEWAY_URL").is_ok() {
            let openclaw_agent = Arc::new(nexmind_openclaw::OpenClawAgent::new(openclaw_config));
            registry.register(Box::new(nexmind_openclaw::OpenClawSendTool::new(openclaw_agent.clone())));
            registry.register(Box::new(nexmind_openclaw::OpenClawDelegateTool::new(openclaw_agent.clone())));
            registry.register(Box::new(nexmind_openclaw::OpenClawStatusTool::new(openclaw_agent)));
            info!("OpenClaw external agent tools registered");
        }
    }

    info!("tool registry initialized with {} tools", registry.list_all_tools().len());
    registry
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();

    info!("nexmind daemon starting");
    info!(data_dir = %args.data_dir, "data directory");

    // Ensure data and workspace directories exist
    std::fs::create_dir_all(&args.data_dir)?;
    std::fs::create_dir_all(&args.workspace_dir)?;

    // Initialize storage
    let db_path = format!("{}/nexmind.db", args.data_dir);
    let db = nexmind_storage::Database::open(&db_path)?;
    db.run_migrations()?;
    info!("database initialized");

    let db = Arc::new(db);

    // Initialize event bus
    let event_bus = Arc::new(nexmind_event_bus::EventBus::with_default_capacity());
    info!("event bus initialized");

    // Initialize model router
    let model_router = Arc::new(init_model_router());

    // Initialize memory store
    let memory_db_path = format!("{}/memory.db", args.data_dir);
    let memory_store = Arc::new(
        nexmind_memory::MemoryStoreImpl::open(
            &memory_db_path,
            model_router.clone(),
            event_bus.clone(),
        )
        .expect("failed to initialize memory store"),
    );
    info!("memory store initialized");

    // Initialize audit logger
    let hmac_key: [u8; 32] = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        "nexmind-audit-key".hash(&mut hasher);
        let h = hasher.finish();
        let mut key = [0u8; 32];
        key[..8].copy_from_slice(&h.to_le_bytes());
        key[8..16].copy_from_slice(&h.to_be_bytes());
        key[16..24].copy_from_slice(&h.to_le_bytes());
        key[24..32].copy_from_slice(&h.to_be_bytes());
        key
    };
    let audit = Arc::new(nexmind_security::AuditLogger::new(db.clone(), hmac_key));
    info!("audit logger initialized");

    // Initialize tool registry
    let workspace_path = std::path::PathBuf::from(&args.workspace_dir);
    let tool_registry = Arc::new(init_tool_registry(
        event_bus.clone(),
        audit,
        memory_store.clone(),
        &workspace_path,
    ));

    // Initialize agent registry and create default agent
    let agent_registry = Arc::new(nexmind_agent_engine::AgentRegistry::new(db.clone()));

    // Auto-select the best available model for the default agent
    let default_model = model_router.select_default_model();
    info!(model = %default_model, "default agent using auto-selected model");

    let mut default_agent = nexmind_agent_engine::AgentDefinition::default_chat("default");
    default_agent.model.primary = default_model;

    match agent_registry.create(&default_agent) {
        Ok(_) => info!("default agent created"),
        Err(e) => info!("default agent: {}", e),
    }

    // Register OpenClaw external agent if configured
    if std::env::var("OPENCLAW_GATEWAY_URL").is_ok() {
        let openclaw_agent_def = nexmind_openclaw::OpenClawAgent::as_agent_definition("default");
        match agent_registry.create(&openclaw_agent_def) {
            Ok(_) => info!("OpenClaw external agent registered (agt_openclaw)"),
            Err(e) => info!("OpenClaw agent: {}", e),
        }
    }

    // Initialize approval manager and cost tracker
    let approval_manager = Arc::new(nexmind_agent_engine::approval::ApprovalManager::new(
        db.clone(),
        event_bus.clone(),
    ));
    info!("approval manager initialized");

    let cost_tracker = Arc::new(nexmind_agent_engine::cost::CostTracker::new(
        db.clone(),
        event_bus.clone(),
    ));
    info!("cost tracker initialized");

    // Initialize agent runtime with memory, tools, approvals, and cost tracking
    let agent_runtime = Arc::new(
        nexmind_agent_engine::AgentRuntime::new(
            model_router.clone(),
            event_bus.clone(),
            db.clone(),
        )
        .with_memory(memory_store.clone())
        .with_tools(tool_registry)
        .with_approvals(approval_manager.clone())
        .with_cost_tracker(cost_tracker.clone()),
    );

    // Stable session ID for this daemon instance
    let session_id = ulid::Ulid::new().to_string();

    // ── Initialize Skill Registry ───────────────────────────────────
    let skills_dir = std::path::PathBuf::from(&args.data_dir).join("skills");
    let skill_registry = Arc::new(nexmind_skill_registry::SkillRegistry::new(skills_dir));

    // Load built-in skills
    let builtin_dir = std::path::PathBuf::from("skills/builtin");
    if builtin_dir.exists() {
        for entry in std::fs::read_dir(&builtin_dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Ok(manifest) = nexmind_skill_registry::SkillManifest::from_file(&path.join("skill.yaml")) {
                    let _ = skill_registry.install(
                        manifest,
                        nexmind_skill_registry::SkillSource::Builtin,
                        Some(path),
                    );
                }
            }
        }
    }
    // Load user-installed skills
    let _ = skill_registry.load_from_dir();
    info!(
        skills = skill_registry.active_count(),
        "skill registry initialized"
    );

    // ── Initialize Scheduler ────────────────────────────────────────
    let scheduler = Arc::new(nexmind_scheduler::SchedulerImpl::new(db.clone()));
    info!("scheduler initialized");

    // ── Initialize Message Router + Telegram Connector ──────────────
    let mut message_router = router::MessageRouter::new(
        agent_runtime.clone(),
        event_bus.clone(),
        agent_registry.clone(),
        "default".into(),
        session_id.clone(),
        workspace_path.clone(),
    );

    message_router.set_approval_manager(approval_manager.clone());

    // Try to set up Telegram connector if token is available
    match nexmind_telegram::TelegramConfig::from_env() {
        Ok(config) => {
            let telegram = Arc::new(nexmind_telegram::TelegramConnector::new(config));
            message_router.register_connector(telegram);
            info!("Telegram connector registered");
        }
        Err(e) => {
            info!("Telegram connector: not configured ({})", e);
        }
    }

    let message_router = Arc::new(tokio::sync::RwLock::new(message_router));

    // Start message router listeners
    {
        let router_guard = message_router.read().await;
        let _handles = router_guard.start().await;
        info!("message router listeners started");
    }

    // ── Start Scheduler with action handler ─────────────────────────
    {
        let sched = scheduler.clone();
        let runtime = agent_runtime.clone();
        let registry = agent_registry.clone();
        let router = message_router.clone();
        let ws_path = workspace_path.clone();

        let handler = Arc::new(DaemonSchedulerHandler {
            agent_runtime: runtime,
            agent_registry: registry,
            message_router: router,
            workspace_path: ws_path,
        });

        if let Err(e) = sched.start(handler).await {
            error!(error = %e, "failed to start scheduler");
        } else {
            info!("scheduler started");
        }
    }

    // ── Start HTTP Dashboard Server ──────────────────────────────────
    let dashboard_token = dashboard::DashboardServer::generate_token();
    let start_time = Instant::now();
    info!("Dashboard: http://localhost:19385/?token={}", dashboard_token);

    let http_state = Arc::new(http_api::AppState {
        db: db.clone(),
        agent_registry: agent_registry.clone(),
        approval_manager: approval_manager.clone(),
        cost_tracker: cost_tracker.clone(),
        dashboard_token,
        event_bus: event_bus.clone(),
        start_time,
        skill_registry: skill_registry.clone(),
        memory_store: memory_store.clone(),
        model_router: model_router.clone(),
    });

    let http_router = http_api::build_router(http_state);
    let http_listener = TcpListener::bind("127.0.0.1:19385").await?;
    tokio::spawn(async move {
        if let Err(e) = axum::serve(http_listener, http_router).await {
            error!(error = %e, "HTTP dashboard server error");
        }
    });

    let service = NexMindService {
        db,
        agent_registry,
        agent_runtime,
        event_bus,
        start_time,
        session_id,
        workspace_path,
        message_router,
        scheduler,
        approval_manager,
        cost_tracker,
    };

    let addr: std::net::SocketAddr = args.socket_path.parse()?;
    info!(addr = %addr, "starting gRPC server");

    let listener = TcpListener::bind(addr).await?;

    let shutdown = async {
        tokio::signal::ctrl_c().await.ok();
        info!("shutdown signal received");
    };

    tonic::transport::Server::builder()
        .add_service(NexMindServer::new(service))
        .serve_with_incoming_shutdown(
            tokio_stream::wrappers::TcpListenerStream::new(listener),
            shutdown,
        )
        .await?;

    Ok(())
}

/// Daemon's implementation of the scheduler action handler.
struct DaemonSchedulerHandler {
    agent_runtime: Arc<nexmind_agent_engine::AgentRuntime>,
    agent_registry: Arc<nexmind_agent_engine::AgentRegistry>,
    message_router: Arc<tokio::sync::RwLock<router::MessageRouter>>,
    workspace_path: std::path::PathBuf,
}

#[async_trait::async_trait]
impl nexmind_scheduler::SchedulerActionHandler for DaemonSchedulerHandler {
    async fn run_agent(&self, agent_id: &str, input: Option<&str>) -> Result<(), String> {
        let agent = self
            .agent_registry
            .get(agent_id)
            .map_err(|e| e.to_string())?;

        let context = nexmind_agent_engine::RunContext::new("default")
            .with_session(&ulid::Ulid::new().to_string())
            .with_workspace_path(self.workspace_path.clone());

        let input_text = input.unwrap_or("Execute your scheduled task.");

        match self.agent_runtime.run(&agent, input_text, context).await {
            Ok(result) => {
                info!(
                    agent_id = %agent_id,
                    status = %result.status,
                    "scheduled agent run completed"
                );

                // If the agent produced a response and we have a default chat_id,
                // send it via Telegram
                if let Some(response) = &result.response {
                    let router = self.message_router.read().await;
                    if let Some(chat_id) = router.get_default_chat_id().await {
                        let _ = router
                            .send_via_connector(
                                "telegram",
                                &chat_id,
                                response,
                                Some(nexmind_connector::ParseMode::Html),
                            )
                            .await;
                    }
                }

                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    async fn run_workflow(
        &self,
        _workflow_id: &str,
        _params: Option<&serde_json::Value>,
    ) -> Result<(), String> {
        Err("Workflow execution not yet implemented".into())
    }
}
