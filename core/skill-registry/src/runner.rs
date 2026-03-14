use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::registry::InstalledSkill;
use crate::manifest::SkillRuntimeType;

/// Configuration for the script skill sandbox.
#[derive(Debug, Clone)]
pub struct ScriptSandboxConfig {
    pub allowed_interpreters: Vec<String>,
    pub timeout_seconds: u64,
    pub max_output_bytes: usize,
    pub network_allowed: bool,
}

impl Default for ScriptSandboxConfig {
    fn default() -> Self {
        Self {
            allowed_interpreters: vec!["python3".into(), "python".into(), "bash".into(), "node".into()],
            timeout_seconds: 30,
            max_output_bytes: 1_048_576, // 1MB
            network_allowed: true,
        }
    }
}

/// Result of a script execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub parsed_output: Option<serde_json::Value>,
}

/// Error from script execution.
#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error("interpreter not found: {0}")]
    InterpreterNotFound(String),
    #[error("interpreter not allowed: {0}")]
    InterpreterNotAllowed(String),
    #[error("script not found: {0}")]
    ScriptNotFound(String),
    #[error("skill has no script entry")]
    NoEntry,
    #[error("skill is not a script type")]
    NotScriptType,
    #[error("execution timed out after {0}s")]
    Timeout(u64),
    #[error("execution failed: {0}")]
    ExecutionFailed(String),
    #[error("output too large: {0} bytes (max {1})")]
    OutputTooLarge(usize, usize),
}

/// Runs script-based skills as subprocesses.
pub struct ScriptSkillRunner {
    config: ScriptSandboxConfig,
}

impl ScriptSkillRunner {
    pub fn new(config: ScriptSandboxConfig) -> Self {
        Self { config }
    }

    pub fn with_default_config() -> Self {
        Self::new(ScriptSandboxConfig::default())
    }

    /// Execute a script-based skill with the given arguments.
    pub async fn execute(
        &self,
        skill: &InstalledSkill,
        _tool_id: &str,
        args: serde_json::Value,
    ) -> Result<ScriptResult, ScriptError> {
        // Validate runtime type
        if skill.manifest.runtime.runtime_type != SkillRuntimeType::Script {
            return Err(ScriptError::NotScriptType);
        }

        // Get interpreter
        let interpreter = skill
            .manifest
            .runtime
            .interpreter
            .as_deref()
            .ok_or_else(|| ScriptError::InterpreterNotFound("no interpreter specified".into()))?;

        if !self.config.allowed_interpreters.contains(&interpreter.to_string()) {
            return Err(ScriptError::InterpreterNotAllowed(interpreter.into()));
        }

        // Get entry script
        let entry = skill
            .manifest
            .runtime
            .entry
            .as_deref()
            .ok_or(ScriptError::NoEntry)?;

        // Resolve script path
        let script_path = if let Some(ref dir) = skill.skill_dir {
            dir.join(entry)
        } else {
            return Err(ScriptError::ScriptNotFound(entry.into()));
        };

        if !script_path.exists() {
            return Err(ScriptError::ScriptNotFound(script_path.display().to_string()));
        }

        self.run_script(interpreter, &script_path, args).await
    }

