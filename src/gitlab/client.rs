use crate::error::{AppError, AppResult, ReviewErrorCode};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    time::{Duration, Instant},
};
use tracing::{info, warn, Span};

const GITLAB_API_TIMEOUT_SECONDS: u64 = 30;

#[derive(Clone)]
pub struct GitLabClient {
    base_url: String,
    token: String,
    http: ureq::Agent,
    api_timeout: Duration,
    archive_timeout: Duration,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MergeRequestChanges {
    pub changes: Vec<GitLabChange>,
    pub diff_refs: DiffRefs,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GitLabChange {
    pub old_path: String,
    pub new_path: String,
    pub new_file: bool,
    pub renamed_file: bool,
    pub deleted_file: bool,
    pub diff: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DiffRefs {
    pub base_sha: Option<String>,
    pub start_sha: Option<String>,
    pub head_sha: Option<String>,
}

impl DiffRefs {
    pub fn is_complete(&self) -> bool {
        self.base_sha.is_some() && self.start_sha.is_some() && self.head_sha.is_some()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CreateDiscussionRequest {
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<DiscussionPosition>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiscussionPosition {
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
    pub position_type: String,
    pub old_path: String,
    pub new_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_line: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreatedDiscussion {
    pub id: String,
    #[serde(default)]
    pub notes: Vec<CreatedNote>,
    #[serde(default)]
    pub publish_position: PublishPosition,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreatedNote {
    pub id: i64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublishPosition {
    #[default]
    Inline,
    MergeRequest,
    MergeRequestFallback,
}

impl PublishPosition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::MergeRequest => "merge_request",
            Self::MergeRequestFallback => "merge_request_fallback",
        }
    }
}

impl GitLabClient {
    pub fn new(base_url: String, token: String) -> Self {
        let timeout = Duration::from_secs(GITLAB_API_TIMEOUT_SECONDS);
        Self::new_with_timeouts(base_url, token, timeout, timeout)
    }

    pub fn new_with_timeouts(
        base_url: String,
        token: String,
        api_timeout: Duration,
        archive_timeout: Duration,
    ) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            http: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout_read(longest_timeout(api_timeout, archive_timeout))
                .timeout_write(longest_timeout(api_timeout, archive_timeout))
                .build(),
            api_timeout,
            archive_timeout,
        }
    }

    #[cfg(test)]
    fn new_with_timeout_for_tests(base_url: String, token: String, timeout: Duration) -> Self {
        Self::new_with_timeouts(base_url, token, timeout, timeout)
    }

    pub async fn merge_request_changes(
        &self,
        project_id: i64,
        mr_iid: i64,
    ) -> AppResult<MergeRequestChanges> {
        info!(
            project_id,
            mr_iid,
            gitlab_base_url = %self.base_url,
            "preparing to fetch merge request changes from gitlab"
        );
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/changes",
            self.base_url, project_id, mr_iid
        );
        info!(
            project_id,
            mr_iid,
            request_url = %url,
            timeout_ms = self.api_timeout.as_millis(),
            "fetching merge request changes from gitlab"
        );
        let started = Instant::now();
        let http = self.http.clone();
        let token = self.token.clone();
        let timeout = self.api_timeout;
        let response = self
            .with_timeout_guard(
                "fetch merge request changes from gitlab",
                timeout,
                ReviewErrorCode::GitLabApiTimeout,
                ReviewErrorCode::GitLabApiFailed,
                move || {
                    let response = http
                        .get(&url)
                        .timeout(timeout)
                        .set("PRIVATE-TOKEN", &token)
                        .call();
                    let response = ensure_gitlab_success_response(
                        response_from_ureq_result(response)?,
                        "fetch merge request changes from gitlab",
                    )?;
                    let status = response.status();
                    let body = read_ureq_text(response)?;
                    let changes = serde_json::from_str(&body)?;
                    Ok((status, changes))
                },
            )
            .await?;
        let (status, changes): (u16, MergeRequestChanges) = response;
        info!(
            project_id,
            mr_iid,
            status,
            elapsed_ms = started.elapsed().as_millis(),
            "gitlab merge request changes response received"
        );
        info!(
            project_id,
            mr_iid,
            changed_files = changes.changes.len(),
            diff_refs_complete = changes.diff_refs.is_complete(),
            base_sha = ?changes.diff_refs.base_sha,
            start_sha = ?changes.diff_refs.start_sha,
            head_sha = ?changes.diff_refs.head_sha,
            "merge request changes fetched from gitlab"
        );
        Ok(changes)
    }

    pub async fn repository_archive(
        &self,
        project_id: i64,
        sha: &str,
        max_archive_bytes: usize,
    ) -> AppResult<Vec<u8>> {
        info!(
            project_id,
            sha,
            gitlab_base_url = %self.base_url,
            max_archive_bytes,
            "preparing to download repository archive from gitlab"
        );
        let url = format!(
            "{}/api/v4/projects/{}/repository/archive.zip",
            self.base_url, project_id
        );
        info!(
            project_id,
            sha,
            request_url = %url,
            timeout_ms = self.archive_timeout.as_millis(),
            max_archive_bytes,
            "downloading repository archive from gitlab"
        );
        let started = Instant::now();
        let http = self.http.clone();
        let token = self.token.clone();
        let timeout = self.archive_timeout;
        let sha = sha.to_string();
        let request_sha = sha.clone();
        let response = self
            .with_timeout_guard(
                "download repository archive from gitlab",
                timeout,
                ReviewErrorCode::ArchiveDownloadTimeout,
                ReviewErrorCode::ArchiveDownloadFailed,
                move || {
                    let response = http
                        .get(&url)
                        .query("sha", &request_sha)
                        .timeout(timeout)
                        .set("PRIVATE-TOKEN", &token)
                        .call();
                    let response = ensure_gitlab_success_response(
                        response_from_ureq_result(response)?,
                        "download repository archive from gitlab",
                    )?;
                    let status = response.status();
                    let archive = read_ureq_bytes_limited(response, max_archive_bytes)?;
                    Ok((status, archive))
                },
            )
            .await
            .map_err(reclassify_archive_download_failure)?;
        let (status, archive): (u16, Vec<u8>) = response;
        info!(
            project_id,
            sha,
            status,
            elapsed_ms = started.elapsed().as_millis(),
            "gitlab repository archive response received"
        );
        info!(
            project_id,
            sha,
            bytes = archive.len(),
            "repository archive downloaded from gitlab"
        );
        Ok(archive)
    }

    pub async fn create_discussion(
        &self,
        project_id: i64,
        mr_iid: i64,
        request: &CreateDiscussionRequest,
    ) -> AppResult<CreatedDiscussion> {
        info!(
            project_id,
            mr_iid,
            has_position = request.position.is_some(),
            "preparing to create gitlab merge request discussion"
        );
        let has_position = request.position.is_some();
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/discussions",
            self.base_url, project_id, mr_iid
        );
        info!(
            project_id,
            mr_iid,
            request_url = %url,
            has_position,
            timeout_ms = self.api_timeout.as_millis(),
            "creating gitlab merge request discussion"
        );
        let started = Instant::now();
        let http = self.http.clone();
        let token = self.token.clone();
        let timeout = self.api_timeout;
        let request = request.clone();
        let response = self
            .with_timeout_guard(
                "create gitlab merge request discussion",
                timeout,
                ReviewErrorCode::GitLabCommentFailed,
                ReviewErrorCode::GitLabCommentFailed,
                move || {
                    let body = serde_json::to_string(&request)?;
                    let response = http
                        .post(&url)
                        .timeout(timeout)
                        .set("PRIVATE-TOKEN", &token)
                        .set("content-type", "application/json")
                        .send_string(&body);
                    let response = response_from_ureq_result(response)?;
                    let status = response.status();
                    if status == 400 && request.position.is_some() {
                        let fallback = CreateDiscussionRequest {
                            body: request.body.clone(),
                            position: None,
                        };
                        let fallback_body = serde_json::to_string(&fallback)?;
                        let created = http
                            .post(&url)
                            .timeout(timeout)
                            .set("PRIVATE-TOKEN", &token)
                            .set("content-type", "application/json")
                            .send_string(&fallback_body);
                        let response = ensure_gitlab_success_response(
                            response_from_ureq_result(created)?,
                            "create fallback gitlab merge request discussion",
                        )?;
                        let body = read_ureq_text(response)?;
                        let created = serde_json::from_str(&body)?;
                        return Ok((status, Some(created)));
                    }
                    let response = ensure_gitlab_success_response(
                        response,
                        "create gitlab merge request discussion",
                    )?;
                    let body = read_ureq_text(response)?;
                    let created = serde_json::from_str(&body)?;
                    Ok((status, Some(created)))
                },
            )
            .await
            .map_err(reclassify_gitlab_comment_failure)?;
        let (status, created): (u16, Option<CreatedDiscussion>) = response;
        info!(
            project_id,
            mr_iid,
            status,
            has_position,
            elapsed_ms = started.elapsed().as_millis(),
            "gitlab create discussion response received"
        );
        if status == 400 && has_position {
            warn!(
                project_id,
                mr_iid,
                "line-level discussion was rejected by gitlab; falling back to merge-request-level discussion"
            );
            let mut created = created.expect("fallback discussion should be created");
            created.publish_position = PublishPosition::MergeRequestFallback;
            info!(
                project_id,
                mr_iid,
                discussion_id = %created.id,
                elapsed_ms = started.elapsed().as_millis(),
                "fallback merge-request-level discussion created"
            );
            return Ok(created);
        }
        let mut created = created.expect("discussion should be created");
        created.publish_position = if has_position {
            PublishPosition::Inline
        } else {
            PublishPosition::MergeRequest
        };
        info!(
            project_id,
            mr_iid,
            discussion_id = %created.id,
            "gitlab merge request discussion created"
        );
        Ok(created)
    }

    pub async fn award_merge_request_note_emoji(
        &self,
        project_id: i64,
        mr_iid: i64,
        note_id: i64,
        name: &str,
    ) -> AppResult<()> {
        info!(
            project_id,
            mr_iid,
            note_id,
            emoji_name = %name,
            "preparing to award gitlab merge request note emoji"
        );
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/notes/{}/award_emoji",
            self.base_url, project_id, mr_iid, note_id
        );
        info!(
            project_id,
            mr_iid,
            note_id,
            emoji_name = %name,
            request_url = %url,
            timeout_ms = self.api_timeout.as_millis(),
            "awarding gitlab merge request note emoji"
        );
        let started = Instant::now();
        let http = self.http.clone();
        let token = self.token.clone();
        let timeout = self.api_timeout;
        let name = name.to_string();
        let request_name = name.clone();
        let response = self
            .with_timeout_guard(
                "award gitlab merge request note emoji",
                timeout,
                ReviewErrorCode::GitLabCommentFailed,
                ReviewErrorCode::GitLabCommentFailed,
                move || {
                    let response = http
                        .post(&url)
                        .query("name", &request_name)
                        .timeout(timeout)
                        .set("PRIVATE-TOKEN", &token)
                        .call();
                    let response = response_from_ureq_result(response)?;
                    let status = response.status();
                    if status == 409 {
                        return Ok(status);
                    }
                    let _ = ensure_gitlab_success_response(
                        response,
                        "award gitlab merge request note emoji",
                    )?;
                    Ok(status)
                },
            )
            .await
            .map_err(reclassify_gitlab_comment_failure)?;
        info!(
            project_id,
            mr_iid,
            note_id,
            emoji_name = %name,
            status = response,
            elapsed_ms = started.elapsed().as_millis(),
            "gitlab merge request note emoji awarded"
        );
        Ok(())
    }

    async fn with_timeout_guard<T, F>(
        &self,
        operation: &'static str,
        timeout: Duration,
        timeout_code: ReviewErrorCode,
        failure_code: ReviewErrorCode,
        task: F,
    ) -> AppResult<T>
    where
        T: Send + 'static,
        F: FnOnce() -> AppResult<T> + Send + 'static,
    {
        let span = Span::current();
        let task = move || {
            let _entered = span.enter();
            task()
        };
        match tokio::time::timeout(timeout, tokio::task::spawn_blocking(task)).await {
            Ok(Ok(result)) => result,
            Ok(Err(err)) => Err(AppError::gitlab(
                failure_code,
                format!("{operation} blocking task failed: {err}"),
            )),
            Err(_) => Err(AppError::gitlab(
                timeout_code,
                format!("{operation} timed out after {} ms", timeout.as_millis()),
            )),
        }
    }
}

fn reclassify_archive_download_failure(error: AppError) -> AppError {
    match error.review_failure() {
        Some(failure) if failure.code == ReviewErrorCode::PermissionDenied => error,
        Some(failure) if failure.code == ReviewErrorCode::ArchiveLimitExceeded => error,
        Some(failure) if failure.code == ReviewErrorCode::ArchiveDownloadTimeout => error,
        Some(failure) => AppError::archive(
            ReviewErrorCode::ArchiveDownloadFailed,
            failure.message.clone(),
        ),
        None => AppError::archive(ReviewErrorCode::ArchiveDownloadFailed, error.to_string()),
    }
}

fn reclassify_gitlab_comment_failure(error: AppError) -> AppError {
    match error.review_failure() {
        Some(failure) if failure.code == ReviewErrorCode::PermissionDenied => error,
        Some(failure) => AppError::gitlab(
            ReviewErrorCode::GitLabCommentFailed,
            failure.message.clone(),
        ),
        None => AppError::gitlab(ReviewErrorCode::GitLabCommentFailed, error.to_string()),
    }
}

fn longest_timeout(first: Duration, second: Duration) -> Duration {
    first.max(second)
}

fn response_from_ureq_result(
    result: Result<ureq::Response, ureq::Error>,
) -> AppResult<ureq::Response> {
    match result {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(_, response)) => Ok(response),
        Err(err) => Err(AppError::gitlab(
            ReviewErrorCode::GitLabApiFailed,
            format!("GitLab HTTP request failed: {err}"),
        )),
    }
}

