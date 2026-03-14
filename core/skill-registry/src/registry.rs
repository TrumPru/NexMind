use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::manifest::SkillManifest;

/// Status of an installed skill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillStatus {
    Active,
    Disabled,
    Error(String),
}

/// Where a skill was installed from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    Builtin,
    LocalFile(PathBuf),
    Git(String),
    Generated,
}

/// An installed skill with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSkill {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub manifest: SkillManifest,
    pub status: SkillStatus,
    pub installed_at: String,
    pub source: SkillSource,
    pub skill_dir: Option<PathBuf>,
}

/// Error type for skill registry operations.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("skill not found: {0}")]
    NotFound(String),
    #[error("skill already installed: {0}")]
    AlreadyInstalled(String),
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
    #[error("io error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("skill error: {0}")]
    Other(String),
}

/// Skill registry — manages installed skills.
pub struct SkillRegistry {
    skills: RwLock<HashMap<String, InstalledSkill>>,
    skills_dir: PathBuf,
}

impl SkillRegistry {
    /// Create a new skill registry with the given skills directory.
    pub fn new(skills_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&skills_dir).ok();
        Self {
            skills: RwLock::new(HashMap::new()),
            skills_dir,
        }
    }

    /// Install a skill from a manifest + source.
    pub fn install(&self, manifest: SkillManifest, source: SkillSource, skill_dir: Option<PathBuf>) -> Result<String, SkillError> {
        let mut skills = self.skills.write().unwrap();

        if skills.contains_key(&manifest.id) {
            return Err(SkillError::AlreadyInstalled(manifest.id.clone()));
        }

        let id = manifest.id.clone();
        let skill = InstalledSkill {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            description: manifest.description.clone(),
            manifest,
            status: SkillStatus::Active,
            installed_at: chrono::Utc::now().to_rfc3339(),
            source,
            skill_dir,
        };

        info!(skill_id = %id, "skill installed");
        skills.insert(id.clone(), skill);
        Ok(id)
    }

    /// Install a skill from a directory containing skill.yaml.
    pub fn install_from_dir(&self, dir: &Path) -> Result<String, SkillError> {
        let manifest_path = dir.join("skill.yaml");
        if !manifest_path.exists() {
            return Err(SkillError::InvalidManifest(format!(
                "skill.yaml not found in {}",
                dir.display()
            )));
        }

        let manifest = SkillManifest::from_file(&manifest_path)
            .map_err(SkillError::InvalidManifest)?;

        // Copy skill dir to skills_dir if it's not already there
        let target_dir = self.skills_dir.join(&manifest.id);
        if dir != target_dir {
            if target_dir.exists() {
                return Err(SkillError::AlreadyInstalled(manifest.id.clone()));
            }
            copy_dir_recursive(dir, &target_dir)?;
        }

        self.install(manifest, SkillSource::LocalFile(dir.to_path_buf()), Some(target_dir))
    }

    /// Uninstall a skill by ID.
    pub fn uninstall(&self, skill_id: &str) -> Result<(), SkillError> {
        let mut skills = self.skills.write().unwrap();
        skills.remove(skill_id).ok_or_else(|| SkillError::NotFound(skill_id.into()))?;
        info!(skill_id = %skill_id, "skill uninstalled");
        Ok(())
    }

    /// Set skill status (enable/disable).
    pub fn set_status(&self, skill_id: &str, status: SkillStatus) -> Result<(), SkillError> {
        let mut skills = self.skills.write().unwrap();
        let skill = skills.get_mut(skill_id).ok_or_else(|| SkillError::NotFound(skill_id.into()))?;
        skill.status = status;
        Ok(())
    }

    /// List all installed skills.
    pub fn list(&self) -> Vec<InstalledSkill> {
        let skills = self.skills.read().unwrap();
        skills.values().cloned().collect()
    }

    /// Get a skill by ID.
    pub fn get(&self, skill_id: &str) -> Result<InstalledSkill, SkillError> {
        let skills = self.skills.read().unwrap();
        skills.get(skill_id).cloned().ok_or_else(|| SkillError::NotFound(skill_id.into()))
    }

    /// Search installed skills by keyword (searches id, name, description, tags).
    pub fn search(&self, query: &str) -> Vec<InstalledSkill> {
        let query_lower = query.to_lowercase();
        let skills = self.skills.read().unwrap();
        skills
            .values()
            .filter(|s| {
                s.id.to_lowercase().contains(&query_lower)
                    || s.name.to_lowercase().contains(&query_lower)
                    || s.description.to_lowercase().contains(&query_lower)
                    || s.manifest.tags.iter().any(|t| t.to_lowercase().contains(&query_lower))
            })
            .cloned()
            .collect()
    }

    /// Find skills that might help with a given task description.
    pub fn find_for_task(&self, task_description: &str) -> Vec<InstalledSkill> {
        let desc_lower = task_description.to_lowercase();
        let skills = self.skills.read().unwrap();
        skills
            .values()
            .filter(|s| {
                if s.status != SkillStatus::Active {
                    return false;
                }
                // Check if any skill tags, tool descriptions, or skill name/description match
                let matches_tags = s.manifest.tags.iter().any(|t| desc_lower.contains(&t.to_lowercase()));
                let matches_name = desc_lower.contains(&s.name.to_lowercase());
                let matches_desc = desc_lower.contains(&s.id.to_lowercase());
                let matches_tools = s.manifest.provides.tools.iter().any(|t| {
                    desc_lower.contains(&t.id.to_lowercase())
                        || desc_lower.contains(&t.description.to_lowercase())
                });
                matches_tags || matches_name || matches_desc || matches_tools
            })
            .cloned()
            .collect()
    }

    /// Get the number of active skills.
    pub fn active_count(&self) -> usize {
        let skills = self.skills.read().unwrap();
        skills.values().filter(|s| s.status == SkillStatus::Active).count()
    }

    /// Get the skills directory path.
    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }

    /// Load all skills from the skills directory.
    pub fn load_from_dir(&self) -> Result<usize, SkillError> {
        let mut count = 0;
        if !self.skills_dir.exists() {
            return Ok(0);
        }

        for entry in std::fs::read_dir(&self.skills_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let manifest_path = path.join("skill.yaml");
                if manifest_path.exists() {
                    match SkillManifest::from_file(&manifest_path) {
                        Ok(manifest) => {
                            let source = SkillSource::LocalFile(path.clone());
                            match self.install(manifest, source, Some(path)) {
                                Ok(_) => count += 1,
                                Err(SkillError::AlreadyInstalled(_)) => {}
                                Err(e) => {
                                    tracing::warn!(error = %e, "failed to load skill");
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to parse skill manifest");
                        }
                    }
                }
            }
        }

        Ok(count)
    }
}

