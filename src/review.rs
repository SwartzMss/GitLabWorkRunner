use crate::{
    comments::build_comment_drafts,
    diff::parse_unified_diff,
    error::AppResult,
    gitlab::{CreateDiscussionRequest, DiscussionPosition, GitLabClient},
    rules::Ruleset,
    storage::{ReviewKey, StateStore, StoredComment},
    webhook::MergeRequestEvent,
};
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
        for change in &changes.changes {
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

        let drafts = build_comment_drafts(&findings);
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
            skipped: false,
            findings: findings.len(),
            comments: published,
        })
    }
}

fn incomplete_diff_refs_body() -> String {
    "**[warning] Review skipped**\n\nGitLab did not return complete diff refs for this merge request, so GitLabWorkRunner cannot create reliable line-level comments. This usually happens when the merge request has conflicts or GitLab cannot prepare the merge diff yet.\n\nPlease resolve the merge conflicts or refresh the merge request, then trigger the webhook again.\n\n<!-- gitlab-work-runner:rule=incomplete-diff-refs -->".into()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSummary {
    pub skipped: bool,
    pub findings: usize,
    pub comments: usize,
}
