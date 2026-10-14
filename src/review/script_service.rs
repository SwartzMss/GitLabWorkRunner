use crate::{
    app::webhook::MergeRequestNoteEvent,
    error::{AppError, AppResult, ReviewErrorCode, ReviewFailure},
    gitlab::{
        CreateDiscussionRequest, CreatedDiscussion, GitLabClient, MergeRequestChanges,
        PublishPosition,
    },
    review::{
        archive::ArchiveLimits,
        comments::CommentDraft,
        scripts::{ScriptTaskContext, ScriptTaskRunner, ScriptTaskStatus, MAX_SCRIPT_RESULT_BYTES},
    },
    rules::ScriptTaskConfig,
    storage::{
        ReviewRequestStart, StateStore, StoredComment, StoredFinding, TaskRunFinish, TaskRunStart,
    },
};
use std::fs;
use tracing::{info, warn};

pub(crate) struct ScriptTaskService {
    gitlab: GitLabClient,
    store: StateStore,
    archive_limits: ArchiveLimits,
    work_root: Option<std::path::PathBuf>,
}

struct ScriptCommentRecord<'a> {
    path: &'a str,
    line: Option<u32>,
    discussion_id: &'a str,
    publish_position: &'a str,
}

impl ScriptTaskService {
    pub(crate) fn new(
        gitlab: GitLabClient,
        store: StateStore,
        archive_limits: ArchiveLimits,
    ) -> Self {
        Self {
            gitlab,
            store,
            archive_limits,
            work_root: None,
        }
    }

    #[cfg(all(test, unix))]
    fn with_work_root(mut self, work_root: std::path::PathBuf) -> Self {
        self.work_root = Some(work_root);
        self
    }

    pub(crate) async fn run(
        &self,
        event: &MergeRequestNoteEvent,
        task: ScriptTaskConfig,
        run_id: &str,
    ) {
        let requested_ids =
            serde_json::to_string(&[task.id.as_str()]).unwrap_or_else(|_| "[]".into());
        let start = ReviewRequestStart {
            review_run_id: run_id,
            trigger_type: "manual_note",
            project_id: event.project_id,
            project_name: event.project_name.as_deref(),
            project_path_with_namespace: event.project_path_with_namespace.as_deref(),
            mr_iid: event.mr_iid,
            commit_sha: &event.commit_sha,
            note_id: Some(event.note_id),
            requested_ids_json: &requested_ids,
            selected_ai_reviews: 0,
            selected_script_tasks: 1,
        };
        if let Err(err) = self.store.start_review_request(&start).await {
            warn!(review_run_id = run_id, script_task_id = %task.id, error = %err, "failed to record script task request");
            self.notify_failure(event, &task, run_id, &err.to_string())
                .await;
            return;
        }
        if let Err(err) = self
            .store
            .start_task_run(&TaskRunStart {
                review_run_id: run_id,
                task_type: "script_task",
                task_id: &task.id,
                title: &task.title,
            })
            .await
        {
            warn!(review_run_id = run_id, script_task_id = %task.id, error = %err, "failed to record script task start");
            let failure = failure_for_error(&err);
            self.finish_request_failure(run_id, 0, 0, &failure).await;
            self.notify_failure(event, &task, run_id, &failure.message)
                .await;
            return;
        }

        let result = self.execute(event, &task, run_id).await;
        match result {
            Ok((findings, comments)) => {
                let _ = self
                    .store
                    .finish_task_run(&TaskRunFinish {
                        review_run_id: run_id,
                        task_type: "script_task",
                        task_id: &task.id,
                        status: "completed",
                        findings,
                        comments,
                        error_code: None,
                        error: None,
                    })
                    .await;
                let _ = self
                    .store
                    .finish_review_request(run_id, "completed", findings, comments)
                    .await;
                info!(review_run_id = run_id, script_task_id = %task.id, findings, comments, "script task completed");
            }
            Err(err) => {
                let failure = failure_for_error(&err);
                let _ = self
                    .store
                    .finish_task_run(&TaskRunFinish {
                        review_run_id: run_id,
                        task_type: "script_task",
                        task_id: &task.id,
                        status: "failed",
                        findings: 0,
                        comments: 0,
                        error_code: Some(failure.code.as_str()),
                        error: Some(&failure.message),
                    })
                    .await;
                self.finish_request_failure(run_id, 0, 0, &failure).await;
                self.notify_failure(event, &task, run_id, &failure.message)
                    .await;
            }
        }
    }

