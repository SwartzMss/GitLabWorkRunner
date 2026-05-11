use crate::{
    comments::build_comment_drafts,
    diff::parse_unified_diff,
    error::AppResult,
    gitlab::{CreateDiscussionRequest, DiscussionPosition, GitLabClient},
    rules::Ruleset,
    storage::{ReviewKey, StateStore, StoredComment},
    webhook::MergeRequestEvent,
};

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
        let key = ReviewKey {
            project_id: event.project_id,
            mr_iid: event.mr_iid,
            commit_sha: &event.commit_sha,
            ruleset_hash: self.ruleset.hash(),
        };
        if self.store.has_processed(&key).await? {
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
        let mut findings = Vec::new();
        for change in &changes.changes {
            if change.deleted_file || change.diff.trim().is_empty() {
                continue;
            }
            let diff_file = parse_unified_diff(&change.old_path, &change.new_path, &change.diff)?;
            findings.extend(self.ruleset.evaluate(&diff_file));
        }

        let drafts = build_comment_drafts(&findings);
        let mut published = 0_usize;
        for draft in &drafts {
            let position = draft.new_line.map(|new_line| DiscussionPosition {
                base_sha: changes.diff_refs.base_sha.clone(),
                start_sha: changes.diff_refs.start_sha.clone(),
                head_sha: changes.diff_refs.head_sha.clone(),
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
            published += 1;
        }

        self.store.mark_processed(&key, "success").await?;
        Ok(ReviewSummary {
            skipped: false,
            findings: findings.len(),
            comments: published,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSummary {
    pub skipped: bool,
    pub findings: usize,
    pub comments: usize,
}
