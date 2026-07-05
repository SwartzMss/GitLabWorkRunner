use gitlab_work_runner::{
    app::config::AppConfig, dashboard::config::DashboardConfig, dashboard::server,
};
use std::io::ErrorKind;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> gitlab_work_runner::error::AppResult<()> {
    init_tracing();
    let config = match AppConfig::from_path("config.toml") {
        Ok(config) => DashboardConfig::from_app_config(&config),
        Err(gitlab_work_runner::error::AppError::Io(err)) if err.kind() == ErrorKind::NotFound => {
            let config = DashboardConfig::default_local();
            tracing::warn!(
                bind = %config.bind,
                database_url = %config.database_url,
                "config.toml not found; using default local dashboard configuration"
            );
            config
        }
        Err(err) => return Err(err),
    };
    tracing::info!(
        bind = %config.bind,
        database_url = %config.database_url,
        "starting gitlab work runner dashboard"
    );
    server::serve(config).await
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}
