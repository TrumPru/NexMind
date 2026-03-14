use std::sync::Arc;
use tokio::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::info;

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

// ── Email Configuration ───────────────────────────────────────────

/// Email configuration for IMAP/SMTP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    pub email: String,
    pub password: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub check_interval_seconds: u64,
}

impl EmailConfig {
    /// Create a Gmail config with app password.
    pub fn gmail(email: &str, app_password: &str) -> Self {
        Self {
            email: email.to_string(),
            password: app_password.to_string(),
            imap_host: "imap.gmail.com".to_string(),
            imap_port: 993,
            smtp_host: "smtp.gmail.com".to_string(),
            smtp_port: 587,
            check_interval_seconds: 300,
        }
    }

    /// Create from environment variables.
    pub fn from_env() -> Option<Self> {
        let email = std::env::var("NEXMIND_EMAIL").ok()?;
        let password = std::env::var("NEXMIND_EMAIL_PASSWORD").ok()?;
        let imap_host = std::env::var("NEXMIND_IMAP_HOST").unwrap_or_else(|_| "imap.gmail.com".into());
        let imap_port: u16 = std::env::var("NEXMIND_IMAP_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(993);
        let smtp_host = std::env::var("NEXMIND_SMTP_HOST").unwrap_or_else(|_| "smtp.gmail.com".into());
        let smtp_port: u16 = std::env::var("NEXMIND_SMTP_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(587);

        Some(Self {
            email,
            password,
            imap_host,
            imap_port,
            smtp_host,
            smtp_port,
            check_interval_seconds: 300,
        })
    }
}

/// Shared email connector state.
pub type SharedEmailConnector = Arc<Mutex<EmailConnector>>;

/// Email connector managing IMAP and SMTP connections.
pub struct EmailConnector {
    config: EmailConfig,
}

impl EmailConnector {
    pub fn new(config: EmailConfig) -> Self {
        Self { config }
    }

    /// Test the IMAP connection.
    pub fn test_connection(&self) -> Result<String, String> {
        let tls = native_tls::TlsConnector::builder()
            .build()
            .map_err(|e| format!("TLS error: {}", e))?;

        let client = imap::connect(
            (&*self.config.imap_host, self.config.imap_port),
            &self.config.imap_host,
            &tls,
        )
        .map_err(|e| format!("IMAP connection failed: {}", e))?;

        let mut session = client
            .login(&self.config.email, &self.config.password)
            .map_err(|e| format!("IMAP login failed: {}", e.0))?;

        let mailbox = session.select("INBOX").map_err(|e| format!("INBOX select failed: {}", e))?;
        let msg_count = mailbox.exists;

        session.logout().ok();
        Ok(format!("Connected to {}. INBOX has {} messages.", self.config.imap_host, msg_count))
    }

    /// Fetch recent emails from INBOX.
    pub fn fetch_emails(&self, folder: &str, unread_only: bool, limit: u32) -> Result<Vec<EmailSummary>, String> {
        let tls = native_tls::TlsConnector::builder()
            .build()
            .map_err(|e| format!("TLS error: {}", e))?;

        let client = imap::connect(
            (&*self.config.imap_host, self.config.imap_port),
            &self.config.imap_host,
            &tls,
        )
        .map_err(|e| format!("IMAP connection failed: {}", e))?;

        let mut session = client
            .login(&self.config.email, &self.config.password)
            .map_err(|e| format!("IMAP login failed: {}", e.0))?;

        session.select(folder).map_err(|e| format!("Folder select failed: {}", e))?;

        let search_query = if unread_only { "UNSEEN" } else { "ALL" };
        let uids = session
            .search(search_query)
            .map_err(|e| format!("Search failed: {}", e))?;

        let mut emails = Vec::new();
        let mut uids: Vec<u32> = uids.into_iter().collect();
        uids.sort_unstable();
        uids.reverse();
        uids.truncate(limit as usize);

        if uids.is_empty() {
            session.logout().ok();
            return Ok(emails);
        }

        let uid_set: String = uids.iter().map(|u: &u32| u.to_string()).collect::<Vec<_>>().join(",");
        let messages = session
            .fetch(&uid_set, "ENVELOPE BODY.PEEK[TEXT]<0.200> FLAGS")
            .map_err(|e| format!("Fetch failed: {}", e))?;

        for msg in messages.iter() {
            let envelope = msg.envelope();
            let uid = msg.uid.unwrap_or(msg.message);

            let from = envelope
                .and_then(|e| e.from.as_ref())
                .and_then(|addrs| addrs.first())
                .map(|a| {
                    let mailbox = a.mailbox.as_ref().map(|m| String::from_utf8_lossy(m).to_string()).unwrap_or_default();
                    let host = a.host.as_ref().map(|h| String::from_utf8_lossy(h).to_string()).unwrap_or_default();
                    format!("{}@{}", mailbox, host)
                })
                .unwrap_or_else(|| "unknown".into());

            let subject = envelope
                .and_then(|e| e.subject.as_ref())
                .map(|s| String::from_utf8_lossy(s).to_string())
                .unwrap_or_else(|| "(no subject)".into());

            let date = envelope
                .and_then(|e| e.date.as_ref())
                .map(|d| String::from_utf8_lossy(d).to_string())
                .unwrap_or_default();

            let preview = msg.text()
                .map(|t| String::from_utf8_lossy(t).chars().take(200).collect::<String>())
                .unwrap_or_default();

            let flags = msg.flags();
            let unread = !flags.iter().any(|f| matches!(f, imap::types::Flag::Seen));

            emails.push(EmailSummary {
                id: uid.to_string(),
                from,
                subject,
                date,
                preview,
                unread,
                has_attachments: false,
            });
        }

        session.logout().ok();
        Ok(emails)
    }

    /// Read full email body by UID.
    pub fn read_email(&self, uid: &str) -> Result<EmailFull, String> {
        let tls = native_tls::TlsConnector::builder()
            .build()
            .map_err(|e| format!("TLS error: {}", e))?;

        let client = imap::connect(
            (&*self.config.imap_host, self.config.imap_port),
            &self.config.imap_host,
            &tls,
        )
        .map_err(|e| format!("IMAP connection failed: {}", e))?;

        let mut session = client
            .login(&self.config.email, &self.config.password)
            .map_err(|e| format!("IMAP login failed: {}", e.0))?;

        session.select("INBOX").map_err(|e| format!("INBOX select failed: {}", e))?;

        let messages = session
            .fetch(uid, "ENVELOPE BODY[TEXT] FLAGS")
            .map_err(|e| format!("Fetch failed: {}", e))?;

        let msg = messages.iter().next().ok_or("Email not found")?;
        let envelope = msg.envelope();

        let from = envelope
            .and_then(|e| e.from.as_ref())
            .and_then(|addrs| addrs.first())
            .map(|a| {
                let mailbox = a.mailbox.as_ref().map(|m| String::from_utf8_lossy(m).to_string()).unwrap_or_default();
                let host = a.host.as_ref().map(|h| String::from_utf8_lossy(h).to_string()).unwrap_or_default();
                format!("{}@{}", mailbox, host)
            })
            .unwrap_or_else(|| "unknown".into());

        let subject = envelope
            .and_then(|e| e.subject.as_ref())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .unwrap_or_else(|| "(no subject)".into());

        let body = msg.text()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .unwrap_or_default();

        session.logout().ok();

        Ok(EmailFull {
            id: uid.to_string(),
            from,
            subject,
            body,
        })
    }

    /// Send email via SMTP.
    pub fn send_email(&self, to: &str, subject: &str, body: &str) -> Result<String, String> {
        use lettre::{Message, SmtpTransport, Transport};
        use lettre::transport::smtp::authentication::Credentials;

        let email = Message::builder()
            .from(self.config.email.parse().map_err(|e| format!("Bad from: {}", e))?)
            .to(to.parse().map_err(|e| format!("Bad to: {}", e))?)
            .subject(subject)
            .body(body.to_string())
            .map_err(|e| format!("Email build error: {}", e))?;

        let creds = Credentials::new(
            self.config.email.clone(),
            self.config.password.clone(),
        );

        let mailer = SmtpTransport::starttls_relay(&self.config.smtp_host)
            .map_err(|e| format!("SMTP relay error: {}", e))?
            .port(self.config.smtp_port)
            .credentials(creds)
            .build();

        mailer.send(&email).map_err(|e| format!("Send failed: {}", e))?;
        Ok(format!("Email sent to {}", to))
    }

    /// Search emails by IMAP SEARCH query.
    pub fn search_emails(&self, query: &str, limit: u32) -> Result<Vec<EmailSummary>, String> {
        let tls = native_tls::TlsConnector::builder()
            .build()
            .map_err(|e| format!("TLS error: {}", e))?;

        let client = imap::connect(
            (&*self.config.imap_host, self.config.imap_port),
            &self.config.imap_host,
            &tls,
        )
        .map_err(|e| format!("IMAP connection failed: {}", e))?;

        let mut session = client
            .login(&self.config.email, &self.config.password)
            .map_err(|e| format!("IMAP login failed: {}", e.0))?;

        session.select("INBOX").map_err(|e| format!("INBOX select failed: {}", e))?;

        // Construct IMAP SEARCH query
        let search_query = format!("SUBJECT \"{}\"", query);
        let uids = session
            .search(&search_query)
            .map_err(|e| format!("Search failed: {}", e))?;

        let mut uids: Vec<u32> = uids.into_iter().collect();
        uids.sort_unstable();
        uids.reverse();
        uids.truncate(limit as usize);
        let mut emails = Vec::new();

        if !uids.is_empty() {
            let uid_set: String = uids.iter().map(|u: &u32| u.to_string()).collect::<Vec<_>>().join(",");
            let messages = session
                .fetch(&uid_set, "ENVELOPE FLAGS")
                .map_err(|e| format!("Fetch failed: {}", e))?;

            for msg in messages.iter() {
                let envelope = msg.envelope();
                let uid = msg.uid.unwrap_or(msg.message);

                let from = envelope
                    .and_then(|e| e.from.as_ref())
                    .and_then(|addrs| addrs.first())
                    .map(|a| {
                        let mailbox = a.mailbox.as_ref().map(|m| String::from_utf8_lossy(m).to_string()).unwrap_or_default();
                        let host = a.host.as_ref().map(|h| String::from_utf8_lossy(h).to_string()).unwrap_or_default();
                        format!("{}@{}", mailbox, host)
                    })
                    .unwrap_or_else(|| "unknown".into());

                let subject = envelope
                    .and_then(|e| e.subject.as_ref())
                    .map(|s| String::from_utf8_lossy(s).to_string())
                    .unwrap_or_else(|| "(no subject)".into());

                let flags = msg.flags();
                let unread = !flags.iter().any(|f| matches!(f, imap::types::Flag::Seen));

                emails.push(EmailSummary {
                    id: uid.to_string(),
                    from,
                    subject,
                    date: String::new(),
                    preview: String::new(),
                    unread,
                    has_attachments: false,
                });
            }
        }

        session.logout().ok();
        Ok(emails)
    }
}

/// Email summary (headers + preview).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailSummary {
    pub id: String,
    pub from: String,
    pub subject: String,
    pub date: String,
    pub preview: String,
    pub unread: bool,
    pub has_attachments: bool,
}

/// Full email with body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailFull {
    pub id: String,
    pub from: String,
    pub subject: String,
    pub body: String,
}

