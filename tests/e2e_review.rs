use axum::{
    body::Bytes,
    extract::Query,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use gitlab_work_runner::{
    ai_review::{run_ai_review, run_ai_review_execution_with_context},
    dashboard::queries::DashboardStore,
    gitlab::GitLabChange,
    gitlab::GitLabClient,
    review::{ArchiveLimits, ReviewService},
    rules::{AiReviewConfig, Ruleset},
    storage::StateStore,
    webhook::MergeRequestNoteEvent,
};
use serde_json::{json, Value};
use sqlx::Row;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::sleep,
};
use zip::{write::SimpleFileOptions, ZipWriter};

fn manual_note_event(note: &str) -> MergeRequestNoteEvent {
    MergeRequestNoteEvent {
        project_id: 123,
        project_name: None,
        project_path_with_namespace: None,
        mr_iid: 45,
        commit_sha: "abc123".into(),
        action: "create".into(),
        note_id: 987,
        note: note.into(),
    }
}

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
    let discussion_count = Arc::new(AtomicUsize::new(0));
    let discussion_count_for_handler = Arc::clone(&discussion_count);
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
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
            "/api/v4/projects/123/repository/archive.zip",
            get(|| async { test_archive() }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    assert!(body["body"].as_str().unwrap().contains("Avoid unwrap"));
                    assert_eq!(body["position"]["new_path"], "src/lib.rs");
                    assert_eq!(body["position"]["new_line"], 1);
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
        )
        .route(
            "/chat/completions",
            post(move |_body: Bytes| {
                let ai_request_count = Arc::clone(&ai_request_count_for_handler);
                async move {
                    ai_request_count.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": serde_json::json!({
                                    "findings": [{
                                        "path": "src/lib.rs",
                                        "line": 1,
                                        "severity": "warning",
                                        "title": "Avoid unwrap",
                                        "message": "Do not unwrap."
                                    }]
                                }).to_string()
                            }
                        }]
                    }))
                }
            }),
        );
    let base_url = spawn_server(app).await;

    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "{}"
api_key = "test-api-key"
model = "test-model"
"#,
        base_url
    ))
    .unwrap();
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset);
    let event = manual_note_event("@ai-review");

    let summary = service.review_merge_request_note(&event).await.unwrap();

    assert_eq!(summary.findings, 1);
    assert_eq!(summary.comments, 1);
    assert!(!summary.skipped);
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 1);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn continues_publishing_review_comments_after_one_comment_fails() {
    let discussion_count = Arc::new(AtomicUsize::new(0));
    let discussion_count_for_handler = Arc::clone(&discussion_count);
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
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
                        "diff": "@@ -1,2 +1,2 @@\n+let first = maybe.unwrap();\n+let second = other.unwrap();\n"
                    }],
                    "diff_refs": {
                        "base_sha": "base",
                        "start_sha": "start",
                        "head_sha": "head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(|| async { test_archive() }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    assert!(body["body"].as_str().unwrap().contains("AI finding"));
                    let attempt = discussion_count.fetch_add(1, Ordering::SeqCst) + 1;
                    if attempt == 1 {
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                    (
                        StatusCode::CREATED,
                        Json(json!({
                            "id": "discussion-2",
                            "notes": [{ "id": 100 }]
                        })),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/chat/completions",
            post(move |_body: Bytes| {
                let ai_request_count = Arc::clone(&ai_request_count_for_handler);
                async move {
                    ai_request_count.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": serde_json::json!({
                                    "findings": [
                                        {
                                            "path": "src/lib.rs",
                                            "line": 1,
                                            "severity": "warning",
                                            "title": "AI finding one",
                                            "message": "First finding."
                                        },
                                        {
                                            "path": "src/lib.rs",
                                            "line": 2,
                                            "severity": "warning",
                                            "title": "AI finding two",
                                            "message": "Second finding."
                                        }
                                    ]
                                }).to_string()
                            }
                        }]
                    }))
                }
            }),
        );
    let base_url = spawn_server(app).await;

    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "{}"
api_key = "test-api-key"
model = "test-model"
"#,
        base_url
    ))
    .unwrap();
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset);
    let event = manual_note_event("@ai-review");

    let summary = service.review_merge_request_note(&event).await.unwrap();

    assert_eq!(summary.findings, 2);
    assert_eq!(summary.comments, 1);
    assert!(!summary.skipped);
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 1);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn rename_inline_comment_uses_old_path_and_records_fallback_position() {
    let discussion_count = Arc::new(AtomicUsize::new(0));
    let discussion_count_for_handler = Arc::clone(&discussion_count);
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
    let app = Router::new()
        .route(
            "/api/v4/projects/123/merge_requests/45/changes",
            get(|| async {
                Json(json!({
                    "changes": [{
                        "old_path": "src/old.rs",
                        "new_path": "src/new.rs",
                        "new_file": false,
                        "renamed_file": true,
                        "deleted_file": false,
                        "diff": "@@ -1 +1 @@\n+let value = unwrap_input();\n"
                    }],
                    "diff_refs": {
                        "base_sha": "base",
                        "start_sha": "start",
                        "head_sha": "head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(|| async { test_archive() }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let attempt = discussion_count.fetch_add(1, Ordering::SeqCst) + 1;
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    if attempt == 1 {
                        assert_eq!(body["position"]["old_path"], "src/old.rs");
                        assert_eq!(body["position"]["new_path"], "src/new.rs");
                        return StatusCode::BAD_REQUEST.into_response();
                    }
                    assert!(body["position"].is_null());
                    (
                        StatusCode::CREATED,
                        Json(json!({
                            "id": format!("discussion-{attempt}"),
                            "notes": [{ "id": 100 + attempt }]
                        })),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/chat/completions",
            post(move |_body: Bytes| {
                let ai_request_count = Arc::clone(&ai_request_count_for_handler);
                async move {
                    ai_request_count.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": serde_json::json!({
                                    "findings": [{
                                        "path": "src/new.rs",
                                        "line": 1,
                                        "severity": "error",
                                        "title": "Bad unwrap",
                                        "message": "Do not unwrap."
                                    }]
                                }).to_string()
                            }
                        }]
                    }))
                }
            }),
        );
    let base_url = spawn_server(app).await;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("rename-fallback.db");
    let database_url = format!("sqlite://{}", db_path.display());
    let store = StateStore::connect(&database_url).await.unwrap();
    store.migrate().await.unwrap();
    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "{}"