    pub(crate) async fn record_busy(
        &self,
        event: &MergeRequestNoteEvent,
        task: &ScriptTaskConfig,
        run_id: &str,
        active_count: usize,
        max_concurrent: usize,
    ) {
        let ids = serde_json::to_string(&[task.id.as_str()]).unwrap_or_else(|_| "[]".into());
        if self
            .store
            .start_review_request(&ReviewRequestStart {
                review_run_id: run_id,
                trigger_type: "manual_note",
                project_id: event.project_id,
                project_name: event.project_name.as_deref(),
                project_path_with_namespace: event.project_path_with_namespace.as_deref(),
                mr_iid: event.mr_iid,
                commit_sha: &event.commit_sha,
                note_id: Some(event.note_id),
                requested_ids_json: &ids,
                selected_ai_reviews: 0,
                selected_script_tasks: 1,
            })
            .await
            .is_ok()
        {
            let _ = self
                .store
                .start_task_run(&TaskRunStart {
                    review_run_id: run_id,
                    task_type: "script_task",
                    task_id: &task.id,
                    title: &task.title,
                })
                .await;
            let message = format!("脚本任务 `{}` 当前并发名额已满（运行中 {active_count} 个，上限 {max_concurrent} 个），本次未启动，请稍后重试。", task.id);
            let failure = ReviewFailure::new(ReviewErrorCode::ScriptTaskBusy, message.clone());
            let _ = self
                .store
                .finish_task_run(&TaskRunFinish {
                    review_run_id: run_id,
                    task_type: "script_task",
                    task_id: &task.id,
                    status: "failed",
                    findings: 0,
                    comments: 0,
                    error_code: Some(failure.code.as_str()),
                    error: Some(&failure.message),
                })
                .await;
            self.finish_request_failure(run_id, 0, 0, &failure).await;
        }
        let message = format!("脚本任务 `{}` 当前并发名额已满（运行中 {active_count} 个，上限 {max_concurrent} 个），本次未启动，请稍后重试。", task.id);
        self.post_comment(event, format!("**脚本任务未启动：{}**\n\n{message}\n\n<!-- gitlab-work-runner:script-task-busy id={} run={} -->", task.title, task.id, run_id)).await;
    }

