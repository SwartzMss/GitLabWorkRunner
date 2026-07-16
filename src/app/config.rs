use crate::error::{AppError, AppResult};
use crate::review::archive::ArchiveLimits;
use serde::Deserialize;
use std::{fs, path::Path};

const DEFAULT_LOG_FILE: &str = "logs/gitlab-work-runner.log";
const DEFAULT_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_LOG_MAX_FILES: usize = 5;
const DEFAULT_MAX_CONCURRENT_REVIEWS: usize = 4;
const DEFAULT_DASHBOARD_BIND: &str = "127.0.0.1:8082";
const DEFAULT_GITLAB_API_TIMEOUT_SECONDS: u64 = 30;
const DEFAULT_GITLAB_ARCHIVE_TIMEOUT_SECONDS: u64 = 30;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub gitlab: GitLabConfig,
    pub storage: StorageConfig,
    pub rules: RulesConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub archive: ArchiveLimits,
    #[serde(default)]
    pub dashboard: DashboardConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    pub bind: String,
    pub webhook_secret: String,
    #[serde(default = "default_max_concurrent_reviews")]
    pub max_concurrent_reviews: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct GitLabConfig {
    pub base_url: String,
    pub token: String,
    #[serde(default = "default_gitlab_api_timeout_seconds")]
    pub api_timeout_seconds: u64,
    #[serde(default = "default_gitlab_archive_timeout_seconds")]
    pub archive_timeout_seconds: u64,
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
pub struct DashboardConfig {
    #[serde(default = "default_dashboard_bind")]
    pub bind: String,
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

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            bind: default_dashboard_bind(),
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

fn default_max_concurrent_reviews() -> usize {
    DEFAULT_MAX_CONCURRENT_REVIEWS
}

fn default_dashboard_bind() -> String {
    DEFAULT_DASHBOARD_BIND.into()
}

fn default_gitlab_api_timeout_seconds() -> u64 {
    DEFAULT_GITLAB_API_TIMEOUT_SECONDS
}

fn default_gitlab_archive_timeout_seconds() -> u64 {
    DEFAULT_GITLAB_ARCHIVE_TIMEOUT_SECONDS
}

impl AppConfig {
    pub fn from_path(path: impl AsRef<Path>) -> AppResult<Self> {
        let text = fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    pub fn gitlab_token(&self) -> AppResult<String> {
        let token = self.gitlab.token.trim();
        if token.is_empty() {
            return Err(AppError::Config(
                "[gitlab].token is empty. Create a GitLab access token with the api scope, then set token in config.toml.".into(),
            ));
        }
        Ok(token.to_owned())
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
token = "glpat-test-token"

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
        assert_eq!(config.server.max_concurrent_reviews, 4);
        assert_eq!(config.gitlab.base_url, "https://gitlab.example.com");
        assert_eq!(config.gitlab.token, "glpat-test-token");
        assert_eq!(config.gitlab.api_timeout_seconds, 30);
        assert_eq!(config.gitlab.archive_timeout_seconds, 30);
        assert_eq!(config.gitlab_token().unwrap(), "glpat-test-token");
        assert_eq!(config.storage.database_url, "sqlite::memory:");
        assert_eq!(config.rules.file, "rules.toml");
        assert_eq!(config.dashboard.bind, "127.0.0.1:8082");
        assert_eq!(config.logging.file, "logs/gitlab-work-runner.log");
        assert_eq!(config.logging.max_bytes, 10 * 1024 * 1024);
        assert_eq!(config.logging.max_files, 5);
        assert_eq!(config.archive, ArchiveLimits::default());
    }

    #[test]
    fn returns_error_when_gitlab_token_is_empty() {
        let config = AppConfig {
            server: ServerConfig {
                bind: "127.0.0.1:8080".into(),
                webhook_secret: "secret".into(),
                max_concurrent_reviews: 4,
            },
            gitlab: GitLabConfig {
                base_url: "https://gitlab.example.com".into(),
                token: "  ".into(),
                api_timeout_seconds: 30,
                archive_timeout_seconds: 30,
            },
            storage: StorageConfig {
                database_url: "sqlite::memory:".into(),
            },
            rules: RulesConfig {
                file: "rules.toml".into(),
            },
            logging: LoggingConfig::default(),
            archive: ArchiveLimits::default(),
            dashboard: DashboardConfig::default(),
        };

        let err = config.gitlab_token().unwrap_err().to_string();
        assert!(err.contains("[gitlab].token is empty"));
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
token = "glpat-test-token"

[storage]
database_url = "sqlite::memory:"

[rules]
file = "rules.toml"

[dashboard]
bind = "127.0.0.1:18082"

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
        assert_eq!(config.dashboard.bind, "127.0.0.1:18082");
    }

    #[test]
    fn loads_custom_gitlab_timeout_config() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(
            file,
            r#"
[server]
bind = "127.0.0.1:8080"
webhook_secret = "secret"

[gitlab]
base_url = "https://gitlab.example.com"
token = "glpat-test-token"
api_timeout_seconds = 45
archive_timeout_seconds = 300

[storage]
database_url = "sqlite::memory:"

[rules]
file = "rules.toml"
"#
        )
        .unwrap();

        let config = AppConfig::from_path(file.path()).unwrap();

        assert_eq!(config.gitlab.api_timeout_seconds, 45);
        assert_eq!(config.gitlab.archive_timeout_seconds, 300);
    }

    #[test]
    fn loads_custom_archive_limits_config() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(
            file,
            r#"
[server]
bind = "127.0.0.1:8080"
webhook_secret = "secret"

[gitlab]
base_url = "https://gitlab.example.com"
token = "glpat-test-token"

[storage]
database_url = "sqlite::memory:"

[rules]
file = "rules.toml"

[archive]
max_archive_bytes = 1
max_extracted_files = 2
max_extracted_bytes = 3
max_single_file_bytes = 4
max_entry_path_bytes = 5
"#
        )
        .unwrap();

        let config = AppConfig::from_path(file.path()).unwrap();

        assert_eq!(
            config.archive,
            ArchiveLimits {
                max_archive_bytes: 1,
                max_extracted_files: 2,
                max_extracted_bytes: 3,
                max_single_file_bytes: 4,
                max_entry_path_bytes: 5,
            }
        );
    }
}
