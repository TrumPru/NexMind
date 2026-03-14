use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::{info, warn, error};
use ulid::Ulid;

use nexmind_event_bus::{Event, EventBus, EventSource, EventType};
use nexmind_storage::Database;

use crate::definition::BudgetPolicy;
use crate::AgentError;

/// Orchestration pattern for a team.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OrchestrationPattern {
    Sequential,
    FanOutFanIn,
    /// Agents run collaboratively: a supervisor delegates tasks and agents
    /// communicate via mailbox. Supports multi-round conversations.
    Collaborative,
}

impl Default for OrchestrationPattern {
    fn default() -> Self {
        OrchestrationPattern::Sequential
    }
}

/// A member of a team.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub agent_id: String,
    pub role: String,
    pub input_mapping: Option<serde_json::Value>,
    pub output_key: String,
    pub depends_on: Vec<String>,
}

/// Shared context configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedContextConfig {
    pub enabled: bool,
    pub max_tokens: u32,
}

impl Default for SharedContextConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_tokens: 8000,
        }
    }
}

/// Failure handling policy for teams.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TeamFailurePolicy {
    SuspendTeam,
    SkipAndContinue,
    RetryMember { max_retries: u32 },
}

impl Default for TeamFailurePolicy {
    fn default() -> Self {
        TeamFailurePolicy::SuspendTeam
    }
}

/// Full team definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamDefinition {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub description: Option<String>,
    pub workspace_id: String,
    pub pattern: OrchestrationPattern,
    pub orchestrator_agent_id: String,
    pub members: Vec<TeamMember>,
    pub shared_context: SharedContextConfig,
    pub failure_policy: TeamFailurePolicy,
    pub budget: BudgetPolicy,
    pub created_at: String,
    /// Supervisor agent for Collaborative pattern (delegates tasks to members).
    #[serde(default)]
    pub supervisor_agent_id: Option<String>,
    /// Maximum communication rounds for Collaborative pattern.
    #[serde(default)]
    pub max_rounds: Option<u32>,
}

impl Default for TeamDefinition {
    fn default() -> Self {
        Self {
            id: format!("team_{}", Ulid::new()),
            name: String::new(),
            version: 1,
            description: None,
            workspace_id: "default".into(),
            pattern: OrchestrationPattern::default(),
            orchestrator_agent_id: String::new(),
            members: Vec::new(),
            shared_context: SharedContextConfig::default(),
            failure_policy: TeamFailurePolicy::default(),
            budget: BudgetPolicy::default(),
            created_at: chrono::Utc::now().to_rfc3339(),
            supervisor_agent_id: None,
            max_rounds: None,
        }
    }
}

/// Result of a team run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamRunResult {
    pub outputs: HashMap<String, serde_json::Value>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
    pub members_completed: u32,
    pub members_failed: u32,
    pub duration_ms: u64,
}

/// Team registry — CRUD for team definitions in SQLite.
pub struct TeamRegistry {
    db: Arc<Database>,
}

impl TeamRegistry {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn create(&self, def: &TeamDefinition) -> Result<String, AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| AgentError::StorageError(e.to_string()))?;
        let definition_json =
            serde_json::to_string(def).map_err(|e| AgentError::StorageError(e.to_string()))?;

        conn.execute(
            "INSERT OR REPLACE INTO teams (id, workspace_id, definition, version) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![def.id, def.workspace_id, definition_json, def.version],
        )
        .map_err(|e| AgentError::StorageError(e.to_string()))?;

        info!(team_id = %def.id, name = %def.name, "team created");
        Ok(def.id.clone())
    }

    pub fn get(&self, id: &str) -> Result<TeamDefinition, AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        let definition_json: String = conn
            .query_row(
                "SELECT definition FROM teams WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AgentError::NotFound(id.to_string()),
                other => AgentError::StorageError(other.to_string()),
            })?;

        serde_json::from_str(&definition_json)
            .map_err(|e| AgentError::StorageError(format!("failed to parse team definition: {}", e)))
    }

    pub fn list(&self, workspace_id: &str) -> Result<Vec<TeamDefinition>, AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        let mut stmt = conn
            .prepare("SELECT definition FROM teams WHERE workspace_id = ?1 ORDER BY id")
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        let teams = stmt
            .query_map(rusqlite::params![workspace_id], |row| {
                let json: String = row.get(0)?;
                Ok(json)
            })
            .map_err(|e| AgentError::StorageError(e.to_string()))?
            .filter_map(|r| {
                r.ok()
                    .and_then(|json| serde_json::from_str::<TeamDefinition>(&json).ok())
            })
            .collect();

        Ok(teams)
    }

    pub fn delete(&self, id: &str) -> Result<(), AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        let rows = conn
            .execute("DELETE FROM teams WHERE id = ?1", rusqlite::params![id])
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        if rows == 0 {
            return Err(AgentError::NotFound(id.to_string()));
        }

        info!(team_id = %id, "team deleted");
        Ok(())
    }
}

/// Team orchestrator — runs teams using AgentRuntime.
pub struct TeamOrchestrator {
    agent_runtime: Arc<crate::runtime::AgentRuntime>,
    agent_registry: Arc<crate::registry::AgentRegistry>,
    team_registry: Arc<TeamRegistry>,
    event_bus: Arc<EventBus>,
    mailbox_router: Option<Arc<nexmind_agent_comm::MailboxRouter>>,
}