fn ensure_gitlab_success_response(
    response: ureq::Response,
    operation: &'static str,
) -> AppResult<ureq::Response> {
    let status = response.status();
    if (200..300).contains(&status) {
        return Ok(response);
    }
    let body = read_ureq_text(response).unwrap_or_else(|err| err.to_string());
    let code = if matches!(status, 401 | 403) {
        ReviewErrorCode::PermissionDenied
    } else {
        ReviewErrorCode::GitLabApiFailed
    };
    Err(AppError::gitlab(
        code,
        format!(
            "{operation} returned HTTP status {status}: {}",
            preview_log_text(&body, 500)
        ),
    ))
}

fn read_ureq_text(response: ureq::Response) -> AppResult<String> {
    Ok(response.into_string()?)
}

fn read_ureq_bytes_limited(response: ureq::Response, max_bytes: usize) -> AppResult<Vec<u8>> {
    let mut reader = response
        .into_reader()
        .take(max_bytes.saturating_add(1) as u64);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(AppError::archive(
            ReviewErrorCode::ArchiveLimitExceeded,
            format!("repository archive download exceeded max_archive_bytes {max_bytes}"),
        ));
    }
    Ok(bytes)
}

fn preview_log_text(value: &str, max_chars: usize) -> String {
    let mut preview = String::new();
    let mut truncated = false;
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            truncated = true;
            break;
        }
        match ch {
            '\n' => preview.push_str("\\n"),
            '\r' => preview.push_str("\\r"),
            '\t' => preview.push_str("\\t"),
            _ => preview.push(ch),
        }
    }
    if truncated {
        preview.push_str("...");
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Json, Router};
    use serde_json::json;
    use tokio::{
        io::AsyncReadExt,
        net::TcpListener,
        time::{sleep, Duration},
    };

    async fn spawn_server(app: Router) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{}", addr)
    }

    #[tokio::test]
    async fn fetches_merge_request_changes() {
        let app = Router::new().route(
            "/api/v4/projects/1/merge_requests/2/changes",
            get(|| async {
                Json(json!({
                    "changes": [{
                        "old_path": "src/lib.rs",
                        "new_path": "src/lib.rs",
                        "new_file": false,
                        "renamed_file": false,
                        "deleted_file": false,
                        "diff": "@@ -1 +1 @@\n+new\n"
                    }],
                    "diff_refs": {
                        "base_sha": "base",
                        "start_sha": "start",
                        "head_sha": "head"
                    }
                }))
            }),
        );
        let base_url = spawn_server(app).await;

        let client = GitLabClient::new(base_url, "token".into());
        let _: &ureq::Agent = &client.http;
        let changes = client.merge_request_changes(1, 2).await.unwrap();

        assert_eq!(changes.changes.len(), 1);
        assert_eq!(changes.diff_refs.head_sha.as_deref(), Some("head"));
    }

    #[tokio::test]
    async fn fetches_merge_request_changes_with_null_diff_refs() {
        let app = Router::new().route(
            "/api/v4/projects/1/merge_requests/2/changes",
            get(|| async {
                Json(json!({
                    "changes": [],
                    "diff_refs": {
                        "base_sha": null,
                        "start_sha": "start",
                        "head_sha": "head"
                    }
                }))
            }),
        );
        let base_url = spawn_server(app).await;

        let client = GitLabClient::new(base_url, "token".into());
        let changes = client.merge_request_changes(1, 2).await.unwrap();

        assert_eq!(changes.diff_refs.base_sha, None);
        assert_eq!(changes.diff_refs.start_sha.as_deref(), Some("start"));
        assert_eq!(changes.diff_refs.head_sha.as_deref(), Some("head"));
    }

    #[tokio::test]
    async fn merge_request_changes_times_out_when_gitlab_never_responds() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer).await;
            sleep(Duration::from_secs(5)).await;
        });

        let client = GitLabClient::new_with_timeout_for_tests(
            format!("http://{}", addr),
            "token".into(),
            Duration::from_millis(50),
        );
        let err = client.merge_request_changes(1, 2).await.unwrap_err();

        assert!(err.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn repository_archive_rejects_download_over_limit() {
        let app = Router::new().route(
            "/api/v4/projects/1/repository/archive.zip",
            get(|| async { vec![b'a'; 5] }),
        );
        let base_url = spawn_server(app).await;
        let client = GitLabClient::new(base_url, "token".into());

        let err = client.repository_archive(1, "abc", 4).await.unwrap_err();

        assert!(err.to_string().contains("max_archive_bytes"));
    }

    #[tokio::test]
    async fn repository_archive_uses_archive_timeout() {
        let app = Router::new().route(
            "/api/v4/projects/1/repository/archive.zip",
            get(|| async {
                sleep(Duration::from_millis(120)).await;
                vec![b'a'; 4]
            }),
        );
        let base_url = spawn_server(app).await;
        let client = GitLabClient::new_with_timeouts(
            base_url,
            "token".into(),
            Duration::from_millis(50),
            Duration::from_secs(2),
        );

        let archive = client.repository_archive(1, "abc", 4).await.unwrap();

        assert_eq!(archive.len(), 4);
    }

    #[tokio::test]
    async fn repository_archive_rejects_zero_byte_limit() {
        let app = Router::new().route(
            "/api/v4/projects/1/repository/archive.zip",
            get(|| async { vec![b'a'; 1] }),
        );
        let base_url = spawn_server(app).await;
        let client = GitLabClient::new(base_url, "token".into());

        let err = client.repository_archive(1, "abc", 0).await.unwrap_err();

        assert!(err.to_string().contains("max_archive_bytes"));
    }
}
