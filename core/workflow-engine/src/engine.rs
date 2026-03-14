use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::{NodeType, WorkflowDefinition, WorkflowEngine, WorkflowError, WorkflowExecutor, WorkflowNode};

/// A DAG-based workflow execution engine.
///
/// Stores workflow definitions in memory, validates that they form acyclic
/// graphs, and executes nodes in topological order while propagating outputs
/// between nodes.
pub struct DagWorkflowEngine {
    workflows: HashMap<String, WorkflowDefinition>,
    executor: Option<std::sync::Arc<dyn WorkflowExecutor>>,
}

impl DagWorkflowEngine {
    /// Create an empty engine.
    pub fn new() -> Self {
        Self {
            workflows: HashMap::new(),
            executor: None,
        }
    }

    /// Create an engine with a real executor for tool/agent nodes.
    pub fn with_executor(executor: std::sync::Arc<dyn WorkflowExecutor>) -> Self {
        Self {
            workflows: HashMap::new(),
            executor: Some(executor),
        }
    }

    /// Register a workflow definition. Overwrites any existing definition with
    /// the same id.
    pub fn register(&mut self, definition: WorkflowDefinition) {
        self.workflows.insert(definition.id.clone(), definition);
    }

    /// Remove a workflow definition by id.
    pub fn remove(&mut self, workflow_id: &str) -> Option<WorkflowDefinition> {
        self.workflows.remove(workflow_id)
    }

    /// Return a reference to a stored definition, if any.
    pub fn get(&self, workflow_id: &str) -> Option<&WorkflowDefinition> {
        self.workflows.get(workflow_id)
    }

    // ── DAG validation ───────────────────────────────────────────────

    /// Compute a topological ordering of the workflow nodes using Kahn's
    /// algorithm.  Returns `Err(CycleDetected)` if the graph contains a cycle.
    fn topological_sort(def: &WorkflowDefinition) -> Result<Vec<String>, WorkflowError> {
        // Build adjacency list and in-degree map.
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

        for node in &def.nodes {
            in_degree.entry(node.id.as_str()).or_insert(0);
            adj.entry(node.id.as_str()).or_default();
        }

        for edge in &def.edges {
            adj.entry(edge.from.as_str()).or_default().push(edge.to.as_str());
            *in_degree.entry(edge.to.as_str()).or_insert(0) += 1;
        }

        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut order: Vec<String> = Vec::with_capacity(def.nodes.len());

        while let Some(node_id) = queue.pop_front() {
            order.push(node_id.to_owned());
            if let Some(neighbors) = adj.get(node_id) {
                for &neighbor in neighbors {
                    let deg = in_degree.get_mut(neighbor).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(neighbor);
                    }
                }
            }
        }

        if order.len() != def.nodes.len() {
            return Err(WorkflowError::CycleDetected);
        }

