use crate::error::{AppError, AppResult};
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitLabWebhookEvent {
    MergeRequest(MergeRequestEvent),
    MergeRequestNote(MergeRequestNoteEvent),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeRequestEvent {
    pub project_id: i64,
    pub mr_iid: i64,
    pub commit_sha: String,
    pub action: String,
    pub source_branch: String,
    pub target_branch: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeRequestNoteEvent {
    pub project_id: i64,
    pub mr_iid: i64,
    pub commit_sha: String,
    pub action: String,
    pub note_id: i64,
    pub note: String,
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

#[derive(Debug, Deserialize)]
struct GitLabObjectKind {
    object_kind: String,
}

#[derive(Debug, Deserialize)]
struct GitLabNotePayload {
    #[serde(default)]
    project_id: Option<i64>,
    #[serde(default)]
    project: Option<ProjectPayload>,
    object_attributes: NoteAttributes,
    #[serde(default)]
    merge_request: Option<NoteMergeRequestPayload>,
}

#[derive(Debug, Deserialize)]
struct NoteAttributes {
    id: i64,
    note: String,
    noteable_type: String,
    #[serde(default)]
    action: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NoteMergeRequestPayload {
    iid: i64,
    #[serde(default)]
    last_commit: Option<LastCommit>,
}

pub fn validate_token(expected: &str, actual: Option<&str>) -> AppResult<()> {
    match actual {
        Some(value) if value == expected => Ok(()),
        _ => Err(AppError::Webhook("invalid X-Gitlab-Token".into())),
    }
}

pub fn parse_gitlab_webhook_event(body: &[u8]) -> AppResult<Option<GitLabWebhookEvent>> {
    let object: GitLabObjectKind = serde_json::from_slice(body)?;
    match object.object_kind.as_str() {
        "merge_request" => {
            parse_merge_request_event(body).map(|event| event.map(GitLabWebhookEvent::MergeRequest))
        }
        "note" => parse_merge_request_note_event(body)
            .map(|event| event.map(GitLabWebhookEvent::MergeRequestNote)),
        _ => Ok(None),
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

pub fn parse_merge_request_note_event(body: &[u8]) -> AppResult<Option<MergeRequestNoteEvent>> {
    let payload: GitLabNotePayload = serde_json::from_slice(body)?;
    if !is_merge_request_note(&payload.object_attributes.noteable_type) {
        return Ok(None);
    }
    let Some(merge_request) = payload.merge_request else {
        return Ok(None);
    };
    let project_id = payload
        .project
        .map(|project| project.id)
        .or(payload.project_id)
        .ok_or_else(|| AppError::Webhook("note hook missing project id".into()))?;
    let commit_sha = merge_request
        .last_commit
        .map(|commit| commit.id)
        .unwrap_or_default();
    Ok(Some(MergeRequestNoteEvent {
        project_id,
        mr_iid: merge_request.iid,
        commit_sha,
        action: payload.object_attributes.action.unwrap_or_default(),
        note_id: payload.object_attributes.id,
        note: payload.object_attributes.note,
    }))
}

fn is_merge_request_note(noteable_type: &str) -> bool {
    matches!(noteable_type, "MergeRequest" | "merge_request")
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

    #[test]
    fn parses_merge_request_note_event() {
        let body = br#"{
            "object_kind": "note",
            "project_id": 123,
            "object_attributes": {
                "id": 987,
                "note": "@check-todo-tbd",
                "noteable_type": "MergeRequest",
                "action": "create"
            },
            "merge_request": {
                "iid": 45,
                "last_commit": { "id": "abc123" }
            }
        }"#;

        let event = parse_gitlab_webhook_event(body).unwrap().unwrap();

        assert_eq!(
            event,
            GitLabWebhookEvent::MergeRequestNote(MergeRequestNoteEvent {
                project_id: 123,
                mr_iid: 45,
                commit_sha: "abc123".into(),
                action: "create".into(),
                note_id: 987,
                note: "@check-todo-tbd".into(),
            })
        );
    }

    #[test]
    fn ignores_non_merge_request_note_event() {
        let body = br#"{
            "object_kind": "note",
            "project_id": 123,
            "object_attributes": {
                "id": 987,
                "note": "@check-todo-tbd",
                "noteable_type": "Issue",
                "action": "create"
            }
        }"#;

        assert!(parse_gitlab_webhook_event(body).unwrap().is_none());
    }
}
