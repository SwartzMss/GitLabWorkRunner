use super::*;
use crate::{
    app::config::{
        DashboardConfig, GitLabConfig, LoggingConfig, RulesConfig, ServerConfig, StorageConfig,
    },
    storage::StateStore,
};
use axum::body::to_bytes;
use axum::http::HeaderValue;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::sync::Notify;

async fn test_state(
    gitlab_base_url: String,
    rules_file: &NamedTempFile,
    max_concurrent_reviews: usize,
) -> Arc<AppState> {
    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    Arc::new(AppState {
        config: AppConfig {
            server: ServerConfig {
                bind: "127.0.0.1:0".into(),
                webhook_secret: "secret".into(),
                max_concurrent_reviews,
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
            archive: Default::default(),
            dashboard: DashboardConfig::default(),
        },
        store,
        active_reviews: Arc::new(ActiveReviews::default()),
    })
}

#[tokio::test]
async fn merge_request_webhook_is_ignored_without_review_work() {
    let rules_file = NamedTempFile::new().unwrap();
    std::fs::write(rules_file.path(), "").unwrap();

    let state = test_state("http://127.0.0.1:1".into(), &rules_file, 4).await;
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", HeaderValue::from_static("secret"));
    let body = Bytes::from_static(include_bytes!("../../tests/fixtures/gitlab_mr_event.json"));

    let response = gitlab_webhook(State(state), headers, body)
        .await
        .into_response();

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["accepted"], false);
    assert_eq!(body["reason"], "merge_request_events_manual_triggers_only");
    assert!(!body["review_run_id"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn non_create_merge_request_note_is_ignored_even_with_manual_command() {
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
"#,
            command.replace('\\', "\\\\").replace('"', "\\\"")
        ),
    )
    .unwrap();

    let state = test_state("http://127.0.0.1:1".into(), &rules_file, 4).await;
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", HeaderValue::from_static("secret"));

    let response = gitlab_webhook(
        State(state),
        headers,
        Bytes::from_static(
            br#"{
                "object_kind": "note",
                "project_id": 123,
                "object_attributes": {
                    "id": 987,
                    "note": "@check",
                    "noteable_type": "MergeRequest",
                    "action": "delete"
                },
                "merge_request": {
                    "iid": 45,
                    "last_commit": { "id": "abc123" }
                }
            }"#,
        ),
    )
    .await
    .into_response();

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["accepted"], false);
    assert_eq!(body["reason"], "no_matching_manual_review");
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

    let state = test_state(gitlab_base_url, &rules_file, 4).await;
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", HeaderValue::from_static("secret"));

    let first = gitlab_webhook(
        State(Arc::clone(&state)),
        headers.clone(),
        Bytes::from_static(
            br#"{
                "object_kind": "note",
                "project_id": 123,
                "object_attributes": {
                    "id": 986,
                    "note": "@check",
                    "noteable_type": "MergeRequest",
                    "action": "create"
                },
                "merge_request": {
                    "iid": 45,
                    "last_commit": { "id": "abc123" }
                }
            }"#,
        ),
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

