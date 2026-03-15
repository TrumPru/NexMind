use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio_stream::StreamExt;

pub mod proto {
    tonic::include_proto!("nexmind");
}

use proto::nex_mind_client::NexMindClient;
use proto::*;

#[derive(Parser)]
#[command(name = "nexmind", about = "NexMind CLI — manage your AI agents")]
struct Cli {
    /// Daemon address
    #[arg(long, global = true)]
    daemon_addr: Option<String>,

    /// Data directory (for direct DB access in schedule commands)
    #[arg(long, global = true)]
    data_dir: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

/// Resolved CLI settings with config fallbacks applied.
struct ResolvedCli {
    daemon_addr: String,
    data_dir: String,
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check daemon health status
    Health,

    /// Send a message to an agent (or enter REPL mode)
    Chat {
        /// Message to send (omit for interactive REPL)
        message: Option<String>,

        /// Agent ID to chat with
        #[arg(long, default_value = "agt_default_chat")]
        agent: String,
    },

    /// Manage agents
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },

    /// Manage scheduled jobs
    Schedule {
        #[command(subcommand)]
        action: ScheduleAction,
    },

    /// Manage approval requests
    Approve {
        #[command(subcommand)]
        action: ApproveAction,
    },

    /// Manage multi-agent teams
    Team {
        #[command(subcommand)]
        action: TeamAction,
    },

    /// View cost tracking data
    Cost {
        #[command(subcommand)]
        action: CostAction,
    },

    /// Manage models and providers
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },

    /// Browser automation commands (debug/test)
    Browser {
        #[command(subcommand)]
        action: BrowserAction,
    },

    /// Manage skills (plugins)
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },

    /// Manage email integration
    Email {
        #[command(subcommand)]
        action: EmailAction,
    },
}

#[derive(Subcommand)]
enum BrowserAction {
    /// Navigate to a URL and print page info
    Open {
        /// URL to navigate to
        url: String,
    },

    /// Take a screenshot of the current page
    Screenshot {
        /// Capture full page (not just viewport)
        #[arg(long)]
        full_page: bool,
    },

    /// Extract text from the current page
    Text {
        /// CSS selector to extract text from (default: full page)
        #[arg(long)]
        selector: Option<String>,
    },

    /// Extract links from the current page
    Links,

    /// Click an element by CSS selector
    Click {
        /// CSS selector of the element to click
        selector: String,
    },

    /// Type text into an input element
    Type {
        /// CSS selector of the input element
        selector: String,

        /// Text to type
        text: String,
    },

    /// Close the browser
    Close,

    /// Execute JavaScript on the current page
    Js {
        /// JavaScript code to execute
        js: String,
    },

    /// Wait for a CSS selector to appear on the page
    WaitFor {
        /// CSS selector to wait for
        selector: String,

        /// Timeout in milliseconds (default: 10000)
        #[arg(long, default_value = "10000")]
        timeout_ms: u64,
    },

    /// Scroll the page
    Scroll {
        /// Direction: up, down, top, bottom, element
        direction: String,

        /// Pixels to scroll (for up/down)
        #[arg(long)]
        amount: Option<i32>,

        /// CSS selector (for element scroll)
        #[arg(long)]
        selector: Option<String>,
    },

    /// Go back in browser history
    Back,

    /// Select a dropdown option
    Select {
        /// CSS selector of the <select> element
        selector: String,

        /// Value or text to select
        value: String,

        /// Select by 'value' or 'text' (default: value)
        #[arg(long, default_value = "value")]
        by: String,
    },

    /// Extract HTML from the current page
    Html {
        /// CSS selector to extract HTML from (default: full page)
        #[arg(long)]
        selector: Option<String>,
    },
}

#[derive(Subcommand)]
enum ApproveAction {
    /// List pending approval requests
    List,

    /// Approve a pending request
    Accept {
        /// Approval request ID
        id: String,

        /// Decision note
        #[arg(long)]
        note: Option<String>,
    },

    /// Deny a pending request
    Deny {
        /// Approval request ID
        id: String,

        /// Reason for denial
        #[arg(long)]
        reason: Option<String>,
    },

    /// Inspect an approval request
    Inspect {
        /// Approval request ID
        id: String,
    },

    /// Expire stale approval requests
    ExpireStale,
}

#[derive(Subcommand)]
enum TeamAction {
    /// List all teams
    List,

    /// Create a demo research team
    CreateDemo,

    /// Inspect a team definition
    Inspect {
        /// Team ID
        id: String,
    },

    /// Delete a team
    Delete {
        /// Team ID
        id: String,
    },
}

#[derive(Subcommand)]
enum CostAction {
    /// Show cost summary for a workspace
    Summary {
        /// Period: today, 7d, 30d, all
        #[arg(long, default_value = "today")]
        period: String,
    },

    /// Show cost for a specific agent
    Agent {
        /// Agent ID
        id: String,

        /// Period: today, 7d, 30d, all
        #[arg(long, default_value = "today")]
        period: String,
    },
}

#[derive(Subcommand)]
enum ModelAction {
    /// List all available models across all providers
    List,

    /// Show provider health status
    Status,

    /// Send a test prompt to verify a model works
    Test {
        /// Model ID (e.g., "claude-code/sonnet", "ollama/llama3.2")
        model_id: String,
    },
}

#[derive(Subcommand)]
enum AgentAction {
    /// List all registered agents
    List,

    /// Inspect a specific agent's full definition
    Inspect {
        /// Agent ID
        id: String,
    },

    /// Create an agent from a template
    Create {
        /// Template name (e.g., "morning-briefing")
        #[arg(long)]
        template: String,

        /// Cron schedule (e.g., "0 0 8 * * *" for 8:00 AM daily)
        #[arg(long)]
        schedule: Option<String>,

        /// Timezone (e.g., "Europe/Moscow")
        #[arg(long, default_value = "UTC")]
        tz: String,
    },

    /// Run an agent immediately
    Run {
        /// Agent ID
        id: String,

        /// Input text (optional)
        input: Option<String>,
    },
}

#[derive(Subcommand)]
enum ScheduleAction {
    /// List all scheduled jobs
    List,

