use crate::{
    config::AppConfig,
    error::AppResult,
    gitlab::{CreateDiscussionRequest, GitLabClient},
    review::work_cleanup::{cleanup_stale_review_work, spawn_periodic_stale_review_work_cleanup},
    review::{service::manual_script_task_ids, ReviewService},
    rules::Ruleset,
    storage::StateStore,
    webhook::{
        parse_gitlab_webhook_event, validate_token, GitLabWebhookEvent, MergeRequestNoteEvent,
    },
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
    collections::HashMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
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

#[derive(Default)]
struct ActiveReviews {
    running: Mutex<HashMap<ActiveReviewKey, String>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ActiveReviewKey {
    project_id: i64,
    mr_iid: i64,
    commit_sha: String,
}

struct ActiveReviewConflict {
    active_review_run_id: String,
}

struct ActiveReviewGuard {
    active_reviews: Arc<ActiveReviews>,
    key: ActiveReviewKey,
    review_run_id: String,
}

impl ActiveReviews {
    fn try_start(
        self: &Arc<Self>,
        key: ActiveReviewKey,
        review_run_id: String,
    ) -> Result<ActiveReviewGuard, ActiveReviewConflict> {
        let mut running = self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(active_review_run_id) = running.get(&key) {
            return Err(ActiveReviewConflict {
                active_review_run_id: active_review_run_id.clone(),
            });
        }
        running.insert(key.clone(), review_run_id.clone());
        Ok(ActiveReviewGuard {
            active_reviews: Arc::clone(self),
            key,
            review_run_id,
        })
    }

    fn finish(&self, key: &ActiveReviewKey, review_run_id: &str) -> bool {
        let mut running = self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if running
            .get(key)
            .is_some_and(|active_review_run_id| active_review_run_id == review_run_id)
        {
            running.remove(key);
            true
        } else {
            false
        }
    }
}

impl Drop for ActiveReviewGuard {
    fn drop(&mut self) {
        let removed = self.active_reviews.finish(&self.key, &self.review_run_id);
        info!(
            review_run_id = %self.review_run_id,
            project_id = self.key.project_id,
            mr_iid = self.key.mr_iid,
            commit_sha = %self.key.commit_sha,
            removed,
            "review run removed from active registry"
        );
    }
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
        script_tasks = ruleset.script_task_count(),
        ai_reviews = ruleset.ai_review_count(),
        "ruleset loaded"
    );
    let response_summary = match &event {
        GitLabWebhookEvent::MergeRequest(event) => {
            info!(
                review_run_id = %review_run_id,
                project_id = event.project_id,
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
                mr_iid: event.mr_iid,
                commit_sha: event.commit_sha.clone(),
            }
        }
        GitLabWebhookEvent::MergeRequestNote(event) => {
            info!(
                review_run_id = %review_run_id,
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                action = %event.action,
                note_id = event.note_id,
                "gitlab merge request note event parsed"
            );
            WebhookReviewSummary {
                review_run_id: review_run_id.clone(),
                project_id: event.project_id,
                mr_iid: event.mr_iid,
                commit_sha: event.commit_sha.clone(),
            }
        }
    };

    let active_review_guard = if event_requests_review(&event, &ruleset) {
        let active_key = ActiveReviewKey {
            project_id: response_summary.project_id,
            mr_iid: response_summary.mr_iid,
            commit_sha: response_summary.commit_sha.clone(),
        };
        match state
            .active_reviews
            .try_start(active_key, review_run_id.clone())
        {
            Ok(guard) => {
                info!(
                    review_run_id = %review_run_id,
                    project_id = response_summary.project_id,
                    mr_iid = response_summary.mr_iid,
                    commit_sha = %response_summary.commit_sha,
                    "review run registered as active"
                );
                Some(guard)
            }
            Err(conflict) => {
                info!(
                    review_run_id = %review_run_id,
                    active_review_run_id = %conflict.active_review_run_id,
                    project_id = response_summary.project_id,
                    mr_iid = response_summary.mr_iid,
                    commit_sha = %response_summary.commit_sha,
                    "gitlab webhook review skipped because commit review is already running"
                );
                if let GitLabWebhookEvent::MergeRequestNote(event) = &event {
                    let gitlab = GitLabClient::new(
                        state.config.gitlab.base_url.clone(),
                        gitlab_token.clone(),
                    );
                    let event = event.clone();
                    let active_review_run_id = conflict.active_review_run_id.clone();
                    let notification_span =
                        info_span!("review_run", review_run_id = %review_run_id);
                    tokio::spawn(
                        notify_duplicate_running_review_request(
                            gitlab,
                            event,
                            active_review_run_id,
                        )
                        .instrument(notification_span),
                    );
                }
                return (
                    StatusCode::ACCEPTED,
                    Json(json!({
                        "accepted": false,
                        "skipped": true,
                        "reason": "review_already_running",
                        "review_run_id": review_run_id,
                        "active_review_run_id": conflict.active_review_run_id
                    })),
                )
                    .into_response();
            }
        }
    } else {
        None
    };

    let gitlab = GitLabClient::new(state.config.gitlab.base_url.clone(), gitlab_token);
    let service = ReviewService::new(gitlab, state.store.clone(), ruleset)
        .with_review_run_id(review_run_id.clone());
    let review_span = info_span!("review_run", review_run_id = %review_run_id);
    tokio::spawn(
        run_webhook_review(
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
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "accepted": true,
            "review_run_id": review_run_id
        })),
    )
        .into_response()
}

