use crate::error::{AppError, AppResult};
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeRequestEvent {
    pub project_id: i64,
    pub mr_iid: i64,
    pub commit_sha: String,
    pub action: String,
    pub source_branch: String,
    pub target_branch: String,
}

#[derive(Debug, Deserialize)]
struct GitLabWebhookPayload {
    object_kind: String,
    project: ProjectPayload,
    object_attributes: MergeRequestAttributes,
}

#[derive(Debug, Deserialize)]
struct ProjectPayload {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct MergeRequestAttributes {
    iid: i64,
    action: String,
    last_commit: LastCommit,
    source_branch: String,
    target_branch: String,
}

#[derive(Debug, Deserialize)]
struct LastCommit {
    id: String,
}

pub fn validate_token(expected: &str, actual: Option<&str>) -> AppResult<()> {
    match actual {
        Some(value) if value == expected => Ok(()),
        _ => Err(AppError::Webhook("invalid X-Gitlab-Token".into())),
    }
}

pub fn parse_merge_request_event(body: &[u8]) -> AppResult<Option<MergeRequestEvent>> {
    let payload: GitLabWebhookPayload = serde_json::from_slice(body)?;
    if payload.object_kind != "merge_request" {
        return Ok(None);
    }
    Ok(Some(MergeRequestEvent {
        project_id: payload.project.id,
        mr_iid: payload.object_attributes.iid,
        commit_sha: payload.object_attributes.last_commit.id,
        action: payload.object_attributes.action,
        source_branch: payload.object_attributes.source_branch,
        target_branch: payload.object_attributes.target_branch,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_secret_token() {
        assert!(validate_token("secret", Some("secret")).is_ok());
        assert!(validate_token("secret", Some("wrong")).is_err());
        assert!(validate_token("secret", None).is_err());
    }

    #[test]
    fn parses_merge_request_event() {
        let body = include_bytes!("../tests/fixtures/gitlab_mr_event.json");
        let event = parse_merge_request_event(body).unwrap().unwrap();

        assert_eq!(event.project_id, 123);
        assert_eq!(event.mr_iid, 45);
        assert_eq!(event.commit_sha, "abc123");
        assert_eq!(event.source_branch, "feature/review");
        assert_eq!(event.target_branch, "main");
    }
}
