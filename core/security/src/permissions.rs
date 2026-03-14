use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Path scope for filesystem permissions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PathScope {
    Exact(PathBuf),
    DirectoryChildren(PathBuf),
    Recursive(PathBuf),
}

/// Domain scope for outbound network permissions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DomainScope {
    Exact(String),
    Wildcard(String),
    WithPort(String, u16),
}

/// Memory scope — controls which memory namespaces an agent can access.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MemoryScope {
    OwnAgent,
    Agent(String),
    Workspace,
}

/// Connector actions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConnectorAction {
    Read,
    Send,
    Subscribe,
    Manage,
}

/// Complete permission enum.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Permission {
    FsRead(Option<PathScope>),
    FsWrite(Option<PathScope>),
    FsDelete(Option<PathScope>),
    ShellExec,
    ShellExecCommand(String),
    NetworkOutbound(Option<DomainScope>),
    NetworkListen(u16),
    ConnectorUse(String, ConnectorAction),
    MemoryRead(MemoryScope),
    MemoryWrite(MemoryScope),
    KnowledgeRead,
    KnowledgeWrite,
    ModelCall,
    ModelVision,
    AgentSpawn,
    AgentMessage(Option<String>),
    ArtifactCreate,
    ArtifactRead,
    BrowserNavigate(Option<DomainScope>),
    BrowserRead,
    BrowserInteract,
    DesktopLaunch,
    DesktopInteract,
    DesktopCapture,
    ConfigRead,
    ConfigWrite,
    AuditRead,
}

/// Result of a permission check.
#[derive(Debug)]
pub enum PermissionVerdict {
    Allowed,
    Denied {
        required: String,
        grants_checked: usize,
    },
}

/// Check if granted permissions satisfy a required permission.
pub fn check_permission(granted: &[Permission], required: &Permission) -> PermissionVerdict {
    for grant in granted {
        if permission_matches(grant, required) {
            return PermissionVerdict::Allowed;
        }
    }
    PermissionVerdict::Denied {
        required: format!("{:?}", required),
        grants_checked: granted.len(),
    }
}

fn permission_matches(grant: &Permission, required: &Permission) -> bool {
    use Permission::*;
    match (grant, required) {
        // Filesystem
        (FsRead(None), FsRead(_)) => true,
        (FsWrite(None), FsWrite(_)) => true,
        (FsDelete(None), FsDelete(_)) => true,
        (FsRead(Some(gs)), FsRead(Some(rs))) => path_scope_contains(gs, rs),
        (FsWrite(Some(gs)), FsWrite(Some(rs))) => path_scope_contains(gs, rs),
        (FsDelete(Some(gs)), FsDelete(Some(rs))) => path_scope_contains(gs, rs),
        (FsRead(Some(_)), FsRead(None)) => false,
        (FsWrite(Some(_)), FsWrite(None)) => false,
        (FsDelete(Some(_)), FsDelete(None)) => false,

        // Network
        (NetworkOutbound(None), NetworkOutbound(_)) => true,
        (NetworkOutbound(Some(gd)), NetworkOutbound(Some(rd))) => domain_scope_contains(gd, rd),
        (NetworkOutbound(Some(_)), NetworkOutbound(None)) => false,

        // Shell
        (ShellExec, ShellExec) => true,
        (ShellExec, ShellExecCommand(_)) => true,
        (ShellExecCommand(g), ShellExecCommand(r)) => g == r,
        (ShellExecCommand(_), ShellExec) => false,

        // Connectors
        (ConnectorUse(gid, gact), ConnectorUse(rid, ract)) => {
            gid == rid && connector_action_covers(gact, ract)
        }

        // Memory
        (MemoryRead(MemoryScope::Workspace), MemoryRead(_)) => true,
        (MemoryRead(MemoryScope::OwnAgent), MemoryRead(MemoryScope::OwnAgent)) => true,
        (MemoryRead(MemoryScope::Agent(g)), MemoryRead(MemoryScope::Agent(r))) => g == r,
        (MemoryWrite(MemoryScope::Workspace), MemoryWrite(_)) => true,
        (MemoryWrite(MemoryScope::OwnAgent), MemoryWrite(MemoryScope::OwnAgent)) => true,
        (MemoryWrite(MemoryScope::Agent(g)), MemoryWrite(MemoryScope::Agent(r))) => g == r,

        // Browser
        (BrowserNavigate(None), BrowserNavigate(_)) => true,
        (BrowserNavigate(Some(g)), BrowserNavigate(Some(r))) => domain_scope_contains(g, r),

        // Agent
        (AgentMessage(None), AgentMessage(_)) => true,
        (AgentMessage(Some(g)), AgentMessage(Some(r))) => g == r,

        // Exact match for other variants
        (a, b) => std::mem::discriminant(a) == std::mem::discriminant(b) && a == b,
    }
}

