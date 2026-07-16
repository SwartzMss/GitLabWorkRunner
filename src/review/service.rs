use crate::{
    ai_review::{
        run_ai_review_execution_with_context, AiReviewExecution, ReviewCoverage, ReviewCoverageFile,
    },
    comments::{build_comment_drafts, CommentDraft},
    error::{AppError, AppResult, ReviewErrorCode, ReviewFailure},
    gitlab::{CreateDiscussionRequest, DiscussionPosition, GitLabChange, GitLabClient},
    review::archive::{extract_zip_archive, ArchiveLimits},
    rules::{AiReviewConfig, Finding, Ruleset, Severity},
    script_tasks::{ScriptTaskContext, ScriptTaskResult, ScriptTaskRunner, ScriptTaskStatus},
    storage::{
        ReviewRequestStart, StateStore, StoredComment, StoredFinding, StoredReviewCoverage,
        StoredReviewCoverageFile, TaskRunFinish, TaskRunStart,
    },
    webhook::{MergeRequestEvent, MergeRequestNoteEvent},
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};
use tracing::{info, warn};

pub struct ReviewService {
    gitlab: GitLabClient,
    store: StateStore,
    ruleset: Ruleset,
    review_run_id: Option<String>,
    archive_limits: ArchiveLimits,
}

impl ReviewService {
    async fn finish_ai_task_run(
        &self,
        task: &TaskRunFinish<'_>,
        coverage: Option<&ReviewCoverage>,
        files: &[ReviewCoverageFile],
    ) -> AppResult<()> {
        let Some(coverage) = coverage else {
            return self.store.finish_task_run(task).await;
        };
        let stored = StoredReviewCoverage {
            total_files: coverage.total_files,
            fully_reviewed_files: coverage.fully_reviewed_files,
            partially_reviewed_files: coverage.partially_reviewed_files,
            unreviewed_files: coverage.unreviewed_files,
            total_diff_bytes: coverage.total_diff_bytes,
            reviewed_diff_bytes: coverage.reviewed_diff_bytes,
            required_batches: coverage.required_batches,
            planned_batches: coverage.planned_batches,
            completed_batches: coverage.completed_batches,
            max_batches: coverage.max_batches,
            tool_calls_used: coverage.tool_calls_used,
            max_tool_calls: coverage.max_tool_calls,
            complete: coverage.complete,
        };
        let stored_files = files
            .iter()
            .map(|file| StoredReviewCoverageFile {
                path: &file.path,
                status: file.status,
                reason: file.reason,
                total_diff_bytes: file.total_diff_bytes,
                reviewed_diff_bytes: file.reviewed_diff_bytes,
            })
            .collect::<Vec<_>>();
        self.store
            .finish_task_run_with_coverage(task, &stored, &stored_files)
            .await
    }

    pub fn new(gitlab: GitLabClient, store: StateStore, ruleset: Ruleset) -> Self {
        Self {
            gitlab,
            store,
            ruleset,
            review_run_id: None,
            archive_limits: ArchiveLimits::default(),
        }
    }

    pub fn with_review_run_id(mut self, review_run_id: String) -> Self {
        self.review_run_id = Some(review_run_id);
        self
    }

    pub fn with_archive_limits(mut self, archive_limits: ArchiveLimits) -> Self {
        self.archive_limits = archive_limits;
        self
    }

    fn review_run_id(&self) -> &str {
        self.review_run_id.as_deref().unwrap_or("unknown")
    }

    pub async fn review_merge_request_note(
        &self,
        event: &MergeRequestNoteEvent,
    ) -> AppResult<ReviewSummary> {
        if !event.is_create_action() {
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                note_id = event.note_id,
                action = %event.action,
                "merge request note ignored because manual reviews only run for create actions"
            );
            return Ok(ReviewSummary {
                skipped: true,
                findings: 0,
                comments: 0,
            });
        }

