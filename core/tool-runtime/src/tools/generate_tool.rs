use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};

/// Generate a new script-based tool/skill.
///
/// Creates a Python script in `generated_skills/{name}/` under the workspace
/// path, together with a `skill.yaml` manifest. The tool has trust_level 2,
/// meaning the registry will return `NeedsApproval` before `execute()` is
/// called, so the user must approve tool generation.
pub struct GenerateToolTool;

#[async_trait::async_trait]
impl Tool for GenerateToolTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            id: "generate_tool".into(),
            name: "generate_tool".into(),
            description: "Generate a new script-based tool/skill. Creates a Python script \
                that the agent can use. Requires approval before activation."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tool name in kebab-case (e.g. 'csv-parser')"
                    },
                    "description": {
                        "type": "string",
                        "description": "What the tool does"
                    },
                    "parameters": {
                        "type": "object",
                        "description": "Tool parameters schema (JSON Schema object)"
                    },
                    "script": {
                        "type": "string",
                        "description": "Python script source code. Must read JSON args from sys.argv[1] and print JSON result to stdout."
                    }
                },
                "required": ["name", "description", "script"]
            }),
            required_permissions: vec![],
            trust_level: 2, // requires approval
            idempotent: false,
            timeout_seconds: 60,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::ValidationError("'name' is required".into()))?;

        // Validate kebab-case: lowercase alphanumeric and hyphens only
        if name.is_empty() {
            return Err(ToolError::ValidationError(
                "'name' must not be empty".into(),
            ));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(ToolError::ValidationError(
                "'name' must be kebab-case (lowercase letters, digits, and hyphens only)".into(),
            ));
        }
        if name.starts_with('-') || name.ends_with('-') {
            return Err(ToolError::ValidationError(
                "'name' must not start or end with a hyphen".into(),
            ));
        }

        if args
            .get("description")
            .and_then(|v| v.as_str())
            .map_or(true, |s| s.is_empty())
        {
            return Err(ToolError::ValidationError(
                "'description' is required and must be non-empty".into(),
            ));
        }

        if args
            .get("script")
            .and_then(|v| v.as_str())
            .map_or(true, |s| s.is_empty())
        {
            return Err(ToolError::ValidationError(
                "'script' is required and must be non-empty".into(),
            ));
        }

        Ok(())
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let name = args["name"].as_str().unwrap();
        let description = args["description"].as_str().unwrap();
        let script = args["script"].as_str().unwrap();
        let parameters = args.get("parameters");

        // Build skill directory path
        let skill_dir = ctx.workspace_path.join("generated_skills").join(name);

        // Create directory
        std::fs::create_dir_all(&skill_dir).map_err(|e| {
            ToolError::ExecutionError(format!(
                "failed to create skill directory {}: {}",
                skill_dir.display(),
                e
            ))
        })?;

        // Build the skill.yaml manifest
        let manifest_yaml = build_skill_yaml(name, description, parameters);

        // Write skill.yaml
        let manifest_path = skill_dir.join("skill.yaml");
        std::fs::write(&manifest_path, &manifest_yaml).map_err(|e| {
            ToolError::ExecutionError(format!(
                "failed to write {}: {}",
                manifest_path.display(),
                e
            ))
        })?;

        // Write main.py
        let script_path = skill_dir.join("main.py");
        std::fs::write(&script_path, script).map_err(|e| {
            ToolError::ExecutionError(format!(
                "failed to write {}: {}",
                script_path.display(),
                e
            ))
        })?;

        tracing::info!(
            skill_name = name,
            skill_dir = %skill_dir.display(),
            "generate_tool: created script-based skill"
        );

        Ok(ToolOutput::Success {
            result: json!({
                "generated": true,
                "name": name,
                "description": description,
                "skill_dir": skill_dir.display().to_string(),
                "manifest_path": manifest_path.display().to_string(),
                "script_path": script_path.display().to_string(),
                "runtime": {
                    "type": "script",
                    "interpreter": "python3",
                    "entry": "main.py"
                }
            }),
            tokens_used: None,
        })
    }
}

