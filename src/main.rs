use gitlab_work_runner::{config::AppConfig, server, storage::StateStore};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> gitlab_work_runner::error::AppResult<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = AppConfig::from_path("config.toml")?;
    let store = StateStore::connect(&config.storage.database_url).await?;
    store.migrate().await?;
    server::serve(config, store).await
}