        let requested_ids = manual_script_task_ids(&event.note);
        let tasks = self.ruleset.script_tasks_by_ids(&requested_ids);
        let ai_reviews = self.ruleset.ai_reviews_by_ids(&requested_ids);
        if tasks.is_empty() && ai_reviews.is_empty() {
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                note_id = event.note_id,
                requested_task_ids = ?requested_ids,
                "merge request note did not request any configured manual review"
            );
            return Ok(ReviewSummary {
                skipped: true,
                findings: 0,
                comments: 0,
            });
        }

        let review_run_id = self.review_run_id();
        let requested_ids_json =
            serde_json::to_string(&requested_ids).unwrap_or_else(|_| "[]".into());
        self.store
            .start_review_request(&ReviewRequestStart {
                review_run_id,
                trigger_type: "manual_note",
                project_id: event.project_id,
                project_name: event.project_name.as_deref(),
                project_path_with_namespace: event.project_path_with_namespace.as_deref(),
                mr_iid: event.mr_iid,
                commit_sha: &event.commit_sha,
                note_id: Some(event.note_id),
                requested_ids_json: &requested_ids_json,
                selected_ai_reviews: ai_reviews.len(),
                selected_script_tasks: tasks.len(),
            })
            .await?;

        let result = self
            .review_merge_request_note_inner(event, tasks, ai_reviews, requested_ids)
            .await;
        match &result {
            Ok(summary) => {
                self.store
                    .finish_review_request(
                        review_run_id,
                        "completed",
                        summary.findings,
                        summary.comments,
                    )
                    .await?;
            }
            Err(err) => {
                let failure = failure_for_error(err, ReviewErrorCode::Internal);
                self.store
                    .finish_review_request_with_failure(
                        review_run_id,
                        "failed",
                        0,
                        0,
                        Some(&failure),
                    )
                    .await?;
                warn!(
                    review_run_id,
                    project_id = event.project_id,
                    mr_iid = event.mr_iid,
                    note_id = event.note_id,
                    error = %err,
                    "manual review request failed"
                );
            }
        }
        result
    }

    async fn review_merge_request_note_inner(
        &self,
        event: &MergeRequestNoteEvent,
        tasks: Vec<crate::rules::ScriptTaskConfig>,
        ai_reviews: Vec<AiReviewConfig>,
        requested_ids: Vec<String>,
    ) -> AppResult<ReviewSummary> {
        let selected_ids = selected_manual_ids(&tasks, &ai_reviews);
        let review_request = manual_review_request_text(&event.note, &selected_ids);
        let started = Instant::now();
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            note_id = event.note_id,
            commit_sha = %event.commit_sha,
            action = %event.action,
            requested_task_ids = ?requested_ids,
            selected_tasks = tasks.len(),
            selected_ai_reviews = ai_reviews.len(),
            "manual review started"
        );
        if let Err(err) = self
            .gitlab
            .award_merge_request_note_emoji(event.project_id, event.mr_iid, event.note_id, "eyes")
            .await
        {
            warn!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                note_id = event.note_id,
                error = %err,
                "failed to award manual review request emoji; continuing review"
            );
        }
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            note_id = event.note_id,
            commit_sha = %event.commit_sha,
            "fetching merge request diff before manual review tasks"
        );
        let changes = self
            .gitlab
            .merge_request_changes(event.project_id, event.mr_iid)
            .await?;
        let mr_event = MergeRequestEvent {
            project_id: event.project_id,
            project_name: event.project_name.clone(),
            project_path_with_namespace: event.project_path_with_namespace.clone(),
            mr_iid: event.mr_iid,
            commit_sha: event.commit_sha.clone(),
            action: format!("manual-note-{}", event.note_id),
            source_branch: String::new(),
            target_branch: String::new(),
        };
        let ai_result = self
            .run_selected_ai_reviews(&mr_event, &changes, ai_reviews, review_request.as_deref())
            .await?;
        let script_result = self
            .run_selected_script_tasks(&mr_event, &changes, tasks)
            .await?;
        let findings = ai_result.findings + script_result.findings;
        let summary_comments = self
            .publish_manual_review_summary(
                &mr_event,
                &changes,
                &ai_result,
                &script_result,
                review_request.as_deref(),
                started.elapsed(),
            )
            .await?;
        let comments = ai_result.comments + script_result.comments + summary_comments;
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            note_id = event.note_id,
            commit_sha = %event.commit_sha,
            findings,
            comments,
            "manual review completed"
        );
        Ok(ReviewSummary {
            skipped: false,
            findings,
            comments,
        })
    }

    async fn publish_line_findings(
        &self,
        event: &MergeRequestEvent,
        changes: &crate::gitlab::MergeRequestChanges,
        findings: &[crate::rules::Finding],
    ) -> AppResult<usize> {
        let drafts = build_comment_drafts(findings);
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = %event.commit_sha,
            findings = findings.len(),
            comment_drafts = drafts.len(),
            "rule evaluation completed"
        );

        self.publish_comment_drafts(event, changes, &drafts, "grouped")
            .await
    }

    async fn run_selected_ai_reviews(
        &self,
        event: &MergeRequestEvent,
        changes: &crate::gitlab::MergeRequestChanges,
        reviews: Vec<AiReviewConfig>,
        review_request: Option<&str>,
    ) -> AppResult<AiReviewRunSummary> {
        if reviews.is_empty() {
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                "no AI reviews selected"
            );
            return Ok(AiReviewRunSummary::default());
        }
        if !changes.diff_refs.is_complete() {
            warn!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                "AI review skipped because gitlab diff refs are incomplete"
            );
            return Ok(AiReviewRunSummary {
                skipped_reviews: reviews.len(),
                ..AiReviewRunSummary::default()
            });
        }

        let mut summary = AiReviewRunSummary::default();
        for review in reviews {
            self.store
                .start_task_run(&TaskRunStart {
                    review_run_id: self.review_run_id(),
                    task_type: "ai_review",
                    task_id: &review.id,
                    title: &review.title,
                })
                .await?;
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                ai_review_id = %review.id,
                "AI review started"
            );
            let execution = self
                .run_ai_review_with_optional_clean_second_pass(
                    &review,
                    changes,
                    event,
                    review_request,
                )
                .await;
            let coverage = execution.coverage;
            let incomplete_files = execution.incomplete_files;
            if let Some(coverage) = coverage.as_ref() {
                summary.apply_coverage(coverage);
            }
            match execution.result {
                Ok(findings) => {
                    let review_findings = findings.len();
                    summary.successful_reviews += 1;
                    summary.findings += review_findings;
                    self.record_findings("ai_review", &review.id, &findings)
                        .await?;
                    let comments = self
                        .publish_line_findings(event, changes, &findings)
                        .await?;
                    summary.comments += comments;
                    self.finish_ai_task_run(
                        &TaskRunFinish {
                            review_run_id: self.review_run_id(),
                            task_type: "ai_review",
                            task_id: &review.id,
                            status: "completed",
                            findings: review_findings,
                            comments,
                            error_code: None,
                            error: None,
                        },
                        coverage.as_ref(),
                        &incomplete_files,
                    )
                    .await?;
                    info!(
                        project_id = event.project_id,
                        mr_iid = event.mr_iid,
                        commit_sha = %event.commit_sha,
                        ai_review_id = %review.id,
                        findings = review_findings,
                        comments = summary.comments,
                        "AI review completed"
                    );
                }
                Err(err) => {
                    let failure = failure_for_error(&err, ReviewErrorCode::AiRequestFailed);
                    self.finish_ai_task_run(
                        &TaskRunFinish {
                            review_run_id: self.review_run_id(),
                            task_type: "ai_review",
                            task_id: &review.id,
                            status: "failed",
                            findings: 0,
                            comments: 0,
                            error_code: Some(failure.code.as_str()),
                            error: Some(&failure.message),
                        },
                        coverage.as_ref(),
                        &incomplete_files,
                    )
                    .await?;
                    if matches!(err, AppError::Archive(_)) {
                        return Err(err);
                    }
                    summary.failed_reviews += 1;
                    summary.failed_review_items.push(AiReviewFailureSummary {
                        id: review.id.clone(),
                        title: review.title.clone(),
                        error_code: failure.code.as_str().to_string(),
                        error: failure.message.clone(),
                    });
                    warn!(
                        project_id = event.project_id,
                        mr_iid = event.mr_iid,
                        commit_sha = %event.commit_sha,
                        ai_review_id = %review.id,
                        error = %err,
                        "AI review failed"
                    );
                }
            }
        }
        Ok(summary)
    }

    async fn record_findings(
        &self,
        task_type: &str,
        task_id: &str,
        findings: &[Finding],
    ) -> AppResult<()> {
        for finding in findings {
            self.store
                .record_finding(&StoredFinding {
                    review_run_id: self.review_run_id(),
                    task_type,
                    task_id,
                    rule_id: &finding.rule_id,
                    severity: severity_name(&finding.severity),
                    path: &finding.path,
                    new_line: finding.new_line.map(i64::from),
                    title: &finding.title,
                    message: &finding.message,
                })
                .await?;
        }
        Ok(())
    }

    async fn publish_manual_review_summary(
        &self,
        event: &MergeRequestEvent,
        changes: &crate::gitlab::MergeRequestChanges,
        ai_summary: &AiReviewRunSummary,
        script_summary: &ScriptTaskRunSummary,
        review_request: Option<&str>,
        elapsed: std::time::Duration,
    ) -> AppResult<usize> {
        let body = build_manual_review_summary_body(
            event,
            changes,
            self.review_run_id(),
            ai_summary,
            script_summary,
            review_request,
            elapsed,
        );
        let draft = CommentDraft {
            path: String::new(),
            new_line: None,
            body,
        };
        self.publish_comment_drafts(event, changes, &[draft], "summary")
            .await
    }

    async fn publish_comment_drafts(
        &self,
        event: &MergeRequestEvent,
        changes: &crate::gitlab::MergeRequestChanges,
        drafts: &[CommentDraft],
        record_rule_id: &str,
    ) -> AppResult<usize> {
        let mut published = 0_usize;
        for draft in drafts {
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                path = %draft.path,
                new_line = ?draft.new_line,
                "publishing review comment"
            );
            let position = draft
                .new_line
                .map(|new_line| discussion_position(changes, draft, new_line));
            let created = match self
                .gitlab
                .create_discussion(
                    event.project_id,
                    event.mr_iid,
                    &CreateDiscussionRequest {
                        body: draft.body.clone(),
                        position,
                    },
                )
                .await
            {
                Ok(created) => created,
                Err(err) => {
                    warn!(
                        project_id = event.project_id,
                        mr_iid = event.mr_iid,
                        path = %draft.path,
                        new_line = ?draft.new_line,
                        error = %err,
                        "failed to publish review comment; continuing with remaining comments"
                    );
                    continue;
                }
            };
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                path = %draft.path,
                new_line = ?draft.new_line,
                discussion_id = %created.id,
                note_id = ?created.notes.first().map(|note| note.id),
                "review comment published"
            );
            let is_inline = matches!(
                created.publish_position,
                crate::gitlab::PublishPosition::Inline
            );
            let record_path = if is_inline { draft.path.as_str() } else { "" };
            let record_new_line = if is_inline {
                draft.new_line.map(i64::from)
            } else {
                None
            };
            self.store
                .record_comment(&StoredComment {
                    review_run_id: self.review_run_id(),
                    project_id: event.project_id,
                    mr_iid: event.mr_iid,
                    commit_sha: &event.commit_sha,
                    rule_id: record_rule_id,
                    path: record_path,
                    new_line: record_new_line,
                    discussion_id: Some(&created.id),
                    note_id: created.notes.first().map(|note| note.id),
                    publish_position: created.publish_position.as_str(),
                })
                .await?;
            if created.notes.is_empty() {
                warn!(
                    project_id = event.project_id,
                    mr_iid = event.mr_iid,
                    discussion_id = %created.id,
                    "gitlab returned a discussion without notes"
                );
            }
            published += 1;
        }
        Ok(published)
    }

    async fn run_selected_script_tasks(
        &self,
        event: &MergeRequestEvent,
        changes: &crate::gitlab::MergeRequestChanges,
        tasks: Vec<crate::rules::ScriptTaskConfig>,
    ) -> AppResult<ScriptTaskRunSummary> {
        if tasks.is_empty() {
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                "no script tasks selected"
            );
            return Ok(ScriptTaskRunSummary::default());
        }

        let archive_sha = changes
            .diff_refs
            .head_sha
            .as_deref()
            .unwrap_or(&event.commit_sha);
        if changes.diff_refs.head_sha.is_none() {
            warn!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                event_commit_sha = %event.commit_sha,
                "script task archive fallback to webhook commit sha because gitlab head_sha is missing"
            );
        }
        let archive = self
            .gitlab
            .repository_archive(
                event.project_id,
                archive_sha,
                self.archive_limits.max_archive_bytes,
            )
            .await?;
        let runner = ScriptTaskRunner::new().with_archive_limits(self.archive_limits.clone());
        let context = ScriptTaskContext {
            project_id: event.project_id,
            mr_iid: event.mr_iid,
            commit_sha: archive_sha,
        };
        let mut summary = ScriptTaskRunSummary::default();
        for task in tasks {
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = archive_sha,
                script_task_id = %task.id,
                "script task selected"
            );
            self.store
                .start_task_run(&TaskRunStart {
                    review_run_id: self.review_run_id(),
                    task_type: "script_task",
                    task_id: &task.id,
                    title: &task.title,
                })
                .await?;
            let result = match runner.run(&task, &context, &archive).await {
                Ok(result) => result,
                Err(err) => {
                    let failure = failure_for_error(&err, ReviewErrorCode::ScriptTaskFailed);
                    self.store
                        .finish_task_run(&TaskRunFinish {
                            review_run_id: self.review_run_id(),
                            task_type: "script_task",
                            task_id: &task.id,
                            status: "failed",
                            findings: 0,
                            comments: 0,
                            error_code: Some(failure.code.as_str()),
                            error: Some(&failure.message),
                        })
                        .await?;
                    return Err(err);
                }
            };
            if result.status == ScriptTaskStatus::IssueFound {
                let (comments, findings) = self
                    .publish_script_task_result(event, changes, &result)
                    .await?;
                summary.comments += comments;
                summary.findings += findings;
                self.store
                    .finish_task_run(&TaskRunFinish {
                        review_run_id: self.review_run_id(),
                        task_type: "script_task",
                        task_id: &task.id,
                        status: "completed",
                        findings,
                        comments,
                        error_code: None,
                        error: None,
                    })
                    .await?;
            } else {
                self.store
                    .finish_task_run(&TaskRunFinish {
                        review_run_id: self.review_run_id(),
                        task_type: "script_task",
                        task_id: &task.id,
                        status: "completed",
                        findings: 0,
                        comments: 0,
                        error_code: None,
                        error: None,
                    })
                    .await?;
                info!(
                    project_id = event.project_id,
                    mr_iid = event.mr_iid,
                    commit_sha = archive_sha,
                    script_task_id = %result.id,
                    status = ?result.status,
                    "script task produced no publishable issue"
                );
            }
        }
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = archive_sha,
            findings = summary.findings,
            comments = summary.comments,
            "script tasks completed"
        );
        Ok(summary)
    }

    async fn publish_script_task_result(
        &self,
        event: &MergeRequestEvent,
        changes: &crate::gitlab::MergeRequestChanges,
        result: &ScriptTaskResult,
    ) -> AppResult<(usize, usize)> {
        let result_text = match fs::read_to_string(&result.result_path) {
            Ok(text) => text,
            Err(err) => {
                warn!(
                    project_id = event.project_id,
                    mr_iid = event.mr_iid,
                    commit_sha = %event.commit_sha,
                    script_task_id = %result.id,
                    result_path = %result.result_path.display(),
                    error = %err,
                    "script task returned issue status but result file could not be read"
                );
                format!("[gitlab-work-runner] failed to read result.txt: {err}")
            }
        };
        let findings = parse_script_result_findings(result, &result_text);
        self.record_findings("script_task", &result.id, &findings)
            .await?;
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = %event.commit_sha,
            script_task_id = %result.id,
            result_bytes = result_text.len(),
            parsed_findings = findings.len(),
            diff_refs_complete = changes.diff_refs.is_complete(),
            "script task result parsed"
        );
        if !findings.is_empty() && changes.diff_refs.is_complete() {
            let comments = self
                .publish_line_findings(event, changes, &findings)
                .await?;
            return Ok((comments, findings.len()));
        }

        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = %event.commit_sha,
            script_task_id = %result.id,
            parsed_findings = findings.len(),
            diff_refs_complete = changes.diff_refs.is_complete(),
            "publishing script task result as merge-request-level summary"
        );
        let body = build_script_result_summary(result, &result_text);
        let draft = CommentDraft {
            path: String::new(),
            new_line: None,
            body,
        };
        let comments = self
            .publish_comment_drafts(event, changes, &[draft], "script")
            .await?;
        Ok((comments, findings.len()))
    }
}