    /// Create a new scheduled job
    Create {
        /// Agent ID to run
        #[arg(long)]
        agent: String,

        /// Cron expression (e.g., "0 0 8 * * *")
        #[arg(long)]
        cron: String,

        /// Timezone (e.g., "Europe/Moscow")
        #[arg(long, default_value = "UTC")]
        tz: String,

        /// Job name
        #[arg(long)]
        name: String,

        /// Missed policy: run_once, skip, run_all_missed
        #[arg(long, default_value = "run_once")]
        missed_policy: String,
    },

    /// Pause a scheduled job
    Pause {
        /// Job ID
        job_id: String,
    },

    /// Resume a paused job
    Resume {
        /// Job ID
        job_id: String,
    },

    /// Delete a scheduled job
    Delete {
        /// Job ID
        job_id: String,
    },

    /// Manually trigger a job (run now)
    Trigger {
        /// Job ID
        job_id: String,
    },

    /// Inspect a job's details
    Inspect {
        /// Job ID
        job_id: String,
    },
}

#[derive(Subcommand)]
enum EmailAction {
    /// Set up email integration (interactive)
    Setup {
        /// Email address
        #[arg(long)]
        email: String,

        /// App password
        #[arg(long)]
        password: String,

        /// IMAP host (default: imap.gmail.com)
        #[arg(long, default_value = "imap.gmail.com")]
        imap_host: String,

        /// SMTP host (default: smtp.gmail.com)
        #[arg(long, default_value = "smtp.gmail.com")]
        smtp_host: String,
    },

    /// Check email connection status
    Status,

    /// Send a test email to self
    Test,
}

#[derive(Subcommand)]
enum SkillAction {
    /// List installed skills
    List,

    /// Search skills by keyword
    Search {
        /// Search query
        query: String,
    },

    /// Install a skill from a directory or git URL
    Install {
        /// Path to skill directory or git URL
        source: String,
    },

    /// Uninstall a skill
    Uninstall {
        /// Skill ID
        skill_id: String,
    },

    /// Enable a skill
    Enable {
        /// Skill ID
        skill_id: String,
    },

    /// Disable a skill
    Disable {
        /// Skill ID
        skill_id: String,
    },

    /// Show skill details
    Info {
        /// Skill ID
        skill_id: String,
    },

    /// Scaffold a new skill
    Create {
        /// Skill name (will be used as directory name)
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    // Load config from ~/.nexmind/ (also injects .env secrets into process env)
    let config = nexmind_config::NexMindConfig::load();

    let parsed = Cli::parse();

    // Resolve: CLI args > config.toml > hardcoded defaults
    let cli = ResolvedCli {
        daemon_addr: parsed
            .daemon_addr
            .unwrap_or_else(|| config.daemon.http_addr()),
        data_dir: parsed.data_dir.unwrap_or_else(|| {
            config
                .paths
                .data_dir_resolved()
                .to_string_lossy()
                .into_owned()
        }),
        command: parsed.command,
    };

    match cli.command {
        Commands::Health => health(&cli.daemon_addr).await?,
        Commands::Chat { message, agent } => {
            if let Some(msg) = message {
                chat_once(&cli.daemon_addr, &agent, &msg).await?;
            } else {
                chat_repl(&cli.daemon_addr, &agent).await?;
            }
        }
        Commands::Agent { action } => match action {
            AgentAction::List => agent_list(&cli.daemon_addr).await?,
            AgentAction::Inspect { id } => agent_inspect(&cli.daemon_addr, &id).await?,
            AgentAction::Create {
                template,
                schedule,
                tz,
            } => agent_create(&cli.data_dir, &template, schedule.as_deref(), &tz)?,
            AgentAction::Run { id, input } => {
                let msg = input.unwrap_or_else(|| "Execute your scheduled task.".into());
                chat_once(&cli.daemon_addr, &id, &msg).await?;
            }
        },
        Commands::Schedule { action } => {
            schedule_command(&cli.data_dir, action)?;
        }
        Commands::Approve { action } => {
            approve_command(&cli.data_dir, action)?;
        }
        Commands::Team { action } => {
            team_command(&cli.data_dir, action)?;
        }
        Commands::Cost { action } => {
            cost_command(&cli.data_dir, action)?;
        }
        Commands::Model { action } => {
            model_command(action).await?;
        }
        Commands::Browser { action } => {
            browser_command(action, &cli.data_dir).await?;
        }
        Commands::Skill { action } => {
            skill_command(&cli.data_dir, action)?;
        }
        Commands::Email { action } => {
            email_command(action)?;
        }
    }

    Ok(())
}

fn connect_error_msg(addr: &str, e: &dyn std::fmt::Display) {
    eprintln!("Cannot connect to daemon at {}: {}", addr, e);
    eprintln!("Is the daemon running? Start it with: nexmind-daemon");
}

async fn health(daemon_addr: &str) -> Result<()> {
    match NexMindClient::connect(daemon_addr.to_string()).await {
        Ok(mut client) => {
            let response = client.get_health(Empty {}).await?;
            let h = response.into_inner();

            println!("NexMind Daemon Health");
            println!("---------------------");
            println!("Status:            {}", h.status);
            println!("Database:          {}", if h.database_ok { "OK" } else { "ERROR" });
            println!("Uptime:            {}s", h.uptime_seconds);
            println!("Version:           {}", h.version);
            println!("Active agents:     {}", h.active_agents);
            println!("Pending approvals: {}", h.pending_approvals);
        }
        Err(e) => {
            connect_error_msg(daemon_addr, &e);
            std::process::exit(1);
        }
    }
    Ok(())
}

async fn chat_once(daemon_addr: &str, agent_id: &str, message: &str) -> Result<()> {
    let mut client = match NexMindClient::connect(daemon_addr.to_string()).await {
        Ok(c) => c,
        Err(e) => {
            connect_error_msg(daemon_addr, &e);
            std::process::exit(1);
        }
    };

    let req = SendMessageRequest {
        workspace_id: "default".into(),
        agent_id: agent_id.into(),
        text: message.into(),
        attachments: vec![],
    };

    let response = client.send_message(req).await?;
    let mut stream = response.into_inner();

    while let Some(event) = stream.next().await {
        match event {
            Ok(chat_event) => {
                handle_chat_event(&chat_event);
            }
            Err(e) => {
                eprintln!("Stream error: {}", e);
                break;
            }
        }
    }

    println!(); // Final newline
    Ok(())
}

async fn chat_repl(daemon_addr: &str, agent_id: &str) -> Result<()> {
    println!("NexMind Chat (type 'exit' or Ctrl-C to quit)");
    println!("Agent: {}", agent_id);
    println!("---");

    let mut rl = rustyline::DefaultEditor::new()?;

    loop {
        let line = match rl.readline("you> ") {
            Ok(line) => line,
            Err(rustyline::error::ReadlineError::Interrupted | rustyline::error::ReadlineError::Eof) => {
                println!("\nGoodbye!");
                break;
            }
            Err(e) => {
                eprintln!("Input error: {}", e);
                break;
            }
        };

        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "exit" || line == "quit" {
            println!("Goodbye!");
            break;
        }

        let _ = rl.add_history_entry(line);

        let mut client = match NexMindClient::connect(daemon_addr.to_string()).await {
            Ok(c) => c,
            Err(e) => {
                connect_error_msg(daemon_addr, &e);
                continue;
            }
        };

        let req = SendMessageRequest {
            workspace_id: "default".into(),
            agent_id: agent_id.into(),
            text: line.to_string(),
            attachments: vec![],
        };

        match client.send_message(req).await {
            Ok(response) => {
                print!("assistant> ");
                let mut stream = response.into_inner();
                while let Some(event) = stream.next().await {
                    match event {
                        Ok(chat_event) => handle_chat_event(&chat_event),
                        Err(e) => {
                            eprintln!("\nStream error: {}", e);
                            break;
                        }
                    }
                }
                println!("\n");
            }
            Err(e) => {
                eprintln!("Error: {}", e);
            }
        }
    }

    Ok(())
}

fn handle_chat_event(event: &ChatEvent) {
    match event.event_type.as_str() {
        "text_delta" => {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&event.data) {
                if let Some(text) = data["text"].as_str() {
                    print!("{}", text);
                    // Flush for streaming effect
                    use std::io::Write;
                    std::io::stdout().flush().ok();
                }
            }
        }
        "done" => {
            // Optionally show metadata
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&event.data) {
                let tokens = data["tokens_used"].as_u64().unwrap_or(0);
                let duration = data["duration_ms"].as_u64().unwrap_or(0);
                if tokens > 0 {
                    eprintln!(
                        "\n  [{} tokens, {}ms]",
                        tokens, duration
                    );
                }
            }
        }
        "error" => {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&event.data) {
                let err = data["error"].as_str().unwrap_or("unknown error");
                eprintln!("\nError: {}", err);
            }
        }
        _ => {}
    }
}