fn path_scope_contains(grant: &PathScope, required: &PathScope) -> bool {
    match (grant, required) {
        (PathScope::Recursive(g), PathScope::Exact(r)) => r.starts_with(g),
        (PathScope::Recursive(g), PathScope::DirectoryChildren(r)) => r.starts_with(g),
        (PathScope::Recursive(g), PathScope::Recursive(r)) => r.starts_with(g),
        (PathScope::DirectoryChildren(g), PathScope::Exact(r)) => r.parent() == Some(g.as_path()),
        (PathScope::DirectoryChildren(g), PathScope::DirectoryChildren(r)) => g == r,
        (PathScope::DirectoryChildren(_), PathScope::Recursive(_)) => false,
        (PathScope::Exact(g), PathScope::Exact(r)) => g == r,
        (PathScope::Exact(_), _) => false,
    }
}

fn domain_scope_contains(grant: &DomainScope, required: &DomainScope) -> bool {
    match (grant, required) {
        (DomainScope::Wildcard(g), DomainScope::Exact(r)) => {
            let suffix = g.trim_start_matches('*');
            r.ends_with(suffix)
        }
        (DomainScope::Wildcard(g), DomainScope::Wildcard(r)) => {
            let g_suffix = g.trim_start_matches('*');
            let r_suffix = r.trim_start_matches('*');
            r_suffix.ends_with(g_suffix)
        }
        (DomainScope::Exact(g), DomainScope::Exact(r)) => g == r,
        (DomainScope::WithPort(gh, gp), DomainScope::WithPort(rh, rp)) => gh == rh && gp == rp,
        (DomainScope::Exact(g), DomainScope::WithPort(rh, _)) => g == rh,
        _ => false,
    }
}

fn connector_action_covers(grant: &ConnectorAction, req: &ConnectorAction) -> bool {
    use ConnectorAction::*;
    matches!(
        (grant, req),
        (Manage, _) | (Send, Send) | (Send, Read) | (Read, Read) | (Subscribe, Subscribe)
    )
}