#[derive(Clone)]
struct WebhookReviewSummary {
    review_run_id: String,
    project_id: i64,
    mr_iid: i64,
    commit_sha: String,
}

async fn run_webhook_review(
    service: ReviewService,
    event: GitLabWebhookEvent,
    response_summary: WebhookReviewSummary,
    _active_review_guard: Option<ActiveReviewGuard>,
) {
    let result = match &event {
        GitLabWebhookEvent::MergeRequest(event) => service.review_merge_request(event).await,
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
        }
    }
}

fn event_requests_review(event: &GitLabWebhookEvent, ruleset: &Ruleset) -> bool {
    match event {
        GitLabWebhookEvent::MergeRequest(_) => true,
        GitLabWebhookEvent::MergeRequestNote(event) => {
            let requested_ids = manual_script_task_ids(&event.note);
            !requested_ids.is_empty()
                && (!ruleset.script_tasks_by_ids(&requested_ids).is_empty()
                    || !ruleset.ai_reviews_by_ids(&requested_ids).is_empty())
        }
    }
}

async fn notify_duplicate_running_review_request(
    gitlab: GitLabClient,
    event: MergeRequestNoteEvent,
    active_review_run_id: String,
) {
    if let Err(err) = gitlab
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

    let body = duplicate_running_review_body(&event.commit_sha, &active_review_run_id);
    match gitlab
        .create_discussion(
            event.project_id,
            event.mr_iid,
            &CreateDiscussionRequest {
                body,
                position: None,
            },
        )
        .await
    {
        Ok(created) => info!(
            active_review_run_id = %active_review_run_id,
            project_id = event.project_id,
            mr_iid = event.mr_iid,
            note_id = event.note_id,
            discussion_id = %created.id,
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

fn duplicate_running_review_body(commit_sha: &str, active_review_run_id: &str) -> String {
    format!(
        "当前 commit `{commit_sha}` 已有 review 正在执行，请稍后再试。\n\n运行中的 review_run_id: `{active_review_run_id}`\n\n<!-- gitlab-work-runner:review-already-running commit={commit_sha} active_review_run_id={active_review_run_id} -->"
    )
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
mod tests {
    use super::*;
    use crate::{
        app::config::{GitLabConfig, LoggingConfig, RulesConfig, ServerConfig, StorageConfig},
        storage::StateStore,
    };
    use axum::body::to_bytes;
    use axum::http::HeaderValue;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use tempfile::NamedTempFile;
    use tokio::io::AsyncReadExt;
    use tokio::sync::Notify;

    #[tokio::test]
    async fn accepts_webhook_before_review_work_completes() {
        let rules_file = NamedTempFile::new().unwrap();
        std::fs::write(rules_file.path(), "").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gitlab_base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer).await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        });

        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        let state = Arc::new(AppState {
            config: AppConfig {
                server: ServerConfig {
                    bind: "127.0.0.1:0".into(),
                    webhook_secret: "secret".into(),
                },
                gitlab: GitLabConfig {
                    base_url: gitlab_base_url,
                    token: "token".into(),
                },
                storage: StorageConfig {
                    database_url: "sqlite::memory:".into(),
                },
                rules: RulesConfig {
                    file: rules_file.path().to_string_lossy().into_owned(),
                },
                logging: LoggingConfig::default(),
            },
            store,
            active_reviews: Arc::new(ActiveReviews::default()),
        });
        let mut headers = HeaderMap::new();
        headers.insert("X-Gitlab-Token", HeaderValue::from_static("secret"));
        let body = Bytes::from_static(include_bytes!("../../tests/fixtures/gitlab_mr_event.json"));

        let started = Instant::now();
        let response = gitlab_webhook(State(state), headers, body)
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["accepted"], true);
        assert!(!body["review_run_id"].as_str().unwrap().is_empty());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn duplicate_running_commit_note_gets_acknowledgement_comment() {
        let rules_file = NamedTempFile::new().unwrap();
        let command = if cfg!(windows) {
            "echo ok"
        } else {
            "printf ok"
        };
        std::fs::write(
            rules_file.path(),
            format!(
                r#"
[[script_tasks]]
id = "check"
title = "Check"
command = "{}"
timeout_seconds = 10
when_changed = ["src/**"]
"#,
                command.replace('\\', "\\\\").replace('"', "\\\"")
            ),
        )
        .unwrap();

        let change_count = Arc::new(AtomicUsize::new(0));
        let emoji_count = Arc::new(AtomicUsize::new(0));
        let duplicate_comment_count = Arc::new(AtomicUsize::new(0));
        let change_started = Arc::new(Notify::new());
        let release_change = Arc::new(Notify::new());

        let change_count_for_handler = Arc::clone(&change_count);
        let change_started_for_handler = Arc::clone(&change_started);
        let release_change_for_handler = Arc::clone(&release_change);
        let emoji_count_for_handler = Arc::clone(&emoji_count);
        let duplicate_comment_count_for_handler = Arc::clone(&duplicate_comment_count);
        let app = Router::new()
            .route(
                "/api/v4/projects/123/merge_requests/45/changes",
                get(move || {
                    let change_count = Arc::clone(&change_count_for_handler);
                    let change_started = Arc::clone(&change_started_for_handler);
                    let release_change = Arc::clone(&release_change_for_handler);
                    async move {
                        change_count.fetch_add(1, Ordering::SeqCst);
                        change_started.notify_one();
                        release_change.notified().await;
                        Json(json!({
                            "changes": [{
                                "old_path": "src/lib.rs",
                                "new_path": "src/lib.rs",
                                "new_file": false,
                                "renamed_file": false,
                                "deleted_file": false,
                                "diff": "@@ -1 +1 @@\n+pub fn value() {}\n"
                            }],
                            "diff_refs": {
                                "base_sha": "base",
                                "start_sha": "start",
                                "head_sha": "abc123"
                            }
                        }))
                    }
                }),
            )
            .route(
                "/api/v4/projects/123/merge_requests/45/notes/987/award_emoji",
                post(move || {
                    let emoji_count = Arc::clone(&emoji_count_for_handler);
                    async move {
                        emoji_count.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::CREATED,
                            Json(json!({
                                "id": 1,
                                "name": "eyes"
                            })),
                        )
                    }
                }),
            )
            .route(
                "/api/v4/projects/123/merge_requests/45/discussions",
                post(move |body: Bytes| {
                    let duplicate_comment_count = Arc::clone(&duplicate_comment_count_for_handler);
                    async move {
                        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let message = body["body"].as_str().unwrap();
                        assert!(message.contains("已有 review 正在执行"));
                        assert!(message.contains("abc123"));
                        assert!(body.get("position").is_none());
                        duplicate_comment_count.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::CREATED,
                            Json(json!({
                                "id": "duplicate-review-running",
                                "notes": [{ "id": 998 }]
                            })),
                        )
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gitlab_base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        let state = Arc::new(AppState {
            config: AppConfig {
                server: ServerConfig {
                    bind: "127.0.0.1:0".into(),
                    webhook_secret: "secret".into(),
                },
                gitlab: GitLabConfig {
                    base_url: gitlab_base_url,
                    token: "token".into(),
                },
                storage: StorageConfig {
                    database_url: "sqlite::memory:".into(),
                },
                rules: RulesConfig {
                    file: rules_file.path().to_string_lossy().into_owned(),
                },
                logging: LoggingConfig::default(),
            },
            store,
            active_reviews: Arc::new(ActiveReviews::default()),
        });
        let mut headers = HeaderMap::new();
        headers.insert("X-Gitlab-Token", HeaderValue::from_static("secret"));

        let first = gitlab_webhook(
            State(Arc::clone(&state)),
            headers.clone(),
            Bytes::from_static(include_bytes!("../../tests/fixtures/gitlab_mr_event.json")),
        )
        .await
        .into_response();
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        change_started.notified().await;

        let duplicate_note = Bytes::from_static(
            br#"{
                "object_kind": "note",
                "project_id": 123,
                "object_attributes": {
                    "id": 987,
                    "note": "@check",
                    "noteable_type": "MergeRequest",
                    "action": "create"
                },
                "merge_request": {
                    "iid": 45,
                    "last_commit": { "id": "abc123" }
                }
            }"#,
        );
        let duplicate = gitlab_webhook(State(state), headers, duplicate_note)
            .await
            .into_response();

        assert_eq!(duplicate.status(), StatusCode::ACCEPTED);
        let body = to_bytes(duplicate.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["accepted"], false);
        assert_eq!(body["reason"], "review_already_running");

        for _ in 0..20 {
            if duplicate_comment_count.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(change_count.load(Ordering::SeqCst), 1);
        assert_eq!(emoji_count.load(Ordering::SeqCst), 1);
        assert_eq!(duplicate_comment_count.load(Ordering::SeqCst), 1);

        release_change.notify_waiters();
    }
}
