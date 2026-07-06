use crate::{
    dashboard::{
        config::DashboardConfig,
        queries::{DashboardListParams, DashboardStore, RunListParams},
        views::DASHBOARD_HTML,
    },
    error::AppResult,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use serde_json::json;
use std::{net::SocketAddr, sync::Arc};
use tower_http::trace::TraceLayer;
use tracing::info;

#[derive(Clone)]
struct DashboardState {
    store: DashboardStore,
}

pub async fn serve(config: DashboardConfig) -> AppResult<()> {
    let addr: SocketAddr = config.bind.parse().map_err(|err| {
        crate::error::AppError::Config(format!("invalid dashboard bind address: {err}"))
    })?;
    let store = DashboardStore::connect(&config.database_url).await?;
    let app = router(store);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(
        bind = %addr,
        database_url = %config.database_url,
        "dashboard http server listening"
    );
    axum::serve(listener, app).await?;
    Ok(())
}

fn router(store: DashboardStore) -> Router {
    let state = Arc::new(DashboardState { store });
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/", get(dashboard_page))
        .route("/dashboard", get(dashboard_page))
        .route("/api/summary", get(summary))
        .route("/api/finding-summary", get(finding_summary))
        .route("/api/runs", get(runs))
        .route("/api/runs/:review_run_id", get(run_detail))
        .route("/api/projects", get(projects))
        .route("/api/merge-requests", get(merge_requests))
        .route("/api/findings", get(findings))
        .route("/api/comments", get(comments))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn readyz(State(state): State<Arc<DashboardState>>) -> impl IntoResponse {
    match state.store.check_schema().await {
        Ok(()) => Json(json!({ "status": "ready" })).into_response(),
        Err(err) => dashboard_error(err),
    }
}

async fn dashboard_page() -> impl IntoResponse {
    Html(DASHBOARD_HTML)
}

async fn summary(State(state): State<Arc<DashboardState>>) -> impl IntoResponse {
    match state.store.summary().await {
        Ok(summary) => Json(json!(summary)).into_response(),
        Err(err) => dashboard_error(err),
    }
}

async fn finding_summary(State(state): State<Arc<DashboardState>>) -> impl IntoResponse {
    match state.store.finding_summary().await {
        Ok(summary) => Json(json!(summary)).into_response(),
        Err(err) => dashboard_error(err),
    }
}

async fn runs(
    State(state): State<Arc<DashboardState>>,
    Query(params): Query<RunListParams>,
) -> impl IntoResponse {
    match state.store.runs(&params).await {
        Ok(runs) => Json(json!({ "runs": runs })).into_response(),
        Err(err) => dashboard_error(err),
    }
}

async fn run_detail(
    State(state): State<Arc<DashboardState>>,
    Path(review_run_id): Path<String>,
) -> impl IntoResponse {
    match state.store.run_detail(&review_run_id).await {
        Ok(Some(detail)) => Json(json!(detail)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "review_run_id not found" })),
        )
            .into_response(),
        Err(err) => dashboard_error(err),
    }
}

async fn projects(State(state): State<Arc<DashboardState>>) -> impl IntoResponse {
    match state.store.projects().await {
        Ok(projects) => Json(json!({ "projects": projects })).into_response(),
        Err(err) => dashboard_error(err),
    }
}

async fn merge_requests(State(state): State<Arc<DashboardState>>) -> impl IntoResponse {
    match state.store.merge_requests().await {
        Ok(merge_requests) => Json(json!({ "merge_requests": merge_requests })).into_response(),
        Err(err) => dashboard_error(err),
    }
}

async fn findings(
    State(state): State<Arc<DashboardState>>,
    Query(params): Query<DashboardListParams>,
) -> impl IntoResponse {
    match state.store.findings_list(&params).await {
        Ok(findings) => Json(json!({ "findings": findings })).into_response(),
        Err(err) => dashboard_error(err),
    }
}

async fn comments(
    State(state): State<Arc<DashboardState>>,
    Query(params): Query<DashboardListParams>,
) -> impl IntoResponse {
    match state.store.comments_list(&params).await {
        Ok(comments) => Json(json!({ "comments": comments })).into_response(),
        Err(err) => dashboard_error(err),
    }
}

fn dashboard_error(err: crate::error::AppError) -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": err.to_string() })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{ReviewRequestStart, StateStore, StoredComment, StoredFinding};
    use axum::body::{to_bytes, Body};
    use std::fs;
    use tower::ServiceExt;

    #[tokio::test]
    async fn dashboard_api_returns_summary_and_runs() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("dashboard.db");
        let database_url = format!("sqlite://{}", db_path.display());
        let writer = StateStore::connect(&database_url).await.unwrap();
        writer.migrate().await.unwrap();
        writer
            .start_review_request(&ReviewRequestStart {
                review_run_id: "rr-dashboard",
                trigger_type: "manual_note",
                project_id: 123,
                project_name: Some("Runner"),
                project_path_with_namespace: Some("platform/runner"),
                mr_iid: 45,
                commit_sha: "abc123",
                note_id: Some(987),
                requested_ids_json: r#"["ai-review"]"#,
                selected_ai_reviews: 1,
                selected_script_tasks: 0,
            })
            .await
            .unwrap();
        writer
            .finish_review_request("rr-dashboard", "completed", 2, 1)
            .await
            .unwrap();
        writer
            .record_finding(&StoredFinding {
                review_run_id: "rr-dashboard",
                task_type: "ai_review",
                task_id: "ai-review",
                rule_id: "ai:ai-review",
                severity: "error",
                path: "src/lib.rs",
                new_line: Some(7),
                title: "Finding title",
                message: "Finding message",
            })
            .await
            .unwrap();
        writer
            .record_comment(&StoredComment {
                review_run_id: "rr-dashboard",
                project_id: 123,
                mr_iid: 45,
                commit_sha: "abc123",
                rule_id: "ai:ai-review",
                path: "src/lib.rs",
                new_line: Some(7),
                discussion_id: Some("discussion-1"),
                note_id: Some(99),
            })
            .await
            .unwrap();
        drop(writer);
        assert!(fs::metadata(&db_path).unwrap().is_file());

        let store = DashboardStore::connect(&database_url).await.unwrap();
        let app = router(store);
        let response = axum::http::Request::builder()
            .uri("/api/summary")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(response).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["total_runs"], 1);
        assert_eq!(body["total_findings"], 2);

        let response = axum::http::Request::builder()
            .uri("/api/runs")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(response).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["runs"][0]["review_run_id"], "rr-dashboard");
        assert_eq!(body["runs"][0]["project_label"], "platform/runner");

        let response = axum::http::Request::builder()
            .uri("/api/projects")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(response).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["projects"][0]["project_label"], "platform/runner");

        let response = axum::http::Request::builder()
            .uri("/api/findings")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(response).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["findings"][0]["review_run_id"], "rr-dashboard");
        assert_eq!(body["findings"][0]["project_id"], 123);
        assert_eq!(body["findings"][0]["project_label"], "platform/runner");
        assert_eq!(body["findings"][0]["mr_iid"], 45);

        let response = axum::http::Request::builder()
            .uri("/api/comments")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(response).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["comments"][0]["review_run_id"], "rr-dashboard");
        assert_eq!(body["comments"][0]["project_label"], "platform/runner");
        assert_eq!(body["comments"][0]["note_id"], 99);
    }
}
