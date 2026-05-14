use axum::{
    body::Bytes,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use gitlab_work_runner::{
    gitlab::GitLabClient, review::ReviewService, rules::Ruleset, storage::StateStore,
    webhook::MergeRequestEvent,
};
use serde_json::{json, Value};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::net::TcpListener;
use zip::{write::SimpleFileOptions, ZipWriter};

async fn spawn_server(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    spawn_server_on(listener, app);
    format!("http://{}", addr)
}

fn spawn_server_on(listener: TcpListener, app: Router) {
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
}

async fn bind_test_listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

fn test_archive() -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut bytes);
        zip.start_file("repo-head/src/lib.rs", SimpleFileOptions::default())
            .unwrap();
        use std::io::Write;
        zip.write_all(b"pub fn value() {}\n").unwrap();
        zip.finish().unwrap();
    }
    bytes.into_inner()
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
    let service = ReviewService::new(
        GitLabClient::new(base_url, "token".into()),
        store,
        ruleset,
        "GITLAB_TOKEN".into(),
    );
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

#[tokio::test]
async fn skips_rule_comments_when_diff_refs_are_incomplete() {
    let discussion_count = Arc::new(AtomicUsize::new(0));
    let discussion_count_for_handler = Arc::clone(&discussion_count);
    let (listener, addr) = bind_test_listener().await;
    let app = Router::new()
        .route(
            "/api/v4/projects/123/merge_requests/45/changes",
            get(|| async {
                Json(json!({
                    "changes": [{
                        "old_path": "src/lib.rs",
                        "new_path": "src/lib.rs",
                        "new_file": false,
                        "renamed_file": false,
                        "deleted_file": false,
                        "diff": "@@ -1 +1 @@\n+let value = maybe.unwrap();\n"
                    }],
                    "diff_refs": {
                        "base_sha": null,
                        "start_sha": "start",
                        "head_sha": "head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    assert!(body["body"].as_str().unwrap().contains("Review 已跳过"));
                    assert!(body["body"].as_str().unwrap().contains("请先解决冲突"));
                    assert!(body.get("position").is_none());
                    discussion_count.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::CREATED,
                        Json(json!({
                            "id": "discussion-1",
                            "notes": [{ "id": 99 }]
                        })),
                    )
                }
            }),
        );
    spawn_server_on(listener, app);
    let base_url = format!("http://{}", addr);

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
    let service = ReviewService::new(
        GitLabClient::new(base_url, "token".into()),
        store,
        ruleset,
        "GITLAB_TOKEN".into(),
    );
    let event = MergeRequestEvent {
        project_id: 123,
        mr_iid: 45,
        commit_sha: "abc123".into(),
        action: "update".into(),
        source_branch: "feature/review".into(),
        target_branch: "main".into(),
    };

    let summary = service.review_merge_request(&event).await.unwrap();

    assert!(summary.skipped);
    assert_eq!(summary.findings, 0);
    assert_eq!(summary.comments, 1);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn runs_script_task_and_posts_output_when_it_fails() {
    let discussion_count = Arc::new(AtomicUsize::new(0));
    let discussion_count_for_handler = Arc::clone(&discussion_count);
    let archive = Arc::new(test_archive());
    let archive_for_handler = Arc::clone(&archive);
    let (listener, addr) = bind_test_listener().await;
    let app = Router::new()
        .route(
            "/api/v4/projects/123/merge_requests/45/changes",
            get(|| async {
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
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(move || {
                let archive = Arc::clone(&archive_for_handler);
                async move { archive.as_ref().clone().into_response() }
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let body = body["body"].as_str().unwrap();
                    assert!(body.contains("Script failure"));
                    assert!(body.contains("script task failed"));
                    assert!(body.contains("gitlab-work-runner:script=check-script"));
                    discussion_count.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::CREATED,
                        Json(json!({
                            "id": "discussion-1",
                            "notes": [{ "id": 100 }]
                        })),
                    )
                }
            }),
        );
    spawn_server_on(listener, app);
    let base_url = format!("http://{}", addr);

    let command = if cfg!(windows) {
        "echo script task failed && exit /B 2"
    } else {
        "echo script task failed; exit 2"
    };
    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[script_tasks]]
id = "check-script"
title = "Script failure"
command = "{}"
timeout_seconds = 10
when_changed = ["src/**"]
"#,
        command.replace('\\', "\\\\").replace('"', "\\\"")
    ))
    .unwrap();
    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let service = ReviewService::new(
        GitLabClient::new(base_url, "token".into()),
        store,
        ruleset,
        "GITLAB_TOKEN".into(),
    );
    let event = MergeRequestEvent {
        project_id: 123,
        mr_iid: 45,
        commit_sha: "abc123".into(),
        action: "update".into(),
        source_branch: "feature/review".into(),
        target_branch: "main".into(),
    };

    let summary = service.review_merge_request(&event).await.unwrap();

    assert_eq!(summary.findings, 0);
    assert_eq!(summary.comments, 1);
    assert!(!summary.skipped);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 1);
}