// ── Email Tools ───────────────────────────────────────────────────

/// Tool: email_fetch — fetch unread/recent emails.
pub struct EmailFetchTool {
    connector: SharedEmailConnector,
}

impl EmailFetchTool {
    pub fn new(connector: SharedEmailConnector) -> Self {
        Self { connector }
    }
}

#[async_trait::async_trait]
impl Tool for EmailFetchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "email_fetch".into(),
            name: "email_fetch".into(),
            description: "Fetch unread or recent emails from inbox".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "folder": {"type": "string", "default": "INBOX"},
                    "unread_only": {"type": "boolean", "default": true},
                    "limit": {"type": "integer", "default": 20}
                }
            }),
            required_permissions: vec![],
            trust_level: 0,
            idempotent: true,
            timeout_seconds: 30,
        }
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let folder = args["folder"].as_str().unwrap_or("INBOX");
        let unread_only = args["unread_only"].as_bool().unwrap_or(true);
        let limit = args["limit"].as_u64().unwrap_or(20) as u32;

        let connector = self.connector.lock().await;
        match connector.fetch_emails(folder, unread_only, limit) {
            Ok(emails) => Ok(ToolOutput::Success {
                result: serde_json::json!({
                    "emails": emails,
                    "count": emails.len(),
                }),
                tokens_used: None,
            }),
            Err(e) => Ok(ToolOutput::Error {
                error: e,
                retryable: true,
            }),
        }
    }

    fn validate_args(&self, _args: &Value) -> Result<(), ToolError> {
        Ok(())
    }
}