api_key = "test-api-key"
model = "test-model"
"#,
        base_url
    ))
    .unwrap();
    let service = ReviewService::new(
        GitLabClient::new(base_url, "token".into()),
        store.clone(),
        ruleset,
    )
    .with_review_run_id("rr-rename-fallback".into());
    let event = manual_note_event("@ai-review");

    let summary = service.review_merge_request_note(&event).await.unwrap();

    assert_eq!(summary.findings, 1);
    assert_eq!(summary.comments, 2);
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 1);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 3);
    let dashboard = DashboardStore::connect(&database_url).await.unwrap();
    let detail = dashboard
        .run_detail("rr-rename-fallback")
        .await
        .unwrap()
        .unwrap();
    let grouped = detail
        .comments
        .iter()
        .find(|comment| comment.rule_id == "grouped")
        .unwrap();
    assert_eq!(grouped.path, "");
    assert_eq!(grouped.new_line, None);
    assert_eq!(grouped.publish_position, "merge_request_fallback");
}

#[tokio::test]
async fn skips_review_when_diff_refs_are_incomplete() {
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
                    let body = body["body"].as_str().unwrap();
                    assert!(body.contains("GitLabWorkRunner Review"));
                    assert!(body.contains("**状态：** 已跳过"));
                    assert!(body.contains("AI Review 未执行，GitLab diff refs 不完整"));
                    assert!(!body.contains("**状态：** 完成"));
                    assert!(!body.contains("未发现高置信度问题"));
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
    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "{}"
api_key = "test-api-key"
model = "test-model"
"#,
        base_url
    ))
    .unwrap();
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset);
    let event = manual_note_event("@ai-review");

    let summary = service.review_merge_request_note(&event).await.unwrap();

    assert!(!summary.skipped);
    assert_eq!(summary.findings, 0);
    assert_eq!(summary.comments, 1);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reviews_merge_request_with_ai_review() {
    let discussion_count = Arc::new(AtomicUsize::new(0));
    let discussion_count_for_handler = Arc::clone(&discussion_count);
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
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
                        "base_sha": "base",
                        "start_sha": "start",
                        "head_sha": "ai-head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(|| async { test_archive() }),
        )
        .route(
            "/chat/completions",
            post(move |body: Bytes| {
                let ai_request_count = Arc::clone(&ai_request_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(body["model"], "test-model");
                    assert!(body["messages"][1]["content"]
                        .as_str()
                        .unwrap()
                        .contains("let value = maybe.unwrap();"));
                    assert!(body["messages"][1]["content"]
                        .as_str()
                        .unwrap()
                        .contains("重点关注 unwrap 的空值分支"));
                    assert!(body["messages"][1]["content"]
                        .as_str()
                        .unwrap()
                        .contains("@decorator ordering"));
                    ai_request_count.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": serde_json::json!({
                                    "findings": [{
                                        "path": "src/lib.rs",
                                        "line": 1,
                                        "severity": "warning",
                                        "title": "Avoid unwrap",
                                        "message": "Handle the None case instead of unwrapping."
                                    }]
                                }).to_string()
                            }
                        }]
                    }))
                }
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    assert!(body["body"].as_str().unwrap().contains("Avoid unwrap"));
                    assert!(body["body"]
                        .as_str()
                        .unwrap()
                        .contains("Handle the None case"));
                    assert!(body["body"]
                        .as_str()
                        .unwrap()
                        .contains("gitlab-work-runner:rule=ai:ai-review"));
                    assert_eq!(body["position"]["new_path"], "src/lib.rs");
                    assert_eq!(body["position"]["new_line"], 1);
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

    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "{}"
api_key = "test-api-key"
model = "test-model"
timeout_seconds = 10
"#,
        base_url
    ))
    .unwrap();
    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset);
    let event = manual_note_event("@ai-review 重点关注 unwrap 的空值分支和 @decorator ordering");

    let summary = service.review_merge_request_note(&event).await.unwrap();

    assert_eq!(summary.findings, 1);
    assert_eq!(summary.comments, 1);
    assert!(!summary.skipped);
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 1);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn manual_ai_review_posts_summary_when_one_review_fails() {
    let discussion_count = Arc::new(AtomicUsize::new(0));
    let discussion_count_for_handler = Arc::clone(&discussion_count);
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
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
                        "head_sha": "partial-ai-head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(|| async { test_archive() }),
        )
        .route(
            "/chat/completions",
            post(move |body: Bytes| {
                let ai_request_count = Arc::clone(&ai_request_count_for_handler);
                async move {
                    ai_request_count.fetch_add(1, Ordering::SeqCst);
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    match body["model"].as_str().unwrap() {
                        "bad-model" => Json(json!({
                            "choices": [{
                                "message": {
                                    "content": "not json"
                                }
                            }]
                        })),
                        "good-model" => Json(json!({
                            "choices": [{
                                "message": {
                                    "content": serde_json::json!({
                                        "findings": []
                                    }).to_string()
                                }
                            }]
                        })),
                        other => panic!("unexpected model {other}"),
                    }
                }
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let message = body["body"].as_str().unwrap();
                    assert!(message.contains("GitLabWorkRunner Review"));
                    assert!(message.contains("**状态：** 部分失败"));
                    assert!(message.contains("- `bad-review` Bad Review"));
                    assert!(
                        message.contains("Commit：** `abc123`")
                            || message.contains("**Commit：** `abc123`")
                    );
                    assert!(message.contains("gitlab-work-runner:summary run=rr-partial-auto"));
                    assert!(body["position"].is_null());
                    discussion_count.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::CREATED,
                        Json(json!({
                            "id": "discussion-partial",
                            "notes": [{ "id": 101 }]
                        })),
                    )
                }
            }),
        );
    spawn_server_on(listener, app);
    let base_url = format!("http://{}", addr);

    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[ai_reviews]]
id = "bad-review"
title = "Bad Review"
base_url = "{}"
api_key = "test-api-key"
model = "bad-model"
timeout_seconds = 10

[[ai_reviews]]
id = "good-review"
title = "Good Review"
base_url = "{}"
api_key = "test-api-key"
model = "good-model"
timeout_seconds = 10
"#,
        base_url, base_url
    ))
    .unwrap();
    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset)
        .with_review_run_id("rr-partial-auto".into());
    let event = manual_note_event("@bad-review @good-review");

    let summary = service.review_merge_request_note(&event).await.unwrap();

    assert_eq!(summary.findings, 0);
    assert_eq!(summary.comments, 1);
    assert!(!summary.skipped);
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 2);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 1);
}

