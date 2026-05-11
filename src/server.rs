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
    tracing::info!("listening on {}", addr);
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
    let token = headers
        .get("X-Gitlab-Token")
        .and_then(|value| value.to_str().ok());
    if let Err(err) = validate_token(&state.config.server.webhook_secret, token) {
        return (StatusCode::UNAUTHORIZED, err.to_string()).into_response();
    }

    let event = match parse_merge_request_event(&body) {
        Ok(Some(event)) => event,
        Ok(None) => return StatusCode::ACCEPTED.into_response(),
        Err(err) => return (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
    };

    let gitlab_token = match state.config.gitlab_token() {
        Ok(token) => token,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let ruleset = match Ruleset::from_path(&state.config.rules.file) {
        Ok(ruleset) => ruleset,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let gitlab = GitLabClient::new(state.config.gitlab.base_url.clone(), gitlab_token);
    let service = ReviewService::new(gitlab, state.store.clone(), ruleset);

    match service.review_merge_request(&event).await {
        Ok(summary) => (
            StatusCode::ACCEPTED,
            Json(json!({
                "skipped": summary.skipped,
                "findings": summary.findings,
                "comments": summary.comments
            })),
        )
            .into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}