impl TeamOrchestrator {
    pub fn new(
        agent_runtime: Arc<crate::runtime::AgentRuntime>,
        agent_registry: Arc<crate::registry::AgentRegistry>,
        team_registry: Arc<TeamRegistry>,
        event_bus: Arc<EventBus>,
    ) -> Self {
        Self {
            agent_runtime,
            agent_registry,
            team_registry,
            event_bus,
            mailbox_router: None,
        }
    }

    pub fn with_mailbox_router(mut self, mailbox_router: Arc<nexmind_agent_comm::MailboxRouter>) -> Self {
        self.mailbox_router = Some(mailbox_router);
        self
    }

    /// Run a team to completion.
    pub async fn run_team(
        &self,
        team_id: &str,
        input: &str,
        workspace_id: &str,
    ) -> Result<TeamRunResult, AgentError> {
        let team = self.team_registry.get(team_id)?;
        let start = std::time::Instant::now();

        info!(team_id = %team_id, pattern = ?team.pattern, members = team.members.len(), "team run starting");

        self.event_bus.emit(Event::new(
            EventSource::Agent,
            EventType::Custom("TeamRunStarted".into()),
            serde_json::json!({
                "team_id": team_id,
                "pattern": format!("{:?}", team.pattern),
                "members": team.members.len(),
            }),
            workspace_id,
            None,
        ));

        let result = match team.pattern {
            OrchestrationPattern::Sequential => {
                self.run_sequential(&team, input, workspace_id).await
            }
            OrchestrationPattern::FanOutFanIn => {
                self.run_fan_out_fan_in(&team, input, workspace_id).await
            }
            OrchestrationPattern::Collaborative => {
                self.run_collaborative(&team, input, workspace_id).await
            }
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        match &result {
            Ok(r) => {
                let mut r = r.clone();
                r.duration_ms = duration_ms;
                info!(
                    team_id = %team_id,
                    completed = r.members_completed,
                    failed = r.members_failed,
                    duration_ms,
                    "team run completed"
                );

                self.event_bus.emit(Event::new(
                    EventSource::Agent,
                    EventType::Custom("TeamRunCompleted".into()),
                    serde_json::json!({
                        "team_id": team_id,
                        "members_completed": r.members_completed,
                        "members_failed": r.members_failed,
                        "duration_ms": duration_ms,
                    }),
                    workspace_id,
                    None,
                ));

                Ok(r)
            }
            Err(e) => {
                error!(team_id = %team_id, error = %e, "team run failed");
                Err(AgentError::ExecutionError(e.to_string()))
            }
        }
    }

    /// Sequential: run members one after another.
    async fn run_sequential(
        &self,
        team: &TeamDefinition,
        input: &str,
        workspace_id: &str,
    ) -> Result<TeamRunResult, AgentError> {
        let mut outputs: HashMap<String, serde_json::Value> = HashMap::new();
        let mut current_input = input.to_string();
        let mut total_in: u64 = 0;
        let mut total_out: u64 = 0;
        let mut completed: u32 = 0;
        let mut failed: u32 = 0;

        for member in &team.members {
            let member_input = resolve_input(&member.input_mapping, &current_input, &outputs);

            let agent = match self.agent_registry.get(&member.agent_id) {
                Ok(a) => a,
                Err(e) => {
                    warn!(agent_id = %member.agent_id, error = %e, "member agent not found");
                    match &team.failure_policy {
                        TeamFailurePolicy::SuspendTeam => return Err(e),
                        TeamFailurePolicy::SkipAndContinue => {
                            outputs.insert(
                                member.output_key.clone(),
                                serde_json::json!({"error": e.to_string(), "skipped": true}),
                            );
                            failed += 1;
                            continue;
                        }
                        TeamFailurePolicy::RetryMember { .. } => {
                            // No retries for "not found"
                            failed += 1;
                            continue;
                        }
                    }
                }
            };

            let context = crate::runtime::RunContext::new(workspace_id);
            let result = self.agent_runtime.run(&agent, &member_input, context).await;

            match result {
                Ok(run_result) => {
                    total_in += run_result.tokens_used.input_tokens as u64;
                    total_out += run_result.tokens_used.output_tokens as u64;

                    let response = run_result.response.unwrap_or_default();
                    outputs.insert(
                        member.output_key.clone(),
                        serde_json::Value::String(response.clone()),
                    );
                    current_input = response;
                    completed += 1;

                    self.event_bus.emit(Event::new(
                        EventSource::Agent,
                        EventType::Custom("TeamMemberCompleted".into()),
                        serde_json::json!({
                            "agent_id": member.agent_id,
                            "role": member.role,
                            "output_key": member.output_key,
                        }),
                        workspace_id,
                        None,
                    ));
                }
                Err(e) => {
                    warn!(agent_id = %member.agent_id, error = %e, "member agent failed");
                    match &team.failure_policy {
                        TeamFailurePolicy::SuspendTeam => return Err(e),
                        TeamFailurePolicy::SkipAndContinue => {
                            outputs.insert(
                                member.output_key.clone(),
                                serde_json::json!({"error": e.to_string(), "skipped": true}),
                            );
                            failed += 1;
                        }
                        TeamFailurePolicy::RetryMember { max_retries } => {
                            let mut retries = 0;
                            let mut last_err = e;
                            while retries < *max_retries {
                                retries += 1;
                                let ctx = crate::runtime::RunContext::new(workspace_id);
                                match self
                                    .agent_runtime
                                    .run(&agent, &member_input, ctx)
                                    .await
                                {
                                    Ok(run_result) => {
                                        total_in += run_result.tokens_used.input_tokens as u64;
                                        total_out += run_result.tokens_used.output_tokens as u64;
                                        let response = run_result.response.unwrap_or_default();
                                        outputs.insert(
                                            member.output_key.clone(),
                                            serde_json::Value::String(response.clone()),
                                        );
                                        current_input = response;
                                        completed += 1;
                                        break;
                                    }
                                    Err(e) => {
                                        last_err = e;
                                    }
                                }
                            }
                            if retries >= *max_retries {
                                outputs.insert(
                                    member.output_key.clone(),
                                    serde_json::json!({"error": last_err.to_string(), "skipped": true}),
                                );
                                failed += 1;
                            }
                        }
                    }
                }
            }
        }

        Ok(TeamRunResult {
            outputs,
            total_input_tokens: total_in,
            total_output_tokens: total_out,
            total_cost_usd: 0.0, // Will be calculated by cost tracker
            members_completed: completed,
            members_failed: failed,
            duration_ms: 0, // Set by caller
        })
    }

    /// Fan-out / Fan-in: run independent members in parallel, then dependent ones.
    async fn run_fan_out_fan_in(
        &self,
        team: &TeamDefinition,
        input: &str,
        workspace_id: &str,
    ) -> Result<TeamRunResult, AgentError> {
        let levels = topological_sort(&team.members)?;
        let mut outputs: HashMap<String, serde_json::Value> = HashMap::new();
        let mut total_in: u64 = 0;
        let mut total_out: u64 = 0;
        let mut completed: u32 = 0;
        let mut failed: u32 = 0;

        // Max 3 parallel agents per level
        let max_concurrent = 3usize;

        for level in levels {
            // Process members in chunks of max_concurrent
            for chunk in level.chunks(max_concurrent) {
                let mut handles = Vec::new();

                for member in chunk {
                    let member_input =
                        resolve_input(&member.input_mapping, input, &outputs);
                    let agent = match self.agent_registry.get(&member.agent_id) {
                        Ok(a) => a,
                        Err(e) => {
                            match &team.failure_policy {
                                TeamFailurePolicy::SuspendTeam => return Err(e),
                                _ => {
                                    outputs.insert(
                                        member.output_key.clone(),
                                        serde_json::json!({"error": e.to_string(), "skipped": true}),
                                    );
                                    failed += 1;
                                    continue;
                                }
                            }
                        }
                    };

                    let runtime = self.agent_runtime.clone();
                    let ctx = crate::runtime::RunContext::new(workspace_id);
                    let output_key = member.output_key.clone();
                    let agent_id = member.agent_id.clone();

                    let handle = tokio::spawn(async move {
                        let result = runtime.run(&agent, &member_input, ctx).await;
                        (output_key, agent_id, result)
                    });

                    handles.push(handle);
                }

                for handle in handles {
                    match handle.await {
                        Ok((output_key, agent_id, result)) => match result {
                            Ok(run_result) => {
                                total_in += run_result.tokens_used.input_tokens as u64;
                                total_out += run_result.tokens_used.output_tokens as u64;
                                let response = run_result.response.unwrap_or_default();
                                outputs.insert(
                                    output_key,
                                    serde_json::Value::String(response),
                                );
                                completed += 1;
                            }
                            Err(e) => {
                                warn!(agent_id = %agent_id, error = %e, "parallel member failed");
                                match &team.failure_policy {
                                    TeamFailurePolicy::SuspendTeam => {
                                        return Err(e);
                                    }
                                    _ => {
                                        outputs.insert(
                                            output_key,
                                            serde_json::json!({"error": e.to_string(), "skipped": true}),
                                        );
                                        failed += 1;
                                    }
                                }
                            }
                        },
                        Err(e) => {
                            error!(error = %e, "join error in parallel member");
                            failed += 1;
                        }
                    }
                }
            }
        }

        Ok(TeamRunResult {
            outputs,
            total_input_tokens: total_in,
            total_output_tokens: total_out,
            total_cost_usd: 0.0,
            members_completed: completed,
            members_failed: failed,
            duration_ms: 0,
        })
    }

    /// Collaborative: supervisor agent coordinates team members via mailbox communication.
    /// Members can send messages, share files, and delegate tasks to each other.
    async fn run_collaborative(
        &self,
        team: &TeamDefinition,
        input: &str,
        workspace_id: &str,
    ) -> Result<TeamRunResult, AgentError> {
        let mailbox_router = self.mailbox_router.as_ref().ok_or_else(|| {
            AgentError::ExecutionError("mailbox router not configured for collaborative mode".into())
        })?;

        // Determine supervisor
        let supervisor_id = team
            .supervisor_agent_id
            .as_deref()
            .unwrap_or(&team.orchestrator_agent_id);

        // Register all team members in mailbox router
        let mut member_infos = Vec::new();
        for member in &team.members {
            let _mailbox = mailbox_router.register_agent(&member.agent_id).await;
            let agent = self.agent_registry.get(&member.agent_id).ok();
            member_infos.push(nexmind_agent_comm::TeamMemberInfo {
                agent_id: member.agent_id.clone(),
                name: agent
                    .as_ref()
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| member.agent_id.clone()),
                role: member.role.clone(),
                description: agent.and_then(|a| a.description.clone()),
            });
        }

        mailbox_router
            .register_team(&team.id, member_infos.clone())
            .await;

        // Create shared workspace directory
        let shared_dir = std::path::PathBuf::from("./data/workspace/shared").join(&team.id);
        let _ = std::fs::create_dir_all(&shared_dir);

        // Build supervisor's enhanced system prompt
        let mut team_roster = String::from("You are the supervisor of a team. Your team members:\n");
        for info in &member_infos {
            team_roster.push_str(&format!(
                "- **{}** (ID: `{}`): role={}, {}\n",
                info.name,
                info.agent_id,
                info.role,
                info.description.as_deref().unwrap_or("no description"),
            ));
        }
        team_roster.push_str("\nUse these tools to coordinate:\n");
        team_roster.push_str("- `agent_delegate_task` — assign a task to a team member and get their response\n");
        team_roster.push_str("- `agent_send_message` — send a message to a team member\n");
        team_roster.push_str("- `agent_receive_messages` — check for messages from team members\n");
        team_roster.push_str("- `agent_send_file` — share a file with a team member\n");
        team_roster.push_str("- `agent_list_team` — list all team members\n");
        team_roster.push_str("\nBreak down the task, delegate to appropriate members, synthesize their outputs, and provide the final result.");

        // Get supervisor agent definition and augment it
        let mut supervisor = self
            .agent_registry
            .get(supervisor_id)
            .map_err(|e| AgentError::ExecutionError(format!("supervisor not found: {}", e)))?;

        supervisor.system_prompt = format!("{}\n\n{}", supervisor.system_prompt, team_roster);

        // Ensure supervisor has communication tools and permissions
        let comm_tools = [
            "agent_send_message",
            "agent_receive_messages",
            "agent_send_file",
            "agent_list_team",
            "agent_delegate_task",
        ];
        for tool in &comm_tools {
            if !supervisor.tools.contains(&tool.to_string()) {
                supervisor.tools.push(tool.to_string());
            }
        }
        let comm_perms = ["agent:communicate", "agent:delegate"];
        for perm in &comm_perms {
            if !supervisor.permissions.contains(&perm.to_string()) {
                supervisor.permissions.push(perm.to_string());
            }
        }

        // Increase max iterations for collaborative mode
        let max_rounds = team.max_rounds.unwrap_or(20);
        supervisor.execution_policy.max_iterations = max_rounds;

        // Run supervisor agent
        let context = crate::runtime::RunContext::new(workspace_id)
            .with_team(&team.id, "supervisor");

        self.event_bus.emit(Event::new(
            EventSource::Agent,
            EventType::Custom("CollaborativeRunStarted".into()),
            serde_json::json!({
                "team_id": team.id,
                "supervisor_id": supervisor_id,
                "members": member_infos.len(),
            }),
            workspace_id,
            None,
        ));

        let result = self.agent_runtime.run(&supervisor, input, context).await;

        // Cleanup: unregister all agents and team
        for member in &team.members {
            mailbox_router.unregister_agent(&member.agent_id).await;
        }
        mailbox_router.unregister_team(&team.id).await;

        match result {
            Ok(run_result) => {
                let mut outputs = HashMap::new();
                if let Some(response) = &run_result.response {
                    outputs.insert(
                        "result".to_string(),
                        serde_json::Value::String(response.clone()),
                    );
                }

                Ok(TeamRunResult {
                    outputs,
                    total_input_tokens: run_result.tokens_used.input_tokens as u64,
                    total_output_tokens: run_result.tokens_used.output_tokens as u64,
                    total_cost_usd: 0.0,
                    members_completed: team.members.len() as u32,
                    members_failed: 0,
                    duration_ms: run_result.duration_ms,
                })
            }
            Err(e) => Err(AgentError::ExecutionError(format!(
                "collaborative run failed: {}",
                e
            ))),
        }
    }
}