fn test_ai_review_config(base_url: String) -> AiReviewConfig {
    AiReviewConfig {
        id: "ai-review".into(),
        title: "AI Review".into(),
        base_url,
        api_key: "test-api-key".into(),
        model: "test-model".into(),
        timeout_seconds: 10,
        request_timeout_seconds: None,
        max_batch_diff_bytes: 30_000,
        max_batches: 6,
        extra_instructions: String::new(),
        max_tool_calls: 8,
        max_tool_rounds: 3,
        max_tool_result_bytes: 60_000,
        max_tool_total_bytes: 40_000,
    }
}

#[tokio::test]
async fn ai_review_batches_large_merge_request_by_file() {
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
    let (listener, addr) = bind_test_listener().await;
    let app = Router::new().route(
        "/chat/completions",
        post(move |body: Bytes| {
            let ai_request_count = Arc::clone(&ai_request_count_for_handler);
            async move {
                ai_request_count.fetch_add(1, Ordering::SeqCst);
                let body: Value = serde_json::from_slice(&body).unwrap();
                let prompt = body["messages"][1]["content"].as_str().unwrap();
                let path = if prompt.contains("File: src/a.rs") {
                    "src/a.rs"
                } else if prompt.contains("File: src/b.rs") {
                    "src/b.rs"
                } else if prompt.contains("File: src/c.rs") {
                    "src/c.rs"
                } else {
                    panic!("batch prompt did not contain expected file path: {prompt}");
                };
                Json(json!({
                    "choices": [{
                        "message": {
                            "content": serde_json::json!({
                                "findings": [{
                                    "path": path,
                                    "line": 1,
                                    "severity": "error",
                                    "title": "Batch finding",
                                    "message": "Found in batch."
                                }]
                            }).to_string()
                        }
                    }]
                }))
            }
        }),
    );
    spawn_server_on(listener, app);

    let config = AiReviewConfig {
        base_url: format!("http://{}", addr),
        max_batch_diff_bytes: 24,
        max_batches: 2,
        ..test_ai_review_config(format!("http://{}", addr))
    };
    let changes = vec![
        GitLabChange {
            old_path: "src/a.rs".into(),
            new_path: "src/a.rs".into(),
            new_file: false,
            renamed_file: false,
            deleted_file: false,
            diff: "@@ -1 +1 @@\n+let a = 1;\n".into(),
        },
        GitLabChange {
            old_path: "src/b.rs".into(),
            new_path: "src/b.rs".into(),
            new_file: false,
            renamed_file: false,
            deleted_file: false,
            diff: "@@ -1 +1 @@\n+let b = 2;\n".into(),
        },
        GitLabChange {
            old_path: "src/c.rs".into(),
            new_path: "src/c.rs".into(),
            new_file: false,
            renamed_file: false,
            deleted_file: false,
            diff: "@@ -1 +1 @@\n+let c = 3;\n".into(),
        },
    ];

    let execution = run_ai_review_execution_with_context(&config, &changes, None, None).await;
    let coverage = execution.coverage.unwrap();
    let incomplete_files = execution.incomplete_files;
    let findings = execution.result.unwrap();

    assert_eq!(ai_request_count.load(Ordering::SeqCst), 2);
    assert_eq!(coverage.required_batches, 3);
    assert_eq!(coverage.planned_batches, 2);
    assert_eq!(coverage.completed_batches, 2);
    assert!(!coverage.complete);
    assert_eq!(incomplete_files.len(), 1);
    assert_eq!(incomplete_files[0].path, "src/c.rs");
    assert_eq!(incomplete_files[0].reason, "max_batches_reached");
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0].path, "src/a.rs");
    assert_eq!(findings[1].path, "src/b.rs");
}

#[tokio::test]
async fn batched_ai_review_preserves_coverage_when_total_deadline_fires() {
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let count_for_handler = Arc::clone(&ai_request_count);
    let (listener, addr) = bind_test_listener().await;
    let app = Router::new().route(
        "/chat/completions",
        post(move || {
            let count = Arc::clone(&count_for_handler);
            async move {
                let request_index = count.fetch_add(1, Ordering::SeqCst);
                if request_index > 0 {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Json(json!({
                    "choices": [{
                        "message": {
                            "content": "{\"findings\":[]}"
                        }
                    }]
                }))
            }
        }),
    );
    spawn_server_on(listener, app);
    let config = AiReviewConfig {
        base_url: format!("http://{}", addr),
        timeout_seconds: 1,
        request_timeout_seconds: Some(5),
        max_batch_diff_bytes: 28,
        max_batches: 2,
        ..test_ai_review_config(format!("http://{}", addr))
    };
    let changes = ["src/a.rs", "src/b.rs"]
        .into_iter()
        .map(|path| GitLabChange {
            old_path: path.into(),
            new_path: path.into(),
            new_file: false,
            renamed_file: false,
            deleted_file: false,
            diff: "@@ -1 +1 @@\n+let value = 1;\n".into(),
        })
        .collect::<Vec<_>>();

    let execution = run_ai_review_execution_with_context(&config, &changes, None, None).await;
    let coverage = execution.coverage.unwrap();

    assert!(execution.result.is_err());
    assert_eq!(coverage.required_batches, 2);
    assert_eq!(coverage.planned_batches, 2);
    assert_eq!(coverage.completed_batches, 1);
    assert_eq!(coverage.fully_reviewed_files, 1);
    assert_eq!(coverage.unreviewed_files, 1);
    assert!(execution
        .incomplete_files
        .iter()
        .any(|file| { file.path == "src/b.rs" && file.reason == "batch_execution_failed" }));
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn ai_review_falls_back_to_json_content_when_tools_are_rejected() {
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
    let (listener, addr) = bind_test_listener().await;
    let app = Router::new().route(
        "/chat/completions",
        post(move |body: Bytes| {
            let ai_request_count = Arc::clone(&ai_request_count_for_handler);
            async move {
                ai_request_count.fetch_add(1, Ordering::SeqCst);
                let body: Value = serde_json::from_slice(&body).unwrap();
                if body.get("tools").is_some() {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "error": {
                                "message": "unknown field: tools"
                            }
                        })),
                    );
                }

                (
                    StatusCode::OK,
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": serde_json::json!({
                                    "findings": [{
                                        "path": "src/lib.rs",
                                        "line": 1,
                                        "severity": "error",
                                        "title": "Fallback finding",
                                        "message": "Parsed from content fallback."
                                    }]
                                }).to_string()
                            }
                        }]
                    })),
                )
            }
        }),
    );
    spawn_server_on(listener, app);

    let config = test_ai_review_config(format!("http://{}", addr));
    let changes = vec![GitLabChange {
        old_path: "src/lib.rs".into(),
        new_path: "src/lib.rs".into(),
        new_file: false,
        renamed_file: false,
        deleted_file: false,
        diff: "@@ -1 +1 @@\n+panic!();\n".into(),
    }];

    let findings = run_ai_review(&config, &changes).await.unwrap();

    assert_eq!(ai_request_count.load(Ordering::SeqCst), 2);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].title, "Fallback finding");
}

