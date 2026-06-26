use axum::{
    body::Bytes,
    extract::Query,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use gitlab_work_runner::{
    gitlab::GitLabClient,
    review::ReviewService,
    rules::Ruleset,
    storage::StateStore,
    webhook::{MergeRequestEvent, MergeRequestNoteEvent},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
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
        zip.start_file("repo-head/check-issue.cmd", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            br#"@echo src\lib.rs:1: TODO found>"%~2"
@exit /B 1"#,
        )
        .unwrap();
        zip.start_file("repo-head/check-issue.sh", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            br#"echo "src/lib.rs:1: TODO found" > "$2"
exit 1"#,
        )
        .unwrap();
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
async fn runs_script_task_without_posting_comment_when_it_fails() {
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
                        "head_sha": "head123"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(move |Query(query): Query<HashMap<String, String>>| {
                let archive = Arc::clone(&archive_for_handler);
                async move {
                    assert_eq!(query.get("sha").map(String::as_str), Some("head123"));
                    archive.as_ref().clone().into_response()
                }
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move || {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    discussion_count.fetch_add(1, Ordering::SeqCst);
                    StatusCode::INTERNAL_SERVER_ERROR
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
        commit_sha: "event123".into(),
        action: "update".into(),
        source_branch: "feature/review".into(),
        target_branch: "main".into(),
    };

    let summary = service.review_merge_request(&event).await.unwrap();

    assert_eq!(summary.findings, 0);
    assert_eq!(summary.comments, 0);
    assert!(!summary.skipped);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn reviews_merge_request_with_ai_review() {
    std::env::set_var("AI_REVIEW_TEST_TOKEN", "test-token");
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
provider = "openai-compatible"
base_url = "{}"
api_key_env = "AI_REVIEW_TEST_TOKEN"
model = "test-model"
timeout_seconds = 10
when_changed = ["src/**"]
"#,
        base_url
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
        commit_sha: "event123".into(),
        action: "update".into(),
        source_branch: "feature/review".into(),
        target_branch: "main".into(),
    };

    let summary = service.review_merge_request(&event).await.unwrap();

    assert_eq!(summary.findings, 1);
    assert_eq!(summary.comments, 1);
    assert!(!summary.skipped);
    assert_eq!(ai_request_count.load(Ordering::SeqCst), 1);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn manual_note_runs_ai_review() {
    std::env::set_var("AI_REVIEW_MANUAL_TEST_TOKEN", "test-token");
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
                        "head_sha": "manual-ai-head"
                    }
                }))
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
enabled = false
id = "ai-review"
title = "AI Review"
provider = "openai-compatible"
base_url = "{}"
api_key_env = "AI_REVIEW_MANUAL_TEST_TOKEN"
model = "manual-test-model"
trigger = "manual"
timeout_seconds = 10
when_changed = ["does-not-match/**"]
"#,
        base_url
    ))
    .unwrap();
    assert!(ruleset
        .ai_reviews_for_changes(&["src/lib.rs".into()])
        .is_empty());
    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let service = ReviewService::new(
        GitLabClient::new(base_url, "token".into()),
        store,
        ruleset,
        "GITLAB_TOKEN".into(),
    );
    let event = MergeRequestNoteEvent {
        project_id: 123,
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
}

#[tokio::test]
async fn posts_line_comment_when_script_task_finds_issue() {
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
                        "head_sha": "script-head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(move |Query(query): Query<HashMap<String, String>>| {
                let archive = Arc::clone(&archive_for_handler);
                async move {
                    assert_eq!(query.get("sha").map(String::as_str), Some("script-head"));
                    archive.as_ref().clone().into_response()
                }
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    assert!(body["body"].as_str().unwrap().contains("TODO found"));
                    assert!(body["body"]
                        .as_str()
                        .unwrap()
                        .contains("gitlab-work-runner:rule=script:comment-script"));
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

    let script_path = std::env::current_dir()
        .unwrap()
        .join("work/script_tasks/123/45/script-head/comment-script/source")
        .join(if cfg!(windows) {
            "check-issue.cmd"
        } else {
            "check-issue.sh"
        });
    let command = if cfg!(windows) {
        script_path.display().to_string()
    } else {
        format!("sh \"{}\"", script_path.display())
    };
    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[script_tasks]]
id = "comment-script"
title = "TODO marker check"
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
        commit_sha: "event123".into(),
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

#[tokio::test]
async fn manual_note_runs_disabled_script_task() {
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
                        "head_sha": "manual-head"
                    }
                }))
            }),
        )
        .route(
            "/api/v4/projects/123/repository/archive.zip",
            get(move |Query(query): Query<HashMap<String, String>>| {
                let archive = Arc::clone(&archive_for_handler);
                async move {
                    assert_eq!(query.get("sha").map(String::as_str), Some("manual-head"));
                    archive.as_ref().clone().into_response()
                }
            }),
        )
        .route(
            "/api/v4/projects/123/merge_requests/45/discussions",
            post(move |body: Bytes| {
                let discussion_count = Arc::clone(&discussion_count_for_handler);
                async move {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    assert!(body["body"].as_str().unwrap().contains("TODO found"));
                    assert!(body["body"]
                        .as_str()
                        .unwrap()
                        .contains("gitlab-work-runner:rule=script:manual-script"));
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

    let script_path = std::env::current_dir()
        .unwrap()
        .join("work/script_tasks/123/45/manual-head/manual-script/source")
        .join(if cfg!(windows) {
            "check-issue.cmd"
        } else {
            "check-issue.sh"
        });
    let command = if cfg!(windows) {
        script_path.display().to_string()
    } else {
        format!("sh \"{}\"", script_path.display())
    };
    let ruleset = Ruleset::from_toml(&format!(
        r#"
[[script_tasks]]
enabled = false
id = "manual-script"
title = "Manual TODO marker check"
command = "{}"
timeout_seconds = 10
when_changed = ["does-not-match/**"]
"#,
        command.replace('\\', "\\\\").replace('"', "\\\"")
    ))
    .unwrap();
    assert!(ruleset
        .script_tasks_for_changes(&["src/lib.rs".into()])
        .is_empty());
    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let service = ReviewService::new(
        GitLabClient::new(base_url, "token".into()),
        store,
        ruleset,
        "GITLAB_TOKEN".into(),
    );
    let event = MergeRequestNoteEvent {
        project_id: 123,
        mr_iid: 45,
        commit_sha: "event123".into(),
        action: "create".into(),
        note_id: 987,
        note: "please run @manual-script".into(),
    };

    let summary = service.review_merge_request_note(&event).await.unwrap();

    assert_eq!(summary.findings, 0);
    assert_eq!(summary.comments, 1);
    assert!(!summary.skipped);
    assert_eq!(discussion_count.load(Ordering::SeqCst), 1);
}
