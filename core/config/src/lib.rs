use serde::Deserialize;
use std::path::PathBuf;
use tracing::info;

/// Main NexMind configuration, loaded from `~/.nexmind/config.toml`.
#[derive(Debug, Deserialize, Clone)]
pub struct NexMindConfig {
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub paths: PathsConfig,
    #[serde(default)]
    pub telegram: TelegramConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ModelConfig {
    #[serde(default = "ModelConfig::default_model")]
    pub default: String,
}

impl ModelConfig {
    fn default_model() -> String {
        "auto".to_string()
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            default: Self::default_model(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct DaemonConfig {
    #[serde(default = "DaemonConfig::default_address")]
    pub address: String,
    #[serde(default = "DaemonConfig::default_port")]
    pub port: u16,
}

impl DaemonConfig {
    fn default_address() -> String {
        "127.0.0.1".to_string()
    }
    fn default_port() -> u16 {
        19384
    }

    pub fn socket_addr(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }

    pub fn http_addr(&self) -> String {
        format!("http://{}:{}", self.address, self.port)
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            address: Self::default_address(),
            port: Self::default_port(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct PathsConfig {
    #[serde(default = "PathsConfig::default_data_dir")]
    pub data_dir: String,
    #[serde(default = "PathsConfig::default_workspace_dir")]
    pub workspace_dir: String,
    #[serde(default = "PathsConfig::default_builtin_skills_dir")]
    pub builtin_skills_dir: String,
    #[serde(default = "PathsConfig::default_user_skills_dir")]
    pub user_skills_dir: String,
}

impl PathsConfig {
    fn default_data_dir() -> String {
        "~/.nexmind/data".to_string()
    }
    fn default_workspace_dir() -> String {
        "~/.nexmind/data/workspace".to_string()
    }
    fn default_builtin_skills_dir() -> String {
        "~/nexmind/skills".to_string()
    }
    fn default_user_skills_dir() -> String {
        "~/.nexmind/skills".to_string()
    }

    /// Resolve a path, expanding `~` to the user's home directory.
    pub fn resolve(path: &str) -> PathBuf {
        if let Some(rest) = path.strip_prefix("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(rest);
            }
        }
        PathBuf::from(path)
    }

    pub fn data_dir_resolved(&self) -> PathBuf {
        Self::resolve(&self.data_dir)
    }

    pub fn workspace_dir_resolved(&self) -> PathBuf {
        Self::resolve(&self.workspace_dir)
    }

    pub fn builtin_skills_dir_resolved(&self) -> PathBuf {
        Self::resolve(&self.builtin_skills_dir)
    }

    pub fn user_skills_dir_resolved(&self) -> PathBuf {
        Self::resolve(&self.user_skills_dir)
    }
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            data_dir: Self::default_data_dir(),
            workspace_dir: Self::default_workspace_dir(),
            builtin_skills_dir: Self::default_builtin_skills_dir(),
            user_skills_dir: Self::default_user_skills_dir(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct TelegramConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl NexMindConfig {
    /// Determine the config directory path.
    /// Priority: `NEXMIND_CONFIG_DIR` env var → `~/.nexmind/`.
    pub fn config_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("NEXMIND_CONFIG_DIR") {
            return PathBuf::from(dir);
        }
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".nexmind")
    }

    /// Load configuration from `~/.nexmind/`.
    ///
    /// 1. Load `~/.nexmind/.env` into process environment (does not override existing vars).
    /// 2. Parse `~/.nexmind/config.toml` if it exists.
    /// 3. Return defaults if no config files found.
    pub fn load() -> Self {
        let config_dir = Self::config_dir();

        // Load .env file (secrets) into process environment
        let env_path = config_dir.join(".env");
        if env_path.exists() {
            match dotenvy::from_path(&env_path) {
                Ok(_) => info!("loaded secrets from {}", env_path.display()),
                Err(e) => tracing::warn!("failed to load {}: {}", env_path.display(), e),
            }
        }

        // Parse config.toml
        let config_path = config_dir.join("config.toml");
        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(contents) => match toml::from_str::<NexMindConfig>(&contents) {
                    Ok(config) => {
                        info!("loaded config from {}", config_path.display());
                        return config;
                    }
                    Err(e) => {
                        tracing::warn!("failed to parse {}: {}", config_path.display(), e);
                    }
                },
                Err(e) => {
                    tracing::warn!("failed to read {}: {}", config_path.display(), e);
                }
            }
        }

        // No config file — return defaults (backward compatibility)
        Self::default()
    }
}

impl Default for NexMindConfig {
    fn default() -> Self {
        Self {
            model: ModelConfig::default(),
            daemon: DaemonConfig::default(),
            paths: PathsConfig::default(),
            telegram: TelegramConfig::default(),
        }
    }
}
