use gitlab_work_runner::{config::AppConfig, server, storage::StateStore};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::{Arc, Mutex},
};
use tracing_subscriber::{fmt::MakeWriter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> gitlab_work_runner::error::AppResult<()> {
    let config = AppConfig::from_path("config.toml")?;
    init_tracing(&config.logging.file)?;

    tracing::info!(
        log_file = %config.logging.file,
        bind = %config.server.bind,
        gitlab_base_url = %config.gitlab.base_url,
        rules_file = %config.rules.file,
        database_url = %config.storage.database_url,
        "starting gitlab work runner"
    );

    let store = StateStore::connect(&config.storage.database_url).await?;
    store.migrate().await?;
    tracing::info!("state store migration completed");
    server::serve(config, store).await
}

fn init_tracing(log_file: &str) -> gitlab_work_runner::error::AppResult<()> {
    let path = Path::new(log_file);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let file_writer = SharedLogWriter::new(file);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(file_writer)
                .with_ansi(false),
        )
        .init();
    Ok(())
}

#[derive(Clone)]
struct SharedLogWriter {
    file: Arc<Mutex<fs::File>>,
}

impl SharedLogWriter {
    fn new(file: fs::File) -> Self {
        Self {
            file: Arc::new(Mutex::new(file)),
        }
    }
}

impl<'a> MakeWriter<'a> for SharedLogWriter {
    type Writer = LockedLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LockedLogWriter {
            file: Arc::clone(&self.file),
        }
    }
}

struct LockedLogWriter {
    file: Arc<Mutex<fs::File>>,
}

impl Write for LockedLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.lock().expect("log file lock poisoned").write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.lock().expect("log file lock poisoned").flush()
    }
}
