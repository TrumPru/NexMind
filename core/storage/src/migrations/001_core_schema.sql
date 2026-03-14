-- NexMind Core Schema: Migration 001
-- All tables for the main nexmind.db database.

-- ── Agents ────────────────────────────────────────────────────────
CREATE TABLE agents (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    definition   TEXT NOT NULL,           -- JSON: full agent definition
    version      INTEGER NOT NULL DEFAULT 1,
    status       TEXT NOT NULL DEFAULT 'idle',
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_agents_workspace ON agents(workspace_id);
CREATE INDEX idx_agents_status ON agents(status);

-- ── Agent Runs ────────────────────────────────────────────────────
CREATE TABLE agent_runs (
    run_id         TEXT PRIMARY KEY,
    agent_id       TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'executing',
    state_snapshot TEXT,                   -- JSON: serialized agent state
    checkpoint     BLOB,                   -- binary checkpoint for resume
    started_at     TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at     TEXT NOT NULL DEFAULT (datetime('now')),

    FOREIGN KEY (agent_id) REFERENCES agents(id)
);

CREATE INDEX idx_agent_runs_agent ON agent_runs(agent_id);
CREATE INDEX idx_agent_runs_status ON agent_runs(status);

-- ── Teams ─────────────────────────────────────────────────────────
CREATE TABLE teams (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    definition   TEXT NOT NULL,           -- JSON: full team definition
    version      INTEGER NOT NULL DEFAULT 1,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_teams_workspace ON teams(workspace_id);

-- ── Tasks ─────────────────────────────────────────────────────────
CREATE TABLE tasks (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    agent_id     TEXT,
    team_id      TEXT,
    status       TEXT NOT NULL DEFAULT 'pending',
    input        TEXT,                     -- JSON: task input
    output       TEXT,                     -- JSON: task output
    error        TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now')),

    FOREIGN KEY (agent_id) REFERENCES agents(id)
);

CREATE INDEX idx_tasks_workspace ON tasks(workspace_id);
CREATE INDEX idx_tasks_status ON tasks(status);
CREATE INDEX idx_tasks_agent ON tasks(agent_id);

-- ── Task Plans (DAG) ──────────────────────────────────────────────
CREATE TABLE task_plans (
    id           TEXT PRIMARY KEY,
    task_id      TEXT NOT NULL,
    version      INTEGER DEFAULT 1,
    status       TEXT DEFAULT 'draft',
    dag          TEXT NOT NULL,            -- JSON: execution DAG
    created_by   TEXT NOT NULL,
    approved_by  TEXT,
    approved_at  TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),

    FOREIGN KEY (task_id) REFERENCES tasks(id)
);

CREATE INDEX idx_task_plans_task ON task_plans(task_id);

-- ── Task Messages (inter-agent) ──────────────────────────────────
CREATE TABLE task_messages (
    id          TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL,
    team_id     TEXT,
    from_agent  TEXT NOT NULL,
    to_agent    TEXT,
    msg_type    TEXT NOT NULL,
    content     TEXT NOT NULL,             -- JSON
    artifacts   TEXT,                      -- JSON: list of artifact_ids
    visibility  TEXT DEFAULT 'team',
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),

    FOREIGN KEY (task_id) REFERENCES tasks(id)
);

CREATE INDEX idx_task_messages_task ON task_messages(task_id, created_at);

-- ── Workflows ─────────────────────────────────────────────────────
CREATE TABLE workflows (
    id              TEXT PRIMARY KEY,
    workspace_id    TEXT NOT NULL,
    name            TEXT NOT NULL,
    description     TEXT,
    definition_yaml TEXT NOT NULL,
    definition_json TEXT NOT NULL,
    version         INTEGER NOT NULL DEFAULT 1,
    status          TEXT NOT NULL DEFAULT 'active',
    created_by      TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
    trigger_type    TEXT,
    trigger_config  TEXT
);

CREATE INDEX idx_workflows_workspace ON workflows(workspace_id);
CREATE INDEX idx_workflows_trigger ON workflows(trigger_type, status);

-- ── Workflow Runs ─────────────────────────────────────────────────
CREATE TABLE workflow_runs (
    id               TEXT PRIMARY KEY,
    workflow_id      TEXT NOT NULL,
    workflow_version INTEGER NOT NULL,
    status           TEXT NOT NULL DEFAULT 'running',
    trigger_type     TEXT NOT NULL,
    trigger_data     TEXT,
    checkpoint       BLOB,
    inputs           TEXT,
    outputs          TEXT,
    error            TEXT,
    started_at       TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at     TEXT,
    duration_ms      INTEGER,
    tokens_used      INTEGER DEFAULT 0,
    cost_microdollars INTEGER DEFAULT 0,

    FOREIGN KEY (workflow_id) REFERENCES workflows(id)
);

CREATE INDEX idx_wf_runs_workflow ON workflow_runs(workflow_id, started_at DESC);
CREATE INDEX idx_wf_runs_status ON workflow_runs(status);

