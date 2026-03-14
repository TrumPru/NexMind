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
        }
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
