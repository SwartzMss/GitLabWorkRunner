use crate::{
    config::AppConfig,
    error::AppResult,
    gitlab::GitLabClient,
    review::ReviewService,
    rules::Ruleset,
    storage::StateStore,
    webhook::{parse_merge_request_event, validate_token},
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

    let event = match parse_merge_request_event(&body) {
        Ok(Some(event)) => event,
        Ok(None) => {
            info!("gitlab webhook ignored because it is not a merge request event");
            return StatusCode::ACCEPTED.into_response();
        }
        Err(err) => {
            warn!(error = %err, "gitlab webhook payload could not be parsed");
            return (StatusCode::BAD_REQUEST, err.to_string()).into_response();
        }
    };
    info!(
        project_id = event.project_id,
        mr_iid = event.mr_iid,
        commit_sha = %event.commit_sha,
        action = %event.action,
        source_branch = %event.source_branch,
        target_branch = %event.target_branch,
        "gitlab merge request event parsed"
    );

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
    let gitlab = GitLabClient::new(state.config.gitlab.base_url.clone(), gitlab_token);
    let service = ReviewService::new(gitlab, state.store.clone(), ruleset);

    match service.review_merge_request(&event).await {
        Ok(summary) => {
            info!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                skipped = summary.skipped,
                findings = summary.findings,
                comments = summary.comments,
                "gitlab webhook review request accepted"
            );
            (
                StatusCode::ACCEPTED,
                Json(json!({
                    "skipped": summary.skipped,
                    "findings": summary.findings,
                    "comments": summary.comments
                })),
            )
                .into_response()
        }
        Err(err) => {
            error!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                error = %err,
                "gitlab webhook review failed"
            );
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}