/// Recursively copy a directory.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::SkillManifest;

    fn test_manifest(id: &str, name: &str) -> SkillManifest {
        let yaml = format!(
            r#"
id: {id}
name: "{name}"
version: "1.0.0"
description: "Test skill: {name}"
tags: ["test", "{id}"]
provides:
  tools:
    - id: {id}_tool
      description: "Tool from {name}"
"#
        );
        SkillManifest::from_yaml(&yaml).unwrap()
    }

    #[test]
    fn test_install_and_list() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SkillRegistry::new(tmp.path().join("skills"));

        let manifest = test_manifest("weather", "Weather");
        registry.install(manifest, SkillSource::Builtin, None).unwrap();

        let skills = registry.list();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].id, "weather");
        assert_eq!(skills[0].status, SkillStatus::Active);
    }

    #[test]
    fn test_install_duplicate_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SkillRegistry::new(tmp.path().join("skills"));

        let manifest = test_manifest("weather", "Weather");
        registry.install(manifest.clone(), SkillSource::Builtin, None).unwrap();

        let result = registry.install(manifest, SkillSource::Builtin, None);
        assert!(matches!(result, Err(SkillError::AlreadyInstalled(_))));
    }

    #[test]
    fn test_uninstall() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SkillRegistry::new(tmp.path().join("skills"));

        let manifest = test_manifest("weather", "Weather");
        registry.install(manifest, SkillSource::Builtin, None).unwrap();
        assert_eq!(registry.list().len(), 1);

        registry.uninstall("weather").unwrap();
        assert_eq!(registry.list().len(), 0);
    }

    #[test]
    fn test_uninstall_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SkillRegistry::new(tmp.path().join("skills"));
        assert!(matches!(registry.uninstall("nope"), Err(SkillError::NotFound(_))));
    }

    #[test]
    fn test_set_status() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SkillRegistry::new(tmp.path().join("skills"));

        let manifest = test_manifest("weather", "Weather");
        registry.install(manifest, SkillSource::Builtin, None).unwrap();

        registry.set_status("weather", SkillStatus::Disabled).unwrap();
        let skill = registry.get("weather").unwrap();
        assert_eq!(skill.status, SkillStatus::Disabled);

        registry.set_status("weather", SkillStatus::Active).unwrap();
        let skill = registry.get("weather").unwrap();
        assert_eq!(skill.status, SkillStatus::Active);
    }

    #[test]
    fn test_search_by_keyword() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SkillRegistry::new(tmp.path().join("skills"));

        registry.install(test_manifest("weather", "Weather Briefing"), SkillSource::Builtin, None).unwrap();
        registry.install(test_manifest("math", "Calculator"), SkillSource::Builtin, None).unwrap();
        registry.install(test_manifest("translator", "Translator"), SkillSource::Builtin, None).unwrap();

        let results = registry.search("weather");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "weather");

        let results = registry.search("calc");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "math");
    }

    #[test]
    fn test_find_for_task() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SkillRegistry::new(tmp.path().join("skills"));

        registry.install(test_manifest("weather", "Weather"), SkillSource::Builtin, None).unwrap();
        registry.install(test_manifest("math", "Calculator"), SkillSource::Builtin, None).unwrap();

        let results = registry.find_for_task("What's the weather like today?");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "weather");
    }

    #[test]
    fn test_get_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SkillRegistry::new(tmp.path().join("skills"));

        registry.install(test_manifest("weather", "Weather"), SkillSource::Builtin, None).unwrap();

        let skill = registry.get("weather").unwrap();
        assert_eq!(skill.id, "weather");

        let err = registry.get("nonexistent");
        assert!(matches!(err, Err(SkillError::NotFound(_))));
    }

    #[test]
    fn test_install_from_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("skill.yaml"),
            r#"
