use axum::{http::StatusCode, routing::get, routing::post, Json, Router};
use gitlab_work_runner::{
    gitlab::GitLabClient, review::ReviewService, rules::Ruleset, storage::StateStore,
    webhook::MergeRequestEvent,
};
use serde_json::{json, Value};
use tokio::net::TcpListener;

async fn spawn_server(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

#[tokio::test]
async fn reviews_merge_request_and_records_state() {
    let app = Router::new()
        .route(
            "/api/v4/projects/123/merge_requests/45/changes",
            get(|| async {
                let body: Value = serde_json::from_str(include_str!("fixtures/mr_changes.json"))
                    .expect("valid fixture");
                Json(body)
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(|| async {
                (
                    StatusCode::CREATED,
                    Json(json!({
                        "id": "discussion-1",
                        "notes": [{ "id": 99 }]
                    })),
                )
            }),
        );
    let base_url = spawn_server(app).await;

    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let ruleset = Ruleset::from_toml(
        r#"
[[rules]]
id = "forbid-unwrap"
title = "Avoid unwrap"
severity = "warning"
path = "**/*.rs"
pattern = "\\.unwrap\\(\\)"
message = "Do not unwrap."
"#,
    )
    .unwrap();
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset);
    let event = MergeRequestEvent {
        project_id: 123,
        mr_iid: 45,
        commit_sha: "abc123".into(),
        action: "update".into(),
        source_branch: "feature/review".into(),
        target_branch: "main".into(),
    };

    let summary = service.review_merge_request(&event).await.unwrap();

    assert_eq!(summary.findings, 1);
    assert_eq!(summary.comments, 1);
    assert!(!summary.skipped);
}