#[tokio::test]
async fn ai_review_falls_back_to_json_content_after_retryable_tool_request_failure() {
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
    let (listener, addr) = bind_test_listener().await;
    let app = Router::new().route(
        "/chat/completions",
        post(move |body: Bytes| {
            let ai_request_count = Arc::clone(&ai_request_count_for_handler);
            async move {
                let request_index = ai_request_count.fetch_add(1, Ordering::SeqCst) + 1;
                let body: Value = serde_json::from_slice(&body).unwrap();
                if request_index == 1 {
                    assert!(body.get("tools").is_some());
                    sleep(Duration::from_secs(2)).await;
                    return (
                        StatusCode::OK,
                        Json(json!({
                            "choices": [{
                                "message": {
                                    "content": "{\"findings\":[]}"
                                }
                            }]
                        })),
                    );
                }
                if request_index == 2 {
                    assert!(body.get("tools").is_some());
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "error": {
                                "message": "unknown field: tools"
                            }
                        })),
                    );
                }

                assert!(body.get("tools").is_none());
                (
                    StatusCode::OK,
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": serde_json::json!({
                                    "findings": [{
                                        "path": "src/lib.rs",
                                        "line": 1,
                                        "severity": "error",
                                        "title": "Fallback after retry",
                                        "message": "Parsed from content fallback after retry."
                                    }]
                                }).to_string()
                            }
                        }]
                    })),
                )
            }
        }),
    );
    spawn_server_on(listener, app);

    let config = AiReviewConfig {
        base_url: format!("http://{}", addr),
        timeout_seconds: 2,
        request_timeout_seconds: Some(1),
        ..test_ai_review_config(format!("http://{}", addr))
    };
    let changes = vec![GitLabChange {
        old_path: "src/lib.rs".into(),
        new_path: "src/lib.rs".into(),
        new_file: false,
        renamed_file: false,
        deleted_file: false,
        diff: "@@ -1 +1 @@\n+panic!();\n".into(),
    }];

    let findings = run_ai_review(&config, &changes).await.unwrap();

    assert_eq!(ai_request_count.load(Ordering::SeqCst), 3);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].title, "Fallback after retry");
}

#[tokio::test]
async fn ai_review_retries_server_side_request_timeout_response() {
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
    let (listener, addr) = bind_test_listener().await;
    let app = Router::new().route(
        "/chat/completions",
        post(move |_body: Bytes| {
            let ai_request_count = Arc::clone(&ai_request_count_for_handler);
            async move {
                let request_index = ai_request_count.fetch_add(1, Ordering::SeqCst) + 1;
                if request_index == 1 {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({
                            "error": {
                                "message": "Request timed out, please try again later.",
                                "type": "RequestTimeOut",
                                "param": "",
                                "code": "RequestTimeOut"
                            }
                        })),
                    );
                }

                (
                    StatusCode::OK,
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": serde_json::json!({
                                    "findings": [{
                                        "path": "src/lib.rs",
                                        "line": 1,
                                        "severity": "error",
                                        "title": "Retried after timeout",
                                        "message": "Parsed after retry."
                                    }]
                                }).to_string()
                            }
                        }]
                    })),
                )
            }
        }),
    );
    spawn_server_on(listener, app);

    let config = test_ai_review_config(format!("http://{}", addr));
    let changes = vec![GitLabChange {
        old_path: "src/lib.rs".into(),
        new_path: "src/lib.rs".into(),
        new_file: false,
        renamed_file: false,
        deleted_file: false,
        diff: "@@ -1 +1 @@\n+panic!();\n".into(),
    }];

    let findings = run_ai_review(&config, &changes).await.unwrap();

    assert_eq!(ai_request_count.load(Ordering::SeqCst), 2);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].title, "Retried after timeout");
}

