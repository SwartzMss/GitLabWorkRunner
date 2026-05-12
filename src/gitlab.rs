use crate::error::AppResult;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

#[derive(Clone)]
pub struct GitLabClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
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
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreatedNote {
    pub id: i64,
}

impl GitLabClient {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::new(),
        }
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
            "fetching merge request changes from gitlab"
        );
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/changes",
            self.base_url, project_id, mr_iid
        );
        let response = self
            .http
            .get(url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await?
            .error_for_status()?;
        let changes = response.json().await?;
        info!(
            project_id,
            mr_iid, "merge request changes fetched from gitlab"
        );
        Ok(changes)
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
            "creating gitlab merge request discussion"
        );
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/discussions",
            self.base_url, project_id, mr_iid
        );
        let response = self
            .http
            .post(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .json(request)
            .send()
            .await?;
        if response.status() == StatusCode::BAD_REQUEST && request.position.is_some() {
            warn!(
                project_id,
                mr_iid,
                "line-level discussion was rejected by gitlab; falling back to merge-request-level discussion"
            );
            let fallback = CreateDiscussionRequest {
                body: request.body.clone(),
                position: None,
            };
            let created: CreatedDiscussion = self
                .http
                .post(url)
                .header("PRIVATE-TOKEN", &self.token)
                .json(&fallback)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            info!(
                project_id,
                mr_iid,
                discussion_id = %created.id,
                "fallback merge-request-level discussion created"
            );
            return Ok(created);
        }
        let created: CreatedDiscussion = response.error_for_status()?.json().await?;
        info!(
            project_id,
            mr_iid,
            discussion_id = %created.id,
            "gitlab merge request discussion created"
        );
        Ok(created)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Json, Router};
    use serde_json::json;
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
}