#[tokio::test]
async fn busy_review_queue_note_gets_acknowledgement_comment() {
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
"#,
            command.replace('\\', "\\\\").replace('"', "\\\"")
        ),
    )
    .unwrap();

    let change_count = Arc::new(AtomicUsize::new(0));
    let emoji_count = Arc::new(AtomicUsize::new(0));
    let busy_comment_count = Arc::new(AtomicUsize::new(0));
    let change_started = Arc::new(Notify::new());
    let release_change = Arc::new(Notify::new());

    let change_count_for_handler = Arc::clone(&change_count);
    let change_started_for_handler = Arc::clone(&change_started);
    let release_change_for_handler = Arc::clone(&release_change);
    let emoji_count_for_handler = Arc::clone(&emoji_count);
    let busy_comment_count_for_handler = Arc::clone(&busy_comment_count);
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
            "/api/v4/projects/123/merge_requests/45/notes/988/award_emoji",
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
                let busy_comment_count = Arc::clone(&busy_comment_count_for_handler);
                async move {
                    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let message = body["body"].as_str().unwrap();
                    assert!(message.contains("review 队列繁忙"));
                    assert!(message.contains("active_count: `1`"));
                    assert!(message.contains("max_concurrent_reviews: `1`"));
                    assert!(message.contains("gitlab-work-runner:review-queue-busy"));
                    assert!(body.get("position").is_none());
                    busy_comment_count.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::CREATED,
                        Json(json!({
                            "id": "review-queue-busy",
                            "notes": [{ "id": 997 }]
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

    let state = test_state(gitlab_base_url, &rules_file, 1).await;
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", HeaderValue::from_static("secret"));

    let first = gitlab_webhook(
        State(Arc::clone(&state)),
        headers.clone(),
        Bytes::from_static(
            br#"{
                "object_kind": "note",
                "project_id": 123,
                "object_attributes": {
                    "id": 986,
                    "note": "@check",
                    "noteable_type": "MergeRequest",
                    "action": "create"
                },
                "merge_request": {
                    "iid": 45,
                    "last_commit": { "id": "abc123" }
                }
            }"#,
        ),
    )
    .await
    .into_response();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    change_started.notified().await;

    let busy_note = Bytes::from_static(
        br#"{
                "object_kind": "note",
                "project_id": 123,
                "object_attributes": {
                    "id": 988,
                    "note": "@check",
                    "noteable_type": "MergeRequest",
                    "action": "create"
                },
                "merge_request": {
                    "iid": 45,
                    "last_commit": { "id": "def456" }
                }
            }"#,
    );
    let busy = gitlab_webhook(State(state), headers, busy_note)
        .await
        .into_response();

    assert_eq!(busy.status(), StatusCode::ACCEPTED);
    let body = to_bytes(busy.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["accepted"], false);
    assert_eq!(body["reason"], "review_queue_busy");
    assert_eq!(body["active_count"], 1);
    assert_eq!(body["max_concurrent_reviews"], 1);

    for _ in 0..20 {
        if busy_comment_count.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(change_count.load(Ordering::SeqCst), 1);
    assert_eq!(emoji_count.load(Ordering::SeqCst), 1);
    assert_eq!(busy_comment_count.load(Ordering::SeqCst), 1);

    release_change.notify_waiters();
}

#[tokio::test]
async fn merge_request_webhook_is_ignored_when_review_queue_is_busy() {
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
"#,
            command.replace('\\', "\\\\").replace('"', "\\\"")
        ),
    )
    .unwrap();

    let change_count = Arc::new(AtomicUsize::new(0));
    let busy_comment_count = Arc::new(AtomicUsize::new(0));
    let change_started = Arc::new(Notify::new());
    let release_change = Arc::new(Notify::new());

    let change_count_for_handler = Arc::clone(&change_count);
    let change_started_for_handler = Arc::clone(&change_started);
    let release_change_for_handler = Arc::clone(&release_change);
    let app = Router::new().route(
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
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gitlab_base_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let state = test_state(gitlab_base_url, &rules_file, 1).await;
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", HeaderValue::from_static("secret"));

    let first = gitlab_webhook(
        State(Arc::clone(&state)),
        headers.clone(),
        Bytes::from_static(
            br#"{
                "object_kind": "note",
                "project_id": 123,
                "object_attributes": {
                    "id": 986,
                    "note": "@check",
                    "noteable_type": "MergeRequest",
                    "action": "create"
                },
                "merge_request": {
                    "iid": 45,
                    "last_commit": { "id": "abc123" }
                }
            }"#,
        ),
    )
    .await
    .into_response();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    change_started.notified().await;

    let busy = gitlab_webhook(
        State(state),
        headers,
        Bytes::from_static(
            br#"{
                    "object_kind": "merge_request",
                    "project": { "id": 123 },
                    "object_attributes": {
                        "iid": 45,
                        "action": "update",
                        "last_commit": { "id": "def456" },
                        "source_branch": "feature/review",
                        "target_branch": "main"
                    }
                }"#,
        ),
    )
    .await
    .into_response();

    assert_eq!(busy.status(), StatusCode::ACCEPTED);
    let body = to_bytes(busy.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["accepted"], false);
    assert_eq!(body["reason"], "merge_request_events_manual_triggers_only");

    assert_eq!(change_count.load(Ordering::SeqCst), 1);
    assert_eq!(busy_comment_count.load(Ordering::SeqCst), 0);

    release_change.notify_waiters();
}

#[tokio::test]
async fn failed_webhook_review_posts_failure_comment() {
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
"#,
            command.replace('\\', "\\\\").replace('"', "\\\"")
        ),
    )
    .unwrap();

    let failure_comment_count = Arc::new(AtomicUsize::new(0));
    let failure_comment_count_for_handler = Arc::clone(&failure_comment_count);
    let app = Router::new()
        .route(
            "/api/v4/projects/123/merge_requests/45/changes",
            get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let failure_comment_count = Arc::clone(&failure_comment_count_for_handler);
                async move {
                    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let message = body["body"].as_str().unwrap();
                    assert!(message.contains("Review 执行失败"));
                    assert!(message.contains("review_run_id"));
                    assert!(message.contains("abc123"));
                    assert!(body.get("position").is_none());
                    failure_comment_count.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::CREATED,
                        Json(json!({
                            "id": "review-failed",
                            "notes": [{ "id": 999 }]
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

    let state = test_state(gitlab_base_url, &rules_file, 4).await;
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", HeaderValue::from_static("secret"));

    let response = gitlab_webhook(
        State(state),
        headers,
        Bytes::from_static(
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
        ),
    )
    .await
    .into_response();

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    for _ in 0..20 {
        if failure_comment_count.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(failure_comment_count.load(Ordering::SeqCst), 1);
}