    async fn execute(
        &self,
        event: &MergeRequestNoteEvent,
        task: &ScriptTaskConfig,
        run_id: &str,
    ) -> AppResult<(usize, usize)> {
        let archive = self
            .gitlab
            .repository_archive(
                event.project_id,
                &event.commit_sha,
                self.archive_limits.max_archive_bytes,
            )
            .await?;
        let context = ScriptTaskContext {
            project_id: event.project_id,
            mr_iid: event.mr_iid,
            commit_sha: &event.commit_sha,
            run_id,
        };
        let mut runner = ScriptTaskRunner::new().with_archive_limits(self.archive_limits.clone());
        if let Some(work_root) = &self.work_root {
            runner = runner.with_work_root(work_root.clone());
        }
        let result = runner.run(task, &context, &archive).await?;
        if result.status == ScriptTaskStatus::OutputLimit {
            return Err(AppError::script_task(
                ReviewErrorCode::ScriptTaskOutputLimit,
                format!(
                    "script task `{}` exceeded the result size limit of {MAX_SCRIPT_RESULT_BYTES} bytes",
                    task.id
                ),
            ));
        }
        let result_bytes = fs::metadata(&result.result_path)?.len();
        if result_bytes > MAX_SCRIPT_RESULT_BYTES {
            return Err(AppError::script_task(
                ReviewErrorCode::ScriptTaskOutputLimit,
                format!(
                    "script task `{}` result.txt is {result_bytes} bytes; limit is {MAX_SCRIPT_RESULT_BYTES} bytes",
                    task.id
                ),
            ));
        }
        let result_text = fs::read_to_string(&result.result_path)?;
        match result.status {
            ScriptTaskStatus::Passed => {
                if result_text.trim().is_empty() {
                    let body = format!(
                        "**[完成] {}**\n\n本次脚本检查已成功完成，未发现问题。\n\ncommit: `{}`\nrun_id: `{}`\n\n<!-- gitlab-work-runner:script-task={} run={} -->",
                        task.title, event.commit_sha, run_id, task.id, run_id
                    );
                    let discussion = self.create_comment(event, body).await?;
                    self.record_script_comment(
                        event,
                        task,
                        run_id,
                        ScriptCommentRecord {
                            path: "",
                            line: None,
                            discussion_id: &discussion,
                            publish_position: "merge_request",
                        },
                    )
                    .await?;
                    return Ok((0, 1));
                }
                let parsed = result_text
                    .lines()
                    .filter_map(parse_result_line)
                    .collect::<Vec<_>>();
                for (path, line, message) in &parsed {
                    self.store
                        .record_finding(&StoredFinding {
                            review_run_id: run_id,
                            task_type: "script_task",
                            task_id: &task.id,
                            rule_id: &format!("script:{}", task.id),
                            severity: "warning",
                            path,
                            new_line: Some(*line as i64),
                            title: &task.title,
                            message,
                        })
                        .await?;
                }
                let changes = match self
                    .gitlab
                    .merge_request_changes(event.project_id, event.mr_iid)
                    .await
                {
                    Ok(changes) => Some(changes),
                    Err(err) => {
                        warn!(project_id = event.project_id, mr_iid = event.mr_iid, error = %err, "could not fetch MR diff for script result positioning; using MR-level comment");
                        None
                    }
                };
                let comments = self
                    .publish_results(event, task, run_id, &result_text, &parsed, changes.as_ref())
                    .await?;
                Ok((parsed.len(), comments))
            }
            ScriptTaskStatus::TimedOut => Err(AppError::script_task(
                ReviewErrorCode::ScriptTaskTimeout,
                format!(
                    "script task `{}` timed out after {} seconds; partial result: {}",
                    task.id,
                    task.timeout_seconds,
                    sanitize(&result_text)
                ),
            )),
            ScriptTaskStatus::OutputLimit => Err(AppError::script_task(
                ReviewErrorCode::ScriptTaskOutputLimit,
                format!(
                    "script task `{}` exceeded the result size limit of {MAX_SCRIPT_RESULT_BYTES} bytes",
                    task.id
                ),
            )),
            ScriptTaskStatus::Failed(code) => Err(AppError::script_task(
                ReviewErrorCode::ScriptTaskFailed,
                format!(
                    "script task `{}` exited with nonzero status {:?}; log: {}; result: {}",
                    task.id,
                    code,
                    result.run_log_path.display(),
                    sanitize(&result_text)
                ),
            )),
        }
    }

    async fn finish_request_failure(
        &self,
        run_id: &str,
        findings: usize,
        comments: usize,
        failure: &ReviewFailure,
    ) {
        let _ = self
            .store
            .finish_review_request_with_failure(run_id, "failed", findings, comments, Some(failure))
            .await;
    }

    async fn notify_failure(
        &self,
        event: &MergeRequestNoteEvent,
        task: &ScriptTaskConfig,
        run_id: &str,
        message: &str,
    ) {
        self.post_comment(event, format!("**脚本任务执行失败：{}**\n\ncommit: `{}`\nrun_id: `{}`\nerror: {}\n\n<!-- gitlab-work-runner:script-task-failed id={} run={} -->", task.title, event.commit_sha, run_id, sanitize(message), task.id, run_id)).await;
    }

    async fn post_comment(&self, event: &MergeRequestNoteEvent, body: String) {
        if let Err(err) = self.create_comment(event, body).await {
            warn!(project_id = event.project_id, mr_iid = event.mr_iid, error = %err, "failed to post script task comment");
        }
    }

    async fn create_comment(
        &self,
        event: &MergeRequestNoteEvent,
        body: String,
    ) -> AppResult<String> {
        Ok(self.create_discussion(event, body, None).await?.id)
    }

    async fn create_discussion(
        &self,
        event: &MergeRequestNoteEvent,
        body: String,
        position: Option<crate::gitlab::DiscussionPosition>,
    ) -> AppResult<CreatedDiscussion> {
        self.gitlab
            .create_discussion(
                event.project_id,
                event.mr_iid,
                &CreateDiscussionRequest { body, position },
            )
            .await
    }