/// Build a YAML manifest string for a generated script-based skill.
fn build_skill_yaml(name: &str, description: &str, parameters: Option<&Value>) -> String {
    let mut yaml = format!(
        r#"id: {name}
name: "{name}"
version: "1.0.0"
description: "{description}"
author: "agent-generated"
tags:
  - generated
  - script

runtime:
  type: script
  interpreter: python3
  entry: main.py
  timeout_seconds: 30

provides:
  tools:
    - id: {name}
      description: "{description}"
"#,
        name = name,
        description = description.replace('"', "\\\""),
    );

    // Append parameter definitions if provided
    if let Some(params) = parameters {
        if let Some(obj) = params.as_object() {
            if !obj.is_empty() {
                yaml.push_str("      parameters:\n");
                for (key, val) in obj {
                    let param_type = val
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("string");
                    let required = val
                        .get("required")
                        .and_then(|r| r.as_bool())
                        .unwrap_or(false);
                    let desc = val
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("");
                    yaml.push_str(&format!(
                        "        {key}: {{ type: {param_type}, required: {required}, description: \"{desc}\" }}\n",
                        key = key,
                        param_type = param_type,
                        required = required,
                        desc = desc.replace('"', "\\\""),
                    ));
                }
            }
        }
    }

    yaml
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_ctx(workspace_path: &std::path::Path) -> ToolContext {
        ToolContext {
            agent_id: "agt_test".into(),
            workspace_id: "ws_test".into(),
            workspace_path: workspace_path.to_path_buf(),
            granted_permissions: vec![],
            correlation_id: "corr_test".into(),
        }
    }

    #[test]
    fn test_definition_has_trust_level_2() {
        let tool = GenerateToolTool;
        let def = tool.definition();
        assert_eq!(def.trust_level, 2);
        assert_eq!(def.id, "generate_tool");
        assert_eq!(def.name, "generate_tool");
        assert!(!def.idempotent);
    }

    #[test]
    fn test_valid_args_pass_validation() {
        let tool = GenerateToolTool;
        let args = json!({
            "name": "my-tool",
            "description": "A test tool",
            "script": "import sys\nprint('{}')"
        });
        assert!(tool.validate_args(&args).is_ok());
    }

    #[test]
    fn test_valid_args_with_parameters_pass_validation() {
        let tool = GenerateToolTool;
        let args = json!({
            "name": "csv-parser",
            "description": "Parses CSV files",
            "parameters": {
                "file_path": {"type": "string", "required": true, "description": "Path to CSV"},
                "delimiter": {"type": "string", "required": false}
            },
            "script": "import sys, json\nargs = json.loads(sys.argv[1])\nprint(json.dumps({}))"
        });
        assert!(tool.validate_args(&args).is_ok());
    }

    #[test]
    fn test_missing_name_fails_validation() {
        let tool = GenerateToolTool;
        let args = json!({
            "description": "A test tool",
            "script": "print('hi')"
        });
        let err = tool.validate_args(&args).unwrap_err();
        assert!(matches!(err, ToolError::ValidationError(_)));
        assert!(err.to_string().contains("name"));
    }

    #[test]
    fn test_missing_script_fails_validation() {
        let tool = GenerateToolTool;
        let args = json!({
            "name": "my-tool",
            "description": "A test tool"
        });
        let err = tool.validate_args(&args).unwrap_err();
        assert!(matches!(err, ToolError::ValidationError(_)));
        assert!(err.to_string().contains("script"));
    }

    #[test]
    fn test_missing_description_fails_validation() {
        let tool = GenerateToolTool;
        let args = json!({
            "name": "my-tool",
            "script": "print('hi')"
        });
        let err = tool.validate_args(&args).unwrap_err();
        assert!(matches!(err, ToolError::ValidationError(_)));
        assert!(err.to_string().contains("description"));
    }

    #[test]
    fn test_invalid_name_fails_validation() {
        let tool = GenerateToolTool;

        // Uppercase
        let args = json!({"name": "MyTool", "description": "test", "script": "x"});
        assert!(tool.validate_args(&args).is_err());

        // Spaces
        let args = json!({"name": "my tool", "description": "test", "script": "x"});
        assert!(tool.validate_args(&args).is_err());

        // Leading hyphen
        let args = json!({"name": "-my-tool", "description": "test", "script": "x"});
        assert!(tool.validate_args(&args).is_err());

        // Trailing hyphen
        let args = json!({"name": "my-tool-", "description": "test", "script": "x"});
        assert!(tool.validate_args(&args).is_err());

        // Empty
        let args = json!({"name": "", "description": "test", "script": "x"});
        assert!(tool.validate_args(&args).is_err());
    }

    #[tokio::test]
    async fn test_execute_creates_skill_directory_and_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let tool = GenerateToolTool;

        let args = json!({
            "name": "test-skill",
            "description": "A test skill for unit testing",
            "parameters": {
                "input": {"type": "string", "required": true, "description": "Input data"}
            },
            "script": "import sys, json\nargs = json.loads(sys.argv[1])\nprint(json.dumps({'result': args['input']}))"
        });

        let result = tool.execute(args, &ctx).await.unwrap();

        match result {
            ToolOutput::Success { result, .. } => {
                assert!(result["generated"].as_bool().unwrap());
                assert_eq!(result["name"], "test-skill");
            }
            other => panic!("expected success, got {:?}", other),
        }

        // Verify files exist
        let skill_dir = tmp.path().join("generated_skills").join("test-skill");
        assert!(skill_dir.exists(), "skill directory should exist");
        assert!(skill_dir.join("skill.yaml").exists(), "skill.yaml should exist");
        assert!(skill_dir.join("main.py").exists(), "main.py should exist");

        // Verify main.py content
        let script_content = std::fs::read_to_string(skill_dir.join("main.py")).unwrap();
        assert!(script_content.contains("import sys, json"));
    }

    #[tokio::test]
    async fn test_generated_skill_yaml_is_valid_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let tool = GenerateToolTool;

        let args = json!({
            "name": "yaml-check",
            "description": "Skill for testing YAML validity",
            "parameters": {
                "query": {"type": "string", "required": true, "description": "Search query"}
            },
            "script": "print('{}')"
        });

        tool.execute(args, &ctx).await.unwrap();

        // Parse the generated skill.yaml with SkillManifest
        let manifest_path = tmp
            .path()
            .join("generated_skills")
            .join("yaml-check")
            .join("skill.yaml");
        let yaml_content = std::fs::read_to_string(&manifest_path).unwrap();
        let manifest = nexmind_skill_registry::SkillManifest::from_yaml(&yaml_content)
            .expect("generated skill.yaml should be a valid SkillManifest");

        assert_eq!(manifest.id, "yaml-check");
        assert_eq!(manifest.name, "yaml-check");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.author, "agent-generated");
        assert!(manifest.tags.contains(&"generated".to_string()));
        assert_eq!(
            manifest.runtime.runtime_type,
            nexmind_skill_registry::SkillRuntimeType::Script
        );
        assert_eq!(manifest.runtime.interpreter.as_deref(), Some("python3"));
        assert_eq!(manifest.runtime.entry.as_deref(), Some("main.py"));
        assert_eq!(manifest.provides.tools.len(), 1);
        assert_eq!(manifest.provides.tools[0].id, "yaml-check");
    }
}
