use super::{
    active_reviews::{ActiveReviewGuard, ActiveReviewKey, ActiveReviewStartError, ActiveReviews},
    script_scheduler::{ScriptTaskPermit, ScriptTaskScheduler},
    webhook_response::{self, WebhookReviewSummary},
};
use crate::{
    config::AppConfig,
    error::AppResult,
    gitlab::GitLabClient,
    review::{
        notifier::{ReviewFailureNotification, ReviewNotifier},
        script_service::ScriptTaskService,
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
    script_tasks: Arc<ScriptTaskScheduler>,
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
        script_tasks: Arc::new(ScriptTaskScheduler::default()),
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

    let (requested_ai_reviews, requested_script_tasks) = match &event {
        GitLabWebhookEvent::MergeRequest(_) => (Vec::new(), Vec::new()),
        GitLabWebhookEvent::MergeRequestNote(note) if note.is_create_action() => {
            let ids = manual_review_ids(&note.note);
            (
                ruleset.ai_reviews_by_ids(&ids),
                ruleset.script_tasks_by_ids(&ids),
            )
        }
        GitLabWebhookEvent::MergeRequestNote(_) => (Vec::new(), Vec::new()),
    };
    if requested_ai_reviews.is_empty() && requested_script_tasks.is_empty() {
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

    let max_concurrent_reviews = ruleset.max_concurrent_reviews();
    let gitlab = GitLabClient::new_with_timeouts(
        state.config.gitlab.base_url.clone(),
        gitlab_token.clone(),
        Duration::from_secs(state.config.gitlab.api_timeout_seconds.max(1)),
        Duration::from_secs(state.config.gitlab.archive_timeout_seconds.max(1)),
    );
    let mut results = Vec::new();
    let mut started = 0_usize;
    let mut ai_rejection = None;

    if !requested_ai_reviews.is_empty() {
        let active_key = ActiveReviewKey {
            project_id: response_summary.project_id,
            mr_iid: response_summary.mr_iid,
            commit_sha: response_summary.commit_sha.clone(),
        };
        match state.active_reviews.try_start(
            active_key,
            review_run_id.clone(),
            max_concurrent_reviews,
        ) {
            Ok(start) => {
                info!(
                    review_run_id = %review_run_id,
                    project_id = response_summary.project_id,
                    mr_iid = response_summary.mr_iid,
                    commit_sha = %response_summary.commit_sha,
                    queue_status = "accepted",
                    active_count = start.active_count,
                    max_concurrent_reviews,
                    "review run registered as active"
                );
                let notifier = ReviewNotifier::new(gitlab.clone());
                let service = ReviewService::new(gitlab.clone(), state.store.clone(), ruleset)
                    .with_review_run_id(review_run_id.clone())
                    .with_archive_limits(state.config.archive.clone());
                let event_for_task = event.clone();
                let summary_for_task = response_summary.clone();
                let review_id = review_run_id.clone();
                let review_span = info_span!("review_run", review_run_id = %review_id);
                tokio::spawn(
                    run_webhook_review(
                        notifier,
                        service,
                        event_for_task,
                        summary_for_task,
                        Some(start.guard),
                    )
                    .instrument(review_span),
                );
                started += 1;
                results.push(serde_json::json!({"type":"ai_review", "status":"started", "review_run_id":review_run_id}));
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
                    max_concurrent_reviews,
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
                results.push(serde_json::json!({
                    "type": "ai_review",
                    "status": "duplicate",
                    "review_run_id": review_run_id,
                    "active_review_run_id": active_review_run_id.clone(),
                    "active_count": active_count,
                    "max_concurrent_reviews": max_concurrent_reviews,
                }));
                ai_rejection = Some(AiRejection::Duplicate {
                    active_review_run_id,
                    active_count,
                });
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
                results.push(serde_json::json!({
                    "type": "ai_review",
                    "status": "busy",
                    "review_run_id": review_run_id,
                    "active_count": active_count,
                    "max_concurrent_reviews": max_concurrent_reviews,
                }));
                ai_rejection = Some(AiRejection::Busy {
                    active_count,
                    max_concurrent_reviews,
                });
            }
        }
    }

    let has_script_tasks = !requested_script_tasks.is_empty();
    if !requested_script_tasks.is_empty() {
        let GitLabWebhookEvent::MergeRequestNote(note_event) = &event else {
            unreachable!()
        };
        for task in requested_script_tasks {
            let task_run_id = new_review_run_id();
            let service = ScriptTaskService::new(
                gitlab.clone(),
                state.store.clone(),
                state.config.archive.clone(),
            );
            match state
                .script_tasks
                .try_acquire(&task.id, task.max_concurrent)
            {
                Ok(permit) => {
                    let event_for_task = note_event.clone();
                    let script_id = task.id.clone();
                    let run_id_for_task = task_run_id.clone();
                    let span = info_span!("script_task", review_run_id = %task_run_id, script_task_id = %script_id);
                    tokio::spawn(
                        async move {
                            let _permit: ScriptTaskPermit = permit;
                            service.run(&event_for_task, task, &run_id_for_task).await;
                        }
                        .instrument(span),
                    );
                    started += 1;
                    results.push(serde_json::json!({"type":"script_task", "id":script_id, "status":"started", "review_run_id":task_run_id}));
                }
                Err(busy) => {
                    let event_for_task = note_event.clone();
                    let task_for_record = task.clone();
                    let script_id = task.id.clone();
                    let run_id_for_task = task_run_id.clone();
                    let active_count = busy.active_count;
                    let max_concurrent = busy.max_concurrent;
                    let span = info_span!("script_task", review_run_id = %task_run_id, script_task_id = %script_id);
                    tokio::spawn(
                        async move {
                            service
                                .record_busy(
                                    &event_for_task,
                                    &task_for_record,
                                    &run_id_for_task,
                                    active_count,
                                    max_concurrent,
                                )
                                .await;
                        }
                        .instrument(span),
                    );
                    results.push(serde_json::json!({"type":"script_task", "id":script_id, "status":"busy", "active_count":active_count, "max_concurrent":max_concurrent, "review_run_id":task_run_id}));
                }
            }
        }
    }

    if started == 0 {
        if !has_script_tasks {
            return match ai_rejection {
                Some(AiRejection::Duplicate {
                    active_review_run_id,
                    active_count,
                }) => webhook_response::review_already_running(
                    review_run_id,
                    active_review_run_id,
                    active_count,
                    max_concurrent_reviews,
                ),
                Some(AiRejection::Busy {
                    active_count,
                    max_concurrent_reviews,
                }) => webhook_response::review_queue_busy(
                    review_run_id,
                    active_count,
                    max_concurrent_reviews,
                ),
                None => webhook_response::ignored(review_run_id, "no_matching_manual_review"),
            };
        }
        return webhook_response::dispatch_rejected(review_run_id, results, "script_task_busy");
    }
    info!(
        review_run_id = %review_run_id,
        project_id = response_summary.project_id,
        mr_iid = response_summary.mr_iid,
        commit_sha = %response_summary.commit_sha,
        "gitlab webhook review task accepted"
    );
    if !has_script_tasks {
        webhook_response::accepted(review_run_id)
    } else {
        webhook_response::accepted_runs(review_run_id, results)
    }
}

enum AiRejection {
    Duplicate {
        active_review_run_id: String,
        active_count: usize,
    },
    Busy {
        active_count: usize,
        max_concurrent_reviews: usize,
    },
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

fn new_review_run_id() -> String {
    let sequence = REVIEW_RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("rr-{nanos}-{}-{sequence}", std::process::id())
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
