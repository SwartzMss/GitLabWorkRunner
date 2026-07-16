pub use crate::review::archive::ArchiveLimits;
use crate::{error::AppResult, review::archive::extract_zip_archive, rules::ScriptTaskConfig};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{process::Command, time};
use tracing::{info, warn};

const DEFAULT_WORK_ROOT: &str = "work/script_tasks";

pub struct ScriptTaskRunner {
    work_root: PathBuf,
    archive_limits: ArchiveLimits,
}

pub struct ScriptTaskContext<'a> {
    pub project_id: i64,
    pub mr_iid: i64,
    pub commit_sha: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptTaskResult {
    pub id: String,
    pub title: String,
    pub status: ScriptTaskStatus,
    pub command: String,
    pub source_dir: PathBuf,
    pub run_log_path: PathBuf,
    pub result_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptTaskStatus {
    Passed,
    IssueFound,
    ExecutionFailed(Option<i32>),
    TimedOut,
}

impl ScriptTaskRunner {
    pub fn new() -> Self {
        Self {
            work_root: PathBuf::from(DEFAULT_WORK_ROOT),
            archive_limits: ArchiveLimits::default(),
        }
    }

    pub(crate) fn with_archive_limits(mut self, archive_limits: ArchiveLimits) -> Self {
        self.archive_limits = archive_limits;
        self
    }

    pub async fn run(
        &self,
        task: &ScriptTaskConfig,
        context: &ScriptTaskContext<'_>,
        archive: &[u8],
    ) -> AppResult<ScriptTaskResult> {
        let task_dir = absolute_path(self.task_dir(context, &task.id))?;
        let source_dir = task_dir.join("source");
        let run_log_path = task_dir.join("run.log");
        let result_path = task_dir.join("result.txt");
        info!(
            project_id = context.project_id,
            mr_iid = context.mr_iid,
            commit_sha = %context.commit_sha,
            script_task_id = %task.id,
            work_dir = %task_dir.display(),
            "preparing script task work directory"
        );
        reset_task_dir(&task_dir)?;
        fs::create_dir_all(&source_dir)?;
        let _source_guard = ScriptTaskSourceDirGuard {
            source_dir: source_dir.clone(),
            project_id: context.project_id,
            mr_iid: context.mr_iid,
            commit_sha: context.commit_sha.to_string(),
            script_task_id: task.id.clone(),
        };
        let extracted_files = extract_zip_archive(archive, &source_dir, &self.archive_limits)?;
        info!(
            project_id = context.project_id,
            mr_iid = context.mr_iid,
            commit_sha = %context.commit_sha,
            script_task_id = %task.id,
            archive_bytes = archive.len(),
            extracted_files,
            source_dir = %source_dir.display(),
            "script task archive extracted"
        );
        let script_cwd = script_working_dir()?;

        let command_with_args = command_with_script_args(&task.command, &source_dir, &result_path);
        info!(
            project_id = context.project_id,
            mr_iid = context.mr_iid,
            commit_sha = %context.commit_sha,
            script_task_id = %task.id,
            command = %command_with_args,
            timeout_seconds = task.timeout_seconds,
            work_dir = %task_dir.display(),
            script_cwd = %script_cwd.display(),
            source_dir = %source_dir.display(),
            run_log_path = %run_log_path.display(),
            result_path = %result_path.display(),
            "running script task"
        );

        let run_log = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&run_log_path)?;
        let mut command = shell_command(&task.command, &source_dir, &result_path);
        configure_process_group(&mut command);
        command
            .current_dir(&script_cwd)
            .stdout(Stdio::from(run_log.try_clone()?))
            .stderr(Stdio::from(run_log));

        let started = Instant::now();
        let mut child = command.spawn()?;
        info!(
            project_id = context.project_id,
            mr_iid = context.mr_iid,
            commit_sha = %context.commit_sha,
            script_task_id = %task.id,
            child_pid = ?child.id(),
            "script task process started"
        );
        let timeout = Duration::from_secs(task.timeout_seconds.max(1));
        let status = match time::timeout(timeout, child.wait()).await {
            Ok(status) => {
                let status = status?;
                let mapped = script_task_status(status.code());
                info!(
                    project_id = context.project_id,
                    mr_iid = context.mr_iid,
                    commit_sha = %context.commit_sha,
                    script_task_id = %task.id,
                    exit_code = ?status.code(),
                    status = ?mapped,
                    elapsed_ms = started.elapsed().as_millis(),
                    "script task process exited"
                );
                mapped
            }
            Err(_) => {
                warn!(
                    project_id = context.project_id,
                    mr_iid = context.mr_iid,
                    commit_sha = %context.commit_sha,
                    script_task_id = %task.id,
                    timeout_seconds = task.timeout_seconds,
                    elapsed_ms = started.elapsed().as_millis(),
                    "script task timed out"
                );
                kill_process_tree(&mut child).await;
                let _ = child.wait().await;
                append_timeout_note(&run_log_path, timeout)?;
                ScriptTaskStatus::TimedOut
            }
        };

        info!(
            project_id = context.project_id,
            mr_iid = context.mr_iid,
            commit_sha = %context.commit_sha,
            script_task_id = %task.id,
            status = ?status,
            source_dir = %source_dir.display(),
            run_log_path = %run_log_path.display(),
            result_path = %result_path.display(),
            elapsed_ms = started.elapsed().as_millis(),
            "script task completed"
        );
        Ok(ScriptTaskResult {
            id: task.id.clone(),
            title: task.title.clone(),
            status,
            command: command_with_args,
            source_dir,
            run_log_path,
            result_path,
        })
    }

    fn task_dir(&self, context: &ScriptTaskContext<'_>, task_id: &str) -> PathBuf {
        self.work_root
            .join(context.project_id.to_string())
            .join(context.mr_iid.to_string())
            .join(sanitize_path_segment(context.commit_sha))
            .join(sanitize_path_segment(task_id))
    }
}