/// Tool: email_read — read full email body by ID.
pub struct EmailReadTool {
    connector: SharedEmailConnector,
}

impl EmailReadTool {
    pub fn new(connector: SharedEmailConnector) -> Self {
        Self { connector }
    }
}

#[async_trait::async_trait]
impl Tool for EmailReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "email_read".into(),
            name: "email_read".into(),
            description: "Read full email body by message ID".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"}
                },
                "required": ["id"]
            }),
            required_permissions: vec![],
            trust_level: 0,
            idempotent: true,
            timeout_seconds: 30,
        }
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let uid = args["id"].as_str().ok_or_else(|| ToolError::ValidationError("id required".into()))?;

        let connector = self.connector.lock().await;
        match connector.read_email(uid) {
            Ok(email) => Ok(ToolOutput::Success {
                result: serde_json::to_value(&email).unwrap(),
                tokens_used: None,
            }),
            Err(e) => Ok(ToolOutput::Error {
                error: e,
                retryable: true,
            }),
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("id").is_none() {
            return Err(ToolError::ValidationError("id is required".into()));
        }
        Ok(())
    }
}

/// Tool: email_search — search emails by query.
pub struct EmailSearchTool {
    connector: SharedEmailConnector,
}

impl EmailSearchTool {
    pub fn new(connector: SharedEmailConnector) -> Self {
        Self { connector }
    }
}

