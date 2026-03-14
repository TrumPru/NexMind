/// Embedded web dashboard served by the daemon.
///
/// The dashboard is a single HTML file with inline JS/CSS,
/// compiled into the binary via `include_str!`.

/// The dashboard HTML content, embedded at compile time.
pub const DASHBOARD_HTML: &str = include_str!("../../../dashboard/index.html");

/// A token used to authenticate dashboard access.
pub struct DashboardToken {
    pub token: String,
}

impl DashboardToken {
    /// Create a new dashboard token with a random value.
    pub fn new() -> Self {
        Self {
            token: DashboardServer::generate_token(),
        }
    }
}

/// Dashboard server configuration and utilities.
pub struct DashboardServer {
    pub port: u16,
}

impl DashboardServer {
    /// Create a new dashboard server configuration.
    pub fn new(port: u16) -> Self {
        Self { port }
    }

    /// Generate a random 32-character hex token.
    pub fn generate_token() -> String {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let bytes: Vec<u8> = (0..16).map(|_| rng.gen()).collect();
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }

    /// Validate a provided token against the expected token.
    /// Uses constant-time comparison to prevent timing attacks.
    pub fn validate_token(provided: &str, expected: &str) -> bool {
        if provided.len() != expected.len() {
            return false;
        }
        provided
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dashboard_html_contains_expected_content() {
        assert!(DASHBOARD_HTML.contains("NexMind"));
        assert!(DASHBOARD_HTML.contains("/api/"));
        assert!(DASHBOARD_HTML.contains("--bg-primary"));
    }

    #[test]
    fn test_generate_token_length() {
        let token = DashboardServer::generate_token();
        assert_eq!(token.len(), 32, "Token should be 32 hex characters");
    }

    #[test]
    fn test_generate_token_is_hex() {
        let token = DashboardServer::generate_token();
        assert!(
            token.chars().all(|c| c.is_ascii_hexdigit()),
            "Token should only contain hex characters"
        );
    }

    #[test]
    fn test_generate_token_uniqueness() {
        let t1 = DashboardServer::generate_token();
        let t2 = DashboardServer::generate_token();
        assert_ne!(t1, t2, "Two generated tokens should differ");
    }

    #[test]
    fn test_validate_token_valid() {
        let token = "abcdef0123456789abcdef0123456789";
        assert!(DashboardServer::validate_token(token, token));
    }

    #[test]
    fn test_validate_token_invalid() {
        let expected = "abcdef0123456789abcdef0123456789";
        let provided = "0000000000000000abcdef0123456789";
        assert!(!DashboardServer::validate_token(provided, expected));
    }

    #[test]
    fn test_validate_token_different_length() {
        assert!(!DashboardServer::validate_token("short", "abcdef0123456789abcdef0123456789"));
    }

    #[test]
    fn test_dashboard_token_new() {
        let dt = DashboardToken::new();
        assert_eq!(dt.token.len(), 32);
    }
}