struct ScriptTaskSourceDirGuard {
    source_dir: PathBuf,
    project_id: i64,
    mr_iid: i64,
    commit_sha: String,
    script_task_id: String,
}

impl Drop for ScriptTaskSourceDirGuard {
    fn drop(&mut self) {
        if !self.source_dir.exists() {
            return;
        }
        match fs::remove_dir_all(&self.source_dir) {
            Ok(()) => info!(
                project_id = self.project_id,
                mr_iid = self.mr_iid,
                commit_sha = %self.commit_sha,
                script_task_id = %self.script_task_id,
                source_dir = %self.source_dir.display(),
                "script task source directory removed"
            ),
            Err(err) => warn!(
                project_id = self.project_id,
                mr_iid = self.mr_iid,
                commit_sha = %self.commit_sha,
                script_task_id = %self.script_task_id,
                source_dir = %self.source_dir.display(),
                error = %err,
                "failed to remove script task source directory"
            ),
        }
    }
}

impl Default for ScriptTaskRunner {
    fn default() -> Self {
        Self::new()
    }
}

fn script_task_status(code: Option<i32>) -> ScriptTaskStatus {
    match code {
        Some(0) => ScriptTaskStatus::Passed,
        Some(1) => ScriptTaskStatus::IssueFound,
        other => ScriptTaskStatus::ExecutionFailed(other),
    }
}

fn script_working_dir() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("current executable has no parent directory"))
}

fn command_with_script_args(command: &str, check_root: &Path, result_path: &Path) -> String {
    format!(
        "{} {} {}",
        command,
        shell_quote_arg(check_root),
        shell_quote_arg(result_path)
    )
}

