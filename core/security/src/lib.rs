pub mod audit;
pub mod permissions;

pub use audit::AuditLogger;
pub use permissions::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    // ── Permission matching tests ────────────────────────────────

    #[test]
    fn test_fs_read_unscoped_grants_all() {
        let granted = vec![Permission::FsRead(None)];
        let required = Permission::FsRead(Some(PathScope::Exact(PathBuf::from("/etc/hosts"))));
        assert!(matches!(
            check_permission(&granted, &required),
            PermissionVerdict::Allowed
        ));
    }

    #[test]
    fn test_fs_read_scoped_denies_unscoped() {
        let granted = vec![Permission::FsRead(Some(PathScope::Exact(PathBuf::from(
            "/tmp/file",
        ))))];
        let required = Permission::FsRead(None);
        assert!(matches!(
            check_permission(&granted, &required),
            PermissionVerdict::Denied { .. }
        ));
    }

    #[test]
    fn test_recursive_path_scope() {
        let granted = vec![Permission::FsRead(Some(PathScope::Recursive(
            PathBuf::from("/home/user"),
        )))];
        let required = Permission::FsRead(Some(PathScope::Exact(PathBuf::from(
            "/home/user/projects/file.txt",
        ))));
        assert!(matches!(
            check_permission(&granted, &required),
            PermissionVerdict::Allowed
        ));
    }

    #[test]
    fn test_recursive_path_scope_no_match() {
        let granted = vec![Permission::FsRead(Some(PathScope::Recursive(
            PathBuf::from("/home/user"),
        )))];
        let required = Permission::FsRead(Some(PathScope::Exact(PathBuf::from("/etc/passwd"))));
        assert!(matches!(
            check_permission(&granted, &required),
            PermissionVerdict::Denied { .. }
        ));
    }

    #[test]
    fn test_directory_children_scope() {
        let granted = vec![Permission::FsRead(Some(PathScope::DirectoryChildren(
            PathBuf::from("/tmp"),
        )))];
        let required = Permission::FsRead(Some(PathScope::Exact(PathBuf::from("/tmp/file.txt"))));
        assert!(matches!(
            check_permission(&granted, &required),
            PermissionVerdict::Allowed
        ));
    }

    #[test]
    fn test_directory_children_no_recursive() {
        let granted = vec![Permission::FsRead(Some(PathScope::DirectoryChildren(
            PathBuf::from("/tmp"),
        )))];
        let required =
            Permission::FsRead(Some(PathScope::Exact(PathBuf::from("/tmp/sub/file.txt"))));
        assert!(matches!(
            check_permission(&granted, &required),
            PermissionVerdict::Denied { .. }
        ));
    }

    #[test]
    fn test_domain_wildcard_match() {
        let granted = vec![Permission::NetworkOutbound(Some(DomainScope::Wildcard(
            "*.googleapis.com".to_string(),
        )))];
        let required = Permission::NetworkOutbound(Some(DomainScope::Exact(
            "maps.googleapis.com".to_string(),
        )));
        assert!(matches!(
            check_permission(&granted, &required),
            PermissionVerdict::Allowed
        ));
    }

    #[test]
    fn test_domain_wildcard_no_match() {
        let granted = vec![Permission::NetworkOutbound(Some(DomainScope::Wildcard(
            "*.googleapis.com".to_string(),
        )))];
        let required =
            Permission::NetworkOutbound(Some(DomainScope::Exact("api.openai.com".to_string())));
        assert!(matches!(
            check_permission(&granted, &required),
            PermissionVerdict::Denied { .. }
        ));
    }

    #[test]
    fn test_shell_exec_covers_specific_command() {
        let granted = vec![Permission::ShellExec];
        let required = Permission::ShellExecCommand("git".to_string());
        assert!(matches!(
            check_permission(&granted, &required),
            PermissionVerdict::Allowed
        ));
    }

    #[test]
    fn test_specific_command_denies_full_exec() {
        let granted = vec![Permission::ShellExecCommand("git".to_string())];
        let required = Permission::ShellExec;
        assert!(matches!(
            check_permission(&granted, &required),
            PermissionVerdict::Denied { .. }
        ));
    }

    #[test]
    fn test_connector_manage_covers_read() {
        let granted = vec![Permission::ConnectorUse(
            "telegram".to_string(),
            ConnectorAction::Manage,
        )];
        let required = Permission::ConnectorUse("telegram".to_string(), ConnectorAction::Read);
        assert!(matches!(
            check_permission(&granted, &required),
            PermissionVerdict::Allowed
        ));
    }

    #[test]
    fn test_memory_workspace_covers_own() {
        let granted = vec![Permission::MemoryRead(MemoryScope::Workspace)];
        let required = Permission::MemoryRead(MemoryScope::OwnAgent);
        assert!(matches!(
            check_permission(&granted, &required),
            PermissionVerdict::Allowed
        ));
    }

    #[test]
    fn test_domain_with_port() {
        let granted = vec![Permission::NetworkOutbound(Some(DomainScope::Exact(
            "localhost".to_string(),
        )))];
        let required =
            Permission::NetworkOutbound(Some(DomainScope::WithPort("localhost".to_string(), 8080)));
        assert!(matches!(
            check_permission(&granted, &required),
            PermissionVerdict::Allowed
        ));
    }

    // ── Permission parsing tests ─────────────────────────────────

    #[test]
    fn test_parse_fs_read() {
        assert_eq!(
            parse_permission("fs:read").unwrap(),
            Permission::FsRead(None)
        );
    }

    #[test]
    fn test_parse_fs_read_exact() {
        assert_eq!(
            parse_permission("fs:read:/etc/hosts").unwrap(),
            Permission::FsRead(Some(PathScope::Exact(PathBuf::from("/etc/hosts"))))
        );
    }

    #[test]
    fn test_parse_fs_read_recursive() {
        assert_eq!(
            parse_permission("fs:read:/home/user/projects/**").unwrap(),
            Permission::FsRead(Some(PathScope::Recursive(PathBuf::from(
                "/home/user/projects"
            ))))
        );
    }

    #[test]
    fn test_parse_fs_read_directory_children() {
        assert_eq!(
            parse_permission("fs:read:/home/user/.ssh/*").unwrap(),
            Permission::FsRead(Some(PathScope::DirectoryChildren(PathBuf::from(
                "/home/user/.ssh"
            ))))
        );
    }

    #[test]
    fn test_parse_network_outbound() {
        assert_eq!(
            parse_permission("net:outbound:api.openai.com").unwrap(),
            Permission::NetworkOutbound(Some(DomainScope::Exact("api.openai.com".to_string())))
        );
    }

    #[test]
    fn test_parse_network_wildcard() {
        assert_eq!(
            parse_permission("net:outbound:*.googleapis.com").unwrap(),
            Permission::NetworkOutbound(Some(DomainScope::Wildcard(
                "*.googleapis.com".to_string()
            )))
        );
    }

    #[test]
    fn test_parse_connector() {
        assert_eq!(
            parse_permission("connector:telegram:send").unwrap(),
            Permission::ConnectorUse("telegram".to_string(), ConnectorAction::Send)
        );
    }

    #[test]
    fn test_parse_shell_exec() {
        assert_eq!(
            parse_permission("shell:exec").unwrap(),
            Permission::ShellExec
        );
    }

    #[test]
    fn test_parse_shell_exec_command() {
        assert_eq!(
            parse_permission("shell:exec:git").unwrap(),
            Permission::ShellExecCommand("git".to_string())
        );
    }

    #[test]
    fn test_parse_memory_read_own() {
        assert_eq!(
            parse_permission("memory:read:own").unwrap(),
            Permission::MemoryRead(MemoryScope::OwnAgent)
        );
    }

    #[test]
    fn test_parse_unknown_permission() {
        assert!(parse_permission("unknown:thing").is_err());
    }

    // ── Audit log tests ──────────────────────────────────────────

    #[test]
    fn test_audit_log_hmac_chain() {
        let db = nexmind_storage::Database::open_in_memory().expect("failed to open db");
        db.run_migrations().expect("migrations failed");
        let db = Arc::new(db);

        let hmac_key: [u8; 32] = [0x42u8; 32];
        let logger = audit::AuditLogger::new(db, hmac_key);

        // Log 3 events
        logger
            .log_event(
                "ws1",
                "agent",
                "agt_001",
                "tool_exec",
                Some("file"),
                Some("/tmp/x"),
                "success",
                None,
                None,
                "desktop",
                None,
            )
            .expect("log event 1 failed");
        logger
            .log_event(
                "ws1",
                "agent",
                "agt_001",
                "permission_check",
                None,
                None,
                "denied",
                Some("no permission"),
                None,
                "desktop",
                None,
            )
            .expect("log event 2 failed");
        logger
            .log_event(
                "ws1",
                "user",
                "user_001",
                "approval_decision",
                Some("approval"),
                Some("apr_001"),
                "success",
                None,
                None,
                "telegram",
                None,
            )
            .expect("log event 3 failed");

        // Verify chain
        let rows = logger.get_rows(10).expect("failed to get rows");
        assert_eq!(rows.len(), 3);

        let result = audit::verify_audit_chain(&hmac_key, &rows);
        assert!(result.is_none(), "chain should be intact");
    }

    #[test]
    fn test_audit_log_tamper_detection() {
        let db = nexmind_storage::Database::open_in_memory().expect("failed to open db");
        db.run_migrations().expect("migrations failed");
        let db = Arc::new(db);

        let hmac_key: [u8; 32] = [0x42u8; 32];
        let logger = audit::AuditLogger::new(db.clone(), hmac_key);

        logger
            .log_event(
                "ws1",
                "agent",
                "agt_001",
                "tool_exec",
                None,
                None,
                "success",
                None,
                None,
                "desktop",
                None,
            )
            .expect("log failed");
        logger
            .log_event(
                "ws1",
                "agent",
                "agt_001",
                "tool_exec",
                None,
                None,
                "success",
                None,
                None,
                "desktop",
                None,
            )
            .expect("log failed");

        // Tamper with the first row (scope conn so it's dropped before get_rows)
        {
            let conn = db.conn().expect("failed to get connection");
            conn.execute(
                "UPDATE audit_log SET action = 'tampered' WHERE rowid = 1",
                [],
            )
            .expect("tamper failed");
        }

        let rows = logger.get_rows(10).expect("failed to get rows");
        let result = audit::verify_audit_chain(&hmac_key, &rows);
        assert!(result.is_some(), "tampered chain should be detected");
    }

    #[test]
    fn test_hmac_genesis() {
        let key: [u8; 32] = [0xABu8; 32];
        let hmac1 = audit::compute_row_hmac(
            &key,
            "id1",
            "2026-01-01",
            "actor1",
            "action1",
            "success",
            None,
        );
        let hmac2 = audit::compute_row_hmac(
            &key,
            "id1",
            "2026-01-01",
            "actor1",
            "action1",
            "success",
            None,
        );
        assert_eq!(hmac1, hmac2, "same inputs should produce same HMAC");

        let hmac3 = audit::compute_row_hmac(
            &key,
            "id1",
            "2026-01-01",
            "actor1",
            "action1",
            "success",
            Some("prev"),
        );
        assert_ne!(
            hmac1, hmac3,
            "different prev_hmac should produce different HMAC"
        );
    }
}