/// Resolve {{variable}} references in input mappings.
fn resolve_input(
    mapping: &Option<serde_json::Value>,
    raw_input: &str,
    outputs: &HashMap<String, serde_json::Value>,
) -> String {
    match mapping {
        Some(serde_json::Value::Object(map)) => {
            if let Some(template) = map.get("template").and_then(|v| v.as_str()) {
                let mut result = template.to_string();
                for (key, value) in outputs {
                    let placeholder = format!("{{{{{}}}}}", key);
                    let replacement = match value {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    result = result.replace(&placeholder, &replacement);
                }
                result = result.replace("{{input}}", raw_input);
                result
            } else {
                raw_input.to_string()
            }
        }
        _ => raw_input.to_string(),
    }
}

/// Topological sort of team members by depends_on.
/// Returns levels: members at level 0 have no deps, level 1 depends on level 0, etc.
/// Detects circular dependencies.
pub fn topological_sort(members: &[TeamMember]) -> Result<Vec<Vec<&TeamMember>>, AgentError> {
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut member_map: HashMap<&str, &TeamMember> = HashMap::new();

    for m in members {
        in_degree.entry(m.agent_id.as_str()).or_insert(0);
        adj.entry(m.agent_id.as_str()).or_default();
        member_map.insert(m.agent_id.as_str(), m);
    }

    for m in members {
        for dep in &m.depends_on {
            adj.entry(dep.as_str()).or_default().push(&m.agent_id);
            *in_degree.entry(m.agent_id.as_str()).or_insert(0) += 1;
        }
    }

    let mut levels: Vec<Vec<&TeamMember>> = Vec::new();
    let mut queue: Vec<&str> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&id, _)| id)
        .collect();

    let mut processed = 0;

    while !queue.is_empty() {
        let mut level = Vec::new();
        let mut next_queue = Vec::new();

        for &node in &queue {
            if let Some(&member) = member_map.get(node) {
                level.push(member);
            }
            processed += 1;

            if let Some(neighbors) = adj.get(node) {
                for &neighbor in neighbors {
                    if let Some(deg) = in_degree.get_mut(neighbor) {
                        *deg -= 1;
                        if *deg == 0 {
                            next_queue.push(neighbor);
                        }
                    }
                }
            }
        }

        if !level.is_empty() {
            levels.push(level);
        }
        queue = next_queue;
    }

    if processed != members.len() {
        return Err(AgentError::ExecutionError(
            "circular dependency detected in team members".into(),
        ));
    }

    Ok(levels)
}

