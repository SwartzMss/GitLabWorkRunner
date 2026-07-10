use crate::{
    error::{AppError, AppResult},
    gitlab::{CreateDiscussionRequest, GitLabClient},
    webhook::MergeRequestNoteEvent,
};
use tracing::{info, warn};

#[derive(Clone)]
pub(crate) struct ReviewNotifier {
    gitlab: GitLabClient,
}

pub(crate) struct ReviewFailureNotification<'a> {
    pub project_id: i64,
    pub mr_iid: i64,
    pub commit_sha: &'a str,
    pub review_run_id: &'a str,
    pub error: &'a AppError,
}

pub(crate) struct ReviewNotification {
    pub body: String,
}

impl ReviewNotifier {
    pub(crate) fn new(gitlab: GitLabClient) -> Self {
        Self { gitlab }
    }

    pub(crate) async fn notify_duplicate_running_review_request(
        &self,
        event: MergeRequestNoteEvent,
        active_review_run_id: String,
    ) {
        if let Err(err) = self
            .gitlab
            .award_merge_request_note_emoji(event.project_id, event.mr_iid, event.note_id, "eyes")
            .await
        {
            warn!(
                active_review_run_id = %active_review_run_id,
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                note_id = event.note_id,
                error = %err,
                "failed to award duplicate review request emoji; continuing notification"
            );
        }

        let notification =
            ReviewNotification::duplicate_running(&event.commit_sha, &active_review_run_id);
        match self
            .post_merge_request_level_comment(event.project_id, event.mr_iid, notification.body)
            .await
        {
            Ok(discussion_id) => info!(
                active_review_run_id = %active_review_run_id,
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                note_id = event.note_id,
                discussion_id = %discussion_id,
                "duplicate review request notification posted"
            ),
            Err(err) => warn!(
                active_review_run_id = %active_review_run_id,
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                note_id = event.note_id,
                error = %err,
                "failed to post duplicate review request notification"
            ),
        }
    }

    pub(crate) async fn notify_review_note_queue_busy(
        &self,
        event: MergeRequestNoteEvent,
        active_count: usize,
        max_concurrent_reviews: usize,
    ) {
        if let Err(err) = self
            .gitlab
            .award_merge_request_note_emoji(event.project_id, event.mr_iid, event.note_id, "eyes")
            .await
        {
            warn!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                note_id = event.note_id,
                active_count,
                max_concurrent_reviews,
                error = %err,
                "failed to award busy review request emoji; continuing notification"
            );
        }

        self.notify_review_queue_busy(
            event.project_id,
            event.mr_iid,
            event.commit_sha,
            active_count,
            max_concurrent_reviews,
        )
        .await;
    }

    pub(crate) async fn notify_review_queue_busy(
        &self,
        project_id: i64,
        mr_iid: i64,
        commit_sha: String,
        active_count: usize,
        max_concurrent_reviews: usize,
    ) {
        let notification =
            ReviewNotification::queue_busy(&commit_sha, active_count, max_concurrent_reviews);
        match self
            .post_merge_request_level_comment(project_id, mr_iid, notification.body)
            .await
        {
            Ok(discussion_id) => info!(
                project_id,
                mr_iid,
                commit_sha = %commit_sha,
                active_count,
                max_concurrent_reviews,
                discussion_id = %discussion_id,
                "busy review request notification posted"
            ),
            Err(err) => warn!(
                project_id,
                mr_iid,
                commit_sha = %commit_sha,
                active_count,
                max_concurrent_reviews,
                error = %err,
                "failed to post busy review request notification"
            ),
        }
    }

    pub(crate) async fn notify_review_failed(&self, failure: ReviewFailureNotification<'_>) {
        let notification = ReviewNotification::failure(ReviewFailureNotification {
            project_id: failure.project_id,
            mr_iid: failure.mr_iid,
            commit_sha: failure.commit_sha,
            review_run_id: failure.review_run_id,
            error: failure.error,
        });
        match self
            .post_merge_request_level_comment(failure.project_id, failure.mr_iid, notification.body)
            .await
        {
            Ok(_) => info!(
                review_run_id = %failure.review_run_id,
                project_id = failure.project_id,
                mr_iid = failure.mr_iid,
                commit_sha = %failure.commit_sha,
                "review failure notification posted"
            ),
            Err(comment_err) => warn!(
                review_run_id = %failure.review_run_id,
                project_id = failure.project_id,
                mr_iid = failure.mr_iid,
                commit_sha = %failure.commit_sha,
                review_error = %failure.error,
                error = %comment_err,
                "failed to post review failure notification"
            ),
        }
    }

    async fn post_merge_request_level_comment(
        &self,
        project_id: i64,
        mr_iid: i64,
        body: String,
    ) -> AppResult<String> {
        let discussion = self
            .gitlab
            .create_discussion(
                project_id,
                mr_iid,
                &CreateDiscussionRequest {
                    body,
                    position: None,
                },
            )
            .await?;
        Ok(discussion.id)
    }
}

