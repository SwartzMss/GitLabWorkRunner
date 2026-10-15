pub use crate::review::archive::ArchiveLimits;
use crate::{
    error::AppResult,
    review::{archive::extract_zip_archive, work_cleanup::register_active_script_work_dir},
    rules::ScriptTaskConfig,
};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::Mutex,
    time,
};
use tracing::{info, warn};

const DEFAULT_WORK_ROOT: &str = "work/script_tasks";
pub(crate) const MAX_SCRIPT_RESULT_BYTES: u64 = 1024 * 1024;
const MAX_SCRIPT_LOG_BYTES: u64 = 10 * 1024 * 1024;
const PROCESS_TERMINATION_GRACE: Duration = Duration::from_secs(2);
const OUTPUT_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub struct ScriptTaskRunner {
    work_root: PathBuf,
    archive_limits: ArchiveLimits,
}

#[derive(Clone, Copy)]
pub struct ScriptTaskContext<'a> {
    pub project_id: i64,
    pub mr_iid: i64,
    pub commit_sha: &'a str,
    pub repo_url: &'a str,
    pub run_id: &'a str,
}

pub struct ScriptTaskResult {
    pub status: ScriptTaskStatus,
    pub run_log_path: PathBuf,
    pub result_path: PathBuf,
    _active_work_dir: crate::review::work_cleanup::ActiveScriptWorkDirGuard,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptTaskStatus {
    Passed,
    Failed(Option<i32>),
    TimedOut,
    OutputLimit,
}

struct LimitedScriptLog {
    file: std::fs::File,
    written: u64,
    failed: bool,
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

    pub(crate) fn with_work_root(mut self, work_root: PathBuf) -> Self {
        self.work_root = work_root;
        self
    }

