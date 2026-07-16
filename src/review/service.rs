use crate::{
    ai_review::{
        run_ai_review_execution_with_runtime_instruction, timeout_fallback_reason,
        AiReviewExecution, AiReviewExecutionMetadata, AiReviewExecutionMode,
        AiReviewFallbackReason, ReviewCoverage, ReviewCoverageFile,
    },
    comments::{build_comment_drafts, CommentDraft},
    error::{AppError, AppResult, ReviewErrorCode, ReviewFailure},
    gitlab::{CreateDiscussionRequest, DiscussionPosition, GitLabChange, GitLabClient},
    review::archive::extract_zip_archive,
    rules::{AiReviewConfig, Finding, Ruleset, Severity},
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

const ARCHIVE_DIFF_ONLY_INSTRUCTION: &str = "Repository context is unavailable for this execution. Review only the supplied MR diff. Do not request or rely on repository context tools.";
const TIMEOUT_DIFF_ONLY_INSTRUCTION: &str = "The context-assisted review timed out. Start a new review using only the supplied MR diff, and base every finding on that diff. Do not request or rely on repository context tools.";

struct AiReviewExecutionWithMetadata {
    execution: AiReviewExecution,
    metadata: AiReviewExecutionMetadata,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct TestAiExecutionCall {
    source_available: bool,
    second_pass_on_clean: bool,
    timeout_seconds: u64,
    max_batches: usize,
    trusted_runtime_instruction: Option<String>,
}

#[cfg(test)]
#[derive(Clone, Default)]
struct TestAiExecutionSeam {
    executions: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<AiReviewExecution>>>,
    calls: std::sync::Arc<std::sync::Mutex<Vec<TestAiExecutionCall>>>,
}

pub use crate::review::archive::ArchiveLimits;

pub struct ReviewService {
    gitlab: GitLabClient,
    store: StateStore,
    ruleset: Ruleset,
    review_run_id: Option<String>,
    archive_limits: ArchiveLimits,
    #[cfg(test)]
    ai_execution_seam: Option<TestAiExecutionSeam>,
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
            #[cfg(test)]
            ai_execution_seam: None,
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

    #[cfg(test)]
    fn with_ai_execution_seam(mut self, seam: TestAiExecutionSeam) -> Self {
        self.ai_execution_seam = Some(seam);
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

        let requested_ids = manual_review_ids(&event.note);
        let ai_reviews = self.ruleset.ai_reviews_by_ids(&requested_ids);
        if ai_reviews.is_empty() {
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
            })
            .await?;

        let result = self
            .review_merge_request_note_inner(event, ai_reviews, requested_ids)
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
        ai_reviews: Vec<AiReviewConfig>,
        requested_ids: Vec<String>,
    ) -> AppResult<ReviewSummary> {
        let selected_ids = selected_manual_ids(&ai_reviews);
        let review_request = manual_review_request_text(&event.note, &selected_ids);
        let started = Instant::now();
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            note_id = event.note_id,
            commit_sha = %event.commit_sha,
            action = %event.action,
            requested_task_ids = ?requested_ids,
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
        let findings = ai_result.findings;
        let summary_comments = self
            .publish_manual_review_summary(
                &mr_event,
                &changes,
                &ai_result,
                review_request.as_deref(),
                started.elapsed(),
            )
            .await?;
        let comments = ai_result.comments + summary_comments;
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
            let execution_with_metadata = self
                .run_ai_review_with_optional_clean_second_pass(
                    &review,
                    changes,
                    event,
                    review_request,
                )
                .await;
            let execution = execution_with_metadata.execution;
            let _metadata = execution_with_metadata.metadata;
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
        review_request: Option<&str>,
        elapsed: std::time::Duration,
    ) -> AppResult<usize> {
        let body = build_manual_review_summary_body(
            event,
            changes,
            self.review_run_id(),
            ai_summary,
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
    review_request: Option<&str>,
    elapsed: std::time::Duration,
) -> String {
    let findings = ai_summary.findings;
    let status = if ai_summary.failed_reviews > 0 {
        "部分失败"
    } else if ai_summary.skipped_reviews > 0 {
        "已跳过"
    } else {
        "完成"
    };
    let result = if ai_summary.skipped_reviews > 0 && ai_summary.successful_reviews == 0 {
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
    async fn execute_ai_review(
        &self,
        review: &AiReviewConfig,
        changes: &[GitLabChange],
        source_dir: Option<&Path>,
        review_request: Option<&str>,
        trusted_runtime_instruction: Option<&str>,
    ) -> AiReviewExecution {
        #[cfg(test)]
        if let Some(seam) = &self.ai_execution_seam {
            seam.calls.lock().unwrap().push(TestAiExecutionCall {
                source_available: source_dir.is_some(),
                second_pass_on_clean: review.second_pass_on_clean,
                timeout_seconds: review.timeout_seconds,
                max_batches: review.max_batches,
                trusted_runtime_instruction: trusted_runtime_instruction.map(str::to_string),
            });
            return seam
                .executions
                .lock()
                .unwrap()
                .pop_front()
                .expect("test AI execution seam exhausted");
        }
        run_ai_review_execution_with_runtime_instruction(
            review,
            changes,
            source_dir,
            review_request,
            trusted_runtime_instruction,
        )
        .await
    }

    async fn timeout_fallback_for_execution(
        &self,
        review: &AiReviewConfig,
        changes: &[GitLabChange],
        review_request: Option<&str>,
        execution: &AiReviewExecution,
        context: &mut Option<AiReviewContextWorkDir>,
        context_started: Instant,
    ) -> Option<AiReviewExecutionWithMetadata> {
        let error = execution.result.as_ref().err()?;
        let reason = timeout_fallback_reason(error)?;
        drop(context.take());
        let context_elapsed_ms = elapsed_ms(context_started);
        let fallback_started = Instant::now();
        let mut fallback_review = review.clone();
        fallback_review.second_pass_on_clean = false;
        let fallback = self
            .execute_ai_review(
                &fallback_review,
                changes,
                None,
                review_request,
                Some(TIMEOUT_DIFF_ONLY_INSTRUCTION),
            )
            .await;
        Some(AiReviewExecutionWithMetadata {
            execution: fallback,
            metadata: AiReviewExecutionMetadata {
                execution_mode: AiReviewExecutionMode::DiffOnlyFallback,
                fallback_reason: Some(reason),
                context_elapsed_ms: Some(context_elapsed_ms),
                fallback_elapsed_ms: Some(elapsed_ms(fallback_started)),
            },
        })
    }

    async fn run_ai_review_with_optional_clean_second_pass(
        &self,
        review: &AiReviewConfig,
        changes: &crate::gitlab::MergeRequestChanges,
        event: &MergeRequestEvent,
        review_request: Option<&str>,
    ) -> AiReviewExecutionWithMetadata {
        let context_started = Instant::now();
        let archive_sha = changes
            .diff_refs
            .head_sha
            .as_deref()
            .unwrap_or(&event.commit_sha);
        let mut context = match self
            .prepare_ai_review_context_inner(review, event, archive_sha)
            .await
        {
            Ok(context) => Some(context),
            Err(err) if is_archive_limit_error(&err) => {
                let context_elapsed_ms = elapsed_ms(context_started);
                warn!(
                    project_id = event.project_id,
                    mr_iid = event.mr_iid,
                    ai_review_id = %review.id,
                    error = %err,
                    "AI review context archive exceeded limits; continuing with diff-only review"
                );
                let fallback_started = Instant::now();
                let mut execution = self
                    .execute_ai_review(
                        review,
                        &changes.changes,
                        None,
                        review_request,
                        Some(ARCHIVE_DIFF_ONLY_INSTRUCTION),
                    )
                    .await;
                if review.second_pass_on_clean
                    && matches!(&execution.result, Ok(findings) if findings.is_empty())
                {
                    execution = self
                        .execute_ai_review(
                            review,
                            &changes.changes,
                            None,
                            review_request,
                            Some(ARCHIVE_DIFF_ONLY_INSTRUCTION),
                        )
                        .await;
                }
                return AiReviewExecutionWithMetadata {
                    execution,
                    metadata: AiReviewExecutionMetadata {
                        execution_mode: AiReviewExecutionMode::DiffOnlyFallback,
                        fallback_reason: Some(AiReviewFallbackReason::ArchiveLimitExceeded),
                        context_elapsed_ms: Some(context_elapsed_ms),
                        fallback_elapsed_ms: Some(elapsed_ms(fallback_started)),
                    },
                };
            }
            Err(err) => {
                return AiReviewExecutionWithMetadata {
                    execution: AiReviewExecution {
                        result: Err(err),
                        coverage: None,
                        incomplete_files: Vec::new(),
                    },
                    metadata: AiReviewExecutionMetadata {
                        execution_mode: AiReviewExecutionMode::Context,
                        fallback_reason: None,
                        context_elapsed_ms: Some(elapsed_ms(context_started)),
                        fallback_elapsed_ms: None,
                    },
                };
            }
        };
        let execution = {
            let source_dir = context.as_ref().map(|context| context.source_dir.as_path());
            self.execute_ai_review(review, &changes.changes, source_dir, review_request, None)
                .await
        };
        if let Some(fallback) = self
            .timeout_fallback_for_execution(
                review,
                &changes.changes,
                review_request,
                &execution,
                &mut context,
                context_started,
            )
            .await
        {
            return fallback;
        }
        let is_clean = matches!(&execution.result, Ok(findings) if findings.is_empty());
        if !review.second_pass_on_clean || !is_clean {
            return AiReviewExecutionWithMetadata {
                execution,
                metadata: AiReviewExecutionMetadata {
                    execution_mode: AiReviewExecutionMode::Context,
                    fallback_reason: None,
                    context_elapsed_ms: Some(elapsed_ms(context_started)),
                    fallback_elapsed_ms: None,
                },
            };
        }

        info!(
            ai_review_id = %review.id,
            "AI review first pass was clean; running second confirmation pass"
        );
        let execution = {
            let source_dir = context.as_ref().map(|context| context.source_dir.as_path());
            self.execute_ai_review(review, &changes.changes, source_dir, review_request, None)
                .await
        };
        if let Some(fallback) = self
            .timeout_fallback_for_execution(
                review,
                &changes.changes,
                review_request,
                &execution,
                &mut context,
                context_started,
            )
            .await
        {
            return fallback;
        }
        AiReviewExecutionWithMetadata {
            execution,
            metadata: AiReviewExecutionMetadata {
                execution_mode: AiReviewExecutionMode::Context,
                fallback_reason: None,
                context_elapsed_ms: Some(elapsed_ms(context_started)),
                fallback_elapsed_ms: None,
            },
        }
    }

    #[cfg(test)]
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
        match self
            .prepare_ai_review_context_inner(review, event, archive_sha)
            .await
        {
            Ok(context) => Ok(Some(context)),
            Err(err) if is_archive_limit_error(&err) => {
                let error_code = err
                    .review_failure()
                    .expect("archive limit errors have a structured review failure")
                    .code
                    .as_str();
                warn!(
                    project_id = event.project_id,
                    mr_iid = event.mr_iid,
                    commit = %event.commit_sha,
                    ai_review_id = %review.id,
                    error_code,
                    error = %err,
                    "AI review context archive exceeded limits; continuing with diff-only review"
                );
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    async fn prepare_ai_review_context_inner(
        &self,
        review: &AiReviewConfig,
        event: &MergeRequestEvent,
        archive_sha: &str,
    ) -> AppResult<AiReviewContextWorkDir> {
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
        let context = AiReviewContextWorkDir {
            work_dir,
            source_dir,
        };
        fs::create_dir_all(&context.source_dir)?;
        let extracted_entries =
            extract_zip_archive(&archive, &context.source_dir, &self.archive_limits)?;
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = archive_sha,
            ai_review_id = %review.id,
            archive_bytes = archive.len(),
            extracted_entries,
            source_dir = %context.source_dir.display(),
            "AI review context archive extracted"
        );
        Ok(context)
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
    if value == "." {
        return "%2E".into();
    }
    if value == ".." {
        return "%2E%2E".into();
    }
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

pub(crate) fn manual_review_ids(text: &str) -> Vec<String> {
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

fn selected_manual_ids(ai_reviews: &[AiReviewConfig]) -> Vec<String> {
    ai_reviews.iter().map(|review| review.id.clone()).collect()
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

fn failure_for_error(error: &AppError, fallback: ReviewErrorCode) -> ReviewFailure {
    error
        .review_failure()
        .cloned()
        .unwrap_or_else(|| ReviewFailure::new(fallback, error.to_string()))
}

fn is_archive_limit_error(error: &AppError) -> bool {
    matches!(
        error.review_failure(),
        Some(failure) if failure.code == ReviewErrorCode::ArchiveLimitExceeded
    )
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        routing::{get, post},
        Json, Router,
    };
    use std::{
        io::Write,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };
    use tokio::net::TcpListener;
    use zip::{write::SimpleFileOptions, ZipWriter};

    fn archive_with_two_files() -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut bytes);
            zip.start_file("repo-head/first.rs", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"first\n").unwrap();
            zip.start_file("repo-head/second.rs", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"second\n").unwrap();
            zip.finish().unwrap();
        }
        bytes.into_inner()
    }

    fn scripted_execution(result: AppResult<Vec<Finding>>, marker: usize) -> AiReviewExecution {
        AiReviewExecution {
            result,
            coverage: Some(ReviewCoverage {
                total_files: marker,
                fully_reviewed_files: 0,
                partially_reviewed_files: 0,
                unreviewed_files: 0,
                total_diff_bytes: 0,
                reviewed_diff_bytes: 0,
                required_batches: marker,
                planned_batches: marker,
                completed_batches: 0,
                max_batches: marker,
                tool_calls_used: 0,
                max_tool_calls: 0,
                complete: false,
            }),
            incomplete_files: vec![ReviewCoverageFile {
                path: format!("marker-{marker}"),
                status: "unreviewed",
                reason: "execution_failed",
                total_diff_bytes: 0,
                reviewed_diff_bytes: 0,
            }],
        }
    }

    async fn scripted_timeout_service(
        executions: Vec<AiReviewExecution>,
        suffix: &str,
    ) -> (
        ReviewService,
        TestAiExecutionSeam,
        AiReviewConfig,
        MergeRequestEvent,
        crate::gitlab::MergeRequestChanges,
    ) {
        let archive = archive_with_two_files();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/api/v4/projects/1/repository/archive.zip",
                    get(move || async move { archive.clone() }),
                ),
            )
            .await
            .unwrap();
        });
        let seam = TestAiExecutionSeam::default();
        seam.executions.lock().unwrap().extend(executions);
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        let run_id = format!("scripted-{suffix}-{}", std::process::id());
        let service = ReviewService::new(
            GitLabClient::new(format!("http://{addr}"), "token".into()),
            store,
            Ruleset::from_toml("").unwrap(),
        )
        .with_review_run_id(run_id)
        .with_ai_execution_seam(seam.clone());
        let review = AiReviewConfig {
            id: format!("review-{suffix}"),
            title: "Review".into(),
            base_url: "http://unused".into(),
            api_key: "key".into(),
            model: "model".into(),
            timeout_seconds: 37,
            request_timeout_seconds: Some(11),
            second_pass_on_clean: true,
            max_batch_diff_bytes: 7,
            max_batches: 3,
            extra_instructions: String::new(),
            max_tool_calls: 4,
            max_tool_result_bytes: 100,
        };
        let event = MergeRequestEvent {
            project_id: 1,
            project_name: None,
            project_path_with_namespace: None,
            mr_iid: 2,
            commit_sha: "head".into(),
            action: "test".into(),
            source_branch: String::new(),
            target_branch: String::new(),
        };
        let changes = crate::gitlab::MergeRequestChanges {
            changes: Vec::new(),
            diff_refs: crate::gitlab::DiffRefs {
                base_sha: Some("base".into()),
                start_sha: Some("start".into()),
                head_sha: Some("head".into()),
            },
        };
        (service, seam, review, event, changes)
    }

    #[tokio::test]
    async fn every_structured_timeout_falls_back_once_and_discards_first_execution_state() {
        for (index, (code, reason)) in [
            (
                ReviewErrorCode::ReviewRunTimeout,
                AiReviewFallbackReason::ReviewRunTimeout,
            ),
            (
                ReviewErrorCode::AiRequestTimeout,
                AiReviewFallbackReason::AiRequestTimeout,
            ),
            (
                ReviewErrorCode::AiToolLoopTimeout,
                AiReviewFallbackReason::AiToolLoopTimeout,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let first = scripted_execution(
                Err(AppError::ai_review(code, format!("timeout {index}"))),
                91,
            );
            let fallback = scripted_execution(Ok(Vec::new()), 7);
            let (service, seam, review, event, changes) =
                scripted_timeout_service(vec![first, fallback], &format!("eligible-{index}")).await;
            let result = service
                .run_ai_review_with_optional_clean_second_pass(&review, &changes, &event, None)
                .await;

            assert_eq!(result.metadata.fallback_reason, Some(reason));
            assert_eq!(result.execution.result.unwrap(), Vec::<Finding>::new());
            assert_eq!(result.execution.coverage.unwrap().total_files, 7);
            assert_eq!(result.execution.incomplete_files[0].path, "marker-7");
            let calls = seam.calls.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert!(calls[0].source_available);
            assert!(!calls[1].source_available);
            assert!(!calls[1].second_pass_on_clean);
            assert_eq!(calls[1].timeout_seconds, 37);
            assert_eq!(calls[1].max_batches, 3);
            assert_eq!(
                calls[1].trusted_runtime_instruction.as_deref(),
                Some(TIMEOUT_DIFF_ONLY_INSTRUCTION)
            );
        }
    }

    #[tokio::test]
    async fn noneligible_error_does_not_fallback_and_fallback_timeout_does_not_run_third_time() {
        let noneligible = scripted_execution(
            Err(AppError::ai_review(
                ReviewErrorCode::AiRequestFailed,
                "failed",
            )),
            4,
        );
        let (service, seam, review, event, changes) =
            scripted_timeout_service(vec![noneligible], "noneligible").await;
        let result = service
            .run_ai_review_with_optional_clean_second_pass(&review, &changes, &event, None)
            .await;
        assert!(
            matches!(result.execution.result, Err(ref error) if timeout_fallback_reason(error).is_none())
        );
        assert_eq!(seam.calls.lock().unwrap().len(), 1);

        let first = scripted_execution(
            Err(AppError::ai_review(
                ReviewErrorCode::ReviewRunTimeout,
                "first",
            )),
            1,
        );
        let second = scripted_execution(
            Err(AppError::ai_review(
                ReviewErrorCode::AiToolLoopTimeout,
                "fallback",
            )),
            2,
        );
        let (service, seam, review, event, changes) =
            scripted_timeout_service(vec![first, second], "fallback-timeout").await;
        let result = service
            .run_ai_review_with_optional_clean_second_pass(&review, &changes, &event, None)
            .await;
        assert!(
            matches!(result.execution.result, Err(ref error) if timeout_fallback_reason(error) == Some(AiReviewFallbackReason::AiToolLoopTimeout))
        );
        assert_eq!(seam.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn timeout_in_clean_context_confirmation_gets_one_independent_fallback() {
        for (suffix, fallback_result) in [
            ("success", scripted_execution(Ok(Vec::new()), 3)),
            (
                "timeout",
                scripted_execution(
                    Err(AppError::ai_review(
                        ReviewErrorCode::AiRequestTimeout,
                        "fallback timeout",
                    )),
                    4,
                ),
            ),
        ] {
            let clean_first = scripted_execution(Ok(Vec::new()), 1);
            let timed_out_confirmation = scripted_execution(
                Err(AppError::ai_review(
                    ReviewErrorCode::AiToolLoopTimeout,
                    "confirmation timeout",
                )),
                2,
            );
            let (service, seam, review, event, changes) = scripted_timeout_service(
                vec![clean_first, timed_out_confirmation, fallback_result],
                &format!("confirmation-{suffix}"),
            )
            .await;

            let result = service
                .run_ai_review_with_optional_clean_second_pass(&review, &changes, &event, None)
                .await;

            assert_eq!(
                result.metadata.fallback_reason,
                Some(AiReviewFallbackReason::AiToolLoopTimeout)
            );
            assert_eq!(
                result.execution.coverage.as_ref().unwrap().total_files,
                if suffix == "success" { 3 } else { 4 }
            );
            if suffix == "success" {
                assert!(result.execution.result.is_ok());
            } else {
                assert!(matches!(
                    result.execution.result,
                    Err(ref error) if timeout_fallback_reason(error)
                        == Some(AiReviewFallbackReason::AiRequestTimeout)
                ));
            }
            let calls = seam.calls.lock().unwrap();
            assert_eq!(
                calls.len(),
                3,
                "fallback timeout must not trigger a fourth run"
            );
            assert!(calls[0].source_available);
            assert!(calls[1].source_available);
            assert!(!calls[2].source_available);
            assert!(!calls[2].second_pass_on_clean);
            assert_eq!(
                calls[2].trusted_runtime_instruction.as_deref(),
                Some(TIMEOUT_DIFF_ONLY_INSTRUCTION)
            );
        }
    }

    #[tokio::test]
    async fn extraction_limit_fallback_removes_partially_extracted_work_dir() {
        let archive = archive_with_two_files();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/api/v4/projects/1/repository/archive.zip",
                    get(move || async move { archive.clone() }),
                ),
            )
            .await
            .unwrap();
        });
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        let limits = ArchiveLimits {
            max_extracted_files: 1,
            ..ArchiveLimits::default()
        };
        let run_id = format!("cleanup-regression-{}", std::process::id());
        let service = ReviewService::new(
            GitLabClient::new(format!("http://{addr}"), "token".into()),
            store,
            Ruleset::from_toml("").unwrap(),
        )
        .with_review_run_id(run_id.clone())
        .with_archive_limits(limits);
        let review = AiReviewConfig {
            id: "cleanup-review".into(),
            title: "Cleanup Review".into(),
            base_url: "http://127.0.0.1".into(),
            api_key: "key".into(),
            model: "model".into(),
            timeout_seconds: 1,
            request_timeout_seconds: None,
            second_pass_on_clean: false,
            max_batch_diff_bytes: 1,
            max_batches: 1,
            extra_instructions: String::new(),
            max_tool_calls: 1,
            max_tool_result_bytes: 1,
        };
        let event = MergeRequestEvent {
            project_id: 1,
            project_name: None,
            project_path_with_namespace: None,
            mr_iid: 2,
            commit_sha: "event-head".into(),
            action: "test".into(),
            source_branch: String::new(),
            target_branch: String::new(),
        };
        let changes = crate::gitlab::MergeRequestChanges {
            changes: Vec::new(),
            diff_refs: crate::gitlab::DiffRefs {
                base_sha: Some("base".into()),
                start_sha: Some("start".into()),
                head_sha: Some("archive-head".into()),
            },
        };
        let work_dir =
            ai_review_context_work_dir(1, 2, "archive-head", &review.id, Some(&run_id)).unwrap();

        let context = service
            .prepare_ai_review_context(&review, &changes, &event)
            .await
            .unwrap();

        assert!(context.is_none());
        assert!(!work_dir.exists());

        let result = service
            .run_ai_review_with_optional_clean_second_pass(&review, &changes, &event, None)
            .await;
        assert!(result.execution.result.is_ok());
        assert_eq!(
            result.metadata.execution_mode,
            AiReviewExecutionMode::DiffOnlyFallback
        );
        assert_eq!(
            result.metadata.fallback_reason,
            Some(AiReviewFallbackReason::ArchiveLimitExceeded)
        );
        assert!(result.metadata.context_elapsed_ms.is_some());
        assert!(result.metadata.fallback_elapsed_ms.is_some());
        assert!(!work_dir.exists());
    }

    #[tokio::test]
    async fn request_timeout_restarts_once_as_diff_only_with_fresh_batching() {
        let archive = archive_with_two_files();
        let archive_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let archive_addr = archive_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                archive_listener,
                Router::new().route(
                    "/api/v4/projects/1/repository/archive.zip",
                    get(move || async move { archive.clone() }),
                ),
            )
            .await
            .unwrap();
        });

        #[derive(Clone)]
        struct AiState {
            calls: Arc<AtomicUsize>,
            fallback_saw_removed_context: Arc<AtomicBool>,
            bodies: Arc<Mutex<Vec<serde_json::Value>>>,
            context_dir: PathBuf,
        }
        async fn ai_handler(
            State(state): State<AiState>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            let call = state.calls.fetch_add(1, Ordering::SeqCst) + 1;
            state.bodies.lock().unwrap().push(body);
            if call <= 2 {
                tokio::time::sleep(Duration::from_millis(2_200)).await;
            } else {
                state
                    .fallback_saw_removed_context
                    .store(!state.context_dir.exists(), Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(700)).await;
            }
            Json(serde_json::json!({
                "choices": [{"message": {"tool_calls": [{
                    "id": "submit_1", "type": "function",
                    "function": {"name": "submit_review_findings", "arguments": "{\"findings\":[]}"}
                }]}}]
            }))
        }

        let run_id = format!("timeout-fallback-{}", std::process::id());
        let context_dir =
            ai_review_context_work_dir(1, 2, "head", "timeout-review", Some(&run_id)).unwrap();
        let state = AiState {
            calls: Arc::new(AtomicUsize::new(0)),
            fallback_saw_removed_context: Arc::new(AtomicBool::new(false)),
            bodies: Arc::new(Mutex::new(Vec::new())),
            context_dir: context_dir.clone(),
        };
        let ai_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ai_addr = ai_listener.local_addr().unwrap();
        let server_state = state.clone();
        tokio::spawn(async move {
            axum::serve(
                ai_listener,
                Router::new()
                    .route("/chat/completions", post(ai_handler))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });

        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        let service = ReviewService::new(
            GitLabClient::new(format!("http://{archive_addr}"), "token".into()),
            store,
            Ruleset::from_toml("").unwrap(),
        )
        .with_review_run_id(run_id);
        let review = AiReviewConfig {
            id: "timeout-review".into(),
            title: "Timeout Review".into(),
            base_url: format!("http://{ai_addr}"),
            api_key: "key".into(),
            model: "model".into(),
            timeout_seconds: 5,
            request_timeout_seconds: Some(2),
            second_pass_on_clean: true,
            max_batch_diff_bytes: 30,
            max_batches: 2,
            extra_instructions: String::new(),
            max_tool_calls: 1,
            max_tool_result_bytes: 1_000,
        };
        let event = MergeRequestEvent {
            project_id: 1,
            project_name: None,
            project_path_with_namespace: None,
            mr_iid: 2,
            commit_sha: "head".into(),
            action: "test".into(),
            source_branch: String::new(),
            target_branch: String::new(),
        };
        let changes = crate::gitlab::MergeRequestChanges {
            changes: vec![
                GitLabChange {
                    old_path: "first.rs".into(),
                    new_path: "first.rs".into(),
                    diff: "@@ -0,0 +1 @@\n+first\n".into(),
                    new_file: false,
                    renamed_file: false,
                    deleted_file: false,
                },
                GitLabChange {
                    old_path: "second.rs".into(),
                    new_path: "second.rs".into(),
                    diff: "@@ -0,0 +1 @@\n+second\n".into(),
                    new_file: false,
                    renamed_file: false,
                    deleted_file: false,
                },
                GitLabChange {
                    old_path: "third.rs".into(),
                    new_path: "third.rs".into(),
                    diff: "@@ -0,0 +1 @@\n+third\n".into(),
                    new_file: false,
                    renamed_file: false,
                    deleted_file: false,
                },
            ],
            diff_refs: crate::gitlab::DiffRefs {
                base_sha: Some("base".into()),
                start_sha: Some("start".into()),
                head_sha: Some("head".into()),
            },
        };

        let overall_started = Instant::now();
        let result = service
            .run_ai_review_with_optional_clean_second_pass(&review, &changes, &event, None)
            .await;

        assert!(result.execution.result.is_ok());
        assert!(
            overall_started.elapsed() > Duration::from_secs(review.timeout_seconds),
            "fallback must succeed after the original shared deadline would have expired"
        );
        assert_eq!(state.calls.load(Ordering::SeqCst), 4);
        assert_eq!(
            result.metadata.fallback_reason,
            Some(AiReviewFallbackReason::AiRequestTimeout)
        );
        assert!(state.fallback_saw_removed_context.load(Ordering::SeqCst));
        assert!(!context_dir.exists());
        let coverage = result.execution.coverage.as_ref().unwrap();
        assert_eq!(coverage.required_batches, 3);
        assert_eq!(coverage.planned_batches, 2);
        assert_eq!(coverage.completed_batches, 2);
        assert_eq!(coverage.max_batches, 2);
        assert_eq!(coverage.unreviewed_files, 1);
        assert!(result
            .execution
            .incomplete_files
            .iter()
            .any(|file| file.path == "third.rs"));
        let bodies = state.bodies.lock().unwrap();
        assert!(bodies
            .iter()
            .all(|body| !body.to_string().contains("third.rs")));
        let fallback = &bodies[2];
        let fallback_messages = fallback["messages"].as_array().unwrap();
        assert!(fallback_messages.iter().any(|message| {
            message["role"] == "system"
                && message["content"].as_str() == Some(TIMEOUT_DIFF_ONLY_INSTRUCTION)
        }));
        let tool_names = fallback["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(tool_names, vec!["submit_review_findings"]);
        assert!(fallback_messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("第 1 / 2 个批次"));
        assert!(
            bodies[3]["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_str()
                .unwrap()
                .contains("第 2 / 2 个批次")
        );
    }

    #[test]
    fn archive_limit_error_is_eligible_for_diff_only_fallback() {
        let error = AppError::archive(
            ReviewErrorCode::ArchiveLimitExceeded,
            "rendered text is intentionally irrelevant",
        );

        assert!(is_archive_limit_error(&error));
    }

    #[test]
    fn archive_timeout_is_not_eligible_for_diff_only_fallback() {
        let error = AppError::archive(
            ReviewErrorCode::ArchiveDownloadTimeout,
            "archive_limit_exceeded appears only in rendered text",
        );

        assert!(!is_archive_limit_error(&error));
    }

    #[test]
    fn non_review_error_is_not_eligible_for_diff_only_fallback() {
        let error = AppError::Storage("archive_limit_exceeded".into());

        assert!(!is_archive_limit_error(&error));
    }

    #[test]
    fn parses_manual_review_ids() {
        assert_eq!(
            manual_review_ids("please run\n@check-todo-tbd, @other_check"),
            vec!["check-todo-tbd".to_string(), "other_check".to_string()]
        );
    }

    #[test]
    fn ignores_non_standalone_manual_review_ids() {
        assert!(manual_review_ids("please@check-todo-tbd").is_empty());
        assert!(manual_review_ids("@").is_empty());
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
    fn ai_review_context_work_dir_encodes_dot_review_id() {
        let path = ai_review_context_work_dir(1, 2, "abc123", ".", Some("run-1")).unwrap();
        let intended_parent = std::env::current_dir()
            .unwrap()
            .join("work/ai_review_context/1/2/abc123");

        assert!(path.starts_with(&intended_parent));
        assert_eq!(
            path.strip_prefix(&intended_parent).unwrap(),
            Path::new("%2E/run-1")
        );
        assert!(path.components().all(|component| !matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        )));
    }

    #[test]
    fn ai_review_context_work_dir_encodes_dotdot_review_run_id() {
        let path = ai_review_context_work_dir(1, 2, "abc123", "ai-review", Some("..")).unwrap();
        let intended_parent = std::env::current_dir()
            .unwrap()
            .join("work/ai_review_context/1/2/abc123/ai-review");

        assert!(path.starts_with(&intended_parent));
        assert_eq!(
            path.strip_prefix(&intended_parent).unwrap(),
            Path::new("%2E%2E")
        );
        assert!(path.components().all(|component| !matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        )));
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
