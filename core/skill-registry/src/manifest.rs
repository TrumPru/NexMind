use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Skill manifest parsed from skill.yaml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default = "default_author")]
    pub author: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub provides: SkillProvides,
    #[serde(default)]
    pub requires: SkillRequires,
    #[serde(default)]
    pub runtime: SkillRuntime,
}

fn default_author() -> String {
    "unknown".into()
}

/// What this skill provides: tools, agent templates, workflows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillProvides {
    #[serde(default)]
    pub tools: Vec<SkillToolDef>,
    #[serde(default)]
    pub agent_templates: Vec<SkillAgentTemplate>,
    #[serde(default)]
    pub workflows: Vec<SkillWorkflowDef>,
}

/// Tool definition within a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillToolDef {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub parameters: HashMap<String, SkillParamDef>,
}

/// Tool parameter definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillParamDef {
    #[serde(rename = "type")]
    pub param_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Agent template provided by a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillAgentTemplate {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// Workflow definition within a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillWorkflowDef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub trigger: Option<SkillTrigger>,
}

/// Trigger for a skill workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTrigger {
    #[serde(rename = "type")]
    pub trigger_type: String,
    #[serde(default)]
    pub schedule: Option<String>,
}

/// What this skill requires: permissions, config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillRequires {
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub config: HashMap<String, SkillConfigField>,
}

/// Configuration field required by a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillConfigField {
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: Option<String>,
}

/// How the skill runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRuntime {
    #[serde(rename = "type", default = "default_runtime_type")]
    pub runtime_type: SkillRuntimeType,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub interpreter: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn default_runtime_type() -> SkillRuntimeType {
    SkillRuntimeType::Native
}

fn default_timeout() -> u64 {
    30
}

impl Default for SkillRuntime {
    fn default() -> Self {
        Self {
            runtime_type: SkillRuntimeType::Native,
            entry: None,
            interpreter: None,
            timeout_seconds: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SkillRuntimeType {
    Native,
    Script,
    Wasm,
}

impl SkillManifest {
    /// Parse a skill manifest from YAML string.
    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Parse a skill manifest from a YAML file.
    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        Self::from_yaml(&content).map_err(|e| format!("failed to parse {}: {}", path.display(), e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_manifest() {
        let yaml = r#"
id: weather-briefing
name: "Weather Briefing"
version: "1.0.0"
description: "Fetches weather forecasts and formats them as briefings"
author: "nexmind-builtin"
tags: ["weather", "briefing", "daily"]

provides:
  tools:
    - id: get_weather
      description: "Get current weather and forecast for a city"
      parameters:
        city: { type: string, required: true }
        days: { type: integer, default: 1 }
  agent_templates:
    - id: weather-agent
      name: "Weather Agent"
      description: "Daily weather briefing agent"
  workflows:
    - id: morning-weather
      name: "Morning Weather Check"
      trigger: { type: cron, schedule: "0 7 * * *" }

requires:
  permissions:
    - "network:outbound:api.openweathermap.org"
    - "network:outbound:wttr.in"
  config:
    openweathermap_api_key:
      type: string
      required: false
      description: "Optional: for premium forecasts"

runtime:
  type: native
  entry: "builtin:weather_briefing"
"#;

        let manifest = SkillManifest::from_yaml(yaml).unwrap();
        assert_eq!(manifest.id, "weather-briefing");
        assert_eq!(manifest.name, "Weather Briefing");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.author, "nexmind-builtin");
        assert_eq!(manifest.tags.len(), 3);
        assert_eq!(manifest.provides.tools.len(), 1);
        assert_eq!(manifest.provides.tools[0].id, "get_weather");
        assert_eq!(manifest.provides.tools[0].parameters.len(), 2);
        assert_eq!(manifest.provides.agent_templates.len(), 1);
        assert_eq!(manifest.provides.workflows.len(), 1);
        assert_eq!(manifest.requires.permissions.len(), 2);
        assert_eq!(manifest.requires.config.len(), 1);
        assert_eq!(manifest.runtime.runtime_type, SkillRuntimeType::Native);
    }

    #[test]
    fn test_parse_minimal_manifest() {
        let yaml = r#"
id: simple
name: "Simple Skill"
version: "1.0.0"
description: "A simple skill with minimal config"
"#;
        let manifest = SkillManifest::from_yaml(yaml).unwrap();
        assert_eq!(manifest.id, "simple");
        assert_eq!(manifest.author, "unknown");
        assert!(manifest.provides.tools.is_empty());
        assert!(manifest.requires.permissions.is_empty());
    }

    #[test]
    fn test_parse_script_runtime() {
        let yaml = r#"
id: currency-converter
name: "Currency Converter"
version: "1.0.0"
description: "Convert between currencies"
runtime:
  type: script
  interpreter: python3
  entry: convert.py
  timeout_seconds: 15
provides:
  tools:
    - id: convert_currency
      description: "Convert amount between currencies"
      parameters:
        from: { type: string, required: true }
        to: { type: string, required: true }
        amount: { type: number, required: true }
"#;
        let manifest = SkillManifest::from_yaml(yaml).unwrap();
        assert_eq!(manifest.runtime.runtime_type, SkillRuntimeType::Script);
        assert_eq!(manifest.runtime.interpreter.as_deref(), Some("python3"));
        assert_eq!(manifest.runtime.entry.as_deref(), Some("convert.py"));
        assert_eq!(manifest.runtime.timeout_seconds, 15);
    }

    #[test]
    fn test_manifest_serialization_roundtrip() {
        let yaml = r#"
id: test-skill
name: "Test Skill"
version: "0.1.0"
description: "Test"
tags: ["test"]
provides:
  tools:
    - id: do_thing
      description: "Does a thing"
"#;
        let manifest = SkillManifest::from_yaml(yaml).unwrap();
        let json = serde_json::to_string(&manifest).unwrap();
        let back: SkillManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "test-skill");
        assert_eq!(back.provides.tools.len(), 1);
    }
}