#[tokio::test]
async fn ai_review_synthesizes_matching_ids_for_empty_context_tool_calls() {
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
    let (listener, addr) = bind_test_listener().await;
    let app = Router::new().route(
        "/chat/completions",
        post(move |body: Bytes| {
            let ai_request_count = Arc::clone(&ai_request_count_for_handler);
            async move {
                let request_index = ai_request_count.fetch_add(1, Ordering::SeqCst) + 1;
                let body: Value = serde_json::from_slice(&body).unwrap();
                if request_index == 1 {
                    return Json(json!({
                        "choices": [{
                            "message": {
                                "tool_calls": [{
                                    "id": "",
                                    "type": "function",
                                    "function": {
                                        "name": "search_code",
                                        "arguments": "{\"query\":\"panic\"}"
                                    }
                                }]
                            }
                        }]
                    }));
                }

                let messages = body["messages"].as_array().unwrap();
                let assistant = messages
                    .iter()
                    .find(|message| message["role"] == "assistant")
                    .unwrap();
                let tool = messages
                    .iter()
                    .find(|message| message["role"] == "tool")
                    .unwrap();
                let assistant_tool_call_id = assistant["tool_calls"][0]["id"].as_str().unwrap();
                assert!(!assistant_tool_call_id.is_empty());
                assert_eq!(tool["tool_call_id"], assistant_tool_call_id);

                Json(json!({
                    "choices": [{
                        "message": {
                            "tool_calls": [{
                                "id": "submit_1",
                                "type": "function",
                                "function": {
                                    "name": "submit_review_findings",
                                    "arguments": "{\"findings\":[]}"
                                }
                            }]
                        }
                    }]
                }))
            }
        }),
    );
    spawn_server_on(listener, app);

    let config = AiReviewConfig {
        base_url: format!("http://{}", addr),
        ..test_ai_review_config(format!("http://{}", addr))
    };
    let changes = vec![GitLabChange {
        old_path: "src/lib.rs".into(),
        new_path: "src/lib.rs".into(),
        new_file: false,
        renamed_file: false,
        deleted_file: false,
        diff: "@@ -1 +1 @@\n+panic!();\n".into(),
    }];

    let source = tempfile::tempdir().unwrap();
    let findings =
        run_ai_review_execution_with_context(&config, &changes, Some(source.path()), None)
            .await
            .result
            .unwrap();

    assert!(findings.is_empty());
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn ai_review_uses_builtin_read_file_context_tool() {
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
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
                        "head_sha": "context-head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(move |_query: Query<HashMap<String, String>>| {
                let archive = Arc::clone(&archive_for_handler);
                async move { archive.as_ref().clone().into_response() }
            }),
        )
        .route(
            "/chat/completions",
            post(move |body: Bytes| {
                let ai_request_count = Arc::clone(&ai_request_count_for_handler);
                async move {
                    let attempt = ai_request_count.fetch_add(1, Ordering::SeqCst) + 1;
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    if attempt == 1 {
                        let tool_names: Vec<_> = body["tools"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|tool| tool["function"]["name"].as_str().unwrap())
                            .collect();
                        assert!(tool_names.contains(&"read_file"));
                        return Json(json!({
                            "choices": [{
                                "message": {
                                    "content": "",
                                    "tool_calls": [{
                                        "id": "call-read-file",
                                        "type": "function",
                                        "function": {
                                            "name": "read_file",
                                            "arguments": "{\"path\":\"src/lib.rs\"}"
                                        }
                                    }]
                                }
                            }]
                        }));
                    }

                    let tool_message = body["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|message| message["role"] == "tool")
                        .expect("tool result message");
                    assert!(tool_message["content"]
                        .as_str()
                        .unwrap()
                        .contains("pub fn value()"));
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": "",
                                "tool_calls": [{
                                    "id": "call-submit",
                                    "type": "function",
                                    "function": {
                                        "name": "submit_review_findings",
                                        "arguments": serde_json::json!({
                                            "findings": [{
                                                "path": "src/lib.rs",
                                                "line": 1,
                                                "severity": "error",
                                                "title": "Context finding",
                                                "message": "Finding produced after reading context."
                                            }]
                                        }).to_string()
                                    }
                                }]
                            }
                        }]
                    }))
                }
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let message = body["body"].as_str().unwrap();
                    assert!(message.contains("Context finding"));
                    assert!(message.contains("Finding produced after reading context"));
                    assert!(message.contains("gitlab-work-runner:rule=ai:ai-review"));
                    assert_eq!(body["position"]["new_line"], 1);
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
    let ruleset = Ruleset::from_toml(&format!(
        r#"
[ai_review]
max_tool_calls = 4

[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "{}"
api_key = "test-api-key"
model = "test-model"
"#,
        base_url
    ))
    .unwrap();
    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset);
    let event = manual_note_event("@ai-review");

    let summary = service.review_merge_request_note(&event).await.unwrap();

    assert_eq!(ai_request_count.load(Ordering::SeqCst), 2);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 1);
    assert_eq!(summary.findings, 1);
    assert_eq!(summary.comments, 1);
}