fn shell_quote_arg(path: &Path) -> String {
    let value = path.to_string_lossy();
    #[cfg(windows)]
    {
        format!("\"{}\"", value.replace('"', "\"\""))
    }
    #[cfg(not(windows))]
    {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn shell_command(command: &str, check_root: &Path, result_path: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut process = Command::new("cmd");
        process
            .arg("/C")
            .arg(command)
            .arg(check_root)
            .arg(result_path);
        process
    }
    #[cfg(not(windows))]
    {
        let mut process = Command::new("sh");
        process
            .arg("-c")
            .arg(command_with_script_args(command, check_root, result_path));
        process
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

async fn kill_process_tree(child: &mut tokio::process::Child) {
    let Some(pid) = child.id() else {
        let _ = child.kill().await;
        return;
    };
    if !kill_process_tree_by_pid(pid).await {
        let _ = child.kill().await;
    }
}

#[cfg(windows)]
async fn kill_process_tree_by_pid(pid: u32) -> bool {
    let status = Command::new("taskkill")
        .arg("/PID")
        .arg(pid.to_string())
        .arg("/T")
        .arg("/F")
        .status()
        .await;
    match status {
        Ok(status) if status.success() => true,
        Ok(status) => {
            warn!(
                pid,
                status = ?status.code(),
                "taskkill failed; falling back to direct process kill"
            );
            false
        }
        Err(err) => {
            warn!(
                pid,
                error = %err,
                "taskkill failed; falling back to direct process kill"
            );
            false
        }
    }
}

#[cfg(unix)]
async fn kill_process_tree_by_pid(pid: u32) -> bool {
    let process_group = format!("-{pid}");
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(&process_group)
        .status()
        .await;
    let _ = time::timeout(Duration::from_secs(2), async {
        loop {
            let status = Command::new("kill")
                .arg("-0")
                .arg(&process_group)
                .status()
                .await;
            match status {
                Ok(status) if status.success() => {}
                _ => {
                    break;
                }
            }
            time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let status = Command::new("kill")
        .arg("-0")
        .arg(&process_group)
        .status()
        .await;
    match status {
        Ok(status) if status.success() => {
            let _ = Command::new("kill")
                .arg("-KILL")
                .arg(&process_group)
                .status()
                .await;
            true
        }
        _ => true,
    }
}

#[cfg(not(any(unix, windows)))]
async fn kill_process_tree_by_pid(_pid: u32) -> bool {
    false
}

fn reset_task_dir(path: &Path) -> io::Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    fs::create_dir_all(path)
}

fn absolute_path(path: PathBuf) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn append_timeout_note(path: &Path, timeout: Duration) -> io::Result<()> {
    let mut output = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(
        output,
        "\n[gitlab-work-runner] script task timed out after {} seconds",
        timeout.as_secs()
    )
}

fn sanitize_path_segment(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "_".into()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn test_archive() -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut bytes);
            zip.start_file(
                "repo-head/README.md",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(b"test\n").unwrap();
            zip.start_file(
                "repo-head/check-root.cmd",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(
                br#"@if NOT EXIST "%~f1\README.md" exit /B 2
@echo ok>"%~2"
@exit /B 0"#,
            )
            .unwrap();
            zip.start_file(
                "repo-head/check-root.sh",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(br#"[ -f "$1/README.md" ] && echo ok > "$2""#)
                .unwrap();
            zip.finish().unwrap();
        }
        bytes.into_inner()
    }

    #[test]
    fn maps_script_exit_codes_to_statuses() {
        assert_eq!(script_task_status(Some(0)), ScriptTaskStatus::Passed);
        assert_eq!(script_task_status(Some(1)), ScriptTaskStatus::IssueFound);
        assert_eq!(
            script_task_status(Some(2)),
            ScriptTaskStatus::ExecutionFailed(Some(2))
        );
        assert_eq!(
            script_task_status(None),
            ScriptTaskStatus::ExecutionFailed(None)
        );
    }

    #[test]
    fn sanitizes_path_segments() {
        assert_eq!(sanitize_path_segment("check/a:b"), "check_a_b");
        assert_eq!(sanitize_path_segment(""), "_");
    }

    #[tokio::test]
    async fn script_task_passes_check_root_as_argument() {
        let temp = tempfile::tempdir().unwrap();
        let runner = ScriptTaskRunner {
            work_root: temp.path().join("work"),
            archive_limits: ArchiveLimits::default(),
        };
        let command = if cfg!(windows) {
            temp.path()
                .join("work/1/2/abc/check-root/source/check-root.cmd")
                .display()
                .to_string()
        } else {
            format!(
                "sh {}",
                shell_quote_arg(
                    &temp
                        .path()
                        .join("work/1/2/abc/check-root/source/check-root.sh")
                )
            )
        };
        let task = ScriptTaskConfig {
            id: "check-root".into(),
            title: "Check root".into(),
            command,
            timeout_seconds: 5,
        };
        let context = ScriptTaskContext {
            project_id: 1,
            mr_iid: 2,
            commit_sha: "abc",
        };

        let result = runner.run(&task, &context, &test_archive()).await.unwrap();

        assert_eq!(
            result.status,
            ScriptTaskStatus::Passed,
            "{}",
            fs::read_to_string(&result.run_log_path).unwrap_or_default()
        );
        assert!(!result.source_dir.exists());
        assert!(result.run_log_path.exists());
        assert_eq!(fs::read_to_string(result.result_path).unwrap().trim(), "ok");
    }
}