fn discussion_position(
    changes: &crate::gitlab::MergeRequestChanges,
    draft: &CommentDraft,
    new_line: u32,
) -> DiscussionPosition {
    let change = change_for_new_path(&changes.changes, &draft.path);
    if change.is_none() {
        warn!(
            path = %draft.path,
            new_line,
            "comment path was not found in merge request changes; using path for both old_path and new_path"
        );
    }
    DiscussionPosition {
        base_sha: changes
            .diff_refs
            .base_sha
            .clone()
            .expect("complete diff refs"),
        start_sha: changes
            .diff_refs
            .start_sha
            .clone()
            .expect("complete diff refs"),
        head_sha: changes
            .diff_refs
            .head_sha
            .clone()
            .expect("complete diff refs"),
        position_type: "text".into(),
        old_path: change
            .map(|change| change.old_path.clone())
            .unwrap_or_else(|| draft.path.clone()),
        new_path: change
            .map(|change| change.new_path.clone())
            .unwrap_or_else(|| draft.path.clone()),
        new_line: Some(new_line),
    }
}

fn change_for_new_path<'a>(changes: &'a [GitLabChange], path: &str) -> Option<&'a GitLabChange> {
    changes.iter().find(|change| change.new_path == path)
}

fn build_manual_review_summary_body(
    event: &MergeRequestEvent,
    changes: &crate::gitlab::MergeRequestChanges,
    review_run_id: &str,
    ai_summary: &AiReviewRunSummary,
    script_summary: &ScriptTaskRunSummary,
    review_request: Option<&str>,
    elapsed: std::time::Duration,
) -> String {
    let findings = ai_summary.findings + script_summary.findings;
    let status = if ai_summary.failed_reviews > 0 {
        "部分失败"
    } else if ai_summary.skipped_reviews > 0 {
        "已跳过"
    } else {
        "完成"
    };
    let result = if ai_summary.skipped_reviews > 0
        && ai_summary.successful_reviews == 0
        && script_summary.findings == 0
    {
        "AI Review 未执行，GitLab diff refs 不完整".to_string()
    } else if findings == 0 {
        "未发现高置信度问题".to_string()
    } else {
        format!("发现 {findings} 个问题")
    };
    let total_diff_bytes = changes
        .changes
        .iter()
        .map(|change| change.diff.len())
        .sum::<usize>();
    let reviewed_diff_bytes = if ai_summary.reviewed_diff_bytes > 0 {
        ai_summary.reviewed_diff_bytes
    } else {
        total_diff_bytes
    };
    let preference = review_request
        .map(|value| sanitize_comment_detail(value, 300))
        .filter(|value| !value.is_empty());
    let mut body = format!(
        "## GitLabWorkRunner Review\n\n**状态：** {}\n**Commit：** `{}`\n**结果：** {}\n\n### 检查范围\n\n- 文件：{} / {}\n- Diff：{} / {}\n- 耗时：{} 秒\n",
        status,
        sanitize_comment_inline(&event.commit_sha),
        result,
        changes.changes.len(),
        changes.changes.len(),
        format_bytes(reviewed_diff_bytes),
        format_bytes(total_diff_bytes),
        elapsed.as_secs().max(1)
    );
    if let Some(preference) = preference {
        body.push_str("\n### 用户偏好\n\n");
        body.push_str(&preference);
        body.push('\n');
    }
    if !ai_summary.failed_review_items.is_empty() {
        let failed_reviews = ai_summary
            .failed_review_items
            .iter()
            .map(|failure| {
                format!(
                    "- `{}` {}\n  - 原因: `{}`\n  - 详情: {}",
                    sanitize_comment_inline(&failure.id),
                    sanitize_comment_inline(&failure.title),
                    sanitize_comment_inline(&failure.error_code),
                    sanitize_comment_detail(&failure.error, 500)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        body.push_str("\n### 失败项\n\n");
        body.push_str(&failed_reviews);
        body.push('\n');
    }
    body.push_str(&format!(
        "\n<!-- gitlab-work-runner:summary run={} commit={} -->",
        sanitize_comment_inline(review_run_id),
        sanitize_comment_inline(&event.commit_sha)
    ));
    body
}

fn sanitize_comment_inline(value: &str) -> String {
    value.replace('`', "'")
}

fn sanitize_comment_detail(value: &str, max_chars: usize) -> String {
    let sanitized = sanitize_comment_inline(value)
        .replace(['\r', '\n'], " ")
        .trim()
        .to_string();
    if sanitized.chars().count() <= max_chars {
        return sanitized;
    }
    let mut truncated = sanitized.chars().take(max_chars).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn format_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    format!("{:.1} KB", bytes as f64 / KIB)
}

fn severity_name(severity: &Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

impl ReviewService {
    async fn run_ai_review_with_optional_clean_second_pass(
        &self,
        review: &AiReviewConfig,
        changes: &crate::gitlab::MergeRequestChanges,
        event: &MergeRequestEvent,
        review_request: Option<&str>,
    ) -> AiReviewExecution {
        let context = match self.prepare_ai_review_context(review, changes, event).await {
            Ok(context) => context,
            Err(err) => {
                return AiReviewExecution {
                    result: Err(err),
                    coverage: None,
                    incomplete_files: Vec::new(),
                }
            }
        };
        let source_dir = context.as_ref().map(|context| context.source_dir.as_path());
        let execution = run_ai_review_execution_with_context(
            review,
            &changes.changes,
            source_dir,
            review_request,
        )
        .await;
        let is_clean = matches!(&execution.result, Ok(findings) if findings.is_empty());
        if !review.second_pass_on_clean || !is_clean {
            return execution;
        }

        info!(
            ai_review_id = %review.id,
            "AI review first pass was clean; running second confirmation pass"
        );
        run_ai_review_execution_with_context(review, &changes.changes, source_dir, review_request)
            .await
    }

    async fn prepare_ai_review_context(
        &self,
        review: &AiReviewConfig,
        changes: &crate::gitlab::MergeRequestChanges,
        event: &MergeRequestEvent,
    ) -> AppResult<Option<AiReviewContextWorkDir>> {
        let archive_sha = changes
            .diff_refs
            .head_sha
            .as_deref()
            .unwrap_or(&event.commit_sha);
        let archive = self
            .gitlab
            .repository_archive(
                event.project_id,
                archive_sha,
                self.archive_limits.max_archive_bytes,
            )
            .await?;
        let work_dir = ai_review_context_work_dir(
            event.project_id,
            event.mr_iid,
            archive_sha,
            &review.id,
            self.review_run_id.as_deref(),
        )?;
        if work_dir.exists() {
            fs::remove_dir_all(&work_dir)?;
        }
        let source_dir = work_dir.join("source");
        fs::create_dir_all(&source_dir)?;
        let extracted_files = extract_zip_archive(&archive, &source_dir, &self.archive_limits)?;
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = archive_sha,
            ai_review_id = %review.id,
            archive_bytes = archive.len(),
            extracted_files,
            source_dir = %source_dir.display(),
            "AI review context archive extracted"
        );
        Ok(Some(AiReviewContextWorkDir {
            work_dir,
            source_dir,
        }))
    }
}

struct AiReviewContextWorkDir {
    work_dir: PathBuf,
    source_dir: PathBuf,
}

impl Drop for AiReviewContextWorkDir {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_dir_all(&self.work_dir) {
            warn!(
                work_dir = %self.work_dir.display(),
                error = %err,
                "failed to remove AI review context work directory"
            );
        }
    }
}

fn ai_review_context_work_dir(
    project_id: i64,
    mr_iid: i64,
    commit_sha: &str,
    review_id: &str,
    review_run_id: Option<&str>,
) -> AppResult<PathBuf> {
    let mut base = Path::new("work")
        .join("ai_review_context")
        .join(project_id.to_string())
        .join(mr_iid.to_string())
        .join(sanitize_work_path_segment(commit_sha))
        .join(sanitize_work_path_segment(review_id));
    if let Some(review_run_id) = review_run_id {
        base = base.join(sanitize_work_path_segment(review_run_id));
    }
    Ok(if base.is_absolute() {
        base
    } else {
        std::env::current_dir()?.join(base)
    })
}

fn sanitize_work_path_segment(value: &str) -> String {
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

pub(crate) fn manual_script_task_ids(text: &str) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for raw in text.split_whitespace() {
        let token = trim_manual_trigger_token(raw);
        let Some(id) = token.strip_prefix('@') else {
            continue;
        };
        if !id.is_empty()
            && id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            ids.insert(id.to_string());
        }
    }
    ids.into_iter().collect()
}

pub(crate) fn manual_review_request_text(text: &str, requested_ids: &[String]) -> Option<String> {
    let request = text
        .split_whitespace()
        .filter(|raw| {
            let token = trim_manual_trigger_token(raw);
            let Some(id) = token.strip_prefix('@') else {
                return true;
            };
            !requested_ids.iter().any(|requested_id| requested_id == id)
        })
        .collect::<Vec<_>>()
        .join(" ");
    let request = request.trim();
    (!request.is_empty()).then(|| request.to_string())
}

fn selected_manual_ids(
    tasks: &[crate::rules::ScriptTaskConfig],
    ai_reviews: &[AiReviewConfig],
) -> Vec<String> {
    tasks
        .iter()
        .map(|task| task.id.clone())
        .chain(ai_reviews.iter().map(|review| review.id.clone()))
        .collect()
}

fn trim_manual_trigger_token(raw: &str) -> &str {
    raw.trim_matches(|ch: char| {
        matches!(
            ch,
            ',' | '.'
                | ';'
                | ':'
                | '!'
                | '?'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
                | '"'
                | '\''
                | '`'
        )
    })
}

fn parse_script_result_findings(result: &ScriptTaskResult, text: &str) -> Vec<Finding> {
    text.lines()
        .filter_map(parse_script_result_line)
        .map(|(path, line, message)| Finding {
            rule_id: format!("script:{}", result.id),
            severity: Severity::Warning,
            path,
            new_line: Some(line),
            title: result.title.clone(),
            message,
        })
        .collect()
}

fn parse_script_result_line(line: &str) -> Option<(String, u32, String)> {
    let mut parts = line.splitn(3, ':');
    let path = parts.next()?.trim().replace('\\', "/");
    let line_no = parts.next()?.trim().parse().ok()?;
    let message = parts.next()?.trim().to_string();
    if path.is_empty() || message.is_empty() {
        return None;
    }
    Some((path, line_no, message))
}

fn build_script_result_summary(result: &ScriptTaskResult, text: &str) -> String {
    let content = if text.trim().is_empty() {
        "(result.txt is empty)"
    } else {
        text.trim()
    };
    format!(
        "**[警告] {}**\n\n脚本任务检测发现问题，但结果无法解析成 `path:line:message` 行级格式。\n\n```text\n{}\n```\n\n结果文件：`{}`\n运行日志：`{}`\n\n<!-- gitlab-work-runner:rule=script:{} -->",
        result.title,
        content,
        result.result_path.display(),
        result.run_log_path.display(),
        result.id
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSummary {
    pub skipped: bool,
    pub findings: usize,
    pub comments: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AiReviewRunSummary {
    findings: usize,
    comments: usize,
    successful_reviews: usize,
    failed_reviews: usize,
    skipped_reviews: usize,
    failed_review_items: Vec<AiReviewFailureSummary>,
    reviewed_diff_bytes: usize,
}

impl AiReviewRunSummary {
    fn apply_coverage(&mut self, coverage: &ReviewCoverage) {
        self.reviewed_diff_bytes = self.reviewed_diff_bytes.max(coverage.reviewed_diff_bytes);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AiReviewFailureSummary {
    id: String,
    title: String,
    error_code: String,
    error: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ScriptTaskRunSummary {
    findings: usize,
    comments: usize,
}

fn failure_for_error(error: &AppError, fallback: ReviewErrorCode) -> ReviewFailure {
    error
        .review_failure()
        .cloned()
        .unwrap_or_else(|| ReviewFailure::new(fallback, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_manual_script_task_ids() {
        assert_eq!(
            manual_script_task_ids("please run\n@check-todo-tbd, @other_check"),
            vec!["check-todo-tbd".to_string(), "other_check".to_string()]
        );
    }

    #[test]
    fn ignores_non_standalone_manual_script_task_ids() {
        assert!(manual_script_task_ids("please@check-todo-tbd").is_empty());
        assert!(manual_script_task_ids("@").is_empty());
    }

    #[test]
    fn extracts_manual_review_request_text_after_trigger_ids() {
        let requested_ids = vec!["ai-review".to_string()];

        let request =
            manual_review_request_text("@ai-review 重点关注 parser 这段边界条件", &requested_ids);

        assert_eq!(request.as_deref(), Some("重点关注 parser 这段边界条件"));
    }

    #[test]
    fn manual_review_request_text_removes_multiple_trigger_ids() {
        let requested_ids = vec!["ai-review".to_string(), "check-script".to_string()];

        let request =
            manual_review_request_text("please run @ai-review, @check-script now", &requested_ids);

        assert_eq!(request.as_deref(), Some("please run now"));
    }

    #[test]
    fn manual_review_request_text_preserves_non_trigger_mentions() {
        let selected_ids = vec!["ai-review".to_string()];

        let request = manual_review_request_text(
            "@ai-review please check @decorator ordering",
            &selected_ids,
        );

        assert_eq!(request.as_deref(), Some("please check @decorator ordering"));
    }

    #[test]
    fn ai_review_context_work_dir_includes_review_run_id() {
        let path = ai_review_context_work_dir(1, 2, "abc123", "ai-review", Some("run/1")).unwrap();
        let normalized = path.to_string_lossy().replace('\\', "/");

        assert!(normalized.contains("/abc123/ai-review/run_1"));
    }

    #[test]
    fn manual_review_summary_includes_failure_details() {
        let event = MergeRequestEvent {
            project_id: 1,
            project_name: None,
            project_path_with_namespace: None,
            mr_iid: 2,
            commit_sha: "abc123".into(),
            action: "manual-note-1".into(),
            source_branch: String::new(),
            target_branch: String::new(),
        };
        let changes = crate::gitlab::MergeRequestChanges {
            changes: vec![GitLabChange {
                old_path: "src/lib.rs".into(),
                new_path: "src/lib.rs".into(),
                new_file: false,
                renamed_file: false,
                deleted_file: false,
                diff: "+x\n".into(),
            }],
            diff_refs: crate::gitlab::DiffRefs {
                base_sha: Some("base".into()),
                start_sha: Some("start".into()),
                head_sha: Some("head".into()),
            },
        };
        let mut ai_summary = AiReviewRunSummary {
            failed_reviews: 1,
            failed_review_items: vec![AiReviewFailureSummary {
                id: "ai-review".into(),
                title: "AI Review".into(),
                error_code: "permission_denied".into(),
                error: "AI review API returned 401\ninvalid token".into(),
            }],
            ..AiReviewRunSummary::default()
        };
        ai_summary.apply_coverage(&ReviewCoverage {
            total_files: 1,
            fully_reviewed_files: 1,
            partially_reviewed_files: 0,
            unreviewed_files: 0,
            total_diff_bytes: 3,
            reviewed_diff_bytes: 3,
            required_batches: 1,
            planned_batches: 1,
            completed_batches: 1,
            max_batches: 10,
            tool_calls_used: 2,
            max_tool_calls: 30,
            complete: true,
        });
        let body = build_manual_review_summary_body(
            &event,
            &changes,
            "rr-1",
            &ai_summary,
            &ScriptTaskRunSummary::default(),
            Some("重点检查线程安全"),
            std::time::Duration::from_secs(42),
        );

        assert!(body.contains("**状态：** 部分失败"));
        assert!(body.contains("- `ai-review` AI Review"));
        assert!(body.contains("原因: `permission_denied`"));
        assert!(body.contains("详情: AI review API returned 401 invalid token"));
        assert!(body.contains("重点检查线程安全"));
        assert!(body.contains("- Diff：3 B / 3 B"));
        assert!(!body.contains("- 批次："));
        assert!(!body.contains("- 上下文工具："));
        assert!(body.contains("<!-- gitlab-work-runner:summary run=rr-1 commit=abc123 -->"));
    }

    #[test]
    fn manual_review_summary_hides_empty_preferences() {
        let event = MergeRequestEvent {
            project_id: 1,
            project_name: None,
            project_path_with_namespace: None,
            mr_iid: 2,
            commit_sha: "abc123".into(),
            action: "manual-note-1".into(),
            source_branch: String::new(),
            target_branch: String::new(),
        };
        let changes = crate::gitlab::MergeRequestChanges {
            changes: vec![GitLabChange {
                old_path: "src/lib.rs".into(),
                new_path: "src/lib.rs".into(),
                new_file: false,
                renamed_file: false,
                deleted_file: false,
                diff: "+x\n".into(),
            }],
            diff_refs: crate::gitlab::DiffRefs {
                base_sha: Some("base".into()),
                start_sha: Some("start".into()),
                head_sha: Some("head".into()),
            },
        };

        let body = build_manual_review_summary_body(
            &event,
            &changes,
            "rr-1",
            &AiReviewRunSummary::default(),
            &ScriptTaskRunSummary::default(),
            None,
            std::time::Duration::from_secs(1),
        );

        assert!(!body.contains("### 用户偏好"));
        assert!(!body.contains("\n无\n"));
        assert!(!body.contains("- 批次："));
        assert!(!body.contains("- 上下文工具："));
    }

    #[test]
    fn discussion_position_uses_rename_old_and_new_paths() {
        let changes = crate::gitlab::MergeRequestChanges {
            changes: vec![GitLabChange {
                old_path: "src/old.rs".into(),
                new_path: "src/new.rs".into(),
                new_file: false,
                renamed_file: true,
                deleted_file: false,
                diff: "@@ -1 +1 @@\n+let value = 1;\n".into(),
            }],
            diff_refs: crate::gitlab::DiffRefs {
                base_sha: Some("base".into()),
                start_sha: Some("start".into()),
                head_sha: Some("head".into()),
            },
        };
        let draft = CommentDraft {
            path: "src/new.rs".into(),
            new_line: Some(1),
            body: "body".into(),
        };

        let position = discussion_position(&changes, &draft, 1);

        assert_eq!(position.old_path, "src/old.rs");
        assert_eq!(position.new_path, "src/new.rs");
    }
}
