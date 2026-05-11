use crate::error::{AppError, AppResult};
use serde::Deserialize;
use std::{fs, path::Path};

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
    pub file: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            file: "logs/gitlab-work-runner.log".into(),
        }
    }
}

impl AppConfig {
    pub fn from_path(path: impl AsRef<Path>) -> AppResult<Self> {
        let text = fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    pub fn gitlab_token(&self) -> AppResult<String> {
        std::env::var(&self.gitlab.token_env).map_err(|_| {
            AppError::Config(format!(
                "environment variable {} is not set",
                self.gitlab.token_env
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
    }
}
