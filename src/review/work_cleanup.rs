use crate::error::AppResult;
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use tracing::{info, warn};

const DEFAULT_STALE_WORK_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60);
const AI_CONTEXT_ROOT: &str = "work/ai_review_context";

pub(crate) fn cleanup_stale_review_work() -> AppResult<usize> {
    let removed =
        cleanup_stale_ai_context_work(Path::new(AI_CONTEXT_ROOT), DEFAULT_STALE_WORK_TTL)?;
    if removed > 0 {
        info!(removed, "stale review work directories cleaned");
    }
    Ok(removed)
}

pub(crate) fn spawn_periodic_stale_review_work_cleanup() {
    tokio::spawn(async {
        let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(err) = cleanup_stale_review_work() {
                warn!(error = %err, "stale review work cleanup failed");
            }
        }
    });
}

pub(crate) fn cleanup_stale_ai_context_work(root: &Path, ttl: Duration) -> io::Result<usize> {
    cleanup_dirs_matching(root, ttl, &|path| path.join("source").is_dir(), true)
}

fn cleanup_dirs_matching(
    root: &Path,
    ttl: Duration,
    should_remove_dir: &dyn Fn(&Path) -> bool,
    use_source_mtime: bool,
) -> io::Result<usize> {
    if !root.exists() {
        return Ok(0);
    }
    let root = absolute_path(root.to_path_buf())?;
    cleanup_dirs_recursive(&root, ttl, should_remove_dir, use_source_mtime)
}

fn cleanup_dirs_recursive(
    dir: &Path,
    ttl: Duration,
    should_remove_dir: &dyn Fn(&Path) -> bool,
    use_source_mtime: bool,
) -> io::Result<usize> {
    if should_remove_dir(dir) {
        let stale_path = if use_source_mtime {
            dir.join("source")
        } else {
            dir.to_path_buf()
        };
        if is_stale(&stale_path, ttl)? {
            match fs::remove_dir_all(dir) {
                Ok(()) => {
                    info!(
                        work_dir = %dir.display(),
                        "stale review work directory removed"
                    );
                    return Ok(1);
                }
                Err(err) => {
                    warn!(
                        work_dir = %dir.display(),
                        error = %err,
                        "failed to remove stale review work directory"
                    );
                    return Ok(0);
                }
            }
        }
    }

    let mut removed = 0_usize;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_dir() {
            continue;
        }
        removed += cleanup_dirs_recursive(&entry.path(), ttl, should_remove_dir, use_source_mtime)?;
    }
    Ok(removed)
}

fn is_stale(path: &Path, ttl: Duration) -> io::Result<bool> {
    let modified = fs::metadata(path)?.modified()?;
    let Ok(age) = SystemTime::now().duration_since(modified) else {
        return Ok(false);
    };
    Ok(age >= ttl)
}

fn absolute_path(path: PathBuf) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, time::Duration};

    #[test]
    fn cleanup_removes_stale_ai_context_run_directories() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("1/2/commit/ai-review/rr-1");
        let source_dir = run_dir.join("source");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(source_dir.join("src.rs"), "content").unwrap();

        let removed = cleanup_stale_ai_context_work(temp.path(), Duration::ZERO).unwrap();

        assert_eq!(removed, 1);
        assert!(!run_dir.exists());
    }
}
