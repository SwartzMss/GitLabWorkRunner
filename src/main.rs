use gitlab_work_runner::{
    config::{AppConfig, LoggingConfig},
    server,
    storage::StateStore,
};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tracing_subscriber::{fmt::MakeWriter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> gitlab_work_runner::error::AppResult<()> {
    let config = AppConfig::from_path("config.toml")?;
    let _log_guard = init_tracing(&config.logging)?;
    config.gitlab_token()?;

    tracing::info!(
        log_file = %config.logging.file,
        log_max_bytes = config.logging.max_bytes,
        log_max_files = config.logging.max_files,
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

fn init_tracing(logging: &LoggingConfig) -> gitlab_work_runner::error::AppResult<TracingGuards> {
    let (stdout_writer, stdout_guard) = non_blocking_stdout_log_writer();
    let (file_writer, file_guard) =
        non_blocking_file_log_writer(&logging.file, logging.max_bytes, logging.max_files)?;

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(stdout_writer))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(file_writer)
                .with_ansi(false),
        )
        .init();
    Ok(TracingGuards {
        _stdout_guard: stdout_guard,
        _file_guard: file_guard,
    })
}

struct TracingGuards {
    _stdout_guard: tracing_appender::non_blocking::WorkerGuard,
    _file_guard: tracing_appender::non_blocking::WorkerGuard,
}

fn non_blocking_stdout_log_writer() -> (
    tracing_appender::non_blocking::NonBlocking,
    tracing_appender::non_blocking::WorkerGuard,
) {
    tracing_appender::non_blocking(io::stdout())
}

fn non_blocking_file_log_writer(
    path: impl AsRef<Path>,
    max_bytes: u64,
    max_files: usize,
) -> io::Result<(
    tracing_appender::non_blocking::NonBlocking,
    tracing_appender::non_blocking::WorkerGuard,
)> {
    let file_writer = SharedLogWriter::new(path, max_bytes, max_files)?;
    Ok(tracing_appender::non_blocking(file_writer.into_writer()))
}

#[derive(Clone)]
struct SharedLogWriter {
    file: Arc<Mutex<RotatingLogFile>>,
}

impl SharedLogWriter {
    fn new(path: impl AsRef<Path>, max_bytes: u64, max_files: usize) -> io::Result<Self> {
        Ok(Self {
            file: Arc::new(Mutex::new(RotatingLogFile::new(
                path, max_bytes, max_files,
            )?)),
        })
    }

    fn into_writer(self) -> LockedLogWriter {
        LockedLogWriter { file: self.file }
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
    file: Arc<Mutex<RotatingLogFile>>,
}

impl Write for LockedLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.lock().expect("log file lock poisoned").write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.lock().expect("log file lock poisoned").flush()
    }
}

struct RotatingLogFile {
    path: PathBuf,
    file: Option<fs::File>,
    current_bytes: u64,
    max_bytes: u64,
    max_files: usize,
}

impl RotatingLogFile {
    fn new(path: impl AsRef<Path>, max_bytes: u64, max_files: usize) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let current_bytes = file.metadata()?.len();
        Ok(Self {
            path,
            file: Some(file),
            current_bytes,
            max_bytes,
            max_files,
        })
    }

    fn should_rotate(&self, incoming_bytes: usize) -> bool {
        self.max_bytes > 0
            && self.current_bytes > 0
            && self.current_bytes.saturating_add(incoming_bytes as u64) > self.max_bytes
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
        }

        if self.max_files == 0 {
            remove_file_if_exists(&self.path)?;
        } else {
            remove_file_if_exists(&self.rotated_path(self.max_files))?;
            for index in (1..self.max_files).rev() {
                rename_file_if_exists(&self.rotated_path(index), &self.rotated_path(index + 1))?;
            }
            rename_file_if_exists(&self.path, &self.rotated_path(1))?;
        }

        self.file = Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        );
        self.current_bytes = 0;
        Ok(())
    }

    fn rotated_path(&self, index: usize) -> PathBuf {
        let mut file_name = self
            .path
            .file_name()
            .map(OsString::from)
            .unwrap_or_else(|| OsString::from("gitlab-work-runner.log"));
        file_name.push(format!(".{index}"));
        self.path.with_file_name(file_name)
    }

    fn file_mut(&mut self) -> io::Result<&mut fs::File> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file is not open"))
    }
}

impl Write for RotatingLogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.should_rotate(buf.len()) {
            self.rotate()?;
        }
        let written = self.file_mut()?.write(buf)?;
        self.current_bytes = self.current_bytes.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file_mut()?.flush()
    }
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn rename_file_if_exists(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_log_file_when_size_limit_is_reached() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        let mut file = RotatingLogFile::new(&path, 10, 2).unwrap();

        file.write_all(b"12345678").unwrap();
        file.write_all(b"abcde").unwrap();
        file.flush().unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "abcde");
        assert_eq!(
            fs::read_to_string(dir.path().join("app.log.1")).unwrap(),
            "12345678"
        );
    }

    #[test]
    fn shifts_existing_rotated_logs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        let mut file = RotatingLogFile::new(&path, 10, 2).unwrap();

        file.write_all(b"12345678").unwrap();
        file.write_all(b"abcde").unwrap();
        file.write_all(b"zzzzzz").unwrap();
        file.flush().unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "zzzzzz");
        assert_eq!(
            fs::read_to_string(dir.path().join("app.log.1")).unwrap(),
            "abcde"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("app.log.2")).unwrap(),
            "12345678"
        );
    }

    #[test]
    fn recreates_active_log_without_history_when_max_files_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        let mut file = RotatingLogFile::new(&path, 10, 0).unwrap();

        file.write_all(b"12345678").unwrap();
        file.write_all(b"abcde").unwrap();
        file.flush().unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "abcde");
        assert!(!dir.path().join("app.log.1").exists());
    }

    #[test]
    fn creates_non_blocking_file_log_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");

        let (_writer, _guard) = non_blocking_file_log_writer(&path, 1024, 1).unwrap();
    }

    #[test]
    fn creates_non_blocking_stdout_log_writer() {
        let (_writer, _guard) = non_blocking_stdout_log_writer();
    }
}
