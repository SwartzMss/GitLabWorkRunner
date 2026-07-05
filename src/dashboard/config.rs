use crate::app::config::AppConfig;

const DEFAULT_DASHBOARD_BIND: &str = "127.0.0.1:8082";
const DEFAULT_DASHBOARD_DATABASE_URL: &str = "sqlite://gitlab-work-runner.db";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardConfig {
    pub bind: String,
    pub database_url: String,
}

impl DashboardConfig {
    pub fn default_local() -> Self {
        Self {
            bind: DEFAULT_DASHBOARD_BIND.into(),
            database_url: DEFAULT_DASHBOARD_DATABASE_URL.into(),
        }
    }

    pub fn from_app_config(config: &AppConfig) -> Self {
        Self {
            bind: config.dashboard.bind.clone(),
            database_url: config.storage.database_url.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::config::{
            AppConfig, DashboardConfig as AppDashboardConfig, GitLabConfig, LoggingConfig,
            RulesConfig, ServerConfig, StorageConfig,
        },
        review::scripts::ArchiveLimits,
    };

    #[test]
    fn derives_dashboard_config_from_app_config() {
        let app_config = AppConfig {
            server: ServerConfig {
                bind: "0.0.0.0:8080".into(),
                webhook_secret: "secret".into(),
                max_concurrent_reviews: 4,
            },
            gitlab: GitLabConfig {
                base_url: "https://gitlab.example.com".into(),
                token: "token".into(),
            },
            storage: StorageConfig {
                database_url: "sqlite://state.db".into(),
            },
            rules: RulesConfig {
                file: "rules.toml".into(),
            },
            logging: LoggingConfig::default(),
            archive: ArchiveLimits::default(),
            dashboard: AppDashboardConfig {
                bind: "127.0.0.1:18082".into(),
            },
        };

        let dashboard_config = DashboardConfig::from_app_config(&app_config);

        assert_eq!(dashboard_config.bind, "127.0.0.1:18082");
        assert_eq!(dashboard_config.database_url, "sqlite://state.db");
    }
}