    /// Run a script with the given interpreter and args.
    async fn run_script(
        &self,
        interpreter: &str,
        script_path: &Path,
        args: serde_json::Value,
    ) -> Result<ScriptResult, ScriptError> {
        let args_json = serde_json::to_string(&args)
            .map_err(|e| ScriptError::ExecutionFailed(format!("failed to serialize args: {}", e)))?;

        let timeout = Duration::from_secs(
            self.config.timeout_seconds.min(
                // Use skill's timeout if specified, otherwise use config default
                self.config.timeout_seconds,
            ),
        );

        info!(
            interpreter = %interpreter,
            script = %script_path.display(),
            "executing script skill"
        );

        let result = tokio::time::timeout(timeout, async {
            let output = tokio::process::Command::new(interpreter)
                .arg(script_path)
                .arg(&args_json)
                .output()
                .await
                .map_err(|e| ScriptError::ExecutionFailed(format!("spawn failed: {}", e)))?;

            Ok::<_, ScriptError>(output)
        })
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                // Check output size
                if stdout.len() > self.config.max_output_bytes {
                    return Err(ScriptError::OutputTooLarge(
                        stdout.len(),
                        self.config.max_output_bytes,
                    ));
                }

                // Try to parse stdout as JSON
                let parsed_output = serde_json::from_str(&stdout).ok();

                let exit_code = output.status.code().unwrap_or(-1);

                Ok(ScriptResult {
                    stdout,
                    stderr,
                    exit_code,
                    parsed_output,
                })
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(ScriptError::Timeout(self.config.timeout_seconds)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::SkillManifest;
    use crate::registry::{InstalledSkill, SkillSource, SkillStatus};

    fn make_script_skill(dir: &Path, interpreter: &str, entry: &str) -> InstalledSkill {
        let yaml = format!(
            r#"
id: test-script
name: "Test Script"
version: "1.0.0"
description: "Test script skill"
runtime:
  type: script
  interpreter: {interpreter}
  entry: {entry}
  timeout_seconds: 5
provides:
  tools:
    - id: test_tool
      description: "Test tool"
"#
        );
        let manifest = SkillManifest::from_yaml(&yaml).unwrap();
        InstalledSkill {
            id: "test-script".into(),
            name: "Test Script".into(),
            version: "1.0.0".into(),
            description: "Test script skill".into(),
            manifest,
            status: SkillStatus::Active,
            installed_at: "2025-01-01T00:00:00Z".into(),
            source: SkillSource::Generated,
            skill_dir: Some(dir.to_path_buf()),
        }
    }

    #[tokio::test]
    async fn test_execute_python_script() {
        let tmp = tempfile::tempdir().unwrap();

        // Create a simple Python script
        let script = r#"
import json, sys
args = json.loads(sys.argv[1])
result = {"greeting": f"Hello, {args.get('name', 'World')}!"}
print(json.dumps(result))
"#;
        std::fs::write(tmp.path().join("hello.py"), script).unwrap();

        let skill = make_script_skill(tmp.path(), "python3", "hello.py");
        let runner = ScriptSkillRunner::with_default_config();

        // python3 may not be available on all systems; try python too
        let result = runner
            .execute(&skill, "test_tool", serde_json::json!({"name": "NexMind"}))
            .await;

        // If python3 is not available, skip this test gracefully
        match result {
            Ok(res) => {
                assert_eq!(res.exit_code, 0);
                assert!(res.parsed_output.is_some());
                let output = res.parsed_output.unwrap();
                assert_eq!(output["greeting"], "Hello, NexMind!");
            }
            Err(ScriptError::ExecutionFailed(msg)) if msg.contains("spawn failed") => {
                // Python not available on this system, skip
                eprintln!("Skipping: python3 not available");
            }
            Err(e) => panic!("unexpected error: {}", e),
        }
    }

    #[tokio::test]
    async fn test_reject_non_script_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml = r#"
id: native-skill
name: "Native"
version: "1.0.0"
description: "Native skill"
runtime:
  type: native
"#;
        let manifest = SkillManifest::from_yaml(yaml).unwrap();
        let skill = InstalledSkill {
            id: "native-skill".into(),
            name: "Native".into(),
            version: "1.0.0".into(),
            description: "Native skill".into(),
            manifest,
            status: SkillStatus::Active,
            installed_at: "2025-01-01T00:00:00Z".into(),
            source: SkillSource::Builtin,
            skill_dir: Some(tmp.path().to_path_buf()),
        };

        let runner = ScriptSkillRunner::with_default_config();
        let result = runner.execute(&skill, "tool", serde_json::json!({})).await;
        assert!(matches!(result, Err(ScriptError::NotScriptType)));
    }

    #[tokio::test]
    async fn test_reject_disallowed_interpreter() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("evil.rb"), "puts 'hello'").unwrap();
        let skill = make_script_skill(tmp.path(), "ruby", "evil.rb");

        let runner = ScriptSkillRunner::with_default_config();
        let result = runner.execute(&skill, "tool", serde_json::json!({})).await;
        assert!(matches!(result, Err(ScriptError::InterpreterNotAllowed(_))));
    }

    #[tokio::test]
    async fn test_script_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = make_script_skill(tmp.path(), "python3", "nonexistent.py");

        let runner = ScriptSkillRunner::with_default_config();
        let result = runner.execute(&skill, "tool", serde_json::json!({})).await;
        assert!(matches!(result, Err(ScriptError::ScriptNotFound(_))));
    }

    #[test]
    fn test_sandbox_config_defaults() {
        let config = ScriptSandboxConfig::default();
        assert!(config.allowed_interpreters.contains(&"python3".to_string()));
        assert!(config.allowed_interpreters.contains(&"bash".to_string()));
        assert_eq!(config.timeout_seconds, 30);
        assert_eq!(config.max_output_bytes, 1_048_576);
    }
}
