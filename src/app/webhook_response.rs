use axum::{http::StatusCode, response::IntoResponse, Json};
use serde_json::json;

#[derive(Clone)]
pub(crate) struct WebhookReviewSummary {
    pub(crate) review_run_id: String,
    pub(crate) project_id: i64,
    pub(crate) project_name: Option<String>,
    pub(crate) project_path_with_namespace: Option<String>,
    pub(crate) mr_iid: i64,
    pub(crate) commit_sha: String,
}

pub(crate) fn accepted(review_run_id: String) -> axum::response::Response {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "accepted": true,
            "review_run_id": review_run_id
        })),
    )
        .into_response()
}

pub(crate) fn ignored(review_run_id: String, reason: &str) -> axum::response::Response {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "accepted": false,
            "skipped": true,
            "reason": reason,
            "review_run_id": review_run_id
        })),
    )
        .into_response()
}

pub(crate) fn review_already_running(
    review_run_id: String,
    active_review_run_id: String,
    active_count: usize,
    max_concurrent_reviews: usize,
) -> axum::response::Response {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "accepted": false,
            "skipped": true,
            "reason": "review_already_running",
            "review_run_id": review_run_id,
            "active_review_run_id": active_review_run_id,
            "active_count": active_count,
            "max_concurrent_reviews": max_concurrent_reviews.max(1)
        })),
    )
        .into_response()
}

pub(crate) fn review_queue_busy(
    review_run_id: String,
    active_count: usize,
    max_concurrent_reviews: usize,
) -> axum::response::Response {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "accepted": false,
            "skipped": true,
            "reason": "review_queue_busy",
            "review_run_id": review_run_id,
            "active_count": active_count,
            "max_concurrent_reviews": max_concurrent_reviews
        })),
    )
        .into_response()
}