    async fn publish_results(
        &self,
        event: &MergeRequestNoteEvent,
        task: &ScriptTaskConfig,
        run_id: &str,
        text: &str,
        findings: &[(String, u32, String)],
        changes: Option<&MergeRequestChanges>,
    ) -> AppResult<usize> {
        let mut published = 0;
        let mut fallback = Vec::new();
        for (path, line, message) in findings {
            let position = changes
                .filter(|changes| {
                    changes.diff_refs.is_complete()
                        && changes.diff_refs.head_sha.as_deref() == Some(event.commit_sha.as_str())
                })
                .and_then(|changes| {
                    changes
                        .changes
                        .iter()
                        .find(|change| change.new_path == *path && !change.deleted_file)
                        .filter(|change| is_added_line(&change.diff, *line))
                        .map(|_| {
                            let draft = CommentDraft {
                                path: path.clone(),
                                new_line: Some(*line),
                                body: finding_comment(task, run_id, path, *line, message),
                            };
                            crate::review::service::discussion_position(changes, &draft, *line)
                        })
                });
            if let Some(position) = position {
                let discussion = self
                    .create_discussion(
                        event,
                        finding_comment(task, run_id, path, *line, message),
                        Some(position),
                    )
                    .await?;
                let is_inline = discussion.publish_position == PublishPosition::Inline;
                self.record_script_comment(
                    event,
                    task,
                    run_id,
                    ScriptCommentRecord {
                        path: if is_inline { path } else { "" },
                        line: if is_inline { Some(*line) } else { None },
                        discussion_id: &discussion.id,
                        publish_position: if is_inline {
                            "inline"
                        } else {
                            discussion.publish_position.as_str()
                        },
                    },
                )
                .await?;
                published += 1;
            } else {
                fallback.push((path.clone(), *line, message.clone()));
            }
        }

        let unparsed = text
            .lines()
            .filter(|line| !line.trim().is_empty() && parse_result_line(line).is_none())
            .collect::<Vec<_>>();
        if !fallback.is_empty() || !unparsed.is_empty() || findings.is_empty() {
            let body = if fallback.is_empty() && !unparsed.is_empty() {
                build_result_comment(task, run_id, &unparsed.join("\n"), &[])
            } else {
                build_result_comment(task, run_id, text, &fallback)
            };
            let discussion = self.create_discussion(event, body, None).await?;
            self.record_script_comment(
                event,
                task,
                run_id,
                ScriptCommentRecord {
                    path: "",
                    line: None,
                    discussion_id: &discussion.id,
                    publish_position: "merge_request",
                },
            )
            .await?;
            published += 1;
        }
        Ok(published)
    }

    async fn record_script_comment(
        &self,
        event: &MergeRequestNoteEvent,
        task: &ScriptTaskConfig,
        run_id: &str,
        comment: ScriptCommentRecord<'_>,
    ) -> AppResult<()> {
        let rule_id = format!("script:{}", task.id);
        self.store
            .record_comment(&StoredComment {
                review_run_id: run_id,
                project_id: event.project_id,
                mr_iid: event.mr_iid,
                commit_sha: &event.commit_sha,
                rule_id: &rule_id,
                path: comment.path,
                new_line: comment.line.map(i64::from),
                discussion_id: Some(comment.discussion_id),
                note_id: None,
                publish_position: comment.publish_position,
            })
            .await
    }
}

fn is_added_line(diff: &str, requested_line: u32) -> bool {
    crate::review::diff::parse_unified_diff("", "", diff)
        .map(|parsed| {
            parsed
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .any(|line| {
                    line.kind == crate::review::diff::DiffLineKind::Added
                        && line.new_line == Some(requested_line)
                })
        })
        .unwrap_or(false)
}

fn parse_result_line(line: &str) -> Option<(String, u32, String)> {
    let mut parts = line.splitn(3, ':');
    let path = parts.next()?.trim().replace('\\', "/");
    let line_no = parts.next()?.trim().parse::<u32>().ok()?;
    let message = parts.next()?.trim().to_owned();
    (!path.is_empty() && line_no > 0 && !message.is_empty()).then_some((path, line_no, message))
}

fn finding_comment(
    task: &ScriptTaskConfig,
    run_id: &str,
    path: &str,
    line: u32,
    message: &str,
) -> String {
    format!(
        "**[警告] {}**\n\n`{}:{}` {}\n\n<!-- gitlab-work-runner:script-task={} run={} -->",
        task.title, path, line, message, task.id, run_id
    )
}

