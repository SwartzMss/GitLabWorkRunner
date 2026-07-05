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
const SCRIPT_TASK_ROOT: &str = "work/script_tasks";

pub(crate) fn cleanup_stale_review_work() -> AppResult<usize> {
    let removed_ai =
        cleanup_stale_ai_context_work(Path::new(AI_CONTEXT_ROOT), DEFAULT_STALE_WORK_TTL)?;
    let removed_scripts =
        cleanup_stale_script_sources(Path::new(SCRIPT_TASK_ROOT), DEFAULT_STALE_WORK_TTL)?;
    let removed = removed_ai + removed_scripts;
    if removed > 0 {
        info!(
            removed_ai_context_dirs = removed_ai,
            removed_script_source_dirs = removed_scripts,
            removed,
            "stale review work directories cleaned"
        );
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

pub(crate) fn cleanup_stale_script_sources(root: &Path, ttl: Duration) -> io::Result<usize> {
    if !root.exists() {
        return Ok(0);
    }
    let root = absolute_path(root.to_path_buf())?;
    cleanup_dirs_recursive(
        &root,
        ttl,
        &|path| is_script_task_source_leaf(&root, path),
        false,
    )
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

fn is_script_task_source_leaf(root: &Path, path: &Path) -> bool {
    if path
        .file_name()
        .is_none_or(|file_name| file_name != "source")
    {
        return false;
    }
    path.strip_prefix(root)
        .map(|relative| relative.components().count() == 5)
        .unwrap_or(false)
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

    #[test]
    fn cleanup_removes_only_stale_script_source_directories() {
        let temp = tempfile::tempdir().unwrap();
        let task_dir = temp.path().join("1/2/commit/task");
        let source_dir = task_dir.join("source");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(source_dir.join("src.rs"), "content").unwrap();
        fs::write(task_dir.join("run.log"), "log").unwrap();
        fs::write(task_dir.join("result.txt"), "result").unwrap();

        let removed = cleanup_stale_script_sources(temp.path(), Duration::ZERO).unwrap();

        assert_eq!(removed, 1);
        assert!(!source_dir.exists());
        assert!(task_dir.join("run.log").exists());
        assert!(task_dir.join("result.txt").exists());
    }

    #[test]
    fn cleanup_preserves_script_logs_when_task_id_is_source() {
        let temp = tempfile::tempdir().unwrap();
        let task_dir = temp.path().join("1/2/commit/source");
        let source_dir = task_dir.join("source");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(source_dir.join("src.rs"), "content").unwrap();
        fs::write(task_dir.join("run.log"), "log").unwrap();
        fs::write(task_dir.join("result.txt"), "result").unwrap();

        let removed = cleanup_stale_script_sources(temp.path(), Duration::ZERO).unwrap();

        assert_eq!(removed, 1);
        assert!(task_dir.exists());
        assert!(!source_dir.exists());
        assert!(task_dir.join("run.log").exists());
        assert!(task_dir.join("result.txt").exists());
    }
}