#[tokio::test]
async fn timeout_fallback_persists_final_diff_only_coverage_metadata_and_summary() {
    let request_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let request_bodies_for_handler = Arc::clone(&request_bodies);
    let summary_body = Arc::new(Mutex::new(String::new()));
    let summary_body_for_handler = Arc::clone(&summary_body);
    let (listener, addr) = bind_test_listener().await;
    let app = Router::new()
        .route(
            "/api/v4/projects/123/merge_requests/45/changes",
            get(|| async {
                Json(json!({
                    "changes": [
                        {"old_path":"src/a.rs","new_path":"src/a.rs","new_file":false,"renamed_file":false,"deleted_file":false,"diff":"@@ -1 +1 @@\n+let a = risky();\n"},
                        {"old_path":"src/b.rs","new_path":"src/b.rs","new_file":false,"renamed_file":false,"deleted_file":false,"diff":"@@ -1 +1 @@\n+let b = risky();\n"}
                    ],
                    "diff_refs":{"base_sha":"base","start_sha":"start","head_sha":"timeout-head"}
                }))
            }),
        )
        .route("/api/v4/projects/123/repository/archive.zip", get(|| async { test_archive() }))
        .route(
            "/chat/completions",
            post(move |body: Bytes| {
                let request_bodies = Arc::clone(&request_bodies_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let is_fallback = body["messages"].as_array().unwrap().iter().any(|message| {
                        message["content"].as_str().is_some_and(|content| content.contains("context-assisted review timed out"))
                    });
                    request_bodies.lock().unwrap().push(body);
                    if !is_fallback {
                        sleep(Duration::from_secs(2)).await;
                        return Json(json!({"choices":[{"message":{"content":"{\"findings\":[]}"}}]}));
                    }
                    Json(json!({"choices":[{"message":{"content":"","tool_calls":[{"id":"fallback-submit","type":"function","function":{"name":"submit_review_findings","arguments":"{\"findings\":[{\"path\":\"src/a.rs\",\"line\":1,\"severity\":\"error\",\"title\":\"Fallback only\",\"message\":\"Found by diff-only fallback.\"}]}"}}]}}]}))
                }
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let summary_body = Arc::clone(&summary_body_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let text = body["body"].as_str().unwrap();
                    if body["position"].is_null() {
                        *summary_body.lock().unwrap() = text.to_string();
                    }
                    (StatusCode::CREATED, Json(json!({"id":"discussion","notes":[{"id":102}]})))
                }
            }),
        );
    spawn_server_on(listener, app);
    let base_url = format!("http://{addr}");
    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "{base_url}"
api_key = "test"
model = "test"
timeout_seconds = 1
request_timeout_seconds = 5
max_batch_diff_bytes = 55
max_batches = 1
"#
    ))
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let database_url = format!(
        "sqlite://{}",
        dir.path().join("timeout-fallback.db").display()
    );
    let store = StateStore::connect(&database_url).await.unwrap();
    store.migrate().await.unwrap();
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset)
        .with_review_run_id("rr-timeout-fallback".into());

    let summary = service
        .review_merge_request_note(&manual_note_event("@ai-review"))
        .await
        .unwrap();

    assert_eq!(summary.findings, 1);
    assert_eq!(summary.comments, 2);
    let fallback = {
        let bodies = request_bodies.lock().unwrap();
        assert_eq!(
            bodies.len(),
            2,
            "one context attempt and one independent fallback"
        );
        bodies[1].clone()
    };
    assert_eq!(fallback["tools"].as_array().unwrap().len(), 1);
    assert_eq!(
        fallback["tools"][0]["function"]["name"],
        "submit_review_findings"
    );
    assert!(fallback["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|message| message["role"] != "tool"));
    let body = summary_body.lock().unwrap().clone();
    assert!(body.contains("### 降级执行"));
    assert!(body.contains("Context 审查超时"));
    assert!(body.contains("已使用 Diff-only 模式完成"));
    let pool = sqlx::SqlitePool::connect(&database_url).await.unwrap();
    let row = sqlx::query("select findings, coverage_required_batches, coverage_planned_batches, coverage_completed_batches, coverage_max_batches, coverage_complete, execution_mode, fallback_reason, context_elapsed_ms, fallback_elapsed_ms from review_task_runs where review_run_id = ?")
        .bind("rr-timeout-fallback").fetch_one(&pool).await.unwrap();
    assert_eq!(row.get::<i64, _>("findings"), 1);
    assert_eq!(row.get::<i64, _>("coverage_required_batches"), 2);
    assert_eq!(row.get::<i64, _>("coverage_planned_batches"), 1);
    assert_eq!(row.get::<i64, _>("coverage_completed_batches"), 1);
    assert_eq!(row.get::<i64, _>("coverage_max_batches"), 1);
    assert!(!row.get::<bool, _>("coverage_complete"));
    assert_eq!(row.get::<String, _>("execution_mode"), "diff_only_fallback");
    assert_eq!(
        row.get::<String, _>("fallback_reason"),
        "review_run_timeout"
    );
    assert!(row.get::<i64, _>("context_elapsed_ms") >= 1_000);
    assert!(row.get::<i64, _>("fallback_elapsed_ms") >= 0);
    let file = sqlx::query("select path, reason from review_coverage_files where review_run_id = ? and reason = 'max_batches_reached'")
        .bind("rr-timeout-fallback").fetch_one(&pool).await.unwrap();
    assert!(matches!(
        file.get::<String, _>("path").as_str(),
        "src/a.rs" | "src/b.rs"
    ));
    assert_eq!(file.get::<String, _>("reason"), "max_batches_reached");
}

#[tokio::test]
async fn archive_limit_uses_diff_only_execution_without_context_tools() {
    let archive_request_count = Arc::new(AtomicUsize::new(0));
    let archive_request_count_for_handler = Arc::clone(&archive_request_count);
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
    let summary_body = Arc::new(Mutex::new(String::new()));
    let summary_body_for_handler = Arc::clone(&summary_body);
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
                        "head_sha": "oversized-head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(move || {
                let archive_request_count = Arc::clone(&archive_request_count_for_handler);
                async move {
                    archive_request_count.fetch_add(1, Ordering::SeqCst);
                    test_archive()
                }
            }),
        )
        .route(
            "/chat/completions",
            post(move |body: Bytes| {
                let ai_request_count = Arc::clone(&ai_request_count_for_handler);
                async move {
                    let attempt = ai_request_count.fetch_add(1, Ordering::SeqCst) + 1;
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let tool_names: Vec<_> = body["tools"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|tool| tool["function"]["name"].as_str().unwrap())
                        .collect();
                    assert_eq!(tool_names, vec!["submit_review_findings"]);
                    assert!(body["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|message| message["role"] != "tool"));
                    if attempt % 2 == 1 {
                        return Json(json!({
                            "choices": [{
                                "message": {
                                    "content": "",
                                    "tool_calls": [{
                                        "id": "hallucinated-read",
                                        "type": "function",
                                        "function": {
                                            "name": "read_file",
                                            "arguments": "{\"path\":\"src/lib.rs\"}"
                                        }
                                    }]
                                }
                            }]
                        }));
                    }
                    assert!(body["messages"].as_array().unwrap().iter().all(|message| {
                        message["tool_calls"].as_array().is_none_or(|calls| {
                            calls.iter().all(|call| {
                                !matches!(
                                    call["function"]["name"].as_str(),
                                    Some("read_file" | "search_code" | "list_files")
                                )
                            })
                        })
                    }));
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": "",
                                "tool_calls": [{
                                    "id": "call-submit",
                                    "type": "function",
                                    "function": {
                                        "name": "submit_review_findings",
                                        "arguments": "{\"findings\":[]}"
                                    }
                                }]
                            }
                        }]
                    }))
                }
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let summary_body = Arc::clone(&summary_body_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    *summary_body.lock().unwrap() = body["body"].as_str().unwrap().to_string();
                    (
                        StatusCode::CREATED,
                        Json(json!({"id":"archive-summary","notes":[{"id":101}]})),
                    )
                }
            }),
        );
    spawn_server_on(listener, app);
    let base_url = format!("http://{}", addr);
    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "{}"
api_key = "test-api-key"
model = "test-model"
"#,
        base_url
    ))
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}", dir.path().join("archive.db").display());
    let store = StateStore::connect(&database_url).await.unwrap();
    store.migrate().await.unwrap();
    let archive_limits = ArchiveLimits {
        max_archive_bytes: 1,
        ..ArchiveLimits::default()
    };
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset)
        .with_archive_limits(archive_limits)
        .with_review_run_id("rr-archive-degraded".into());

    let summary = service
        .review_merge_request_note(&manual_note_event("@ai-review"))
        .await
        .unwrap();

    assert_eq!(archive_request_count.load(Ordering::SeqCst), 1);
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 2);
    assert_eq!(summary.findings, 0);
    assert_eq!(summary.comments, 1);
    let body = summary_body.lock().unwrap().clone();
    assert!(body.contains("### 降级执行"));
    assert!(body.contains("仓库上下文归档超过限制"));
    assert!(body.contains("已使用 Diff-only 模式完成"));
    let pool = sqlx::SqlitePool::connect(&database_url).await.unwrap();
    let row = sqlx::query("select execution_mode, fallback_reason, context_elapsed_ms, fallback_elapsed_ms from review_task_runs where review_run_id = ?")
        .bind("rr-archive-degraded")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("execution_mode"), "diff_only_fallback");
    assert_eq!(
        row.get::<String, _>("fallback_reason"),
        "archive_limit_exceeded"
    );
    assert!(row.get::<i64, _>("context_elapsed_ms") >= 0);
    assert!(row.get::<i64, _>("fallback_elapsed_ms") >= 0);
}