        Ok(order)
    }

    // ── Edge condition evaluation ────────────────────────────────────

    /// Evaluate whether a conditional edge should be traversed.
    ///
    /// The condition string is interpreted as a simple `contains:<value>`
    /// check against the serialised output of the source node.  If no
    /// condition is present the edge is always traversed.
    fn should_traverse_edge(
        condition: &Option<String>,
        source_output: Option<&Value>,
    ) -> bool {
        let cond = match condition {
            Some(c) => c,
            None => return true,
        };

        let output = match source_output {
            Some(v) => v,
            None => return false,
        };

        // Support "contains:<substring>" conditions.
        if let Some(needle) = cond.strip_prefix("contains:") {
            let haystack = match output {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            return haystack.contains(needle);
        }

        // Support "equals:<value>" conditions.
        if let Some(expected) = cond.strip_prefix("equals:") {
            let actual = match output {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            return actual == expected;
        }

        // Support "truthy" – the output must be a JSON bool true or non-empty
        // string.
        if cond == "truthy" {
            return match output {
                Value::Bool(b) => *b,
                Value::String(s) => !s.is_empty(),
                Value::Null => false,
                _ => true,
            };
        }

        // Unknown condition format – default to traversing.
        warn!(condition = %cond, "unknown edge condition format, defaulting to traverse");
        true
    }

    // ── Per-node execution ───────────────────────────────────────────

    /// Execute a single workflow node and return its output value.
    async fn execute_node(
        &self,
        node: &WorkflowNode,
        node_outputs: &HashMap<String, Value>,
    ) -> Result<Value, WorkflowError> {
        match node.node_type {
            NodeType::Tool => self.execute_tool_node(node).await,
            NodeType::Agent => self.execute_agent_node(node).await,
            NodeType::Condition => Self::execute_condition_node(node, node_outputs).await,
            NodeType::Timer => Self::execute_timer_node(node).await,
            NodeType::Transform => Self::execute_transform_node(node, node_outputs).await,
            NodeType::Approval => Self::execute_approval_node(node).await,
        }
    }

    /// Tool node: execute via the WorkflowExecutor if available, otherwise placeholder.
    async fn execute_tool_node(&self, node: &WorkflowNode) -> Result<Value, WorkflowError> {
        let tool_name = node.config.get("tool_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown_tool");
        let arguments = node.config.get("arguments")
            .cloned()
            .unwrap_or(Value::Null);
        let workspace_id = node.config.get("workspace_id")
            .and_then(|v| v.as_str())
            .unwrap_or("default");

        if let Some(ref executor) = self.executor {
            info!(node_id = %node.id, tool = %tool_name, "executing tool node");
            let result = executor
                .execute_tool(tool_name, &arguments, workspace_id)
                .await
                .map_err(|e| WorkflowError::ExecutionError(format!("tool '{}': {}", tool_name, e)))?;
            Ok(json!({
                "status": "completed",
                "node_id": node.id,
                "tool_name": tool_name,
                "result": result,
            }))
        } else {
            info!(node_id = %node.id, tool = %tool_name, "executing tool node (no executor)");
            Ok(json!({
                "status": "completed",
                "node_id": node.id,
                "tool_name": tool_name,
                "arguments": arguments,
                "result": format!("placeholder result from tool '{}'", tool_name),
            }))
        }
    }

    /// Agent node: execute via the WorkflowExecutor if available, otherwise placeholder.
    async fn execute_agent_node(&self, node: &WorkflowNode) -> Result<Value, WorkflowError> {
        let agent_id = node.config.get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown_agent");
        let input = node.config.get("input")
            .and_then(|v| v.as_str())
            .unwrap_or("Execute your task.");
        let workspace_id = node.config.get("workspace_id")
            .and_then(|v| v.as_str())
            .unwrap_or("default");

        if let Some(ref executor) = self.executor {
            info!(node_id = %node.id, agent = %agent_id, "executing agent node");
            let result = executor
                .execute_agent(agent_id, input, workspace_id)
                .await
                .map_err(|e| WorkflowError::ExecutionError(format!("agent '{}': {}", agent_id, e)))?;
            Ok(json!({
                "status": "completed",
                "node_id": node.id,
                "agent_id": agent_id,
                "result": result,
            }))
        } else {
            info!(node_id = %node.id, agent = %agent_id, "executing agent node (no executor)");
            Ok(json!({
                "status": "completed",
                "node_id": node.id,
                "agent_id": agent_id,
                "result": format!("placeholder result from agent '{}'", agent_id),
            }))
        }
    }

    /// Condition node: evaluate a simple condition from config.
    ///
    /// The config should contain `check_node` (the id of a previous node whose
    /// output we inspect) and `check_contains` (a substring to look for).
    /// Returns a boolean result.
    async fn execute_condition_node(
        node: &WorkflowNode,
        node_outputs: &HashMap<String, Value>,
    ) -> Result<Value, WorkflowError> {
        let check_node = node.config.get("check_node")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let check_contains = node.config.get("check_contains")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let matched = if let Some(prev_output) = node_outputs.get(check_node) {
            let serialised = match prev_output {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            serialised.contains(check_contains)
        } else {
            false
        };

        debug!(
            node_id = %node.id,
            check_node = %check_node,
            check_contains = %check_contains,
            matched = %matched,
            "evaluated condition node"
        );

        Ok(json!({
            "status": "completed",
            "node_id": node.id,
            "matched": matched,
        }))
    }

    /// Timer node: sleep for the configured duration (`duration_ms` in the
    /// config).
    async fn execute_timer_node(node: &WorkflowNode) -> Result<Value, WorkflowError> {
        let duration_ms = node.config.get("duration_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        info!(node_id = %node.id, duration_ms = %duration_ms, "timer node sleeping");
        tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;

        Ok(json!({
            "status": "completed",
            "node_id": node.id,
            "slept_ms": duration_ms,
        }))
    }

    /// Transform node: apply a simple pass-through transformation.  In a real
    /// implementation this would apply user-defined data mappings.
    async fn execute_transform_node(
        node: &WorkflowNode,
        node_outputs: &HashMap<String, Value>,
    ) -> Result<Value, WorkflowError> {
        // If an `input_node` is specified, forward that node's output.
        let pass_through = if let Some(input_node) = node.config.get("input_node").and_then(|v| v.as_str()) {
            node_outputs.get(input_node).cloned().unwrap_or(Value::Null)
        } else {
            Value::Null
        };

        debug!(node_id = %node.id, "transform node (pass-through)");

        Ok(json!({
            "status": "completed",
            "node_id": node.id,
            "transformed": pass_through,
        }))
    }

    /// Approval node: always returns a pending status.  A real implementation
    /// would park the workflow and wait for an external approval signal.
    async fn execute_approval_node(node: &WorkflowNode) -> Result<Value, WorkflowError> {
        info!(node_id = %node.id, "approval node returning pending status");

        Ok(json!({
            "status": "pending",
            "node_id": node.id,
            "message": "awaiting approval",
        }))
    }
}

impl Default for DagWorkflowEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl WorkflowEngine for DagWorkflowEngine {
    /// Execute the workflow identified by `workflow_id`.
    ///
    /// 1. Look up the definition.
    /// 2. Validate the DAG (cycle detection via topological sort).
    /// 3. Execute each node in topological order, propagating outputs.
    /// 4. Return the output of the last executed node.
    async fn run(&self, workflow_id: &str) -> Result<Value, WorkflowError> {
        let def = self
            .workflows
            .get(workflow_id)
            .ok_or_else(|| WorkflowError::NotFound(workflow_id.to_owned()))?;

        info!(workflow_id = %workflow_id, name = %def.name, "starting workflow execution");

        // Validate DAG and obtain execution order.
        let order = Self::topological_sort(def)?;
        debug!(?order, "topological order computed");

        // Index nodes by id for quick lookup.
        let nodes_by_id: HashMap<&str, &WorkflowNode> = def
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n))
            .collect();

        // Build a map of outgoing edges per node.
        let mut outgoing_edges: HashMap<&str, Vec<&crate::WorkflowEdge>> = HashMap::new();
        for edge in &def.edges {
            outgoing_edges.entry(edge.from.as_str()).or_default().push(edge);
        }

        // Track which nodes are reachable through satisfied conditional edges.
        // All root nodes (no incoming edges) are reachable by default.
        let mut reachable: HashSet<String> = HashSet::new();
        {
            let has_incoming: HashSet<&str> = def.edges.iter().map(|e| e.to.as_str()).collect();
            for node in &def.nodes {
                if !has_incoming.contains(node.id.as_str()) {
                    reachable.insert(node.id.clone());
                }
            }
        }

        // Outputs collected from every executed node.
        let mut node_outputs: HashMap<String, Value> = HashMap::new();
        let mut last_output = Value::Null;

        for node_id in &order {
            // Skip nodes that are not reachable via satisfied edges.
            if !reachable.contains(node_id.as_str()) {
                debug!(node_id = %node_id, "skipping unreachable node");
                continue;
            }

            let node = nodes_by_id
                .get(node_id.as_str())
                .ok_or_else(|| {
                    WorkflowError::ExecutionError(format!("node '{}' missing from definition", node_id))
                })?;

            debug!(node_id = %node_id, node_type = ?node.node_type, "executing node");

            let output = self.execute_node(node, &node_outputs).await?;
            node_outputs.insert(node_id.clone(), output.clone());
            last_output = output;

            // Determine which downstream nodes become reachable.
            if let Some(edges) = outgoing_edges.get(node_id.as_str()) {
                for edge in edges {
                    let source_output = node_outputs.get(node_id.as_str());
                    if Self::should_traverse_edge(&edge.condition, source_output) {
                        reachable.insert(edge.to.clone());
                    }
                }
            }
        }

        info!(workflow_id = %workflow_id, "workflow execution completed");
        Ok(last_output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WorkflowEdge, WorkflowNode};

    fn make_node(id: &str, node_type: NodeType, config: Value) -> WorkflowNode {
        WorkflowNode {
            id: id.to_owned(),
            node_type,
            config,
            timeout_seconds: 30,
            retry_config: None,
        }
    }

    fn simple_workflow() -> WorkflowDefinition {
        WorkflowDefinition {
            id: "wf-1".into(),
            name: "Test Workflow".into(),
            description: "A simple test workflow".into(),
            nodes: vec![
                make_node("tool-1", NodeType::Tool, json!({"tool_name": "search", "arguments": {"query": "hello"}})),
                make_node("transform-1", NodeType::Transform, json!({"input_node": "tool-1"})),
                make_node("agent-1", NodeType::Agent, json!({"agent_id": "agent-42"})),
            ],
            edges: vec![
                WorkflowEdge { from: "tool-1".into(), to: "transform-1".into(), condition: None },
                WorkflowEdge { from: "transform-1".into(), to: "agent-1".into(), condition: None },
            ],
        }
    }

    #[tokio::test]
    async fn test_simple_execution() {
        let mut engine = DagWorkflowEngine::new();
        engine.register(simple_workflow());

        let result = engine.run("wf-1").await.unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["agent_id"], "agent-42");
    }

    #[tokio::test]
    async fn test_not_found() {
        let engine = DagWorkflowEngine::new();
        let err = engine.run("nonexistent").await.unwrap_err();
        assert!(matches!(err, WorkflowError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_cycle_detection() {
        let mut engine = DagWorkflowEngine::new();
        engine.register(WorkflowDefinition {
            id: "wf-cycle".into(),
            name: "Cyclic".into(),
            description: "Has a cycle".into(),
            nodes: vec![
                make_node("a", NodeType::Tool, json!({"tool_name": "t"})),
                make_node("b", NodeType::Tool, json!({"tool_name": "t"})),
            ],
            edges: vec![
                WorkflowEdge { from: "a".into(), to: "b".into(), condition: None },
                WorkflowEdge { from: "b".into(), to: "a".into(), condition: None },
            ],
        });

        let err = engine.run("wf-cycle").await.unwrap_err();
        assert!(matches!(err, WorkflowError::CycleDetected));
    }

    #[tokio::test]
    async fn test_conditional_edge_skips_node() {
        let mut engine = DagWorkflowEngine::new();
        engine.register(WorkflowDefinition {
            id: "wf-cond".into(),
            name: "Conditional".into(),
            description: "Tests conditional edges".into(),
            nodes: vec![
                make_node("tool-1", NodeType::Tool, json!({"tool_name": "search"})),
                make_node("agent-1", NodeType::Agent, json!({"agent_id": "a1"})),
            ],
            edges: vec![
                WorkflowEdge {
                    from: "tool-1".into(),
                    to: "agent-1".into(),
                    condition: Some("contains:will_not_match_this_xyz".into()),
                },
            ],
        });

        // The agent node should be skipped because the condition doesn't match.
        let result = engine.run("wf-cond").await.unwrap();
        // Last output is from tool-1 since agent-1 was skipped.
        assert_eq!(result["tool_name"], "search");
    }

    #[tokio::test]
    async fn test_approval_returns_pending() {
        let mut engine = DagWorkflowEngine::new();
        engine.register(WorkflowDefinition {
            id: "wf-approval".into(),
            name: "Approval".into(),
            description: "Tests approval node".into(),
            nodes: vec![
                make_node("approve-1", NodeType::Approval, json!({})),
            ],
            edges: vec![],
        });

        let result = engine.run("wf-approval").await.unwrap();
        assert_eq!(result["status"], "pending");
    }

    #[tokio::test]
    async fn test_timer_node() {
        let mut engine = DagWorkflowEngine::new();
        engine.register(WorkflowDefinition {
            id: "wf-timer".into(),
            name: "Timer".into(),
            description: "Tests timer node".into(),
            nodes: vec![
                make_node("timer-1", NodeType::Timer, json!({"duration_ms": 10})),
            ],
            edges: vec![],
        });

        let result = engine.run("wf-timer").await.unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["slept_ms"], 10);
    }

    #[tokio::test]
    async fn test_condition_node_matched() {
        let mut engine = DagWorkflowEngine::new();
        engine.register(WorkflowDefinition {
            id: "wf-condnode".into(),
            name: "CondNode".into(),
            description: "Tests condition node evaluation".into(),
            nodes: vec![
                make_node("tool-1", NodeType::Tool, json!({"tool_name": "search"})),
                make_node("cond-1", NodeType::Condition, json!({
                    "check_node": "tool-1",
                    "check_contains": "search"
                })),
            ],
            edges: vec![
                WorkflowEdge { from: "tool-1".into(), to: "cond-1".into(), condition: None },
            ],
        });

        let result = engine.run("wf-condnode").await.unwrap();
        assert_eq!(result["matched"], true);
    }
}
