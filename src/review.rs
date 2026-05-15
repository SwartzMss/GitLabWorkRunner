use crate::{
    comments::build_comment_drafts,
    diff::parse_unified_diff,
    error::AppResult,
    gitlab::{CreateDiscussionRequest, DiscussionPosition, GitLabChange, GitLabClient},
    rules::Ruleset,
    script_tasks::{ScriptTaskContext, ScriptTaskRunner},
    storage::{ReviewKey, StateStore, StoredComment},
    webhook::MergeRequestEvent,
};
use tracing::{info, warn};

pub struct ReviewService {
    gitlab: GitLabClient,
    store: StateStore,
    ruleset: Ruleset,
    gitlab_token_env: String,
}

impl ReviewService {
    pub fn new(
        gitlab: GitLabClient,
        store: StateStore,
        ruleset: Ruleset,
        gitlab_token_env: String,
    ) -> Self {
        Self {
            gitlab,
            store,
            ruleset,
            gitlab_token_env,
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

        let changes = self
            .gitlab
            .merge_request_changes(event.project_id, event.mr_iid)
            .await?;
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = %event.commit_sha,
            changed_files = changes.changes.len(),
            base_sha = ?changes.diff_refs.base_sha,
            start_sha = ?changes.diff_refs.start_sha,
            head_sha = ?changes.diff_refs.head_sha,
            "merge request diff fetched"
        );

        let mut findings = Vec::new();
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

        published += self.run_script_tasks(event, &changes).await?;

        self.store.mark_processed(&key, "success").await?;
        info!(
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            commit_sha = %event.commit_sha,
            ruleset_hash = %self.ruleset.hash(),
            findings = findings.len(),
            comments = published,
            "review completed"
        );
        Ok(ReviewSummary {
            skipped: line_review_skipped && published == 1,
            findings: findings.len(),
            comments: published,
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

        let mut published = 0_usize;
        for draft in &drafts {
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
            let created = self
                .gitlab
                .create_discussion(
                    event.project_id,
                    event.mr_iid,
                    &CreateDiscussionRequest {
                        body: draft.body.clone(),
                        position,
                    },
                )
                .await?;
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
                    rule_id: "grouped",
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
        if tasks.is_empty() {
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
            token_env: &self.gitlab_token_env,
        };
        for task in tasks {
            runner.run(&task, &context, &archive).await?;
        }
        Ok(0)
    }
}

fn incomplete_diff_refs_body() -> String {
    "**[warning] Review 已跳过**\n\n当前 MR 的 diff 信息不完整，无法可靠发布行级评论。请先解决冲突或刷新 MR 后重新触发检查。\n\n<!-- gitlab-work-runner:rule=incomplete-diff-refs -->".into()
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSummary {
    pub skipped: bool,
    pub findings: usize,
    pub comments: usize,
}