-- ── Workflow Node States ──────────────────────────────────────────
CREATE TABLE workflow_node_states (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id       TEXT NOT NULL,
    node_id      TEXT NOT NULL,
    state        TEXT NOT NULL,
    output       TEXT,
    error        TEXT,
    retries_used INTEGER DEFAULT 0,
    started_at   TEXT,
    completed_at TEXT,
    duration_ms  INTEGER,
    updated_at   TEXT NOT NULL DEFAULT (datetime('now')),

    FOREIGN KEY (run_id) REFERENCES workflow_runs(id),
    UNIQUE(run_id, node_id)
);

CREATE INDEX idx_wfns_run ON workflow_node_states(run_id);
CREATE INDEX idx_wfns_state ON workflow_node_states(state);

-- ── Trigger Bindings ──────────────────────────────────────────────
CREATE TABLE trigger_bindings (
    id              TEXT PRIMARY KEY,
    workspace_id    TEXT NOT NULL,
    trigger_type    TEXT NOT NULL,
    trigger_config  TEXT NOT NULL,
    target_type     TEXT NOT NULL,
    target_id       TEXT NOT NULL,
    enabled         INTEGER NOT NULL DEFAULT 1,
    last_fired_at   TEXT,
    fire_count      INTEGER NOT NULL DEFAULT 0,
    created_by      TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_triggers_workspace ON trigger_bindings(workspace_id);
CREATE INDEX idx_triggers_target ON trigger_bindings(target_type, target_id);

-- ── Approval Requests ─────────────────────────────────────────────
CREATE TABLE approval_requests (
    id                  TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL,
    requester_agent_id  TEXT NOT NULL,
    requester_run_id    TEXT NOT NULL,
    requester_node_id   TEXT,
    tool_id             TEXT NOT NULL,
    tool_args           TEXT NOT NULL,
    tool_args_hash      TEXT NOT NULL,
    action_description  TEXT NOT NULL,
    risk_level          TEXT NOT NULL DEFAULT 'medium',
    policy_id           TEXT NOT NULL,
    context_snapshot    TEXT,
    related_approvals   TEXT,
    status              TEXT NOT NULL DEFAULT 'pending',
    decided_by          TEXT,
    decided_at          TEXT,
    decision_note       TEXT,
    created_at          TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at          TEXT NOT NULL,
    notified_channels   TEXT
);

CREATE INDEX idx_approvals_status ON approval_requests(status);
CREATE INDEX idx_approvals_workspace ON approval_requests(workspace_id);

-- ── Audit Log ─────────────────────────────────────────────────────
CREATE TABLE audit_log (
    id              TEXT PRIMARY KEY,
    timestamp       TEXT NOT NULL,
    workspace_id    TEXT NOT NULL,
    actor_type      TEXT NOT NULL CHECK(actor_type IN ('agent', 'user', 'system', 'plugin')),
    actor_id        TEXT NOT NULL,
    action          TEXT NOT NULL,
    resource_type   TEXT,
    resource_id     TEXT,
    outcome         TEXT NOT NULL CHECK(outcome IN ('success', 'denied', 'error', 'pending')),
    error_message   TEXT,
    correlation_id  TEXT,
    channel         TEXT NOT NULL DEFAULT 'desktop' CHECK(channel IN ('desktop', 'telegram', 'cli', 'api', 'system')),
    metadata        TEXT,
    prev_hmac       TEXT,
    row_hmac        TEXT NOT NULL
);

CREATE INDEX idx_audit_timestamp ON audit_log(timestamp);
CREATE INDEX idx_audit_actor ON audit_log(actor_type, actor_id);
CREATE INDEX idx_audit_action ON audit_log(action);
CREATE INDEX idx_audit_correlation ON audit_log(correlation_id);
CREATE INDEX idx_audit_workspace ON audit_log(workspace_id);

-- ── Cost Records ──────────────────────────────────────────────────
CREATE TABLE cost_records (
    id                TEXT PRIMARY KEY,
    timestamp         TEXT NOT NULL,
    workspace_id      TEXT NOT NULL,
    agent_id          TEXT NOT NULL,
    run_id            TEXT NOT NULL,
    model             TEXT NOT NULL,
    provider          TEXT NOT NULL,
    input_tokens      INTEGER NOT NULL,
    output_tokens     INTEGER NOT NULL,
    thinking_tokens   INTEGER NOT NULL DEFAULT 0,
    cached_tokens     INTEGER NOT NULL DEFAULT 0,
    cost_microdollars INTEGER NOT NULL,
    request_type      TEXT NOT NULL DEFAULT 'chat' CHECK(request_type IN ('chat', 'embedding', 'vision', 'tts'))
);

CREATE INDEX idx_cost_timestamp ON cost_records(timestamp);
CREATE INDEX idx_cost_agent ON cost_records(agent_id);
CREATE INDEX idx_cost_workspace ON cost_records(workspace_id);