#[tokio::test]
async fn non_limit_archive_failure_still_fails_before_ai_review() {
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
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
                        "head_sha": "failed-head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "archive unavailable") }),
        )
        .route(
            "/chat/completions",
            post(move || {
                let ai_request_count = Arc::clone(&ai_request_count_for_handler);
                async move {
                    ai_request_count.fetch_add(1, Ordering::SeqCst);
                    Json(json!({}))
                }
            }),
        );
    spawn_server_on(listener, app);
    let base_url = format!("http://{}", addr);
    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "{}"
api_key = "test-api-key"
model = "test-model"
"#,
        base_url
    ))
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let database_url = format!(
        "sqlite://{}",
        dir.path().join("archive-failure.db").display()
    );
    let store = StateStore::connect(&database_url).await.unwrap();
    store.migrate().await.unwrap();
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset)
        .with_review_run_id("rr-archive-failure".into());

    let error = service
        .review_merge_request_note(&manual_note_event("@ai-review"))
        .await
        .unwrap_err();

    assert_eq!(
        error.review_failure().map(|failure| failure.code),
        Some(gitlab_work_runner::error::ReviewErrorCode::ArchiveDownloadFailed)
    );
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 0);
    let pool = sqlx::SqlitePool::connect(&database_url).await.unwrap();
    let row = sqlx::query("select status, execution_mode, context_elapsed_ms, coverage_total_files, coverage_complete from review_task_runs where review_run_id = ?")
        .bind("rr-archive-failure").fetch_one(&pool).await.unwrap();
    assert_eq!(row.get::<String, _>("status"), "failed");
    assert_eq!(row.get::<String, _>("execution_mode"), "context");
    assert!(row.get::<i64, _>("context_elapsed_ms") >= 0);
    assert!(row.get::<Option<i64>, _>("coverage_total_files").is_none());
    assert!(row.get::<Option<bool>, _>("coverage_complete").is_none());
    let files =
        sqlx::query("select count(*) as count from review_coverage_files where review_run_id = ?")
            .bind("rr-archive-failure")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(files.get::<i64, _>("count"), 0);
}

#[tokio::test]
async fn ai_review_timeout_does_not_block_merge_request_review() {
    let discussion_count = Arc::new(AtomicUsize::new(0));
    let discussion_count_for_handler = Arc::clone(&discussion_count);
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
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
                        "base_sha": "base",
                        "start_sha": "start",
                        "head_sha": "slow-ai-head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(|| async { test_archive() }),
        )
        .route(
            "/chat/completions",
            post(move || {
                let ai_request_count = Arc::clone(&ai_request_count_for_handler);
                async move {
                    ai_request_count.fetch_add(1, Ordering::SeqCst);
                    sleep(Duration::from_secs(2)).await;
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": serde_json::json!({
                                    "findings": [{
                                        "path": "src/lib.rs",
                                        "line": 1,
                                        "severity": "error",
                                        "title": "Late finding",
                                        "message": "This response should arrive too late."
                                    }]
                                }).to_string()
                            }
                        }]
                    }))
                }
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let message = body["body"].as_str().unwrap();
                    assert!(message.contains("GitLabWorkRunner Review"));
                    assert!(message.contains("**状态：** 部分失败"));
                    assert!(message.contains("- `ai-review` AI Review"));
                    assert!(!message.contains("### 降级执行"));
                    assert!(!message.contains("已使用 Diff-only 模式完成"));
                    assert!(message.contains("**Commit：** `abc123`"));
                    assert!(message.contains("gitlab-work-runner:summary run=rr-timeout"));
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

    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "{}"
api_key = "test-api-key"
model = "test-model"
timeout_seconds = 1
"#,
        base_url
    ))
    .unwrap();
    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset)
        .with_review_run_id("rr-timeout".into());
    let event = manual_note_event("@ai-review");

    let summary = service.review_merge_request_note(&event).await.unwrap();

    assert_eq!(summary.findings, 0);
    assert_eq!(summary.comments, 1);
    assert!(!summary.skipped);
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 2);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ai_review_timeout_covers_incomplete_response_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0_u8; 4096];
        let _ = stream.read(&mut buffer).await.unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 1000\r\n\r\n{\"choices\":[",
            )
            .await
            .unwrap();
        sleep(Duration::from_secs(2)).await;
    });
    let config = AiReviewConfig {
        base_url: format!("http://{}", addr),
        timeout_seconds: 1,
        ..test_ai_review_config(format!("http://{}", addr))
    };
    let changes = vec![GitLabChange {
        old_path: "src/lib.rs".into(),
        new_path: "src/lib.rs".into(),
        new_file: false,
        renamed_file: false,
        deleted_file: false,
        diff: "@@ -1 +1 @@\n+let value = maybe.unwrap();\n".into(),
    }];

    let result = run_ai_review(&config, &changes).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn manual_note_runs_ai_review() {
    let discussion_count = Arc::new(AtomicUsize::new(0));
    let discussion_count_for_handler = Arc::clone(&discussion_count);
    let emoji_count = Arc::new(AtomicUsize::new(0));
    let emoji_count_for_handler = Arc::clone(&emoji_count);
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
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
                        "base_sha": "base",
                        "start_sha": "start",
                        "head_sha": "manual-ai-head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(|| async { test_archive() }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/notes/987/award_emoji",
            post(move |Query(query): Query<HashMap<String, String>>| {
                let emoji_count = Arc::clone(&emoji_count_for_handler);
                async move {
                    assert_eq!(query.get("name").map(String::as_str), Some("eyes"));
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
            "/chat/completions",
            post(move |body: Bytes| {
                let ai_request_count = Arc::clone(&ai_request_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(body["model"], "manual-test-model");
                    ai_request_count.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": serde_json::json!({
                                    "findings": [{
                                        "path": "src/lib.rs",
                                        "line": 1,
                                        "severity": "warning",
                                        "title": "Manual AI finding",
                                        "message": "This manual trigger should publish."
                                    }]
                                }).to_string()
                            }
                        }]
                    }))
                }
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    assert!(body["body"].as_str().unwrap().contains("Manual AI finding"));
                    assert!(body["body"]
                        .as_str()
                        .unwrap()
                        .contains("gitlab-work-runner:rule=ai:ai-review"));
                    assert_eq!(body["position"]["new_line"], 1);
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

    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "{}"