/// Parse a colon-separated permission string into a Permission enum.
pub fn parse_permission(s: &str) -> Result<Permission, String> {
    let parts: Vec<&str> = s.splitn(4, ':').collect();
    match parts.as_slice() {
        ["fs", "read"] => Ok(Permission::FsRead(None)),
        ["fs", "read", path] => Ok(Permission::FsRead(Some(parse_path_scope(path)))),
        ["fs", "write"] => Ok(Permission::FsWrite(None)),
        ["fs", "write", path] => Ok(Permission::FsWrite(Some(parse_path_scope(path)))),
        ["fs", "delete"] => Ok(Permission::FsDelete(None)),
        ["fs", "delete", path] => Ok(Permission::FsDelete(Some(parse_path_scope(path)))),
        ["shell", "exec"] => Ok(Permission::ShellExec),
        ["shell", "exec", cmd] => Ok(Permission::ShellExecCommand(cmd.to_string())),
        ["net", "outbound"] | ["network", "outbound"] => Ok(Permission::NetworkOutbound(None)),
        ["net", "outbound", domain] | ["network", "outbound", domain] => Ok(
            Permission::NetworkOutbound(Some(parse_domain_scope(domain))),
        ),
        ["net", "listen", port] | ["network", "listen", port] => {
            let p: u16 = port
                .parse()
                .map_err(|_| format!("invalid port: {}", port))?;
            Ok(Permission::NetworkListen(p))
        }
        ["connector", id, action] => {
            let act = match *action {
                "read" => ConnectorAction::Read,
                "send" => ConnectorAction::Send,
                "subscribe" => ConnectorAction::Subscribe,
                "manage" => ConnectorAction::Manage,
                _ => return Err(format!("unknown connector action: {}", action)),
            };
            Ok(Permission::ConnectorUse(id.to_string(), act))
        }
        ["memory", "read"] => Ok(Permission::MemoryRead(MemoryScope::Workspace)),
        ["memory", "read", scope] => Ok(Permission::MemoryRead(parse_memory_scope(scope))),
        ["memory", "write"] => Ok(Permission::MemoryWrite(MemoryScope::Workspace)),
        ["memory", "write", scope] => Ok(Permission::MemoryWrite(parse_memory_scope(scope))),
        ["knowledge", "read"] => Ok(Permission::KnowledgeRead),
        ["knowledge", "write"] => Ok(Permission::KnowledgeWrite),
        ["model", "call"] => Ok(Permission::ModelCall),
        ["model", "vision"] => Ok(Permission::ModelVision),
        ["agent", "spawn"] => Ok(Permission::AgentSpawn),
        ["agent", "message"] => Ok(Permission::AgentMessage(None)),
        ["agent", "message", id] => Ok(Permission::AgentMessage(Some(id.to_string()))),
        ["artifact", "create"] => Ok(Permission::ArtifactCreate),
        ["artifact", "read"] => Ok(Permission::ArtifactRead),
        ["browser", "navigate"] => Ok(Permission::BrowserNavigate(None)),
        ["browser", "navigate", domain] => Ok(Permission::BrowserNavigate(Some(
            parse_domain_scope(domain),
        ))),
        ["browser", "read"] => Ok(Permission::BrowserRead),
        ["browser", "interact"] => Ok(Permission::BrowserInteract),
        ["desktop", "launch"] => Ok(Permission::DesktopLaunch),
        ["desktop", "interact"] => Ok(Permission::DesktopInteract),
        ["desktop", "capture"] => Ok(Permission::DesktopCapture),
        ["config", "read"] => Ok(Permission::ConfigRead),
        ["config", "write"] => Ok(Permission::ConfigWrite),
        ["audit", "read"] => Ok(Permission::AuditRead),
        _ => Err(format!("unknown permission string: {}", s)),
    }
}

fn parse_path_scope(path: &str) -> PathScope {
    if let Some(stripped) = path.strip_suffix("/**") {
        PathScope::Recursive(PathBuf::from(stripped))
    } else if let Some(stripped) = path.strip_suffix("/*") {
        PathScope::DirectoryChildren(PathBuf::from(stripped))
    } else {
        PathScope::Exact(PathBuf::from(path))
    }
}

fn parse_domain_scope(domain: &str) -> DomainScope {
    if domain.starts_with("*.") {
        DomainScope::Wildcard(domain.to_string())
    } else if let Some((host, port)) = domain.rsplit_once(':') {
        if let Ok(p) = port.parse::<u16>() {
            DomainScope::WithPort(host.to_string(), p)
        } else {
            DomainScope::Exact(domain.to_string())
        }
    } else {
        DomainScope::Exact(domain.to_string())
    }
}

fn parse_memory_scope(scope: &str) -> MemoryScope {
    match scope {
        "own" | "own_agent" => MemoryScope::OwnAgent,
        "workspace" => MemoryScope::Workspace,
        agent_id => MemoryScope::Agent(agent_id.to_string()),
    }
}