    pub async fn run(
        &self,
        task: &ScriptTaskConfig,
        context: &ScriptTaskContext<'_>,
        archive: &[u8],
    ) -> AppResult<ScriptTaskResult> {
        let task_dir = absolute_path(self.task_dir(context, &task.id))?;
        let _active_work_dir = register_active_script_work_dir(task_dir.clone())?;
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
        create_task_dir(&task_dir)?;
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
        let run_log = Arc::new(Mutex::new(LimitedScriptLog {
            file: run_log,
            written: 0,
            failed: false,
        }));
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&result_path)?;
        let mut command = shell_command(&task.command, &source_dir, &result_path);
        configure_process_group(&mut command);
        command
            .current_dir(&script_cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env("GWR_REPO_URL", context.repo_url)
            .env("GWR_COMMIT_SHA", context.commit_sha)
            .env("GWR_TASK_ID", &task.id)
            .env("GWR_RUN_ID", context.run_id);

        let started = Instant::now();
        let mut child = command.spawn()?;
        let stdout_reader = tokio::spawn(drain_output(
            child.stdout.take().expect("piped stdout is available"),
            Arc::clone(&run_log),
        ));
        let stderr_reader = tokio::spawn(drain_output(
            child.stderr.take().expect("piped stderr is available"),
            Arc::clone(&run_log),
        ));
        info!(
            project_id = context.project_id,
            mr_iid = context.mr_iid,
            commit_sha = %context.commit_sha,
            script_task_id = %task.id,
            child_pid = ?child.id(),
            "script task process started"
        );
        let timeout = Duration::from_secs(task.timeout_seconds.max(1));
        let timeout_deadline = time::Instant::now() + timeout;
        let mut output_check = time::interval_at(
            time::Instant::now() + OUTPUT_POLL_INTERVAL,
            OUTPUT_POLL_INTERVAL,
        );
        let (status, note) = loop {
            tokio::select! {
                status = child.wait() => {
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
                    break (mapped, None);
                }
                _ = time::sleep_until(timeout_deadline) => {
                    warn!(
                        project_id = context.project_id,
                        mr_iid = context.mr_iid,
                        commit_sha = %context.commit_sha,
                        script_task_id = %task.id,
                        timeout_seconds = task.timeout_seconds,
                        elapsed_ms = started.elapsed().as_millis(),
                        "script task timed out"
                    );
                    terminate_and_reap(&mut child).await;
                    break (
                        ScriptTaskStatus::TimedOut,
                        Some(format!(
                            "\n[gitlab-work-runner] script task timed out after {} seconds\n",
                            timeout.as_secs()
                        )),
                    );
                }
                _ = output_check.tick() => {
                    if file_exceeds_limit(&result_path, MAX_SCRIPT_RESULT_BYTES) {
                        warn!(
                            project_id = context.project_id,
                            mr_iid = context.mr_iid,
                            commit_sha = %context.commit_sha,
                            script_task_id = %task.id,
                            max_result_bytes = MAX_SCRIPT_RESULT_BYTES,
                            "script task result exceeded the size limit"
                        );
                        terminate_and_reap(&mut child).await;
                        break (
                            ScriptTaskStatus::OutputLimit,
                            Some("\n[gitlab-work-runner] script task result exceeded the size limit\n".into()),
                        );
                    }
                }
            }
        };
        wait_for_output_reader(stdout_reader).await;
        wait_for_output_reader(stderr_reader).await;
        if let Some(note) = &note {
            append_bounded_note(&run_log_path, note)?;
        }
        let status = if file_exceeds_limit(&result_path, MAX_SCRIPT_RESULT_BYTES) {
            ScriptTaskStatus::OutputLimit
        } else {
            status
        };
        if status == ScriptTaskStatus::OutputLimit && note.is_none() {
            append_bounded_note(
                &run_log_path,
                "\n[gitlab-work-runner] script task result exceeded the size limit\n",
            )?;
        }
        if status == ScriptTaskStatus::OutputLimit {
            match fs::remove_file(&result_path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => warn!(
                    result_path = %result_path.display(),
                    error = %err,
                    "failed to remove oversized script task result"
                ),
            }
        }

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
            status,
            run_log_path,
            result_path,
            _active_work_dir,
        })
    }

    fn task_dir(&self, context: &ScriptTaskContext<'_>, task_id: &str) -> PathBuf {
        self.work_root
            .join(context.project_id.to_string())
            .join(context.mr_iid.to_string())
            .join(sanitize_path_segment(context.commit_sha))
            .join(sanitize_path_segment(context.run_id))
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
        other => ScriptTaskStatus::Failed(other),
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

pub(crate) fn shell_quote_arg(path: &Path) -> String {
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

async fn terminate_and_reap(child: &mut Child) {
    #[cfg(unix)]
    let group_terminated = child.id().is_some_and(kill_process_tree_by_pid);
    #[cfg(not(unix))]
    let group_terminated = match child.id() {
        Some(pid) => kill_process_tree_by_pid(pid).await,
        None => false,
    };
    if !group_terminated {
        warn!("could not terminate script process group; killing the direct child");
        let _ = child.start_kill();
    }
    if time::timeout(PROCESS_TERMINATION_GRACE, child.wait())
        .await
        .is_err()
    {
        warn!("script process did not exit after termination; killing the direct child");
        let _ = child.start_kill();
        if time::timeout(PROCESS_TERMINATION_GRACE, child.wait())
            .await
            .is_err()
        {
            warn!("script process could not be reaped after forced termination");
        }
    }
}

#[cfg(windows)]
async fn kill_process_tree_by_pid(pid: u32) -> bool {
    let status = time::timeout(
        PROCESS_TERMINATION_GRACE,
        Command::new("taskkill")
            .arg("/PID")
            .arg(pid.to_string())
            .arg("/T")
            .arg("/F")
            .kill_on_drop(true)
            .status(),
    )
    .await;
    match status {
        Ok(Ok(status)) if status.success() => true,
        Ok(Ok(status)) => {
            warn!(
                pid,
                status = ?status.code(),
                "taskkill failed; falling back to direct process kill"
            );
            false
        }
        Ok(Err(err)) => {
            warn!(
                pid,
                error = %err,
                "taskkill failed; falling back to direct process kill"
            );
            false
        }
        Err(_) => {
            warn!(
                pid,
                "taskkill timed out; falling back to direct process kill; descendants may remain"
            );
            false
        }
    }
}

#[cfg(unix)]
fn kill_process_tree_by_pid(pid: u32) -> bool {
    let Ok(process_group) = i32::try_from(pid) else {
        return false;
    };
    let current_group = unsafe { libc::getpgrp() };
    if process_group <= 1 || process_group == current_group {
        warn!(
            pid,
            current_group, "refusing to signal the runner process group"
        );
        return false;
    }

    match signal_process_group(process_group, libc::SIGKILL) {
        Ok(()) => {}
        Err(err) if err.raw_os_error() == Some(libc::ESRCH) => return true,
        Err(err) => {
            warn!(pid, error = %err, "failed to send SIGKILL to script process group");
            return false;
        }
    }
    true
}

#[cfg(unix)]
fn signal_process_group(process_group: i32, signal: i32) -> io::Result<()> {
    let result = unsafe { libc::kill(-process_group, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(unix, windows)))]
async fn kill_process_tree_by_pid(_pid: u32) -> bool {
    false
}

fn create_task_dir(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("script task directory has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::create_dir(path)
}

fn absolute_path(path: PathBuf) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

async fn drain_output<R>(mut stream: R, log: Arc<Mutex<LimitedScriptLog>>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = [0_u8; 8192];
    loop {
        let bytes_read = match stream.read(&mut buffer).await {
            Ok(0) => return,
            Ok(bytes_read) => bytes_read,
            Err(err) => {
                warn!(error = %err, "failed to read script output stream");
                return;
            }
        };
        let mut output = log.lock().await;
        if output.failed {
            continue;
        }
        let available = MAX_SCRIPT_LOG_BYTES.saturating_sub(output.written) as usize;
        let bytes_to_write = bytes_read.min(available);
        if bytes_to_write == 0 {
            continue;
        }
        if let Err(err) = output.file.write_all(&buffer[..bytes_to_write]) {
            warn!(error = %err, "failed to write script output log; continuing to drain output");
            output.failed = true;
            continue;
        }
        output.written += bytes_to_write as u64;
    }
}

async fn wait_for_output_reader(mut reader: tokio::task::JoinHandle<()>) {
    if time::timeout(PROCESS_TERMINATION_GRACE, &mut reader)
        .await
        .is_err()
    {
        reader.abort();
        warn!("script output stream did not close after process termination");
    }
}

fn file_exceeds_limit(path: &Path, limit: u64) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.len() > limit)
        .unwrap_or(false)
}

fn append_bounded_note(path: &Path, note: &str) -> io::Result<()> {
    let current_len = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let remaining = MAX_SCRIPT_LOG_BYTES.saturating_sub(current_len) as usize;
    let note = &note.as_bytes()[..note.len().min(remaining)];
    if note.is_empty() {
        return Ok(());
    }
    let mut output = OpenOptions::new().create(true).append(true).open(path)?;
    output.write_all(note)
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

    fn native_script_command(script_path: &Path) -> String {
        if cfg!(windows) {
            shell_quote_arg(script_path)
        } else {
            format!("sh {}", shell_quote_arg(script_path))
        }
    }

    fn script_command(script_path: &Path) -> String {
        if cfg!(windows) {
            format!(
                "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File {}",
                shell_quote_arg(script_path)
            )
        } else {
            format!("sh {}", shell_quote_arg(script_path))
        }
    }

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
        assert_eq!(
            script_task_status(Some(1)),
            ScriptTaskStatus::Failed(Some(1))
        );
        assert_eq!(
            script_task_status(Some(2)),
            ScriptTaskStatus::Failed(Some(2))
        );
        assert_eq!(script_task_status(None), ScriptTaskStatus::Failed(None));
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
                .join("work/1/2/abc/run-one/check-root/source/check-root.cmd")
                .display()
                .to_string()
        } else {
            format!(
                "sh {}",
                shell_quote_arg(
                    &temp
                        .path()
                        .join("work/1/2/abc/run-one/check-root/source/check-root.sh")
                )
            )
        };
        let task = ScriptTaskConfig {
            enabled: true,
            id: "check-root".into(),
            title: "Check root".into(),
            command,
            timeout_seconds: 5,
            max_concurrent: 1,
        };
        let context = ScriptTaskContext {
            project_id: 1,
            mr_iid: 2,
            commit_sha: "abc",
            repo_url: "https://gitlab.example/group/repo.git",
            run_id: "run-one",
        };

        let result = runner.run(&task, &context, &test_archive()).await.unwrap();

        assert_eq!(
            result.status,
            ScriptTaskStatus::Passed,
            "{}",
            fs::read_to_string(&result.run_log_path).unwrap_or_default()
        );
        assert!(!result.result_path.parent().unwrap().join("source").exists());
        assert!(result.run_log_path.exists());
        assert_eq!(fs::read_to_string(result.result_path).unwrap().trim(), "ok");
    }

    #[tokio::test]
    async fn script_receives_run_context_in_environment_and_keeps_two_path_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let runner = ScriptTaskRunner {
            work_root: temp.path().join("work"),
            archive_limits: ArchiveLimits::default(),
        };
        let script_path = temp.path().join(if cfg!(windows) {
            "record-context.cmd"
        } else {
            "record-context.sh"
        });
        #[cfg(windows)]
        fs::write(
            &script_path,
            "@echo off\r\nif not \"%~3\"==\"\" exit /b 1\r\n> \"%~2\" echo 2\r\n>> \"%~2\" echo %~1\r\n>> \"%~2\" echo %~2\r\n>> \"%~2\" echo %GWR_REPO_URL%\r\n>> \"%~2\" echo %GWR_COMMIT_SHA%\r\n>> \"%~2\" echo %GWR_TASK_ID%\r\n>> \"%~2\" echo %GWR_RUN_ID%\r\n",
        )
        .unwrap();
        #[cfg(not(windows))]
        fs::write(
            &script_path,
            "printf '%s\\n' \"$#\" \"$1\" \"$2\" \"$GWR_REPO_URL\" \"$GWR_COMMIT_SHA\" \"$GWR_TASK_ID\" \"$GWR_RUN_ID\" > \"$2\"\n",
        )
        .unwrap();
        let command = native_script_command(&script_path);
        let task = ScriptTaskConfig {
            enabled: true,
            id: "check-env".into(),
            title: "Check environment".into(),
            command,
            timeout_seconds: 5,
            max_concurrent: 1,
        };
        let repo_url = "https://gitlab.example/group/source.git";
        let context = ScriptTaskContext {
            project_id: 12,
            mr_iid: 34,
            commit_sha: "0123456789abcdef0123456789abcdef01234567",
            repo_url,
            run_id: "run-unique-1",
        };

        let result = runner.run(&task, &context, &test_archive()).await.unwrap();

        assert_eq!(result.status, ScriptTaskStatus::Passed);
        let values = fs::read_to_string(result.result_path).unwrap();
        let values = values.lines().collect::<Vec<_>>();
        assert_eq!(values.len(), 7);
        assert_eq!(values[0], "2");
        assert!(values[1].ends_with("/source") || values[1].ends_with("\\source"));
        assert!(values[2].ends_with("/result.txt") || values[2].ends_with("\\result.txt"));
        assert_eq!(values[3], repo_url);
        assert_eq!(values[4], context.commit_sha);
        assert_eq!(values[5], task.id);
        assert_eq!(values[6], context.run_id);
    }

    #[tokio::test]
    async fn concurrent_script_runs_keep_task_and_commit_environment_separate() {
        let temp = tempfile::tempdir().unwrap();
        let runner = ScriptTaskRunner {
            work_root: temp.path().join("work"),
            archive_limits: ArchiveLimits::default(),
        };
        let script_path = temp.path().join(if cfg!(windows) {
            "record-context.cmd"
        } else {
            "record-context.sh"
        });
        #[cfg(windows)]
        fs::write(
            &script_path,
            "@echo off\r\n> \"%~2\" echo %GWR_REPO_URL%\r\n>> \"%~2\" echo %GWR_COMMIT_SHA%\r\n>> \"%~2\" echo %GWR_TASK_ID%\r\n>> \"%~2\" echo %GWR_RUN_ID%\r\n",
        )
        .unwrap();
        #[cfg(not(windows))]
        fs::write(
            &script_path,
            "printf '%s\\n' \"$GWR_REPO_URL\" \"$GWR_COMMIT_SHA\" \"$GWR_TASK_ID\" \"$GWR_RUN_ID\" > \"$2\"\n",
        )
        .unwrap();
        let command = native_script_command(&script_path);
        let task_a = ScriptTaskConfig {
            enabled: true,
            id: "task-a".into(),
            title: "Task A".into(),
            command: command.clone(),
            timeout_seconds: 5,
            max_concurrent: 2,
        };
        let task_b = ScriptTaskConfig {
            id: "task-b".into(),
            title: "Task B".into(),
            ..task_a.clone()
        };
        let context_a = ScriptTaskContext {
            project_id: 12,
            mr_iid: 34,
            commit_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            repo_url: "https://gitlab.example/group/a.git",
            run_id: "run-a-1",
        };
        let context_b = ScriptTaskContext {
            project_id: 12,
            mr_iid: 34,
            commit_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            repo_url: "https://gitlab.example/group/b.git",
            run_id: "run-b-1",
        };
        let context_a_again = ScriptTaskContext {
            commit_sha: "cccccccccccccccccccccccccccccccccccccccc",
            repo_url: "https://gitlab.example/group/c.git",
            run_id: "run-a-2",
            ..context_a
        };

        let archive = test_archive();
        let (run_a, run_b, run_a_again) = tokio::join!(
            runner.run(&task_a, &context_a, &archive),
            runner.run(&task_b, &context_b, &archive),
            runner.run(&task_a, &context_a_again, &archive),
        );
        let run_a = run_a.unwrap();
        let run_b = run_b.unwrap();
        let run_a_again = run_a_again.unwrap();

        assert_ne!(run_a.result_path, run_a_again.result_path);
        assert_ne!(run_a.result_path, run_b.result_path);
        assert_eq!(
            fs::read_to_string(run_a.result_path)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "https://gitlab.example/group/a.git",
                context_a.commit_sha,
                "task-a",
                "run-a-1"
            ]
        );
        assert_eq!(
            fs::read_to_string(run_b.result_path)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "https://gitlab.example/group/b.git",
                context_b.commit_sha,
                "task-b",
                "run-b-1"
            ]
        );
        assert_eq!(
            fs::read_to_string(run_a_again.result_path)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "https://gitlab.example/group/c.git",
                context_a_again.commit_sha,
                "task-a",
                "run-a-2"
            ]
        );
    }

    #[tokio::test]
    async fn every_nonzero_exit_code_is_a_task_failure() {
        let temp = tempfile::tempdir().unwrap();
        let runner = ScriptTaskRunner {
            work_root: temp.path().join("work"),
            archive_limits: ArchiveLimits::default(),
        };
        let task = ScriptTaskConfig {
            enabled: true,
            id: "failing".into(),
            title: "Failing check".into(),
            command: "exit 1".into(),
            timeout_seconds: 5,
            max_concurrent: 1,
        };
        let context = ScriptTaskContext {
            project_id: 1,
            mr_iid: 2,
            commit_sha: "abc",
            repo_url: "https://gitlab.example/group/repo.git",
            run_id: "run-1",
        };

        let result = runner.run(&task, &context, &test_archive()).await.unwrap();

        assert_eq!(result.status, ScriptTaskStatus::Failed(Some(1)));
    }

    #[tokio::test]
    async fn parallel_runs_get_distinct_result_file_paths() {
        let temp = tempfile::tempdir().unwrap();
        let runner = ScriptTaskRunner {
            work_root: temp.path().join("work"),
            archive_limits: ArchiveLimits::default(),
        };
        let task = ScriptTaskConfig {
            enabled: true,
            id: "check".into(),
            title: "Check".into(),
            command: "true".into(),
            timeout_seconds: 5,
            max_concurrent: 2,
        };
        let first = ScriptTaskContext {
            project_id: 1,
            mr_iid: 2,
            commit_sha: "abc",
            repo_url: "https://gitlab.example/group/repo.git",
            run_id: "run-1",
        };
        let second = ScriptTaskContext {
            run_id: "run-2",
            ..first
        };

        let first_result = runner.run(&task, &first, &test_archive()).await.unwrap();
        let second_result = runner.run(&task, &second, &test_archive()).await.unwrap();

        assert_ne!(first_result.result_path, second_result.result_path);
        assert!(first_result.run_log_path.exists());
        assert!(second_result.run_log_path.exists());
        let work_root = temp.path().join("work");
        assert_eq!(
            crate::review::work_cleanup::cleanup_stale_script_task_work(&work_root, Duration::ZERO)
                .unwrap(),
            0,
            "active run directories must survive cleanup"
        );
        drop(first_result);
        drop(second_result);
        assert_eq!(
            crate::review::work_cleanup::cleanup_stale_script_task_work(&work_root, Duration::ZERO)
                .unwrap(),
            2,
            "completed run directories should be removed after their retention period"
        );
    }

    #[tokio::test]
    async fn script_logs_are_capped_without_failing_the_check() {
        let temp = tempfile::tempdir().unwrap();
        let runner = ScriptTaskRunner {
            work_root: temp.path().join("work"),
            archive_limits: ArchiveLimits::default(),
        };
        let script_path = temp.path().join(if cfg!(windows) {
            "verbose.ps1"
        } else {
            "verbose.sh"
        });
        #[cfg(windows)]
        fs::write(
            &script_path,
            "$payload = 'x' * 6000000\n[Console]::Out.Write($payload)\n[Console]::Error.Write($payload)\n",
        )
        .unwrap();
        #[cfg(not(windows))]
        fs::write(
            &script_path,
            "head -c 6000000 /dev/zero\nhead -c 6000000 /dev/zero >&2\n",
        )
        .unwrap();
        let task = ScriptTaskConfig {
            enabled: true,
            id: "verbose".into(),
            title: "Verbose check".into(),
            command: script_command(&script_path),
            timeout_seconds: 10,
            max_concurrent: 1,
        };
        let context = ScriptTaskContext {
            project_id: 1,
            mr_iid: 2,
            commit_sha: "abc",
            repo_url: "https://gitlab.example/group/repo.git",
            run_id: "run-verbose",
        };

        let result = runner.run(&task, &context, &test_archive()).await.unwrap();

        assert_eq!(result.status, ScriptTaskStatus::Passed);
        assert!(fs::metadata(result.run_log_path).unwrap().len() <= 10 * 1024 * 1024);
    }

    #[tokio::test]
    async fn oversized_result_file_terminates_a_still_running_check() {
        let temp = tempfile::tempdir().unwrap();
        let runner = ScriptTaskRunner {
            work_root: temp.path().join("work"),
            archive_limits: ArchiveLimits::default(),
        };
        let script_path = temp.path().join(if cfg!(windows) {
            "oversized-result.ps1"
        } else {
            "oversized-result.sh"
        });
        #[cfg(windows)]
        fs::write(
            &script_path,
            "$bytes = New-Object byte[] 1048577\n[System.IO.File]::WriteAllBytes($args[1], $bytes)\nStart-Sleep -Seconds 30\n",
        )
        .unwrap();
        #[cfg(not(windows))]
        fs::write(
            &script_path,
            "i=0\nwhile [ \"$i\" -lt 129 ]; do\n  printf '%8192s' '' >> \"$2\"\n  i=$((i + 1))\ndone\nsleep 30\n",
        )
        .unwrap();
        let task = ScriptTaskConfig {
            enabled: true,
            id: "oversized-result".into(),
            title: "Oversized result check".into(),
            command: script_command(&script_path),
            timeout_seconds: 60,
            max_concurrent: 1,
        };
        let context = ScriptTaskContext {
            project_id: 1,
            mr_iid: 2,
            commit_sha: "abc",
            repo_url: "https://gitlab.example/group/repo.git",
            run_id: "run-oversized-result",
        };

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            runner.run(&task, &context, &test_archive()),
        )
        .await
        .expect("result output limit should stop a script before its timeout")
        .unwrap();

        assert_eq!(format!("{:?}", result.status), "OutputLimit");
        assert!(
            !result.result_path.exists(),
            "oversized result files should not be retained with the run artifacts"
        );
    }

    #[tokio::test]
    async fn oversized_result_from_failed_check_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let runner = ScriptTaskRunner {
            work_root: temp.path().join("work"),
            archive_limits: ArchiveLimits::default(),
        };
        let script_path = temp.path().join(if cfg!(windows) {
            "failed-oversized-result.ps1"
        } else {
            "failed-oversized-result.sh"
        });
        #[cfg(windows)]
        fs::write(
            &script_path,
            "$bytes = New-Object byte[] 1048577\n[System.IO.File]::WriteAllBytes($args[1], $bytes)\nStart-Sleep -Seconds 2\nexit 7\n",
        )
        .unwrap();
        #[cfg(not(windows))]
        fs::write(
            &script_path,
            "i=0\nwhile [ \"$i\" -lt 129 ]; do\n  printf '%8192s' '' >> \"$2\"\n  i=$((i + 1))\ndone\nexit 7\n",
        )
        .unwrap();
        let task = ScriptTaskConfig {
            enabled: true,
            id: "failed-oversized-result".into(),
            title: "Failed oversized result check".into(),
            command: script_command(&script_path),
            timeout_seconds: 10,
            max_concurrent: 1,
        };
        let context = ScriptTaskContext {
            project_id: 1,
            mr_iid: 2,
            commit_sha: "abc",
            repo_url: "https://gitlab.example/group/repo.git",
            run_id: "run-failed-oversized-result",
        };

        let result = runner.run(&task, &context, &test_archive()).await.unwrap();

        assert_eq!(result.status, ScriptTaskStatus::OutputLimit);
        assert!(!result.result_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn script_timeout_terminates_the_process_group_without_external_kill() {
        let temp = tempfile::tempdir().unwrap();
        let runner = ScriptTaskRunner {
            work_root: temp.path().join("work"),
            archive_limits: ArchiveLimits::default(),
        };
        let script_path = temp.path().join("timeout.sh");
        let marker_path = temp.path().join("child-finished");
        let child_pid_path = temp.path().join("child-pid");
        fs::write(
            &script_path,
            format!(
                "sleep 30 &\nchild_pid=$!\nprintf '%s' \"$child_pid\" > {}\nwait \"$child_pid\"\nprintf done > {}\n",
                shell_quote_arg(&child_pid_path),
                shell_quote_arg(&marker_path)
            ),
        )
        .unwrap();
        let task = ScriptTaskConfig {
            enabled: true,
            id: "timeout".into(),
            title: "Timeout check".into(),
            command: format!("sh {}", shell_quote_arg(&script_path)),
            timeout_seconds: 1,
            max_concurrent: 1,
        };
        let context = ScriptTaskContext {
            project_id: 1,
            mr_iid: 2,
            commit_sha: "abc",
            repo_url: "https://gitlab.example/group/repo.git",
            run_id: "run-timeout",
        };

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            runner.run(&task, &context, &test_archive()),
        )
        .await
        .expect("timeout cleanup must not wait forever for an un-killed process group")
        .unwrap();

        assert_eq!(result.status, ScriptTaskStatus::TimedOut);
        let child_pid = fs::read_to_string(child_pid_path).unwrap();
        let child_is_running = Command::new("sh")
            .arg("-c")
            .arg(format!("kill -0 {child_pid} 2>/dev/null"))
            .status()
            .await
            .unwrap()
            .success();
        if child_is_running {
            let _ = Command::new("sh")
                .arg("-c")
                .arg(format!("kill -KILL {child_pid} 2>/dev/null"))
                .status()
                .await;
        }
        assert!(
            !child_is_running && !marker_path.exists(),
            "the child process continued after timeout"
        );
    }
}