/// Create a simple 2-agent research team demo.
pub fn simple_research_team(workspace_id: &str) -> TeamDefinition {
    TeamDefinition {
        id: "team_simple_research".into(),
        name: "Simple Research Team".into(),
        version: 1,
        description: Some("A 2-agent team: researcher gathers data, writer produces a report.".into()),
        workspace_id: workspace_id.into(),
        pattern: OrchestrationPattern::Sequential,
        orchestrator_agent_id: "agt_researcher".into(),
        supervisor_agent_id: None,
        max_rounds: None,
        members: vec![
            TeamMember {
                agent_id: "agt_researcher".into(),
                role: "researcher".into(),
                input_mapping: None,
                output_key: "research".into(),
                depends_on: vec![],
            },
            TeamMember {
                agent_id: "agt_writer".into(),
                role: "writer".into(),
                input_mapping: Some(serde_json::json!({
                    "template": "Write a clear, well-structured report based on the following research data:\n\n{{research}}"
                })),
                output_key: "report".into(),
                depends_on: vec!["agt_researcher".into()],
            },
        ],
        shared_context: SharedContextConfig::default(),
        failure_policy: TeamFailurePolicy::SuspendTeam,
        budget: BudgetPolicy {
            max_tokens_per_run: 100_000,
            max_cost_per_run_usd: 5.00,
            max_cost_per_day_usd: 20.00,
        },
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

/// Create the researcher agent definition.
pub fn researcher_agent(workspace_id: &str) -> crate::definition::AgentDefinition {
    crate::definition::AgentDefinition {
        id: "agt_researcher".into(),
        name: "Research Agent".into(),
        version: 1,
        description: Some("Researches topics using web fetching. Gathers facts, data, and sources.".into()),
        system_prompt: "You are a research agent. Research the given topic using http_fetch. Gather facts, data, and sources. Output a structured research summary with key findings, relevant data points, and source URLs.".into(),
        model: crate::definition::ModelConfig::default(),
        tools: vec!["http_fetch".into(), "memory_read".into(), "memory_write".into()],
        memory_policy: crate::definition::MemoryPolicy::default(),
        execution_policy: crate::definition::ExecutionPolicy::default(),
        budget: BudgetPolicy::default(),
        trust_level: crate::definition::TrustLevel::Standard,
        permissions: vec![
            "network:outbound".into(),
            "memory:read:workspace".into(),
            "memory:write:workspace".into(),
        ],
        schedule: None,
        tags: vec!["research".into(), "team".into()],
        workspace_id: workspace_id.into(),
    }
}

/// Create the writer agent definition.
pub fn writer_agent(workspace_id: &str) -> crate::definition::AgentDefinition {
    crate::definition::AgentDefinition {
        id: "agt_writer".into(),
        name: "Writer Agent".into(),
        version: 1,
        description: Some("Writes clear, well-structured reports from research data.".into()),
        system_prompt: "You are a professional writer agent. Write a clear, well-structured report based on the provided research data. Include an executive summary, key findings, analysis, and conclusion.".into(),
        model: crate::definition::ModelConfig::default(),
        tools: vec!["memory_read".into()],
        memory_policy: crate::definition::MemoryPolicy::default(),
        execution_policy: crate::definition::ExecutionPolicy::default(),
        budget: BudgetPolicy::default(),
        trust_level: crate::definition::TrustLevel::Standard,
        permissions: vec!["memory:read:workspace".into()],
        schedule: None,
        tags: vec!["writer".into(), "team".into()],
        workspace_id: workspace_id.into(),
    }
}

// ── Collaborative team presets ──────────────────────────────────

/// Common tools and permissions for collaborative agents.
fn collaborative_tools() -> Vec<String> {
    vec![
        "agent_send_message".into(),
        "agent_receive_messages".into(),
        "agent_send_file".into(),
        "agent_list_team".into(),
        "memory_read".into(),
        "memory_write".into(),
        "fs_read".into(),
        "fs_write".into(),
    ]
}

fn collaborative_permissions() -> Vec<String> {
    vec![
        "agent:communicate".into(),
        "memory:read:workspace".into(),
        "memory:write:workspace".into(),
        "fs:read".into(),
        "fs:write".into(),
    ]
}

/// Create a coding team: planner + coder + reviewer.
pub fn coding_team(workspace_id: &str) -> TeamDefinition {
    TeamDefinition {
        id: "team_coding".into(),
        name: "Coding Team".into(),
        version: 1,
        description: Some("A 3-agent collaborative team: planner designs the approach, coder implements, reviewer validates.".into()),
        workspace_id: workspace_id.into(),
        pattern: OrchestrationPattern::Collaborative,
        orchestrator_agent_id: "agt_planner".into(),
        supervisor_agent_id: Some("agt_planner".into()),
        max_rounds: Some(15),
        members: vec![
            TeamMember {
                agent_id: "agt_planner".into(),
                role: "planner".into(),
                input_mapping: None,
                output_key: "plan".into(),
                depends_on: vec![],
            },
            TeamMember {
                agent_id: "agt_coder".into(),
                role: "coder".into(),
                input_mapping: None,
                output_key: "code".into(),
                depends_on: vec![],
            },
            TeamMember {
                agent_id: "agt_reviewer".into(),
                role: "reviewer".into(),
                input_mapping: None,
                output_key: "review".into(),
                depends_on: vec![],
            },
        ],
        shared_context: SharedContextConfig::default(),
        failure_policy: TeamFailurePolicy::SuspendTeam,
        budget: BudgetPolicy {
            max_tokens_per_run: 200_000,
            max_cost_per_run_usd: 10.00,
            max_cost_per_day_usd: 50.00,
        },
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

/// Create the planner agent definition.
pub fn planner_agent(workspace_id: &str) -> crate::definition::AgentDefinition {
    let mut tools = collaborative_tools();
    tools.push("agent_delegate_task".into());
    tools.push("http_fetch".into());
    let mut perms = collaborative_permissions();
    perms.push("agent:delegate".into());
    perms.push("network:outbound".into());

    crate::definition::AgentDefinition {
        id: "agt_planner".into(),
        name: "Planner Agent".into(),
        version: 1,
        description: Some("Plans coding tasks, designs architecture, and coordinates team members.".into()),
        system_prompt: "You are a planning agent and team supervisor. Break down coding tasks into clear steps. Delegate implementation to the coder agent and review to the reviewer agent. Synthesize results into a final deliverable.".into(),
        model: crate::definition::ModelConfig::default(),
        tools,
        memory_policy: crate::definition::MemoryPolicy::default(),
        execution_policy: crate::definition::ExecutionPolicy {
            max_iterations: 20,
            ..crate::definition::ExecutionPolicy::default()
        },
        budget: BudgetPolicy {
            max_tokens_per_run: 100_000,
            max_cost_per_run_usd: 5.0,
            max_cost_per_day_usd: 25.0,
        },
        trust_level: crate::definition::TrustLevel::Standard,
        permissions: perms,
        schedule: None,
        tags: vec!["planner".into(), "team".into(), "collaborative".into()],
        workspace_id: workspace_id.into(),
    }
}

/// Create the coder agent definition.
pub fn coder_agent(workspace_id: &str) -> crate::definition::AgentDefinition {
    let mut tools = collaborative_tools();
    tools.push("http_fetch".into());

    let mut perms = collaborative_permissions();
    perms.push("network:outbound".into());

    crate::definition::AgentDefinition {
        id: "agt_coder".into(),
        name: "Coder Agent".into(),
        version: 1,
        description: Some("Implements code based on plans. Writes clean, tested code.".into()),
        system_prompt: "You are a coding agent. Implement code based on the task or plan provided. Write clean, well-structured code. Save files to the workspace using fs_write. Communicate progress and results to other agents using agent_send_message.".into(),
        model: crate::definition::ModelConfig::default(),
        tools,
        memory_policy: crate::definition::MemoryPolicy::default(),
        execution_policy: crate::definition::ExecutionPolicy::default(),
        budget: BudgetPolicy::default(),
        trust_level: crate::definition::TrustLevel::Standard,
        permissions: perms,
        schedule: None,
        tags: vec!["coder".into(), "team".into(), "collaborative".into()],
        workspace_id: workspace_id.into(),
    }
}

/// Create the reviewer agent definition.
pub fn reviewer_agent(workspace_id: &str) -> crate::definition::AgentDefinition {
    crate::definition::AgentDefinition {
        id: "agt_reviewer".into(),
        name: "Reviewer Agent".into(),
        version: 1,
        description: Some("Reviews code for quality, bugs, and best practices.".into()),
        system_prompt: "You are a code review agent. Review code for correctness, quality, bugs, security issues, and best practices. Provide constructive feedback. Read files with fs_read and communicate findings via agent_send_message.".into(),
        model: crate::definition::ModelConfig::default(),
        tools: collaborative_tools(),
        memory_policy: crate::definition::MemoryPolicy::default(),
        execution_policy: crate::definition::ExecutionPolicy::default(),
        budget: BudgetPolicy::default(),
        trust_level: crate::definition::TrustLevel::Standard,
        permissions: collaborative_permissions(),
        schedule: None,
        tags: vec!["reviewer".into(), "team".into(), "collaborative".into()],
        workspace_id: workspace_id.into(),
    }
}

/// Create an analysis team: researcher + analyst + reporter.
pub fn analysis_team(workspace_id: &str) -> TeamDefinition {
    TeamDefinition {
        id: "team_analysis".into(),
        name: "Analysis Team".into(),
        version: 1,
        description: Some("A 3-agent collaborative team: researcher gathers data, analyst processes it, reporter writes findings.".into()),
        workspace_id: workspace_id.into(),
        pattern: OrchestrationPattern::Collaborative,
        orchestrator_agent_id: "agt_analyst".into(),
        supervisor_agent_id: Some("agt_analyst".into()),
        max_rounds: Some(15),
        members: vec![
            TeamMember {
                agent_id: "agt_data_researcher".into(),
                role: "researcher".into(),
                input_mapping: None,
                output_key: "research_data".into(),
                depends_on: vec![],
            },
            TeamMember {
                agent_id: "agt_analyst".into(),
                role: "analyst".into(),
                input_mapping: None,
                output_key: "analysis".into(),
                depends_on: vec![],
            },
            TeamMember {
                agent_id: "agt_reporter".into(),
                role: "reporter".into(),
                input_mapping: None,
                output_key: "report".into(),
                depends_on: vec![],
            },
        ],
        shared_context: SharedContextConfig::default(),
        failure_policy: TeamFailurePolicy::SuspendTeam,
        budget: BudgetPolicy {
            max_tokens_per_run: 200_000,
            max_cost_per_run_usd: 10.00,
            max_cost_per_day_usd: 50.00,
        },
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

/// Create the data researcher agent definition.
pub fn data_researcher_agent(workspace_id: &str) -> crate::definition::AgentDefinition {
    let mut tools = collaborative_tools();
    tools.push("http_fetch".into());

    let mut perms = collaborative_permissions();
    perms.push("network:outbound".into());

    crate::definition::AgentDefinition {
        id: "agt_data_researcher".into(),
        name: "Data Researcher".into(),
        version: 1,
        description: Some("Researches topics, gathers data from web and files.".into()),
        system_prompt: "You are a data research agent. Gather information from web sources using http_fetch and from local files using fs_read. Save collected data to workspace files. Share findings with other agents via agent_send_message and agent_send_file.".into(),
        model: crate::definition::ModelConfig::default(),
        tools,
        memory_policy: crate::definition::MemoryPolicy::default(),
        execution_policy: crate::definition::ExecutionPolicy::default(),
        budget: BudgetPolicy::default(),
        trust_level: crate::definition::TrustLevel::Standard,
        permissions: perms,
        schedule: None,
        tags: vec!["researcher".into(), "team".into(), "collaborative".into()],
        workspace_id: workspace_id.into(),
    }
}

/// Create the analyst agent definition.
pub fn analyst_agent(workspace_id: &str) -> crate::definition::AgentDefinition {
    let mut tools = collaborative_tools();
    tools.push("agent_delegate_task".into());

    let mut perms = collaborative_permissions();
    perms.push("agent:delegate".into());

    crate::definition::AgentDefinition {
        id: "agt_analyst".into(),
        name: "Analyst Agent".into(),
        version: 1,
        description: Some("Analyzes data, identifies patterns and insights.".into()),
        system_prompt: "You are an analyst agent and team supervisor. Coordinate the research and reporting. Delegate data gathering to the researcher and report writing to the reporter. Analyze data for patterns, trends, and insights. Provide the final synthesized analysis.".into(),
        model: crate::definition::ModelConfig::default(),
        tools,
        memory_policy: crate::definition::MemoryPolicy::default(),
        execution_policy: crate::definition::ExecutionPolicy {
            max_iterations: 20,
            ..crate::definition::ExecutionPolicy::default()
        },
        budget: BudgetPolicy {
            max_tokens_per_run: 100_000,
            max_cost_per_run_usd: 5.0,
            max_cost_per_day_usd: 25.0,
        },
        trust_level: crate::definition::TrustLevel::Standard,
        permissions: perms,
        schedule: None,
        tags: vec!["analyst".into(), "team".into(), "collaborative".into()],
        workspace_id: workspace_id.into(),
    }
}

/// Create the reporter agent definition.
pub fn reporter_agent(workspace_id: &str) -> crate::definition::AgentDefinition {
    crate::definition::AgentDefinition {
        id: "agt_reporter".into(),
        name: "Reporter Agent".into(),
        version: 1,
        description: Some("Writes polished reports and documents from analysis results.".into()),
        system_prompt: "You are a reporter agent. Write polished, well-structured reports based on analysis data provided by other agents. Save reports to workspace files and share them via agent_send_file.".into(),
        model: crate::definition::ModelConfig::default(),
        tools: collaborative_tools(),
        memory_policy: crate::definition::MemoryPolicy::default(),
        execution_policy: crate::definition::ExecutionPolicy::default(),
        budget: BudgetPolicy::default(),
        trust_level: crate::definition::TrustLevel::Standard,
        permissions: collaborative_permissions(),
        schedule: None,
        tags: vec!["reporter".into(), "team".into(), "collaborative".into()],
        workspace_id: workspace_id.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, TeamRegistry) {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let db = Arc::new(db);
        let registry = TeamRegistry::new(db.clone());
        (db, registry)
    }

    #[test]
    fn test_team_create_and_get() {
        let (_db, registry) = setup();
        let team = simple_research_team("default");

        let id = registry.create(&team).unwrap();
        assert_eq!(id, "team_simple_research");

        let retrieved = registry.get("team_simple_research").unwrap();
        assert_eq!(retrieved.name, "Simple Research Team");
        assert_eq!(retrieved.members.len(), 2);
        assert_eq!(retrieved.pattern, OrchestrationPattern::Sequential);
    }

    #[test]
    fn test_team_list() {
        let (_db, registry) = setup();
        let team = simple_research_team("ws1");
        registry.create(&team).unwrap();

        let teams = registry.list("ws1").unwrap();
        assert_eq!(teams.len(), 1);
        assert_eq!(teams[0].id, "team_simple_research");

        let empty = registry.list("ws_other").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_team_delete() {
        let (_db, registry) = setup();
        let team = simple_research_team("default");
        registry.create(&team).unwrap();

        registry.delete("team_simple_research").unwrap();

        let result = registry.get("team_simple_research");
        assert!(result.is_err());
    }

    #[test]
    fn test_team_delete_not_found() {
        let (_db, registry) = setup();
        let result = registry.delete("nonexistent");
        assert!(matches!(result, Err(AgentError::NotFound(_))));
    }

    #[test]
    fn test_topological_sort_simple() {
        let members = vec![
            TeamMember {
                agent_id: "a".into(),
                role: "first".into(),
                input_mapping: None,
                output_key: "out_a".into(),
                depends_on: vec![],
            },
            TeamMember {
                agent_id: "b".into(),
                role: "second".into(),
                input_mapping: None,
                output_key: "out_b".into(),
                depends_on: vec!["a".into()],
            },
        ];

        let levels = topological_sort(&members).unwrap();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].len(), 1);
        assert_eq!(levels[0][0].agent_id, "a");
        assert_eq!(levels[1].len(), 1);
        assert_eq!(levels[1][0].agent_id, "b");
    }

    #[test]
    fn test_topological_sort_parallel() {
        let members = vec![
            TeamMember {
                agent_id: "a".into(),
                role: "r1".into(),
                input_mapping: None,
                output_key: "out_a".into(),
                depends_on: vec![],
            },
            TeamMember {
                agent_id: "b".into(),
                role: "r2".into(),
                input_mapping: None,
                output_key: "out_b".into(),
                depends_on: vec![],
            },
            TeamMember {
                agent_id: "c".into(),
                role: "merger".into(),
                input_mapping: None,
                output_key: "out_c".into(),
                depends_on: vec!["a".into(), "b".into()],
            },
        ];

        let levels = topological_sort(&members).unwrap();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].len(), 2); // a and b in parallel
        assert_eq!(levels[1].len(), 1); // c depends on both
        assert_eq!(levels[1][0].agent_id, "c");
    }

    #[test]
    fn test_topological_sort_circular_dependency() {
        let members = vec![
            TeamMember {
                agent_id: "a".into(),
                role: "r1".into(),
                input_mapping: None,
                output_key: "out_a".into(),
                depends_on: vec!["b".into()],
            },
            TeamMember {
                agent_id: "b".into(),
                role: "r2".into(),
                input_mapping: None,
                output_key: "out_b".into(),
                depends_on: vec!["a".into()],
            },
        ];

        let result = topological_sort(&members);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("circular dependency"));
    }

    #[test]
    fn test_resolve_input_no_mapping() {
        let outputs = HashMap::new();
        let result = resolve_input(&None, "raw input", &outputs);
        assert_eq!(result, "raw input");
    }

    #[test]
    fn test_resolve_input_with_template() {
        let mut outputs = HashMap::new();
        outputs.insert(
            "research".into(),
            serde_json::Value::String("EV data here".into()),
        );

        let mapping = Some(serde_json::json!({
            "template": "Write a report based on:\n\n{{research}}"
        }));

        let result = resolve_input(&mapping, "original input", &outputs);
        assert!(result.contains("EV data here"));
        assert!(result.contains("Write a report based on:"));
    }

    #[test]
    fn test_simple_research_team_definition() {
        let team = simple_research_team("ws1");
        assert_eq!(team.id, "team_simple_research");
        assert_eq!(team.members.len(), 2);
        assert_eq!(team.members[0].role, "researcher");
        assert_eq!(team.members[1].role, "writer");
        assert!(team.members[1].input_mapping.is_some());
    }

    #[test]
    fn test_team_run_result_serialization() {
        let result = TeamRunResult {
            outputs: HashMap::from([
                ("research".into(), serde_json::json!("data")),
                ("report".into(), serde_json::json!("final report")),
            ]),
            total_input_tokens: 5000,
            total_output_tokens: 2000,
            total_cost_usd: 0.05,
            members_completed: 2,
            members_failed: 0,
            duration_ms: 3000,
        };

        let json = serde_json::to_string(&result).unwrap();
        let de: TeamRunResult = serde_json::from_str(&json).unwrap();
        assert_eq!(de.members_completed, 2);
        assert_eq!(de.total_input_tokens, 5000);
    }
}
