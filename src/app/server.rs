use crate::{
    config::AppConfig,
    error::AppResult,
    gitlab::GitLabClient,
    review::ReviewService,
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
use std::{net::SocketAddr, sync::Arc};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};

#[derive(Clone)]
struct AppState {
    config: AppConfig,
    store: StateStore,
}

pub async fn serve(config: AppConfig, store: StateStore) -> AppResult<()> {
    let addr: SocketAddr =
        config.server.bind.parse().map_err(|err| {
            crate::error::AppError::Config(format!("invalid bind address: {err}"))
        })?;
    let app = router(config, store);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(bind = %addr, "http server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn router(config: AppConfig, store: StateStore) -> Router {
    let state = Arc::new(AppState { config, store });
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
    info!(bytes = body.len(), "gitlab webhook received");
    let token = headers
        .get("X-Gitlab-Token")
        .and_then(|value| value.to_str().ok());
    if let Err(err) = validate_token(&state.config.server.webhook_secret, token) {
        warn!(error = %err, "gitlab webhook rejected");
        return (StatusCode::UNAUTHORIZED, err.to_string()).into_response();
    }

    let event = match parse_gitlab_webhook_event(&body) {
        Ok(Some(event)) => event,
        Ok(None) => {
            info!("gitlab webhook ignored because it is not a supported event");
            return StatusCode::ACCEPTED.into_response();
        }
        Err(err) => {
            warn!(error = %err, "gitlab webhook payload could not be parsed");
            return (StatusCode::BAD_REQUEST, err.to_string()).into_response();
        }
    };
    let gitlab_token = match state.config.gitlab_token() {
        Ok(token) => token,
        Err(err) => {
            error!(error = %err, "gitlab token configuration failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    };
    let ruleset = match Ruleset::from_path(&state.config.rules.file) {
        Ok(ruleset) => ruleset,
        Err(err) => {
            error!(
                error = %err,
                rules_file = %state.config.rules.file,
                "ruleset loading failed"
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    };
    info!(
        rules_file = %state.config.rules.file,
        ruleset_hash = %ruleset.hash(),
        line_rules = ruleset.line_rule_count(),
        script_tasks = ruleset.script_task_count(),
        ai_reviews = ruleset.ai_review_count(),
        "ruleset loaded"
    );
    let gitlab = GitLabClient::new(state.config.gitlab.base_url.clone(), gitlab_token);
    let service = ReviewService::new(gitlab, state.store.clone(), ruleset);

    let response_summary = match &event {
        GitLabWebhookEvent::MergeRequest(event) => {
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                action = %event.action,
                source_branch = %event.source_branch,
                target_branch = %event.target_branch,
                "gitlab merge request event parsed"
            );
            WebhookReviewSummary {
                project_id: event.project_id,
                mr_iid: event.mr_iid,
                commit_sha: event.commit_sha.clone(),
            }
        }
        GitLabWebhookEvent::MergeRequestNote(event) => {
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                action = %event.action,
                note_id = event.note_id,
                "gitlab merge request note event parsed"
            );
            WebhookReviewSummary {
                project_id: event.project_id,
                mr_iid: event.mr_iid,
                commit_sha: event.commit_sha.clone(),
            }
        }
    };

    tokio::spawn(run_webhook_review(service, event, response_summary.clone()));
    info!(
        project_id = response_summary.project_id,
        mr_iid = response_summary.mr_iid,
        commit_sha = %response_summary.commit_sha,
        "gitlab webhook review task accepted"
    );
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "accepted": true
        })),
    )
        .into_response()
}

#[derive(Clone)]
struct WebhookReviewSummary {
    project_id: i64,
    mr_iid: i64,
    commit_sha: String,
}

async fn run_webhook_review(
    service: ReviewService,
    event: GitLabWebhookEvent,
    response_summary: WebhookReviewSummary,
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
                project_id = response_summary.project_id,
                mr_iid = response_summary.mr_iid,
                commit_sha = %response_summary.commit_sha,
                error = %err,
                "gitlab webhook review failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::config::{GitLabConfig, LoggingConfig, RulesConfig, ServerConfig, StorageConfig},
        storage::StateStore,
    };
    use axum::http::HeaderValue;
    use std::time::{Duration, Instant};
    use tempfile::NamedTempFile;
    use tokio::io::AsyncReadExt;

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
        });
        let mut headers = HeaderMap::new();
        headers.insert("X-Gitlab-Token", HeaderValue::from_static("secret"));
        let body = Bytes::from_static(include_bytes!("../../tests/fixtures/gitlab_mr_event.json"));

        let started = Instant::now();
        let response = gitlab_webhook(State(state), headers, body)
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