async fn agent_list(daemon_addr: &str) -> Result<()> {
    let mut client = match NexMindClient::connect(daemon_addr.to_string()).await {
        Ok(c) => c,
        Err(e) => {
            connect_error_msg(daemon_addr, &e);
            std::process::exit(1);
        }
    };

    let response = client
        .list_agents(ListAgentsRequest {
            workspace_id: "default".into(),
            status_filter: None,
        })
        .await?;

    let agents = response.into_inner().agents;

    if agents.is_empty() {
        println!("No agents registered.");
        return Ok(());
    }

    println!("{:<25} {:<25} {:<10} {:<8}", "ID", "NAME", "STATUS", "VERSION");
    println!("{}", "-".repeat(68));
    for a in &agents {
        println!("{:<25} {:<25} {:<10} v{}", a.id, a.name, a.status, a.version);
    }
    println!("\n{} agent(s)", agents.len());

    Ok(())
}

async fn agent_inspect(daemon_addr: &str, agent_id: &str) -> Result<()> {
    let mut client = match NexMindClient::connect(daemon_addr.to_string()).await {
        Ok(c) => c,
        Err(e) => {
            connect_error_msg(daemon_addr, &e);
            std::process::exit(1);
        }
    };

    // Use GetAgentStatus to verify agent exists first
    match client
        .get_agent_status(AgentId {
            id: agent_id.into(),
        })
        .await
    {
        Ok(response) => {
            let status = response.into_inner();
            println!("Agent: {}", status.id);
            println!("Status: {}", status.status);
            println!("Total runs: {}", status.total_runs);
            println!("Cost today: {} microdollars", status.cost_microdollars_today);
        }
        Err(e) => {
            eprintln!("Error: {}", e.message());
            std::process::exit(1);
        }
    }

    // Also get the full definition from the agent list
    let response = client
        .list_agents(ListAgentsRequest {
            workspace_id: "default".into(),
            status_filter: None,
        })
        .await?;

    for a in response.into_inner().agents {
        if a.id == agent_id {
            println!("\nAgent Definition:");
            println!("  Name:        {}", a.name);
            println!("  Description: {}", a.description);
            println!("  Version:     {}", a.version);
            println!("  Status:      {}", a.status);
        }
    }

    Ok(())
}

