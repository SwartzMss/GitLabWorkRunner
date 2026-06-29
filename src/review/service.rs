use crate::{
    ai_review::run_ai_review,
    comments::{build_comment_drafts, CommentDraft},
    diff::parse_unified_diff,
    error::{AppError, AppResult},
    gitlab::{CreateDiscussionRequest, DiscussionPosition, GitLabChange, GitLabClient},
    rules::{AiReviewConfig, Finding, Ruleset, Severity},
    script_tasks::{ScriptTaskContext, ScriptTaskResult, ScriptTaskRunner, ScriptTaskStatus},
    storage::{ReviewKey, StateStore, StoredComment},
    webhook::MergeRequestEvent,
    webhook::MergeRequestNoteEvent,
};
use std::{collections::BTreeSet, fs, future::Future, time::Duration};
use tracing::{info, warn};

pub struct ReviewService {
    gitlab: GitLabClient,
    store: StateStore,
    ruleset: Ruleset,
}

impl ReviewService {
    pub fn new(gitlab: GitLabClient, store: StateStore, ruleset: Ruleset) -> Self {
        Self {
            gitlab,
            store,
            ruleset,
        }
    }

    pub async fn review_merge_request(
        &self,
        event: &MergeRequestEvent,
    ) -> AppResult<ReviewSummary> {
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = %event.commit_sha,
            action = %event.action,
            source_branch = %event.source_branch,
            target_branch = %event.target_branch,
            ruleset_hash = %self.ruleset.hash(),
            "review started"
        );