#[async_trait::async_trait]
impl Tool for EmailSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "email_search".into(),
            name: "email_search".into(),
            description: "Search emails by subject/query".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "default": 10}
                },
                "required": ["query"]
            }),
            required_permissions: vec![],
            trust_level: 0,
            idempotent: true,
            timeout_seconds: 30,
        }
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let query = args["query"].as_str().ok_or_else(|| ToolError::ValidationError("query required".into()))?;
        let limit = args["limit"].as_u64().unwrap_or(10) as u32;

        let connector = self.connector.lock().await;
        match connector.search_emails(query, limit) {
            Ok(results) => Ok(ToolOutput::Success {
                result: serde_json::json!({
                    "results": results,
                    "count": results.len(),
                }),
                tokens_used: None,
            }),
            Err(e) => Ok(ToolOutput::Error {
                error: e,
                retryable: true,
            }),
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        if args.get("query").and_then(|q| q.as_str()).is_none() {
            return Err(ToolError::ValidationError("query is required".into()));
        }
        Ok(())
    }
}

/// Tool: email_send — send email (trust level 2 = requires approval).
pub struct EmailSendTool {
    connector: SharedEmailConnector,
}

impl EmailSendTool {
    pub fn new(connector: SharedEmailConnector) -> Self {
        Self { connector }
    }
}