id: my-skill
name: "My Skill"
version: "1.0.0"
description: "A test skill"
provides:
  tools:
    - id: my_tool
      description: "Does something"
"#,
        )
        .unwrap();

        let registry = SkillRegistry::new(tmp.path().join("skills"));
        let id = registry.install_from_dir(&skill_dir).unwrap();
        assert_eq!(id, "my-skill");

        let skill = registry.get("my-skill").unwrap();
        assert_eq!(skill.manifest.provides.tools.len(), 1);
    }

    #[test]
    fn test_load_from_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        // Create two skill directories
        for name in &["skill-a", "skill-b"] {
            let dir = skills_dir.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("skill.yaml"),
                format!(
                    r#"
id: {name}
name: "Skill {name}"
version: "1.0.0"
description: "Test skill {name}"
"#
                ),
            )
            .unwrap();
        }

        let registry = SkillRegistry::new(skills_dir);
        let count = registry.load_from_dir().unwrap();
        assert_eq!(count, 2);
        assert_eq!(registry.list().len(), 2);
    }

    #[test]
    fn test_active_count() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SkillRegistry::new(tmp.path().join("skills"));

        registry.install(test_manifest("a", "A"), SkillSource::Builtin, None).unwrap();
        registry.install(test_manifest("b", "B"), SkillSource::Builtin, None).unwrap();
        assert_eq!(registry.active_count(), 2);

        registry.set_status("b", SkillStatus::Disabled).unwrap();
        assert_eq!(registry.active_count(), 1);
    }

    #[test]
    fn test_disabled_skill_excluded_from_find_for_task() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = SkillRegistry::new(tmp.path().join("skills"));

        registry.install(test_manifest("weather", "Weather"), SkillSource::Builtin, None).unwrap();
        registry.set_status("weather", SkillStatus::Disabled).unwrap();

        let results = registry.find_for_task("weather forecast");
        assert!(results.is_empty());
    }
}
