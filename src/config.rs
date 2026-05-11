use crate::error::{AppError, AppResult};
use serde::Deserialize;
use std::{fs, path::Path};

const DEFAULT_LOG_FILE: &str = "logs/gitlab-work-runner.log";
const DEFAULT_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_LOG_MAX_FILES: usize = 5;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub gitlab: GitLabConfig,
    pub storage: StorageConfig,
    pub rules: RulesConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    pub bind: String,
    pub webhook_secret: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct GitLabConfig {
    pub base_url: String,
    pub token_env: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct StorageConfig {
    pub database_url: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct RulesConfig {
    pub file: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct LoggingConfig {
    #[serde(default = "default_log_file")]
    pub file: String,
    #[serde(default = "default_log_max_bytes")]
    pub max_bytes: u64,
    #[serde(default = "default_log_max_files")]
    pub max_files: usize,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            file: default_log_file(),
            max_bytes: default_log_max_bytes(),
            max_files: default_log_max_files(),
        }
    }
}

fn default_log_file() -> String {
    DEFAULT_LOG_FILE.into()
}

fn default_log_max_bytes() -> u64 {
    DEFAULT_LOG_MAX_BYTES
}

fn default_log_max_files() -> usize {
    DEFAULT_LOG_MAX_FILES
}

impl AppConfig {
    pub fn from_path(path: impl AsRef<Path>) -> AppResult<Self> {
        let text = fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    pub fn gitlab_token(&self) -> AppResult<String> {
        std::env::var(&self.gitlab.token_env).map_err(|_| {
            AppError::Config(format!(
                "environment variable {name} is not set. Create a GitLab access token with the api scope, then set it before starting the service. Windows cmd: set {name}=<your-gitlab-token>. PowerShell: $env:{name} = \"<your-gitlab-token>\". Linux/macOS: export {name}=<your-gitlab-token>.",
                name = self.gitlab.token_env
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn loads_config_from_toml() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(
            file,
            r#"
[server]
bind = "127.0.0.1:8080"
webhook_secret = "secret"

[gitlab]
base_url = "https://gitlab.example.com"
token_env = "GITLAB_TOKEN"

[storage]
database_url = "sqlite::memory:"

[rules]
file = "rules.toml"
"#
        )
        .unwrap();

        let config = AppConfig::from_path(file.path()).unwrap();

        assert_eq!(config.server.bind, "127.0.0.1:8080");
        assert_eq!(config.server.webhook_secret, "secret");
        assert_eq!(config.gitlab.base_url, "https://gitlab.example.com");
        assert_eq!(config.storage.database_url, "sqlite::memory:");
        assert_eq!(config.rules.file, "rules.toml");
        assert_eq!(config.logging.file, "logs/gitlab-work-runner.log");
        assert_eq!(config.logging.max_bytes, 10 * 1024 * 1024);
        assert_eq!(config.logging.max_files, 5);
    }

    #[test]
    fn returns_error_when_token_env_is_missing() {
        let config = AppConfig {
            server: ServerConfig {
                bind: "127.0.0.1:8080".into(),
                webhook_secret: "secret".into(),
            },
            gitlab: GitLabConfig {
                base_url: "https://gitlab.example.com".into(),
                token_env: "GITLAB_WORK_RUNNER_MISSING_TOKEN".into(),
            },
            storage: StorageConfig {
                database_url: "sqlite::memory:".into(),
            },
            rules: RulesConfig {
                file: "rules.toml".into(),
            },
            logging: LoggingConfig::default(),
        };

        let err = config.gitlab_token().unwrap_err().to_string();
        assert!(err.contains("GITLAB_WORK_RUNNER_MISSING_TOKEN"));
        assert!(err.contains("set GITLAB_WORK_RUNNER_MISSING_TOKEN=<your-gitlab-token>"));
        assert!(err.contains("$env:GITLAB_WORK_RUNNER_MISSING_TOKEN"));
        assert!(err.contains("export GITLAB_WORK_RUNNER_MISSING_TOKEN=<your-gitlab-token>"));
    }

    #[test]
    fn loads_custom_logging_rotation_config() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(
            file,
            r#"
[server]
bind = "127.0.0.1:8080"
webhook_secret = "secret"

[gitlab]
base_url = "https://gitlab.example.com"
token_env = "GITLAB_TOKEN"

[storage]
database_url = "sqlite::memory:"

[rules]
file = "rules.toml"

[logging]
file = "runner.log"
max_bytes = 1024
max_files = 3
"#
        )
        .unwrap();

        let config = AppConfig::from_path(file.path()).unwrap();

        assert_eq!(config.logging.file, "runner.log");
        assert_eq!(config.logging.max_bytes, 1024);
        assert_eq!(config.logging.max_files, 3);
    }

    #[test]
    fn loads_custom_logging_rotation_config() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(
            file,
            r#"
[server]
bind = "127.0.0.1:8080"
webhook_secret = "secret"

[gitlab]
base_url = "https://gitlab.example.com"
token_env = "GITLAB_TOKEN"

[storage]
database_url = "sqlite::memory:"

[rules]
file = "rules.toml"

[logging]
file = "runner.log"
max_bytes = 1024
max_files = 3
"#
        )
        .unwrap();

        let config = AppConfig::from_path(file.path()).unwrap();

        assert_eq!(config.logging.file, "runner.log");
        assert_eq!(config.logging.max_bytes, 1024);
        assert_eq!(config.logging.max_files, 3);
    }
}