        let key = ReviewKey {
            project_id: event.project_id,
            mr_iid: event.mr_iid,
            commit_sha: &event.commit_sha,
            ruleset_hash: self.ruleset.hash(),
        };
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = %event.commit_sha,
            ruleset_hash = %self.ruleset.hash(),
            "checking processed review state"
        );
        if self.store.has_processed(&key).await? {
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                ruleset_hash = %self.ruleset.hash(),
                "review skipped because commit and ruleset were already processed"
            );
            return Ok(ReviewSummary {
                skipped: true,
                findings: 0,
                comments: 0,
            });
        }

        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = %event.commit_sha,
            "fetching merge request diff before review tasks"
        );
        let changes = self
            .gitlab
            .merge_request_changes(event.project_id, event.mr_iid)
            .await?;
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = %event.commit_sha,
            changed_files = changes.changes.len(),
            changed_paths = changed_paths(&changes.changes).len(),
            base_sha = ?changes.diff_refs.base_sha,
            start_sha = ?changes.diff_refs.start_sha,
            head_sha = ?changes.diff_refs.head_sha,
            "merge request diff fetched"
        );

        if !changes.diff_refs.is_complete() {
            warn!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                base_sha = ?changes.diff_refs.base_sha,
                start_sha = ?changes.diff_refs.start_sha,
                head_sha = ?changes.diff_refs.head_sha,
                "review skipped because gitlab diff refs are incomplete"
            );
            let created = self
                .gitlab
                .create_discussion(
                    event.project_id,
                    event.mr_iid,
                    &CreateDiscussionRequest {
                        body: incomplete_diff_refs_body(),
                        position: None,
                    },
                )
                .await?;
            self.store
                .record_comment(&StoredComment {
                    project_id: event.project_id,
                    mr_iid: event.mr_iid,
                    commit_sha: &event.commit_sha,
                    ruleset_hash: self.ruleset.hash(),
                    rule_id: "incomplete-diff-refs",
                    path: "",
                    new_line: None,
                    discussion_id: Some(&created.id),
                    note_id: created.notes.first().map(|note| note.id),
                })
                .await?;
            self.store.mark_processed(&key, "skipped").await?;
            return Ok(ReviewSummary {
                skipped: true,
                findings: 0,
                comments: 1,
            });
        }

        let mut findings = Vec::new();
        let mut ai_finding_count = 0_usize;
        let mut published = 0_usize;
        let mut line_review_skipped = false;

        if self.ruleset.has_line_rules() && !changes.diff_refs.is_complete() {
            warn!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                base_sha = ?changes.diff_refs.base_sha,
                start_sha = ?changes.diff_refs.start_sha,
                head_sha = ?changes.diff_refs.head_sha,
                "line review skipped because gitlab diff refs are incomplete"
            );
            let created = self
                .gitlab
                .create_discussion(
                    event.project_id,
                    event.mr_iid,
                    &CreateDiscussionRequest {
                        body: incomplete_diff_refs_body(),
                        position: None,
                    },
                )
                .await?;
            self.store
                .record_comment(&StoredComment {
                    project_id: event.project_id,
                    mr_iid: event.mr_iid,
                    commit_sha: &event.commit_sha,
                    ruleset_hash: self.ruleset.hash(),
                    rule_id: "incomplete-diff-refs",
                    path: "",
                    new_line: None,
                    discussion_id: Some(&created.id),
                    note_id: created.notes.first().map(|note| note.id),
                })
                .await?;
            published += 1;
            line_review_skipped = true;
        } else if changes.diff_refs.is_complete() {
            findings = self.evaluate_line_rules(event, &changes.changes)?;
            published += self
                .publish_line_findings(event, &changes, &findings)
                .await?;
        }

        let (ai_findings, ai_comments) = self.run_ai_reviews(event, &changes).await?;
        ai_finding_count += ai_findings;
        published += ai_comments;

        published += self.run_script_tasks(event, &changes).await?;

        self.store.mark_processed(&key, "success").await?;
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = %event.commit_sha,
            ruleset_hash = %self.ruleset.hash(),
            findings = findings.len() + ai_finding_count,
            comments = published,
            "review completed"
        );
        Ok(ReviewSummary {
            skipped: line_review_skipped && published == 1,
            findings: findings.len() + ai_finding_count,
            comments: published,
        })
    }

    pub async fn review_merge_request_note(
        &self,
        event: &MergeRequestNoteEvent,
    ) -> AppResult<ReviewSummary> {
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
            mr_iid: event.mr_iid,
            commit_sha: event.commit_sha.clone(),
            action: format!("manual-note-{}", event.note_id),
            source_branch: String::new(),
            target_branch: String::new(),
        };
        let ai_result = self
            .run_selected_ai_reviews(&mr_event, &changes, ai_reviews)
            .await?;
        let ai_completion_comments = self
            .publish_manual_ai_review_clean_comments(&mr_event, &changes, &ai_result)
            .await?;
        let script_comments = self
            .run_selected_script_tasks(&mr_event, &changes, tasks)
            .await?;
        let comments = ai_result.comments + ai_completion_comments + script_comments;
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            note_id = event.note_id,
            commit_sha = %event.commit_sha,
            findings = ai_result.findings,
            comments,
            "manual review completed"
        );
        Ok(ReviewSummary {
            skipped: false,
            findings: ai_result.findings,
            comments,
        })
    }

    fn evaluate_line_rules(
        &self,
        event: &MergeRequestEvent,
        changes: &[GitLabChange],
    ) -> AppResult<Vec<crate::rules::Finding>> {
        let mut findings = Vec::new();
        for change in changes {
            if change.deleted_file || change.diff.trim().is_empty() {
                info!(
                    project_id = event.project_id,
                    mr_iid = event.mr_iid,
                    path = %change.new_path,
                    deleted_file = change.deleted_file,
                    empty_diff = change.diff.trim().is_empty(),
                    "diff file skipped"
                );
                continue;
            }
            let diff_file = parse_unified_diff(&change.old_path, &change.new_path, &change.diff)?;
            let file_findings = self.ruleset.evaluate(&diff_file);
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                old_path = %change.old_path,
                new_path = %change.new_path,
                hunks = diff_file.hunks.len(),
                findings = file_findings.len(),
                new_file = change.new_file,
                renamed_file = change.renamed_file,
                deleted_file = change.deleted_file,
                "diff file evaluated"
            );
            findings.extend(file_findings);
        }
        Ok(findings)
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

    async fn run_ai_reviews(
        &self,
        event: &MergeRequestEvent,
        changes: &crate::gitlab::MergeRequestChanges,
    ) -> AppResult<(usize, usize)> {
        let changed_paths = changed_paths(&changes.changes);
        let reviews = self.ruleset.ai_reviews_for_changes(&changed_paths);
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = %event.commit_sha,
            changed_paths = changed_paths.len(),
            selected_ai_reviews = reviews.len(),
            "automatic AI reviews selected"
        );
        let summary = self
            .run_selected_ai_reviews(event, changes, reviews)
            .await?;
        Ok((summary.findings, summary.comments))
    }

    async fn run_selected_ai_reviews(
        &self,
        event: &MergeRequestEvent,
        changes: &crate::gitlab::MergeRequestChanges,
        reviews: Vec<AiReviewConfig>,
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
            return Ok(AiReviewRunSummary::default());
        }

        let mut summary = AiReviewRunSummary::default();
        for review in reviews {
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                ai_review_id = %review.id,
                "AI review started"
            );
            match run_ai_review_with_optional_clean_second_pass(&review, changes).await {
                Ok(findings) => {
                    let review_findings = findings.len();
                    summary.successful_reviews += 1;
                    summary.findings += review_findings;
                    if review_findings == 0 {
                        summary.clean_review_ids.push(review.id.clone());
                    }
                    summary.comments += self
                        .publish_line_findings(event, changes, &findings)
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
                    summary.failed_reviews += 1;
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

    async fn publish_manual_ai_review_clean_comments(
        &self,
        event: &MergeRequestEvent,
        changes: &crate::gitlab::MergeRequestChanges,
        summary: &AiReviewRunSummary,
    ) -> AppResult<usize> {
        if summary.failed_reviews > 0 || summary.findings > 0 || summary.comments > 0 {
            return Ok(0);
        }
        let mut published = 0_usize;
        for review_id in &summary.clean_review_ids {
            let draft = CommentDraft {
                path: String::new(),
                new_line: None,
                body: build_ai_review_clean_body(review_id),
            };
            published += self
                .publish_comment_drafts(event, changes, &[draft], &format!("ai:{review_id}:clean"))
                .await?;
        }
        Ok(published)
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
            let position = draft.new_line.map(|new_line| DiscussionPosition {
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
                old_path: draft.path.clone(),
                new_path: draft.path.clone(),
                new_line: Some(new_line),
            });
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
            self.store
                .record_comment(&StoredComment {
                    project_id: event.project_id,
                    mr_iid: event.mr_iid,
                    commit_sha: &event.commit_sha,
                    ruleset_hash: self.ruleset.hash(),
                    rule_id: record_rule_id,
                    path: &draft.path,
                    new_line: draft.new_line.map(i64::from),
                    discussion_id: Some(&created.id),
                    note_id: created.notes.first().map(|note| note.id),
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

    async fn run_script_tasks(
        &self,
        event: &MergeRequestEvent,
        changes: &crate::gitlab::MergeRequestChanges,
    ) -> AppResult<usize> {
        let changed_paths = changed_paths(&changes.changes);
        let tasks = self.ruleset.script_tasks_for_changes(&changed_paths);
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = %event.commit_sha,
            changed_paths = changed_paths.len(),
            selected_script_tasks = tasks.len(),
            "automatic script tasks selected"
        );
        self.run_selected_script_tasks(event, changes, tasks).await
    }

    async fn run_selected_script_tasks(
        &self,
        event: &MergeRequestEvent,
        changes: &crate::gitlab::MergeRequestChanges,
        tasks: Vec<crate::rules::ScriptTaskConfig>,
    ) -> AppResult<usize> {
        if tasks.is_empty() {
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                "no script tasks selected"
            );
            return Ok(0);
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
            .repository_archive(event.project_id, archive_sha)
            .await?;
        let runner = ScriptTaskRunner::new();
        let context = ScriptTaskContext {
            project_id: event.project_id,
            mr_iid: event.mr_iid,
            commit_sha: archive_sha,
        };
        let mut published = 0_usize;
        for task in tasks {
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = archive_sha,
                script_task_id = %task.id,
                "script task selected"
            );
            let result = runner.run(&task, &context, &archive).await?;
            if result.status == ScriptTaskStatus::IssueFound {
                published += self
                    .publish_script_task_result(event, changes, &result)
                    .await?;
            } else {
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
            comments = published,
            "script tasks completed"
        );
        Ok(published)
    }

    async fn publish_script_task_result(
        &self,
        event: &MergeRequestEvent,
        changes: &crate::gitlab::MergeRequestChanges,
        result: &ScriptTaskResult,
    ) -> AppResult<usize> {
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
            return self.publish_line_findings(event, changes, &findings).await;
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
        self.publish_comment_drafts(event, changes, &[draft], "script")
            .await
    }
}

fn incomplete_diff_refs_body() -> String {
    "**[警告] Review 已跳过**\n\n当前 MR 的 diff 信息不完整，无法可靠发布行级评论。请先解决冲突或刷新 MR 后重新触发检查。\n\n<!-- gitlab-work-runner:rule=incomplete-diff-refs -->".into()
}

fn build_ai_review_clean_body(review_id: &str) -> String {
    format!(
        "**AI Review 完成**\n\n未发现高置信度问题。\n\n<!-- gitlab-work-runner:rule=ai:{review_id}:clean -->"
    )
}

fn changed_paths(changes: &[GitLabChange]) -> Vec<String> {
    let mut paths = Vec::new();
    for change in changes {
        paths.push(change.new_path.clone());
        if change.old_path != change.new_path {
            paths.push(change.old_path.clone());
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

async fn run_ai_review_with_deadline<F>(
    review_id: &str,
    timeout_seconds: u64,
    future: F,
) -> AppResult<Vec<Finding>>
where
    F: Future<Output = AppResult<Vec<Finding>>>,
{
    let timeout_seconds = timeout_seconds.max(1);
    match tokio::time::timeout(Duration::from_secs(timeout_seconds), future).await {
        Ok(result) => result,
        Err(_) => Err(AppError::AiReview(format!(
            "AI review {review_id} timed out after {timeout_seconds} seconds"
        ))),
    }
}

async fn run_ai_review_with_optional_clean_second_pass(
    review: &AiReviewConfig,
    changes: &crate::gitlab::MergeRequestChanges,
) -> AppResult<Vec<Finding>> {
    let findings = run_ai_review_with_deadline(
        &review.id,
        review.timeout_seconds,
        run_ai_review(review, &changes.changes),
    )
    .await?;
    if !review.second_pass_on_clean || !findings.is_empty() {
        return Ok(findings);
    }

    info!(
        ai_review_id = %review.id,
        "AI review first pass was clean; running second confirmation pass"
    );
    run_ai_review_with_deadline(
        &review.id,
        review.timeout_seconds,
        run_ai_review(review, &changes.changes),
    )
    .await
}

fn manual_script_task_ids(text: &str) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|ch: char| {
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
        });
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
    clean_review_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AppError, AppResult};
    use std::future;

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

    #[tokio::test]
    async fn ai_review_deadline_times_out_pending_review_future() {
        let result = run_ai_review_with_deadline(
            "ai-review",
            1,
            future::pending::<AppResult<Vec<Finding>>>(),
        )
        .await;

        let Err(AppError::AiReview(message)) = result else {
            panic!("expected AI review timeout error");
        };

        assert!(message.contains("AI review ai-review timed out after 1 seconds"));
    }
}
