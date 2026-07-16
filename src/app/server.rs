use super::{
    active_reviews::{ActiveReviewGuard, ActiveReviewKey, ActiveReviewStartError, ActiveReviews},
    webhook_response::{self, WebhookReviewSummary},
};
use crate::{
    config::AppConfig,
    error::AppResult,
    gitlab::GitLabClient,
    review::{
        notifier::{ReviewFailureNotification, ReviewNotifier},
        work_cleanup::{cleanup_stale_review_work, spawn_periodic_stale_review_work_cleanup},
    },
    review::{service::manual_review_ids, ReviewService},
    rules::Ruleset,
    storage::StateStore,
    webhook::{parse_gitlab_webhook_event, validate_token, GitLabWebhookEvent},
};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tower_http::trace::TraceLayer;
use tracing::{error, info, info_span, warn, Instrument};

static REVIEW_RUN_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct AppState {
    config: AppConfig,
    store: StateStore,
    active_reviews: Arc<ActiveReviews>,
}

pub async fn serve(config: AppConfig, store: StateStore) -> AppResult<()> {
    let addr: SocketAddr =
        config.server.bind.parse().map_err(|err| {
            crate::error::AppError::Config(format!("invalid bind address: {err}"))
        })?;
    let app = router(config, store);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    if let Err(err) = cleanup_stale_review_work() {
        warn!(error = %err, "initial stale review work cleanup failed");
    }
    spawn_periodic_stale_review_work_cleanup();
    info!(bind = %addr, "http server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn router(config: AppConfig, store: StateStore) -> Router {
    let state = Arc::new(AppState {
        config,
        store,
        active_reviews: Arc::new(ActiveReviews::default()),
    });
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/webhooks/gitlab", post(gitlab_webhook))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn readyz() -> impl IntoResponse {
    Json(json!({ "status": "ready" }))
}

async fn gitlab_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let review_run_id = new_review_run_id();
    info!(
        review_run_id = %review_run_id,
        bytes = body.len(),
        "gitlab webhook received"
    );
    let token = headers
        .get("X-Gitlab-Token")
        .and_then(|value| value.to_str().ok());
    if let Err(err) = validate_token(&state.config.server.webhook_secret, token) {
        warn!(
            review_run_id = %review_run_id,
            error = %err,
            "gitlab webhook rejected"
        );
        return (StatusCode::UNAUTHORIZED, err.to_string()).into_response();
    }

    let event = match parse_gitlab_webhook_event(&body) {
        Ok(Some(event)) => event,
        Ok(None) => {
            info!(
                review_run_id = %review_run_id,
                "gitlab webhook ignored because it is not a supported event"
            );
            return StatusCode::ACCEPTED.into_response();
        }
        Err(err) => {
            warn!(
                review_run_id = %review_run_id,
                error = %err,
                "gitlab webhook payload could not be parsed"
            );
            return (StatusCode::BAD_REQUEST, err.to_string()).into_response();
        }
    };
    if let GitLabWebhookEvent::MergeRequest(event) = &event {
        info!(
            review_run_id = %review_run_id,
            project_id = event.project_id,
            project_name = event.project_name.as_deref(),
            project_path_with_namespace = event.project_path_with_namespace.as_deref(),
            mr_iid = event.mr_iid,
            commit_sha = %event.commit_sha,
            action = %event.action,
            source_branch = %event.source_branch,
            target_branch = %event.target_branch,
            "gitlab merge request event ignored because only manual mr note triggers are supported"
        );
        return webhook_response::ignored(
            review_run_id,
            "merge_request_events_manual_triggers_only",
        );
    }
    let gitlab_token = match state.config.gitlab_token() {
        Ok(token) => token,
        Err(err) => {
            error!(
                review_run_id = %review_run_id,
                error = %err,
                "gitlab token configuration failed"
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    };
    let ruleset = match Ruleset::from_path(&state.config.rules.file) {
        Ok(ruleset) => ruleset,
        Err(err) => {
            error!(
                review_run_id = %review_run_id,
                error = %err,
                rules_file = %state.config.rules.file,
                "ruleset loading failed"
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    };
    info!(
        review_run_id = %review_run_id,
        rules_file = %state.config.rules.file,
        ruleset_hash = %ruleset.hash(),
        ai_reviews = ruleset.ai_review_count(),
        "ruleset loaded"
    );
    let response_summary = match &event {
        GitLabWebhookEvent::MergeRequest(event) => {
            info!(
                review_run_id = %review_run_id,
                project_id = event.project_id,
                project_name = event.project_name.as_deref(),
                project_path_with_namespace = event.project_path_with_namespace.as_deref(),
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                action = %event.action,
                source_branch = %event.source_branch,
                target_branch = %event.target_branch,
                "gitlab merge request event parsed"
            );
            WebhookReviewSummary {
                review_run_id: review_run_id.clone(),
                project_id: event.project_id,
                project_name: event.project_name.clone(),
                project_path_with_namespace: event.project_path_with_namespace.clone(),
                mr_iid: event.mr_iid,
                commit_sha: event.commit_sha.clone(),
            }
        }
        GitLabWebhookEvent::MergeRequestNote(event) => {
            info!(
                review_run_id = %review_run_id,
                project_id = event.project_id,
                project_name = event.project_name.as_deref(),
                project_path_with_namespace = event.project_path_with_namespace.as_deref(),
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                action = %event.action,
                note_id = event.note_id,
                "gitlab merge request note event parsed"
            );
            WebhookReviewSummary {
                review_run_id: review_run_id.clone(),
                project_id: event.project_id,
                project_name: event.project_name.clone(),
                project_path_with_namespace: event.project_path_with_namespace.clone(),
                mr_iid: event.mr_iid,
                commit_sha: event.commit_sha.clone(),
            }
        }
    };

    if !event_requests_review(&event, &ruleset) {
        info!(
            review_run_id = %review_run_id,
            project_id = response_summary.project_id,
            project_name = response_summary.project_name.as_deref(),
            project_path_with_namespace = response_summary.project_path_with_namespace.as_deref(),
            mr_iid = response_summary.mr_iid,
            commit_sha = %response_summary.commit_sha,
            "gitlab webhook ignored because it did not request any configured manual review"
        );
        return webhook_response::ignored(review_run_id, "no_matching_manual_review");
    }

    let active_review_guard = {
        let active_key = ActiveReviewKey {
            project_id: response_summary.project_id,
            mr_iid: response_summary.mr_iid,
            commit_sha: response_summary.commit_sha.clone(),
        };
        match state.active_reviews.try_start(
            active_key,
            review_run_id.clone(),
            state.config.server.max_concurrent_reviews,
        ) {
            Ok(start) => {
                info!(
                    review_run_id = %review_run_id,
                    project_id = response_summary.project_id,
                    mr_iid = response_summary.mr_iid,
                    commit_sha = %response_summary.commit_sha,
                    queue_status = "accepted",
                    active_count = start.active_count,
                    max_concurrent_reviews = state.config.server.max_concurrent_reviews.max(1),
                    "review run registered as active"
                );
                Some(start.guard)
            }
            Err(ActiveReviewStartError::Duplicate {
                active_review_run_id,
                active_count,
            }) => {
                info!(
                    review_run_id = %review_run_id,
                    active_review_run_id = %active_review_run_id,
                    project_id = response_summary.project_id,
                    mr_iid = response_summary.mr_iid,
                    commit_sha = %response_summary.commit_sha,
                    queue_status = "duplicate",
                    active_count,
                    max_concurrent_reviews = state.config.server.max_concurrent_reviews.max(1),
                    "gitlab webhook review skipped because commit review is already running"
                );
                if let GitLabWebhookEvent::MergeRequestNote(event) = &event {
                    let gitlab = GitLabClient::new(
                        state.config.gitlab.base_url.clone(),
                        gitlab_token.clone(),
                    );
                    let notifier = ReviewNotifier::new(gitlab);
                    let event = event.clone();
                    let active_review_run_id = active_review_run_id.clone();
                    let notification_span =
                        info_span!("review_run", review_run_id = %review_run_id);
                    tokio::spawn(
                        async move {
                            notifier
                                .notify_duplicate_running_review_request(
                                    event,
                                    active_review_run_id,
                                )
                                .await;
                        }
                        .instrument(notification_span),
                    );
                }
                return webhook_response::review_already_running(
                    review_run_id,
                    active_review_run_id,
                    active_count,
                    state.config.server.max_concurrent_reviews,
                );
            }
            Err(ActiveReviewStartError::QueueBusy {
                active_count,
                max_concurrent_reviews,
            }) => {
                info!(
                    review_run_id = %review_run_id,
                    project_id = response_summary.project_id,
                    mr_iid = response_summary.mr_iid,
                    commit_sha = %response_summary.commit_sha,
                    queue_status = "busy",
                    active_count,
                    max_concurrent_reviews,
                    "gitlab webhook review skipped because global review concurrency is full"
                );
                let gitlab =
                    GitLabClient::new(state.config.gitlab.base_url.clone(), gitlab_token.clone());
                let notifier = ReviewNotifier::new(gitlab);
                let notification_span = info_span!("review_run", review_run_id = %review_run_id);
                if let GitLabWebhookEvent::MergeRequestNote(event) = &event {
                    let event = event.clone();
                    tokio::spawn(
                        async move {
                            notifier
                                .notify_review_note_queue_busy(
                                    event,
                                    active_count,
                                    max_concurrent_reviews,
                                )
                                .await;
                        }
                        .instrument(notification_span),
                    );
                }
                return webhook_response::review_queue_busy(
                    review_run_id,
                    active_count,
                    max_concurrent_reviews,
                );
            }
        }
    };

    let gitlab = GitLabClient::new_with_timeouts(
        state.config.gitlab.base_url.clone(),
        gitlab_token,
        Duration::from_secs(state.config.gitlab.api_timeout_seconds.max(1)),
        Duration::from_secs(state.config.gitlab.archive_timeout_seconds.max(1)),
    );
    let notifier = ReviewNotifier::new(gitlab.clone());
    let service = ReviewService::new(gitlab, state.store.clone(), ruleset)
        .with_review_run_id(review_run_id.clone())
        .with_archive_limits(state.config.archive.clone());
    let review_span = info_span!("review_run", review_run_id = %review_run_id);
    tokio::spawn(
        run_webhook_review(
            notifier,
            service,
            event,
            response_summary.clone(),
            active_review_guard,
        )
        .instrument(review_span),
    );
    info!(
        review_run_id = %review_run_id,
        project_id = response_summary.project_id,
        mr_iid = response_summary.mr_iid,
        commit_sha = %response_summary.commit_sha,
        "gitlab webhook review task accepted"
    );
    webhook_response::accepted(review_run_id)
}

async fn run_webhook_review(
    notifier: ReviewNotifier,
    service: ReviewService,
    event: GitLabWebhookEvent,
    response_summary: WebhookReviewSummary,
    _active_review_guard: Option<ActiveReviewGuard>,
) {
    let result = match &event {
        GitLabWebhookEvent::MergeRequest(_) => return,
        GitLabWebhookEvent::MergeRequestNote(event) => {
            service.review_merge_request_note(event).await
        }
    };

    match result {
        Ok(summary) => {
            info!(
                review_run_id = %response_summary.review_run_id,
                project_id = response_summary.project_id,
                mr_iid = response_summary.mr_iid,
                commit_sha = %response_summary.commit_sha,
                skipped = summary.skipped,
                findings = summary.findings,
                comments = summary.comments,
                "gitlab webhook review completed"
            );
        }
        Err(err) => {
            error!(
                review_run_id = %response_summary.review_run_id,
                project_id = response_summary.project_id,
                mr_iid = response_summary.mr_iid,
                commit_sha = %response_summary.commit_sha,
                error = %err,
                "gitlab webhook review failed"
            );
            notifier
                .notify_review_failed(ReviewFailureNotification {
                    project_id: response_summary.project_id,
                    mr_iid: response_summary.mr_iid,
                    commit_sha: &response_summary.commit_sha,
                    review_run_id: &response_summary.review_run_id,
                    error: &err,
                })
                .await;
        }
    }
}

fn event_requests_review(event: &GitLabWebhookEvent, ruleset: &Ruleset) -> bool {
    match event {
        GitLabWebhookEvent::MergeRequest(_) => false,
        GitLabWebhookEvent::MergeRequestNote(event) => {
            if !event.is_create_action() {
                return false;
            }
            let requested_ids = manual_review_ids(&event.note);
            !requested_ids.is_empty() && !ruleset.ai_reviews_by_ids(&requested_ids).is_empty()
        }
    }
}

fn new_review_run_id() -> String {
    let sequence = REVIEW_RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("rr-{millis}-{sequence}")
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
