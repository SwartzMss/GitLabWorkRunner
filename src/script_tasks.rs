use crate::{
    error::{AppError, AppResult},
    rules::ScriptTaskConfig,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Cursor, Read, Write},
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{process::Command, time};
use tracing::{info, warn};
use zip::ZipArchive;

const DEFAULT_WORK_ROOT: &str = "work/script_tasks";
const MAX_OUTPUT_BYTES: u64 = 16 * 1024;

pub struct ScriptTaskRunner {
    work_root: PathBuf,
    max_output_bytes: u64,
}

pub struct ScriptTaskContext<'a> {
    pub project_id: i64,
    pub mr_iid: i64,
    pub commit_sha: &'a str,
    pub token_env: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptTaskResult {
    pub id: String,
    pub title: String,
    pub status: ScriptTaskStatus,
    pub command: String,
    pub source_dir: PathBuf,
    pub output_path: PathBuf,
    pub output_excerpt: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptTaskStatus {
    Passed,
    Failed(Option<i32>),
    TimedOut,
}

impl ScriptTaskResult {
    pub fn should_comment(&self) -> bool {
        !matches!(self.status, ScriptTaskStatus::Passed)
    }
}

impl ScriptTaskRunner {
    pub fn new() -> Self {
        Self {
            work_root: PathBuf::from(DEFAULT_WORK_ROOT),
            max_output_bytes: MAX_OUTPUT_BYTES,
        }
    }

    pub async fn run(
        &self,
        task: &ScriptTaskConfig,
        context: &ScriptTaskContext<'_>,
        archive: &[u8],
    ) -> AppResult<ScriptTaskResult> {
        let task_dir = self.task_dir(context, &task.id);
        let source_dir = task_dir.join("source");
        let output_path = task_dir.join("output.log");
        reset_task_dir(&task_dir)?;
        fs::create_dir_all(&source_dir)?;
        extract_zip_archive(archive, &source_dir)?;

        info!(
            project_id = context.project_id,
            mr_iid = context.mr_iid,
            commit_sha = %context.commit_sha,
            script_task_id = %task.id,
            command = %task.command,
            timeout_seconds = task.timeout_seconds,
            work_dir = %task_dir.display(),
            source_dir = %source_dir.display(),
            output_path = %output_path.display(),
            "running script task"
        );

        let output = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&output_path)?;
        let mut command = shell_command(&task.command);
        configure_process_group(&mut command);
        command
            .current_dir(&source_dir)
            .env("GITLAB_WORK_RUNNER_SOURCE_DIR", &source_dir)
            .env("GITLAB_WORK_RUNNER_TASK_DIR", &task_dir)
            .env("GITLAB_WORK_RUNNER_OUTPUT_PATH", &output_path)
            .env("GITLAB_WORK_RUNNER_COMMIT_SHA", context.commit_sha)
            .env_remove(context.token_env)
            .env_remove("GITLAB_TOKEN")
            .stdout(Stdio::from(output.try_clone()?))
            .stderr(Stdio::from(output));

        let mut child = command.spawn()?;
        let timeout = Duration::from_secs(task.timeout_seconds.max(1));
        let status = match time::timeout(timeout, child.wait()).await {
            Ok(status) => {
                let status = status?;
                if status.success() {
                    ScriptTaskStatus::Passed
                } else {
                    ScriptTaskStatus::Failed(status.code())
                }
            }
            Err(_) => {
                warn!(
                    project_id = context.project_id,
                    mr_iid = context.mr_iid,
                    commit_sha = %context.commit_sha,
                    script_task_id = %task.id,
                    timeout_seconds = task.timeout_seconds,
                    "script task timed out"
                );
                kill_process_tree(&mut child).await;
                let _ = child.wait().await;
                append_timeout_note(&output_path, timeout)?;
                ScriptTaskStatus::TimedOut
            }
        };

        if source_dir.exists() {
            fs::remove_dir_all(&source_dir)?;
        }

        let output_excerpt = read_output_excerpt(&output_path, self.max_output_bytes)?;
        info!(
            project_id = context.project_id,
            mr_iid = context.mr_iid,
            commit_sha = %context.commit_sha,
            script_task_id = %task.id,
            status = ?status,
            source_dir = %source_dir.display(),
            output_path = %output_path.display(),
            "script task completed"
        );
        Ok(ScriptTaskResult {
            id: task.id.clone(),
            title: task.title.clone(),
            status,
            command: task.command.clone(),
            source_dir,
            output_path,
            output_excerpt,
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

impl Default for ScriptTaskRunner {
    fn default() -> Self {
        Self::new()
    }
}

pub fn build_script_task_comment(result: &ScriptTaskResult) -> String {
    let execution_note = format!(
        "执行命令：`{}`\n\n检查目录：`{}`\n\n> 检查目录是 MR head 的临时代码快照，任务结束后会被清理；排查时看下面的输出文件。",
        result.command,
        result.source_dir.display()
    );
    let status = match result.status {
        ScriptTaskStatus::Passed => "passed",
        ScriptTaskStatus::Failed(Some(code)) => {
            let hint = exit_code_hint(code);
            return format!(
                "**[error] {}**\n\n脚本任务执行失败，退出码：`{}`。{}\n\n{}\n\n```text\n{}\n```\n\n输出文件：`{}`\n\n<!-- gitlab-work-runner:script={} -->",
                result.title,
                code,
                hint,
                execution_note,
                result.output_excerpt,
                result.output_path.display(),
                result.id
            );
        }
        ScriptTaskStatus::Failed(None) => "failed",
        ScriptTaskStatus::TimedOut => "timed out",
    };
    format!(
        "**[error] {}**\n\n脚本任务执行失败：`{}`。\n\n{}\n\n```text\n{}\n```\n\n输出文件：`{}`\n\n<!-- gitlab-work-runner:script={} -->",
        result.title,
        status,
        execution_note,
        result.output_excerpt,
        result.output_path.display(),
        result.id
    )
}

fn exit_code_hint(code: i32) -> &'static str {
    match code {
        9009 => " Windows 上 `9009` 通常表示命令未找到，请确认脚本解释器或命令已安装并在 PATH 中。",
        127 => " Unix 上 `127` 通常表示命令未找到，请确认脚本解释器或命令已安装并在 PATH 中。",
        _ => "",
    }
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut process = Command::new("cmd");
        process.arg("/C").arg(command);
        process
    }
    #[cfg(not(windows))]
    {
        let mut process = Command::new("sh");
        process.arg("-c").arg(command);
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

fn append_timeout_note(path: &Path, timeout: Duration) -> io::Result<()> {
    let mut output = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(
        output,
        "\n[gitlab-work-runner] script task timed out after {} seconds",
        timeout.as_secs()
    )
}

fn read_output_excerpt(path: &Path, max_bytes: u64) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut buffer = Vec::new();
    let mut limited = std::io::Read::by_ref(&mut file).take(max_bytes + 1);
    limited.read_to_end(&mut buffer)?;
    let truncated = buffer.len() as u64 > max_bytes;
    if truncated {
        buffer.truncate(max_bytes as usize);
    }
    let mut text = String::from_utf8_lossy(&buffer).to_string();
    if truncated {
        text.push_str("\n[gitlab-work-runner] output truncated");
    }
    if text.trim().is_empty() {
        text.push_str("[gitlab-work-runner] no output captured");
    }
    Ok(text)
}

fn extract_zip_archive(bytes: &[u8], destination: &Path) -> AppResult<()> {
    let reader = Cursor::new(bytes);
    let mut archive =
        ZipArchive::new(reader).map_err(|err| AppError::ScriptTask(err.to_string()))?;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|err| AppError::ScriptTask(err.to_string()))?;
        let Some(path) = file.enclosed_name() else {
            continue;
        };
        let relative = strip_first_component(&path);
        if relative.as_os_str().is_empty() {
            continue;
        }
        let output_path = destination.join(relative);
        if file.is_dir() {
            fs::create_dir_all(&output_path)?;
            continue;
        }
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = File::create(&output_path)?;
        io::copy(&mut file, &mut output)?;
        set_unix_mode(&output_path, file.unix_mode())?;
    }
    Ok(())
}

fn strip_first_component(path: &Path) -> PathBuf {
    path.components()
        .skip(1)
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect()
}

#[cfg(unix)]
fn set_unix_mode(path: &Path, mode: Option<u32>) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_unix_mode(_path: &Path, _mode: Option<u32>) -> io::Result<()> {
    Ok(())
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

    #[test]
    fn sanitizes_path_segments() {
        assert_eq!(sanitize_path_segment("check/a:b"), "check_a_b");
        assert_eq!(sanitize_path_segment(""), "_");
    }

    #[test]
    fn script_task_comment_includes_execution_context_and_command_hint() {
        let result = ScriptTaskResult {
            id: "check-todo-tbd".into(),
            title: "TODO/TBD marker check".into(),
            status: ScriptTaskStatus::Failed(Some(9009)),
            command: "python3 examples/scripts/check_todo_tbd.py".into(),
            source_dir: PathBuf::from("work/script_tasks/1/1/abc/check-todo-tbd/source"),
            output_path: PathBuf::from("work/script_tasks/1/1/abc/check-todo-tbd/output.log"),
            output_excerpt: "[gitlab-work-runner] no output captured".into(),
        };

        let comment = build_script_task_comment(&result);

        assert!(comment.contains("执行命令"));
        assert!(comment.contains("检查目录"));
        assert!(comment.contains("MR head"));
        assert!(comment.contains("9009"));
        assert!(comment.contains("命令未找到"));
        assert!(comment.contains("output.log"));
    }
}