fn build_result_comment(
    task: &ScriptTaskConfig,
    run_id: &str,
    text: &str,
    findings: &[(String, u32, String)],
) -> String {
    let details = if findings.is_empty() {
        format!("```text\n{}\n```", truncate(text.trim(), 6000))
    } else {
        findings
            .iter()
            .map(|(path, line, message)| format!("- `{path}:{line}` {message}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!("**[警告] {}**\n\n脚本检查结果：\n\n{details}\n\n<!-- gitlab-work-runner:script-task={} run={} -->", task.title, task.id, run_id)
}

fn failure_for_error(error: &AppError) -> ReviewFailure {
    error
        .review_failure()
        .cloned()
        .unwrap_or_else(|| ReviewFailure::new(ReviewErrorCode::ScriptTaskFailed, error.to_string()))
}

fn sanitize(text: &str) -> String {
    truncate(&text.replace('`', "'").replace(['\r', '\n'], " "), 1200)
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut result = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        result.push_str("...");
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use axum::{
        body::Bytes,
        extract::State,
        routing::{get, post},
        Json, Router,
    };
    use std::{
        io::Write,
        net::SocketAddr,
        sync::{Arc, Mutex},
    };
    use tokio::net::TcpListener;
    use zip::{write::SimpleFileOptions, ZipWriter};

    #[derive(Clone)]
    struct TestState {
        archive: Arc<Vec<u8>>,
        body_record: Arc<Mutex<String>>,
    }

    async fn archive(State(state): State<TestState>) -> Bytes {
        Bytes::from(state.archive.as_ref().clone())
    }

    async fn discussion(State(state): State<TestState>, body: Bytes) -> Json<serde_json::Value> {
        *state.body_record.lock().unwrap() = String::from_utf8_lossy(&body).into_owned();
        Json(serde_json::json!({ "id": "discussion-1", "notes": [] }))
    }

    fn archive_with_source() -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        let mut zip = ZipWriter::new(&mut bytes);
        zip.start_file("repo-head/src/lib.rs", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"line\n").unwrap();
        zip.finish().unwrap();
        bytes.into_inner()
    }

    #[tokio::test]
    async fn successful_script_result_is_posted_as_an_mr_comment() {
        let archive_bytes = Arc::new(archive_with_source());
        let body_record = Arc::new(Mutex::new(String::new()));
        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("script.sh");
        fs::write(
            &script_path,
            "printf 'src/lib.rs:7:bad value\\n' > \"$2\"\n",
        )
        .unwrap();
        let app = Router::new()
            .route("/api/v4/projects/123/repository/archive.zip", get(archive))
            .route(
                "/api/v4/projects/123/merge_requests/45/discussions",
                post(discussion),
            )
            .with_state(TestState {
                archive: archive_bytes,
                body_record: body_record.clone(),
            });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let database_url = format!("sqlite://{}", temp.path().join("state.db").display());
        let store = StateStore::connect(&database_url).await.unwrap();
        store.migrate().await.unwrap();
        let service = ScriptTaskService::new(
            GitLabClient::new(format!("http://{address}"), "token".into()),
            store,
            ArchiveLimits::default(),
        )
        .with_work_root(temp.path().join("work"));
        let event = MergeRequestNoteEvent {
            project_id: 123,
            project_name: None,
            project_path_with_namespace: None,
            mr_iid: 45,
            commit_sha: "abc123".into(),
            action: "create".into(),
            note_id: 1,
            note: "@check".into(),
        };
        let task = ScriptTaskConfig {
            enabled: true,
            id: "check".into(),
            title: "Check source".into(),
            command: format!(
                "sh {}",
                crate::review::scripts::shell_quote_arg(&script_path)
            ),
            timeout_seconds: 5,
            max_concurrent: 1,
        };

        let run_id = format!(
            "rr-script-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        service.run(&event, task, &run_id).await;

        let body = body_record.lock().unwrap().clone();
        assert!(body.contains("Check source"));
        assert!(body.contains("src/lib.rs:7"));
        assert!(body.contains("bad value"));
        assert!(body.contains("gitlab-work-runner:script-task=check"));
        drop(service);
        let dashboard = crate::dashboard::queries::DashboardStore::connect(&database_url)
            .await
            .unwrap();
        let run = dashboard.run_detail(&run_id).await.unwrap().unwrap();
        assert_eq!(run.run.selected_ai_reviews, 0);
        assert_eq!(run.run.selected_script_tasks, 1);
        assert_eq!(run.run.status, "completed");
        assert_eq!(run.tasks[0].task_type, "script_task");
        assert_eq!(run.tasks[0].findings, 1);
    }
}