api_key = "manual-test-api-key"
model = "manual-test-model"
timeout_seconds = 10
"#,
        base_url
    ))
    .unwrap();
    assert_eq!(ruleset.ai_reviews_by_ids(&["ai-review".into()]).len(), 1);
    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset);
    let event = MergeRequestNoteEvent {
        project_id: 123,
        project_name: None,
        project_path_with_namespace: None,
        mr_iid: 45,
        commit_sha: "event123".into(),
        action: "create".into(),
        note_id: 987,
        note: "please run @ai-review".into(),
    };

    let summary = service.review_merge_request_note(&event).await.unwrap();

    assert_eq!(summary.findings, 1);
    assert_eq!(summary.comments, 1);
    assert!(!summary.skipped);
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 1);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 1);
    assert_eq!(emoji_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn manual_note_posts_summary_when_one_ai_review_fails() {
    let discussion_count = Arc::new(AtomicUsize::new(0));
    let discussion_count_for_handler = Arc::clone(&discussion_count);
    let emoji_count = Arc::new(AtomicUsize::new(0));
    let emoji_count_for_handler = Arc::clone(&emoji_count);
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
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
                        "head_sha": "manual-partial-ai-head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(|| async { test_archive() }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/notes/989/award_emoji",
            post(move |Query(query): Query<HashMap<String, String>>| {
                let emoji_count = Arc::clone(&emoji_count_for_handler);
                async move {
                    assert_eq!(query.get("name").map(String::as_str), Some("eyes"));
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
            "/chat/completions",
            post(move |body: Bytes| {
                let ai_request_count = Arc::clone(&ai_request_count_for_handler);
                async move {
                    ai_request_count.fetch_add(1, Ordering::SeqCst);
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    match body["model"].as_str().unwrap() {
                        "manual-bad-model" => Json(json!({
                            "choices": [{
                                "message": {
                                    "content": "not json"
                                }
                            }]
                        })),
                        "manual-good-model" => Json(json!({
                            "choices": [{
                                "message": {
                                    "content": serde_json::json!({
                                        "findings": []
                                    }).to_string()
                                }
                            }]
                        })),
                        other => panic!("unexpected model {other}"),
                    }
                }
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let message = body["body"].as_str().unwrap();
                    assert!(message.contains("GitLabWorkRunner Review"));
                    assert!(message.contains("**状态：** 部分失败"));
                    assert!(message.contains("- `bad-review` Bad Review"));
                    assert!(message.contains("**Commit：** `event123`"));
                    assert!(message.contains("gitlab-work-runner:summary run=rr-partial-manual"));
                    assert!(body["position"].is_null());
                    discussion_count.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::CREATED,
                        Json(json!({
                            "id": "discussion-manual-partial",
                            "notes": [{ "id": 102 }]
                        })),
                    )
                }
            }),
        );
    spawn_server_on(listener, app);
    let base_url = format!("http://{}", addr);

    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[ai_reviews]]
id = "bad-review"
title = "Bad Review"
base_url = "{}"
api_key = "manual-test-api-key"
model = "manual-bad-model"
timeout_seconds = 10

[[ai_reviews]]
id = "good-review"
title = "Good Review"
base_url = "{}"
api_key = "manual-test-api-key"
model = "manual-good-model"
timeout_seconds = 10
"#,
        base_url, base_url
    ))
    .unwrap();
    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset)
        .with_review_run_id("rr-partial-manual".into());
    let event = MergeRequestNoteEvent {
        project_id: 123,
        project_name: None,
        project_path_with_namespace: None,
        mr_iid: 45,
        commit_sha: "event123".into(),
        action: "create".into(),
        note_id: 989,
        note: "please run @bad-review @good-review".into(),
    };

    let summary = service.review_merge_request_note(&event).await.unwrap();

    assert_eq!(summary.findings, 0);
    assert_eq!(summary.comments, 1);
    assert!(!summary.skipped);
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 2);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 1);
    assert_eq!(emoji_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn manual_note_posts_ai_review_completion_when_no_findings() {
    let discussion_count = Arc::new(AtomicUsize::new(0));
    let discussion_count_for_handler = Arc::clone(&discussion_count);
    let ai_request_count = Arc::new(AtomicUsize::new(0));
    let ai_request_count_for_handler = Arc::clone(&ai_request_count);
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
                        "head_sha": "manual-ai-clean-head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(|| async { test_archive() }),
        )
        .route(
            "/chat/completions",
            post(move |body: Bytes| {
                let ai_request_count = Arc::clone(&ai_request_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(body["model"], "manual-test-model");
                    ai_request_count.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": serde_json::json!({
                                    "findings": []
                                }).to_string()
                            }
                        }]
                    }))
                }
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    assert!(body["body"]
                        .as_str()
                        .unwrap()
                        .contains("GitLabWorkRunner Review"));
                    assert!(body["body"].as_str().unwrap().contains("**状态：** 完成"));
                    assert!(body["body"]
                        .as_str()
                        .unwrap()
                        .contains("未发现高置信度问题"));
                    assert!(body["body"]
                        .as_str()
                        .unwrap()
                        .contains("gitlab-work-runner:summary"));
                    assert!(body["position"].is_null());
                    discussion_count.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::CREATED,
                        Json(json!({
                            "id": "discussion-clean",
                            "notes": [{ "id": 100 }]
                        })),
                    )
                }
            }),
        );
    spawn_server_on(listener, app);
    let base_url = format!("http://{}", addr);

    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "{}"
api_key = "manual-test-api-key"
model = "manual-test-model"
timeout_seconds = 10
"#,
        base_url
    ))
    .unwrap();
    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let service = ReviewService::new(GitLabClient::new(base_url, "token".into()), store, ruleset);
    let event = MergeRequestNoteEvent {
        project_id: 123,
        project_name: None,
        project_path_with_namespace: None,
        mr_iid: 45,
        commit_sha: "event123".into(),
        action: "create".into(),
        note_id: 988,
        note: "please run @ai-review".into(),
    };

    let summary = service.review_merge_request_note(&event).await.unwrap();

    assert_eq!(summary.findings, 0);
    assert_eq!(summary.comments, 1);
    assert!(!summary.skipped);
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 1);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 1);
}