/// Create an agent from a template (direct DB access, no daemon needed).
fn agent_create(
    data_dir: &str,
    template_name: &str,
    schedule: Option<&str>,
    tz: &str,
) -> Result<()> {
    let db_path = format!("{}/nexmind.db", data_dir);
    let db = nexmind_storage::Database::open(&db_path)?;
    db.run_migrations()?;
    let db = Arc::new(db);

    let agent_registry = nexmind_agent_engine::AgentRegistry::new(db.clone());

    let agent = match template_name {
        "morning-briefing" => nexmind_agent_engine::templates::morning_briefing_template("default"),
        _ => {
            eprintln!("Unknown template: {}", template_name);
            eprintln!("Available templates:");
            for (name, desc) in nexmind_agent_engine::templates::available_templates() {
                eprintln!("  {} — {}", name, desc);
            }
            std::process::exit(1);
        }
    };

    // Register the agent
    match agent_registry.create(&agent) {
        Ok(id) => println!("Agent created: {} ({})", agent.name, id),
        Err(e) => {
            // May already exist (INSERT OR REPLACE)
            println!("Agent registered: {} ({})", agent.name, e);
        }
    }

    // Create schedule if requested
    if let Some(cron_expr) = schedule {
        // Validate cron
        if let Err(e) = nexmind_scheduler::validate_cron(cron_expr) {
            eprintln!("Invalid cron expression: {}", e);
            std::process::exit(1);
        }
        if let Err(e) = nexmind_scheduler::validate_timezone(tz) {
            eprintln!("Invalid timezone: {}", e);
            std::process::exit(1);
        }

        let scheduler = nexmind_scheduler::SchedulerImpl::new(db);
        let job = nexmind_scheduler::ScheduledJob {
            id: format!("job_{}", ulid::Ulid::new()),
            name: format!("{} Schedule", agent.name),
            trigger: nexmind_scheduler::Trigger::Cron {
                expression: cron_expr.to_string(),
                timezone: tz.to_string(),
            },
            action: nexmind_scheduler::ScheduledAction::RunAgent {
                agent_id: agent.id.clone(),
                input: None,
            },
            status: nexmind_scheduler::JobStatus::Active,
            last_run_at: None,
            next_run_at: None,
            run_count: 0,
            error_count: 0,
            missed_policy: nexmind_scheduler::MissedPolicy::RunOnce,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            workspace_id: "default".into(),
        };

        match scheduler.register(job) {
            Ok(id) => {
                let next = nexmind_scheduler::next_fire_time(cron_expr, tz)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_else(|_| "unknown".into());
                println!("Schedule created: {} (cron: {}, tz: {})", id, cron_expr, tz);
                println!("Next run at: {}", next);
            }
            Err(e) => {
                eprintln!("Failed to create schedule: {}", e);
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

/// Handle schedule subcommands (direct DB access).
fn schedule_command(data_dir: &str, action: ScheduleAction) -> Result<()> {
    let db_path = format!("{}/nexmind.db", data_dir);
    let db = nexmind_storage::Database::open(&db_path)?;
    db.run_migrations()?;
    let db = Arc::new(db);

    let scheduler = nexmind_scheduler::SchedulerImpl::new(db);

    match action {
        ScheduleAction::List => {
            let jobs = scheduler.list()?;
            if jobs.is_empty() {
                println!("No scheduled jobs.");
                return Ok(());
            }

            println!(
                "{:<20} {:<25} {:<10} {:<8} {:<6}",
                "ID", "NAME", "STATUS", "RUNS", "ERRORS"
            );
            println!("{}", "-".repeat(69));
            for j in &jobs {
                let id_short = if j.id.len() > 18 {
                    format!("{}...", &j.id[..15])
                } else {
                    j.id.clone()
                };
                println!(
                    "{:<20} {:<25} {:<10} {:<8} {:<6}",
                    id_short, j.name, j.status, j.run_count, j.error_count
                );
            }
            println!("\n{} job(s)", jobs.len());
        }

        ScheduleAction::Create {
            agent,
            cron,
            tz,
            name,
            missed_policy,
        } => {
            if let Err(e) = nexmind_scheduler::validate_cron(&cron) {
                eprintln!("Invalid cron expression: {}", e);
                std::process::exit(1);
            }
            if let Err(e) = nexmind_scheduler::validate_timezone(&tz) {
                eprintln!("Invalid timezone: {}", e);
                std::process::exit(1);
            }

            let mp = missed_policy
                .parse::<nexmind_scheduler::MissedPolicy>()
                .unwrap_or(nexmind_scheduler::MissedPolicy::RunOnce);

            let job = nexmind_scheduler::ScheduledJob {
                id: format!("job_{}", ulid::Ulid::new()),
                name,
                trigger: nexmind_scheduler::Trigger::Cron {
                    expression: cron.clone(),
                    timezone: tz.clone(),
                },
                action: nexmind_scheduler::ScheduledAction::RunAgent {
                    agent_id: agent,
                    input: None,
                },
                status: nexmind_scheduler::JobStatus::Active,
                last_run_at: None,
                next_run_at: None,
                run_count: 0,
                error_count: 0,
                missed_policy: mp,
                created_at: chrono::Utc::now().to_rfc3339(),
                updated_at: chrono::Utc::now().to_rfc3339(),
                workspace_id: "default".into(),
            };

            let job_id = scheduler.register(job)?;
            let next = nexmind_scheduler::next_fire_time(&cron, &tz)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|_| "unknown".into());
            println!("Job created: {}", job_id);
            println!("Next run at: {}", next);
        }

        ScheduleAction::Pause { job_id } => {
            scheduler.pause(&job_id)?;
            println!("Job paused: {}", job_id);
        }

        ScheduleAction::Resume { job_id } => {
            scheduler.resume(&job_id)?;
            println!("Job resumed: {}", job_id);
        }

        ScheduleAction::Delete { job_id } => {
            scheduler.delete(&job_id)?;
            println!("Job deleted: {}", job_id);
        }

        ScheduleAction::Trigger { job_id } => {
            // For trigger, we just print info — actual execution requires daemon
            let job = scheduler.get(&job_id)?;
            println!("Job: {} ({})", job.name, job.id);
            println!("To trigger, run the daemon and use the agent directly:");
            match &job.action {
                nexmind_scheduler::ScheduledAction::RunAgent { agent_id, input } => {
                    if let Some(inp) = input {
                        println!("  nexmind agent run {} \"{}\"", agent_id, inp);
                    } else {
                        println!("  nexmind agent run {}", agent_id);
                    }
                }
                nexmind_scheduler::ScheduledAction::RunWorkflow { workflow_id, .. } => {
                    println!("  (workflow {} — not yet supported)", workflow_id);
                }
            }
        }

        ScheduleAction::Inspect { job_id } => {
            let job = scheduler.get(&job_id)?;
            println!("Job: {}", job.id);
            println!("Name: {}", job.name);
            println!("Status: {}", job.status);
            match &job.trigger {
                nexmind_scheduler::Trigger::Cron {
                    expression,
                    timezone,
                } => {
                    println!("Trigger: cron \"{}\" ({})", expression, timezone);
                    if let Ok(next) = nexmind_scheduler::next_fire_time(expression, timezone) {
                        println!("Next run: {}", next.to_rfc3339());
                    }
                }
                nexmind_scheduler::Trigger::Manual => {
                    println!("Trigger: manual");
                }
            }
            match &job.action {
                nexmind_scheduler::ScheduledAction::RunAgent { agent_id, input } => {
                    println!("Action: run_agent {}", agent_id);
                    if let Some(inp) = input {
                        println!("Input: {}", inp);
                    }
                }
                nexmind_scheduler::ScheduledAction::RunWorkflow { workflow_id, .. } => {
                    println!("Action: run_workflow {}", workflow_id);
                }
            }
            println!("Missed policy: {}", job.missed_policy);
            println!("Run count: {}", job.run_count);
            println!("Error count: {}", job.error_count);
            if let Some(ref last) = job.last_run_at {
                println!("Last run: {}", last);
            }
            if let Some(ref next) = job.next_run_at {
                println!("Next run (stored): {}", next);
            }
            println!("Created: {}", job.created_at);
        }
    }

    Ok(())
}

/// Handle model subcommands (direct provider detection, no daemon needed).
async fn model_command(action: ModelAction) -> Result<()> {
    // Build a model router with auto-detected providers
    let mut router = nexmind_model_router::ModelRouter::new();

    // Try Anthropic
    let anthropic_ok = match nexmind_model_router::AnthropicProvider::from_auto_detect() {
        Ok(provider) => {
            router.register_provider(Arc::new(provider));
            true
        }
        Err(_) => false,
    };

    // Try Claude Code
    let claude_code_ok = if let Some(provider) = nexmind_model_router::ClaudeCodeProvider::detect() {
        router.register_provider(Arc::new(provider));
        true
    } else {
        false
    };

    // Try OpenAI
    let openai_ok = match nexmind_model_router::OpenAIProvider::from_env() {
        Ok(provider) => {
            router.register_provider(Arc::new(provider));
            true
        }
        Err(_) => false,
    };

    // Try Ollama
    let ollama_ok = match nexmind_model_router::OllamaProvider::default_local() {
        Ok(provider) => {
            router.register_provider(Arc::new(provider));
            true
        }
        Err(_) => false,
    };

    match action {
        ModelAction::List => {
            println!(
                "{:<15} {:<40} {:<10} {:<6} {:<7} {}",
                "PROVIDER", "MODEL", "STATUS", "TOOLS", "VISION", "COST"
            );
            println!("{}", "-".repeat(90));

            // Show all possible models, marking availability
            let model_entries: Vec<(&str, &str, &str, bool, bool, bool, &str)> = vec![
                ("anthropic", "anthropic/claude-sonnet-4-20250514", "Claude Sonnet 4", true, true, anthropic_ok, "$0.003/$0.015"),
                ("anthropic", "anthropic/claude-opus-4-20250514", "Claude Opus 4", true, true, anthropic_ok, "$0.015/$0.075"),
                ("claude-code", "claude-code/sonnet", "Claude Sonnet (via CLI)", false, false, claude_code_ok, "$0 (subscription)"),
                ("claude-code", "claude-code/opus", "Claude Opus (via CLI)", false, false, claude_code_ok, "$0 (subscription)"),
                ("openai", "openai/gpt-4o", "GPT-4o", true, true, openai_ok, "$0.0025/$0.010"),
                ("openai", "openai/gpt-4o-mini", "GPT-4o Mini", true, true, openai_ok, "$0.00015/$0.0006"),
                ("ollama", "ollama/llama3.2", "Llama 3.2", true, false, ollama_ok, "$0 (local)"),
                ("ollama", "ollama/qwen2.5-coder", "Qwen 2.5 Coder", true, false, ollama_ok, "$0 (local)"),
            ];

            for (provider, model_id, _name, tools, vision, available, cost) in &model_entries {
                let status = if *available { "ready" } else { "no key" };
                let status_icon = if *available { "\u{2713}" } else { "\u{2717}" };
                let tools_icon = if *tools { "\u{2713}" } else { "\u{2717}" };
                let vision_icon = if *vision { "\u{2713}" } else { "\u{2717}" };
                println!(
                    "{:<15} {:<40} {} {:<7} {:<6} {:<7} {}",
                    provider, model_id, status_icon, status, tools_icon, vision_icon, cost
                );
            }

            if router.has_providers() {
                println!("\nDefault model: {}", router.select_default_model());
            } else {
                println!("\nNo providers available. Set up at least one:");
                println!("  1. npm install -g @anthropic-ai/claude-code && claude login");
                println!("  2. Set ANTHROPIC_API_KEY");
                println!("  3. Set OPENAI_API_KEY");
                println!("  4. Install Ollama and run: ollama pull llama3.2");
            }
        }

        ModelAction::Status => {
            println!("Provider Health Status");
            println!("{}", "-".repeat(50));

            let entries: Vec<(&str, bool, &str)> = vec![
                ("anthropic", anthropic_ok, "ANTHROPIC_API_KEY (must start with sk-ant-api)"),
                ("claude-code", claude_code_ok, "Claude Code CLI (npm install -g @anthropic-ai/claude-code)"),
                ("openai", openai_ok, "OPENAI_API_KEY"),
                ("ollama", ollama_ok, "Ollama (http://localhost:11434)"),
            ];

            for (name, ok, hint) in &entries {
                let icon = if *ok { "\u{2713}" } else { "\u{2717}" };
                let status = if *ok { "available" } else { "not configured" };
                println!("{} {:<15} {}", icon, name, status);
                if !ok {
                    println!("  Setup: {}", hint);
                }
            }

            if router.has_providers() {
                println!("\nDefault model: {}", router.select_default_model());
            }
        }

        ModelAction::Test { model_id } => {
            let (provider_id, _) = model_id.split_once('/').unwrap_or(("", &model_id));

            if !router.has_provider(provider_id) {
                eprintln!("Provider '{}' is not available.", provider_id);
                eprintln!("Run 'nexmind model status' to see available providers.");
                std::process::exit(1);
            }

            println!("Testing {}...", model_id);
            let prompt = "Say hello in one word.";
            println!("Prompt: \"{}\"", prompt);

            let req = nexmind_model_router::CompletionRequest {
                model: model_id.clone(),
                messages: vec![nexmind_model_router::ChatMessage::user(prompt)],
                tools: None,
                temperature: 0.0,
                max_tokens: 50,
                stream: false,
            };

            let start = std::time::Instant::now();
            match router.complete(req).await {
                Ok(resp) => {
                    let latency = start.elapsed();
                    let text = resp.message.text().unwrap_or("(no text)");
                    println!("Response: \"{}\"", text);
                    println!("Latency: {:.1}s", latency.as_secs_f64());
                    println!("Status: \u{2713} Working");
                }
                Err(e) => {
                    println!("Error: {}", e);
                    println!("Status: \u{2717} Failed");
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
}

fn parse_cost_period(s: &str) -> nexmind_agent_engine::cost::CostPeriod {
    match s {
        "7d" => nexmind_agent_engine::cost::CostPeriod::Last7Days,
        "30d" => nexmind_agent_engine::cost::CostPeriod::Last30Days,
        "all" => nexmind_agent_engine::cost::CostPeriod::AllTime,
        _ => nexmind_agent_engine::cost::CostPeriod::Today,
    }
}

/// Handle approve subcommands (direct DB access).
fn approve_command(data_dir: &str, action: ApproveAction) -> Result<()> {
    let db_path = format!("{}/nexmind.db", data_dir);
    let db = nexmind_storage::Database::open(&db_path)?;
    db.run_migrations()?;
    let db = Arc::new(db);

    let event_bus = Arc::new(nexmind_event_bus::EventBus::new(64));
    let mgr = nexmind_agent_engine::approval::ApprovalManager::new(db, event_bus);

    match action {
        ApproveAction::List => {
            let pending = mgr.list_pending("default")?;
            if pending.is_empty() {
                println!("No pending approvals.");
                return Ok(());
            }

            println!(
                "{:<20} {:<20} {:<20} {:<10} {}",
                "ID", "AGENT", "TOOL", "RISK", "CREATED"
            );
            println!("{}", "-".repeat(90));
            for r in &pending {
                let id_short = if r.id.len() > 18 {
                    format!("{}...", &r.id[..15])
                } else {
                    r.id.clone()
                };
                println!(
                    "{:<20} {:<20} {:<20} {:<10} {}",
                    id_short, r.requester_agent_id, r.tool_id, r.risk_level, r.created_at
                );
            }
            println!("\n{} pending approval(s)", pending.len());
        }

        ApproveAction::Accept { id, note: _ } => {
            mgr.approve(&id, "cli_user")?;
            println!("Approved: {}", id);
        }

        ApproveAction::Deny { id, reason } => {
            mgr.deny(&id, "cli_user", reason.as_deref())?;
            println!("Denied: {}", id);
        }

        ApproveAction::Inspect { id } => {
            let r = mgr.get(&id)?;
            println!("Approval: {}", r.id);
            println!("Status: {}", r.status);
            println!("Agent: {}", r.requester_agent_id);
            println!("Run: {}", r.requester_run_id);
            println!("Tool: {}", r.tool_id);
            println!("Args: {}", serde_json::to_string_pretty(&r.tool_args)?);
            println!("Description: {}", r.action_description);
            println!("Risk: {}", r.risk_level);
            if let Some(ref by) = r.decided_by {
                println!("Decided by: {}", by);
            }
            if let Some(ref at) = r.decided_at {
                println!("Decided at: {}", at);
            }
            if let Some(ref note) = r.decision_note {
                println!("Note: {}", note);
            }
            println!("Created: {}", r.created_at);
            println!("Expires: {}", r.expires_at);
        }

        ApproveAction::ExpireStale => {
            let count = mgr.expire_stale()?;
            println!("Expired {} stale approval(s)", count);
        }
    }

    Ok(())
}

/// Handle team subcommands (direct DB access).
fn team_command(data_dir: &str, action: TeamAction) -> Result<()> {
    let db_path = format!("{}/nexmind.db", data_dir);
    let db = nexmind_storage::Database::open(&db_path)?;
    db.run_migrations()?;
    let db = Arc::new(db);

    let registry = nexmind_agent_engine::team::TeamRegistry::new(db);

    match action {
        TeamAction::List => {
            let teams = registry.list("default")?;
            if teams.is_empty() {
                println!("No teams registered.");
                return Ok(());
            }

            println!(
                "{:<20} {:<25} {:<15} {:<8}",
                "ID", "NAME", "PATTERN", "MEMBERS"
            );
            println!("{}", "-".repeat(68));
            for t in &teams {
                let id_short = if t.id.len() > 18 {
                    format!("{}...", &t.id[..15])
                } else {
                    t.id.clone()
                };
                println!(
                    "{:<20} {:<25} {:<15} {:<8}",
                    id_short,
                    t.name,
                    format!("{:?}", t.pattern),
                    t.members.len()
                );
            }
            println!("\n{} team(s)", teams.len());
        }

        TeamAction::CreateDemo => {
            let team = nexmind_agent_engine::team::simple_research_team("default");
            let id = registry.create(&team)?;
            println!("Demo team created: {} ({})", team.name, id);
            println!("Pattern: {:?}", team.pattern);
            println!("Members: {}", team.members.len());
            for m in &team.members {
                println!(
                    "  - {} (role: {}, deps: {:?})",
                    m.agent_id, m.role, m.depends_on
                );
            }
        }

        TeamAction::Inspect { id } => {
            let t = registry.get(&id)?;
            println!("Team: {}", t.id);
            println!("Name: {}", t.name);
            println!("Version: {}", t.version);
            if let Some(ref desc) = t.description {
                println!("Description: {}", desc);
            }
            println!("Pattern: {:?}", t.pattern);
            println!("Orchestrator: {}", t.orchestrator_agent_id);
            println!("Failure policy: {:?}", t.failure_policy);
            println!("Members:");
            for m in &t.members {
                println!(
                    "  - {} (role: {}, deps: {:?})",
                    m.agent_id, m.role, m.depends_on
                );
            }
            println!("Created: {}", t.created_at);
        }

        TeamAction::Delete { id } => {
            registry.delete(&id)?;
            println!("Team deleted: {}", id);
        }
    }

    Ok(())
}

/// Handle cost subcommands (direct DB access).
fn cost_command(data_dir: &str, action: CostAction) -> Result<()> {
    let db_path = format!("{}/nexmind.db", data_dir);
    let db = nexmind_storage::Database::open(&db_path)?;
    db.run_migrations()?;
    let db = Arc::new(db);

    let event_bus = Arc::new(nexmind_event_bus::EventBus::new(64));
    let tracker = nexmind_agent_engine::cost::CostTracker::new(db, event_bus);

    match action {
        CostAction::Summary { period } => {
            let p = parse_cost_period(&period);
            let summary = tracker.summary("default", p)?;

            println!("Cost Summary ({})", period);
            println!("{}", "-".repeat(40));
            println!("Total cost:     ${:.6}", summary.total_cost_usd);
            println!("Input tokens:   {}", summary.total_input_tokens);
            println!("Output tokens:  {}", summary.total_output_tokens);
            println!("Total requests: {}", summary.total_requests);

            if !summary.by_model.is_empty() {
                println!("\nBy model:");
                for (model, cost) in &summary.by_model {
                    println!("  {:<40} ${:.6}", model, cost);
                }
            }
            if !summary.by_agent.is_empty() {
                println!("\nBy agent:");
                for (agent, cost) in &summary.by_agent {
                    println!("  {:<40} ${:.6}", agent, cost);
                }
            }
        }

        CostAction::Agent { id, period } => {
            let p = parse_cost_period(&period);
            let summary = tracker.agent_cost(&id, p)?;

            println!("Cost for agent: {} ({})", id, period);
            println!("{}", "-".repeat(40));
            println!("Total cost:     ${:.6}", summary.total_cost_usd);
            println!("Input tokens:   {}", summary.total_input_tokens);
            println!("Output tokens:  {}", summary.total_output_tokens);
            println!("Total requests: {}", summary.total_requests);

            if !summary.by_model.is_empty() {
                println!("\nBy model:");
                for (model, cost) in &summary.by_model {
                    println!("  {:<40} ${:.6}", model, cost);
                }
            }
        }
    }

    Ok(())
}

/// Handle browser subcommands (direct browser control, no daemon needed).
async fn browser_command(action: BrowserAction, data_dir: &str) -> Result<()> {
    use nexmind_tool_runtime::tools::{BrowserConfig, BrowserManager};

    let screenshot_dir = std::path::PathBuf::from(data_dir).join("workspace/screenshots");
    let config = BrowserConfig {
        headless: true,
        screenshot_dir,
        ..Default::default()
    };
    let mut browser = BrowserManager::new(config);

    match action {
        BrowserAction::Open { url } => {
            println!("Navigating to {}...", url);
            let info = browser.navigate(&url).await.map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("Title: {}", info.title);
            println!("URL:   {}", info.url);
        }

        BrowserAction::Screenshot { full_page } => {
            let screenshot = browser.screenshot(full_page).await.map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("Screenshot saved: {}", screenshot.path.display());
            println!("Size: {} bytes ({}x{})", screenshot.size_bytes, screenshot.width, screenshot.height);
        }

        BrowserAction::Text { selector } => {
            let text = browser.extract_text(selector.as_deref()).await.map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("{}", text);
        }

        BrowserAction::Links => {
            let links = browser.extract_links().await.map_err(|e| anyhow::anyhow!("{}", e))?;
            if links.is_empty() {
                println!("No links found.");
            } else {
                for link in &links {
                    println!("{:<60} {}", link.text.chars().take(58).collect::<String>(), link.href);
                }
                println!("\n{} link(s)", links.len());
            }
        }

        BrowserAction::Click { selector } => {
            let new_url = browser.click(&selector).await.map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("Clicked: {}", selector);
            if let Some(url) = new_url {
                println!("New URL: {}", url);
            }
        }

        BrowserAction::Type { selector, text } => {
            browser.type_text(&selector, &text).await.map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("Typed into: {}", selector);
        }

        BrowserAction::Close => {
            browser.close().await.map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("Browser closed.");
        }

        BrowserAction::Js { js } => {
            let result = browser.execute_js(&js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
        }

        BrowserAction::WaitFor { selector, timeout_ms } => {
            println!("Waiting for '{}'...", selector);
            let found = browser
                .wait_for_selector(&selector, timeout_ms)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("Found: {}", found);
        }

        BrowserAction::Scroll { direction, amount, selector } => {
            browser
                .scroll(&direction, amount, selector.as_deref())
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("Scrolled: {}", direction);
        }

        BrowserAction::Back => {
            let info = browser.go_back().await.map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("Title: {}", info.title);
            println!("URL:   {}", info.url);
        }

        BrowserAction::Select { selector, value, by } => {
            browser
                .select_option(&selector, &value, &by)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("Selected '{}' in {}", value, selector);
        }

        BrowserAction::Html { selector } => {
            let html = browser
                .extract_html(selector.as_deref())
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("{}", html);
        }
    }

    // Clean up
    let _ = browser.close().await;
    Ok(())
}

/// Handle skill subcommands (direct file access).
fn skill_command(data_dir: &str, action: SkillAction) -> Result<()> {
    let skills_dir = std::path::PathBuf::from(data_dir).join("skills");
    let registry = nexmind_skill_registry::SkillRegistry::new(skills_dir.clone());

    // Load built-in skills from the bundled directory
    let builtin_dir = std::path::PathBuf::from("skills/builtin");
    if builtin_dir.exists() {
        let _ = load_builtin_skills(&registry, &builtin_dir);
    }

    // Load user-installed skills
    let _ = registry.load_from_dir();

    match action {
        SkillAction::List => {
            let skills = registry.list();
            if skills.is_empty() {
                println!("No skills installed.");
                return Ok(());
            }

            println!(
                "{:<20} {:<25} {:<10} {:<10} {}",
                "ID", "NAME", "VERSION", "STATUS", "TOOLS"
            );
            println!("{}", "-".repeat(80));

            for s in &skills {
                let status = match &s.status {
                    nexmind_skill_registry::SkillStatus::Active => "active",
                    nexmind_skill_registry::SkillStatus::Disabled => "disabled",
                    nexmind_skill_registry::SkillStatus::Error(_) => "error",
                };
                let tools_count = s.manifest.provides.tools.len();
                println!(
                    "{:<20} {:<25} {:<10} {:<10} {}",
                    s.id, s.name, s.version, status, tools_count
                );
            }
            println!("\n{} skill(s) installed", skills.len());
        }

        SkillAction::Search { query } => {
            let results = registry.search(&query);
            if results.is_empty() {
                println!("No skills matching '{}'.", query);
                return Ok(());
            }

            println!("Skills matching '{}':", query);
            for s in &results {
                println!("  {} — {} (v{})", s.id, s.description, s.version);
            }
        }

        SkillAction::Install { source } => {
            let path = std::path::PathBuf::from(&source);
            if path.exists() {
                let id = registry.install_from_dir(&path)?;
                println!("Skill installed: {}", id);
            } else {
                eprintln!("Source not found: {}", source);
                eprintln!("Git URL installation is not yet supported.");
                std::process::exit(1);
            }
        }

        SkillAction::Uninstall { skill_id } => {
            registry.uninstall(&skill_id)?;
            println!("Skill uninstalled: {}", skill_id);
        }

        SkillAction::Enable { skill_id } => {
            registry.set_status(&skill_id, nexmind_skill_registry::SkillStatus::Active)?;
            println!("Skill enabled: {}", skill_id);
        }

        SkillAction::Disable { skill_id } => {
            registry.set_status(&skill_id, nexmind_skill_registry::SkillStatus::Disabled)?;
            println!("Skill disabled: {}", skill_id);
        }

        SkillAction::Info { skill_id } => {
            let skill = registry.get(&skill_id)?;
            println!("Skill: {}", skill.id);
            println!("Name: {}", skill.name);
            println!("Version: {}", skill.version);
            println!("Description: {}", skill.description);
            println!("Author: {}", skill.manifest.author);
            println!("Tags: {}", skill.manifest.tags.join(", "));
            println!("Status: {:?}", skill.status);
            println!("Source: {:?}", skill.source);
            println!("Installed: {}", skill.installed_at);

            if !skill.manifest.provides.tools.is_empty() {
                println!("\nTools:");
                for t in &skill.manifest.provides.tools {
                    println!("  {} — {}", t.id, t.description);
                }
            }

            if !skill.manifest.requires.permissions.is_empty() {
                println!("\nRequired permissions:");
                for p in &skill.manifest.requires.permissions {
                    println!("  {}", p);
                }
            }

            println!("\nRuntime: {:?}", skill.manifest.runtime.runtime_type);
            if let Some(ref entry) = skill.manifest.runtime.entry {
                println!("Entry: {}", entry);
            }
        }

        SkillAction::Create { name } => {
            let skill_dir = std::path::PathBuf::from(&name);
            if skill_dir.exists() {
                eprintln!("Directory already exists: {}", name);
                std::process::exit(1);
            }

            std::fs::create_dir_all(&skill_dir)?;

            let manifest = format!(
                r#"id: {name}
name: "{name}"
version: "1.0.0"
description: "TODO: describe your skill"
author: "user"
tags: []

provides:
  tools:
    - id: {name}_tool
      description: "TODO: describe what this tool does"
      parameters:
        input: {{ type: string, required: true }}

runtime:
  type: script
  interpreter: python3
  entry: main.py
  timeout_seconds: 30
"#
            );

            std::fs::write(skill_dir.join("skill.yaml"), manifest)?;
            std::fs::write(
                skill_dir.join("main.py"),
                r#"import json, sys

args = json.loads(sys.argv[1])
result = {"status": "ok", "input": args.get("input", "")}
print(json.dumps(result))
"#,
            )?;

            println!("Skill scaffolded: {}/", name);
            println!("  skill.yaml — skill manifest");
            println!("  main.py    — entry point");
            println!("\nEdit skill.yaml and main.py, then install:");
            println!("  nexmind skill install {}", name);
        }
    }

    Ok(())
}

/// Load built-in skills from the bundled skills directory.
fn load_builtin_skills(
    registry: &nexmind_skill_registry::SkillRegistry,
    builtin_dir: &std::path::Path,
) -> Result<usize> {
    let mut count = 0;
    if !builtin_dir.exists() {
        return Ok(0);
    }

    for entry in std::fs::read_dir(builtin_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let manifest_path = path.join("skill.yaml");
            if manifest_path.exists() {
                match nexmind_skill_registry::SkillManifest::from_file(&manifest_path) {
                    Ok(manifest) => {
                        match registry.install(
                            manifest,
                            nexmind_skill_registry::SkillSource::Builtin,
                            Some(path),
                        ) {
                            Ok(_) => count += 1,
                            Err(_) => {} // already installed
                        }
                    }
                    Err(e) => {
                        eprintln!("Warning: failed to load {}: {}", manifest_path.display(), e);
                    }
                }
            }
        }
    }

    Ok(count)
}

/// Handle email subcommands.
fn email_command(action: EmailAction) -> Result<()> {
    use nexmind_tool_runtime::tools::email::{EmailConfig, EmailConnector};

    match action {
        EmailAction::Setup { email, password, imap_host, smtp_host } => {
            println!("Testing email connection...");
            let config = EmailConfig {
                email: email.clone(),
                password,
                imap_host,
                imap_port: 993,
                smtp_host,
                smtp_port: 587,
                check_interval_seconds: 300,
            };

            let connector = EmailConnector::new(config);
            match connector.test_connection() {
                Ok(msg) => {
                    println!("{}", msg);
                    println!("\nTo use email with NexMind, set these environment variables:");
                    println!("  NEXMIND_EMAIL={}", email);
                    println!("  NEXMIND_EMAIL_PASSWORD=<your-app-password>");
                }
                Err(e) => {
                    eprintln!("Connection failed: {}", e);
                    std::process::exit(1);
                }
            }
        }

        EmailAction::Status => {
            match EmailConfig::from_env() {
                Some(config) => {
                    println!("Email: {}", config.email);
                    println!("IMAP: {}:{}", config.imap_host, config.imap_port);
                    println!("SMTP: {}:{}", config.smtp_host, config.smtp_port);
                    let connector = EmailConnector::new(config);
                    match connector.test_connection() {
                        Ok(msg) => println!("Status: {}", msg),
                        Err(e) => println!("Status: ERROR - {}", e),
                    }
                }
                None => {
                    println!("Email not configured.");
                    println!("Set NEXMIND_EMAIL and NEXMIND_EMAIL_PASSWORD environment variables.");
                }
            }
        }

        EmailAction::Test => {
            match EmailConfig::from_env() {
                Some(config) => {
                    let to = config.email.clone();
                    let connector = EmailConnector::new(config);
                    match connector.send_email(&to, "NexMind Test Email", "This is a test email from NexMind.") {
                        Ok(msg) => println!("{}", msg),
                        Err(e) => {
                            eprintln!("Failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                None => {
                    eprintln!("Email not configured. Run 'nexmind email setup' first.");
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
}