impl ReviewNotification {
    pub(crate) fn duplicate_running(commit_sha: &str, active_review_run_id: &str) -> Self {
        Self {
            body: format!(
                "当前 commit `{commit_sha}` 已有 review 正在执行，请稍后再试。\n\n运行中的 review_run_id: `{active_review_run_id}`\n\n<!-- gitlab-work-runner:review-already-running commit={commit_sha} active_review_run_id={active_review_run_id} -->"
            ),
        }
    }

    pub(crate) fn queue_busy(
        commit_sha: &str,
        active_count: usize,
        max_concurrent_reviews: usize,
    ) -> Self {
        Self {
            body: format!(
                "当前 review 队列繁忙，请稍后再试。\n\ncommit: `{commit_sha}`\nactive_count: `{active_count}`\nmax_concurrent_reviews: `{max_concurrent_reviews}`\n\n<!-- gitlab-work-runner:review-queue-busy commit={commit_sha} active_count={active_count} max_concurrent_reviews={max_concurrent_reviews} -->"
            ),
        }
    }

    pub(crate) fn failure(failure: ReviewFailureNotification<'_>) -> Self {
        let error_text = truncate_for_comment(&failure.error.to_string(), 1200);
        Self {
            body: format!(
                "Review 执行失败，请查看 runner 日志后重试。\n\ncommit: `{}`\nreview_run_id: `{}`\nerror: `{}`\n\n<!-- gitlab-work-runner:review-failed review_run_id={} commit={} -->",
                failure.commit_sha,
                failure.review_run_id,
                error_text,
                failure.review_run_id,
                failure.commit_sha
            ),
        }
    }
}

fn truncate_for_comment(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for ch in value.chars().take(max_chars) {
        output.push(ch);
    }
    if value.chars().count() > max_chars {
        output.push_str("...");
    }
    output.replace('`', "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_notifications_build_mr_level_bodies() {
        let duplicate = ReviewNotification::duplicate_running("abc123", "rr-active");
        assert!(duplicate
            .body
            .contains("当前 commit `abc123` 已有 review 正在执行"));
        assert!(duplicate
            .body
            .contains("运行中的 review_run_id: `rr-active`"));
        assert!(duplicate
            .body
            .contains("gitlab-work-runner:review-already-running"));

        let busy = ReviewNotification::queue_busy("def456", 3, 4);
        assert!(busy.body.contains("当前 review 队列繁忙"));
        assert!(busy.body.contains("commit: `def456`"));
        assert!(busy.body.contains("active_count: `3`"));
        assert!(busy.body.contains("max_concurrent_reviews: `4`"));
        assert!(busy.body.contains("gitlab-work-runner:review-queue-busy"));

        let failure = ReviewNotification::failure(ReviewFailureNotification {
            project_id: 123,
            mr_iid: 45,
            commit_sha: "abc123",
            review_run_id: "rr-failed",
            error: &AppError::ai_review(
                crate::error::ReviewErrorCode::AiResponseParseFailed,
                "bad `json`",
            ),
        });
        assert!(failure.body.contains("Review 执行失败"));
        assert!(failure.body.contains("commit: `abc123`"));
        assert!(failure.body.contains("review_run_id: `rr-failed`"));
        assert!(failure.body.contains("bad 'json'"));
        assert!(failure.body.contains("gitlab-work-runner:review-failed"));
    }
}
