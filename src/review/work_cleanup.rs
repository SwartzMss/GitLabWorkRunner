use crate::error::AppResult;
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime},
};
use tracing::{info, warn};

const DEFAULT_STALE_WORK_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60);
const AI_CONTEXT_ROOT: &str = "work/ai_review_context";
const SCRIPT_TASK_ROOT: &str = "work/script_tasks";

static ACTIVE_SCRIPT_WORK_DIRS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

pub(crate) struct ActiveScriptWorkDirGuard {
    path: PathBuf,
}

impl Drop for ActiveScriptWorkDirGuard {
    fn drop(&mut self) {
        active_script_work_dirs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.path);
    }
}

pub(crate) fn register_active_script_work_dir(
    path: PathBuf,
) -> io::Result<ActiveScriptWorkDirGuard> {
    let path = absolute_path(path)?;
    active_script_work_dirs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.clone());
    Ok(ActiveScriptWorkDirGuard { path })
}

pub(crate) fn cleanup_stale_review_work() -> AppResult<usize> {
    let removed_ai =
        cleanup_stale_ai_context_work(Path::new(AI_CONTEXT_ROOT), DEFAULT_STALE_WORK_TTL)?;
    let removed_scripts =
        cleanup_stale_script_task_work(Path::new(SCRIPT_TASK_ROOT), DEFAULT_STALE_WORK_TTL)?;
    let removed = removed_ai + removed_scripts;
    if removed > 0 {
        info!(
            removed_ai_context_dirs = removed_ai,
            removed_script_run_dirs = removed_scripts,
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

pub(crate) fn cleanup_stale_script_task_work(root: &Path, ttl: Duration) -> io::Result<usize> {
    if !root.exists() {
        return Ok(0);
    }
    let root = absolute_path(root.to_path_buf())?;
    let active = active_script_work_dirs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let mut empty_run_dirs = HashSet::new();
    let removed =
        cleanup_script_task_dirs_recursive(&root, &root, ttl, &active, &mut empty_run_dirs)?;
    for run_dir in empty_run_dirs {
        let mut dir = run_dir;
        while dir != root && dir.starts_with(&root) {
            let Some(parent) = dir.parent().map(Path::to_path_buf) else {
                break;
            };
            match fs::remove_dir(&dir) {
                Ok(()) => {
                    info!(work_dir = %dir.display(), "empty script task work directory removed");
                    dir = parent;
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => dir = parent,
                Err(err) if err.kind() == io::ErrorKind::DirectoryNotEmpty => break,
                Err(err) => {
                    warn!(
                        work_dir = %dir.display(),
                        error = %err,
                        "failed to remove empty script task work directory"
                    );
                    break;
                }
            }
        }
    }
    Ok(removed)
}

fn cleanup_script_task_dirs_recursive(
    root: &Path,
    dir: &Path,
    ttl: Duration,
    active: &HashSet<PathBuf>,
    empty_run_dirs: &mut HashSet<PathBuf>,
) -> io::Result<usize> {
    if is_script_task_dir(root, dir) {
        if !active.contains(dir) && is_stale(dir, ttl)? {
            match fs::remove_dir_all(dir) {
                Ok(()) => {
                    info!(work_dir = %dir.display(), "stale script task work directory removed");
                    if let Some(run_dir) = dir.parent() {
                        empty_run_dirs.insert(run_dir.to_path_buf());
                    }
                    return Ok(1);
                }
                Err(err) => {
                    warn!(work_dir = %dir.display(), error = %err, "failed to remove stale script task work directory");
                    return Ok(0);
                }
            }
        }
        return Ok(0);
    }

    let mut removed = 0_usize;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_dir() {
            continue;
        }
        removed +=
            cleanup_script_task_dirs_recursive(root, &entry.path(), ttl, active, empty_run_dirs)?;
    }
    Ok(removed)
}

fn is_script_task_dir(root: &Path, dir: &Path) -> bool {
    dir.strip_prefix(root)
        .map(|relative| relative.components().count() == 5)
        .unwrap_or(false)
}

fn active_script_work_dirs() -> &'static Mutex<HashSet<PathBuf>> {
    ACTIVE_SCRIPT_WORK_DIRS.get_or_init(|| Mutex::new(HashSet::new()))
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

    #[test]
    fn script_work_cleanup_removes_stale_runs_but_keeps_active_runs() {
        let temp = tempfile::tempdir().unwrap();
        let stale_dir = temp.path().join("1/2/commit/run-stale/check");
        let active_dir = temp.path().join("1/2/commit/run-active/check");
        fs::create_dir_all(&stale_dir).unwrap();
        fs::write(stale_dir.join("run.log"), "stale log").unwrap();
        fs::write(stale_dir.join("result.txt"), "stale result").unwrap();
        fs::create_dir_all(&active_dir).unwrap();
        fs::write(active_dir.join("run.log"), "active log").unwrap();
        fs::write(active_dir.join("result.txt"), "active result").unwrap();
        let active_guard = register_active_script_work_dir(active_dir.clone()).unwrap();

        let removed = cleanup_stale_script_task_work(temp.path(), Duration::ZERO).unwrap();

        assert_eq!(removed, 1);
        assert!(!stale_dir.exists());
        assert!(
            !stale_dir.parent().unwrap().exists(),
            "empty per-run directories should be pruned after stale task cleanup"
        );
        assert!(active_dir.join("run.log").exists());
        assert!(active_dir.join("result.txt").exists());
        drop(active_guard);
        assert_eq!(
            cleanup_stale_script_task_work(temp.path(), Duration::ZERO).unwrap(),
            1
        );
        assert!(!active_dir.exists());
        assert!(!active_dir.parent().unwrap().exists());
        assert!(
            !temp.path().join("1").exists(),
            "empty project/MR/commit ancestors should be pruned without deleting the work root"
        );
    }
}