#[async_trait::async_trait]
impl Tool for EmailSendTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "email_send".into(),
            name: "email_send".into(),
            description: "Send an email (requires approval)".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string"},
                    "subject": {"type": "string"},
                    "body": {"type": "string"}
                },
                "required": ["to", "subject", "body"]
            }),
            required_permissions: vec![],
            trust_level: 2,
            idempotent: false,
            timeout_seconds: 30,
        }
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let to = args["to"].as_str().ok_or_else(|| ToolError::ValidationError("to required".into()))?;
        let subject = args["subject"].as_str().ok_or_else(|| ToolError::ValidationError("subject required".into()))?;
        let body = args["body"].as_str().ok_or_else(|| ToolError::ValidationError("body required".into()))?;

        info!(to = %to, subject = %subject, "sending email");

        let connector = self.connector.lock().await;
        match connector.send_email(to, subject, body) {
            Ok(msg) => Ok(ToolOutput::Success {
                result: serde_json::json!({"message": msg, "sent": true}),
                tokens_used: None,
            }),
            Err(e) => Ok(ToolOutput::Error {
                error: e,
                retryable: true,
            }),
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        for field in &["to", "subject", "body"] {
            if args.get(*field).and_then(|v| v.as_str()).is_none() {
                return Err(ToolError::ValidationError(format!("{} is required", field)));
            }
        }
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gmail_config() {
        let config = EmailConfig::gmail("test@gmail.com", "apppassword");
        assert_eq!(config.email, "test@gmail.com");
        assert_eq!(config.imap_host, "imap.gmail.com");
        assert_eq!(config.imap_port, 993);
        assert_eq!(config.smtp_host, "smtp.gmail.com");
        assert_eq!(config.smtp_port, 587);
    }

    #[test]
    fn test_email_summary_serialization() {
        let summary = EmailSummary {
            id: "123".into(),
            from: "alice@example.com".into(),
            subject: "Test Email".into(),
            date: "2026-03-14T10:00:00Z".into(),
            preview: "Hello, this is a test...".into(),
            unread: true,
            has_attachments: false,
        };
        let json = serde_json::to_string(&summary).unwrap();
        let de: EmailSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(de.id, "123");
        assert_eq!(de.from, "alice@example.com");
        assert!(de.unread);
    }

    #[test]
    fn test_email_full_serialization() {
        let email = EmailFull {
            id: "456".into(),
            from: "bob@example.com".into(),
            subject: "Full Email".into(),
            body: "This is the full body of the email.".into(),
        };
        let json = serde_json::to_string(&email).unwrap();
        let de: EmailFull = serde_json::from_str(&json).unwrap();
        assert_eq!(de.body, "This is the full body of the email.");
    }

    #[test]
    fn test_email_fetch_tool_definition() {
        let config = EmailConfig::gmail("test@gmail.com", "pass");
        let connector = Arc::new(Mutex::new(EmailConnector::new(config)));
        let tool = EmailFetchTool::new(connector);
        let def = tool.definition();
        assert_eq!(def.id, "email_fetch");
        assert_eq!(def.trust_level, 0);
    }

    #[test]
    fn test_email_send_tool_definition() {
        let config = EmailConfig::gmail("test@gmail.com", "pass");
        let connector = Arc::new(Mutex::new(EmailConnector::new(config)));
        let tool = EmailSendTool::new(connector);
        let def = tool.definition();
        assert_eq!(def.id, "email_send");
        assert_eq!(def.trust_level, 2); // requires approval
    }

    #[test]
    fn test_email_send_validation() {
        let config = EmailConfig::gmail("test@gmail.com", "pass");
        let connector = Arc::new(Mutex::new(EmailConnector::new(config)));
        let tool = EmailSendTool::new(connector);

        // Missing fields should fail validation
        let result = tool.validate_args(&serde_json::json!({"to": "a@b.com"}));
        assert!(result.is_err());

        // All fields present should pass
        let result = tool.validate_args(&serde_json::json!({
            "to": "a@b.com",
            "subject": "Test",
            "body": "Hello"
        }));
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_email_send_needs_approval() {
        // email_send has trust_level 2, so ToolRegistry would return NeedsApproval
        let config = EmailConfig::gmail("test@gmail.com", "pass");
        let connector = Arc::new(Mutex::new(EmailConnector::new(config)));
        let tool = EmailSendTool::new(connector);
        let def = tool.definition();
        assert!(def.trust_level >= 2, "email_send should require approval");
    }

    #[test]
    fn test_email_search_tool_definition() {
        let config = EmailConfig::gmail("test@gmail.com", "pass");
        let connector = Arc::new(Mutex::new(EmailConnector::new(config)));
        let tool = EmailSearchTool::new(connector);
        let def = tool.definition();
        assert_eq!(def.id, "email_search");
        assert_eq!(def.trust_level, 0);
    }

    #[test]
    fn test_email_search_validation() {
        let config = EmailConfig::gmail("test@gmail.com", "pass");
        let connector = Arc::new(Mutex::new(EmailConnector::new(config)));
        let tool = EmailSearchTool::new(connector);

        let result = tool.validate_args(&serde_json::json!({}));
        assert!(result.is_err());

        let result = tool.validate_args(&serde_json::json!({"query": "test"}));
        assert!(result.is_ok());
    }
}
