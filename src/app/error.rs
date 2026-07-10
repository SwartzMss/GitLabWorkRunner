#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewErrorCode {
    GitLabApiTimeout,
    GitLabApiFailed,
    ArchiveDownloadTimeout,
    ArchiveDownloadFailed,
    ArchiveExtractFailed,
    ArchiveLimitExceeded,
    AiRequestTimeout,
    AiRequestFailed,
    AiToolLoopTimeout,
    AiResponseParseFailed,
    ReviewRunTimeout,
    GitLabCommentFailed,
    PermissionDenied,
    InvalidConfiguration,
    ScriptTaskFailed,
    Internal,
}

impl ReviewErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GitLabApiTimeout => "gitlab_api_timeout",
            Self::GitLabApiFailed => "gitlab_api_failed",
            Self::ArchiveDownloadTimeout => "archive_download_timeout",
            Self::ArchiveDownloadFailed => "archive_download_failed",
            Self::ArchiveExtractFailed => "archive_extract_failed",
            Self::ArchiveLimitExceeded => "archive_limit_exceeded",
            Self::AiRequestTimeout => "ai_request_timeout",
            Self::AiRequestFailed => "ai_request_failed",
            Self::AiToolLoopTimeout => "ai_tool_loop_timeout",
            Self::AiResponseParseFailed => "ai_response_parse_failed",
            Self::ReviewRunTimeout => "review_run_timeout",
            Self::GitLabCommentFailed => "gitlab_comment_failed",
            Self::PermissionDenied => "permission_denied",
            Self::InvalidConfiguration => "invalid_configuration",
            Self::ScriptTaskFailed => "script_task_failed",
            Self::Internal => "internal",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewFailure {
    pub code: ReviewErrorCode,
    pub message: String,
}

impl ReviewFailure {
    pub fn new(code: ReviewErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ReviewFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("invalid webhook payload: {0}")]
    Webhook(String),
    #[error("diff parse error: {0}")]
    Diff(String),
    #[error("rule error: {0}")]
    Rule(String),
    #[error("script task error: {0}")]
    ScriptTask(ReviewFailure),
    #[error("ai review error: {0}")]
    AiReview(ReviewFailure),
    #[error("gitlab api error: {0}")]
    GitLab(ReviewFailure),
    #[error("archive error: {0}")]
    Archive(ReviewFailure),
    #[error("storage error: {0}")]
    Storage(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    TomlDe(#[from] toml::de::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

impl AppError {
    pub fn ai_review(code: ReviewErrorCode, message: impl Into<String>) -> Self {
        Self::AiReview(ReviewFailure::new(code, message))
    }

    pub fn gitlab(code: ReviewErrorCode, message: impl Into<String>) -> Self {
        Self::GitLab(ReviewFailure::new(code, message))
    }

    pub fn archive(code: ReviewErrorCode, message: impl Into<String>) -> Self {
        Self::Archive(ReviewFailure::new(code, message))
    }

    pub fn script_task(code: ReviewErrorCode, message: impl Into<String>) -> Self {
        Self::ScriptTask(ReviewFailure::new(code, message))
    }

    pub fn review_failure(&self) -> Option<&ReviewFailure> {
        match self {
            Self::AiReview(failure)
            | Self::GitLab(failure)
            | Self::Archive(failure)
            | Self::ScriptTask(failure) => Some(failure),
            _ => None,
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_error_codes_are_stable_snake_case_values() {
        assert_eq!(
            ReviewErrorCode::GitLabApiTimeout.as_str(),
            "gitlab_api_timeout"
        );
        assert_eq!(
            ReviewErrorCode::ArchiveLimitExceeded.as_str(),
            "archive_limit_exceeded"
        );
        assert_eq!(
            ReviewErrorCode::AiToolLoopTimeout.as_str(),
            "ai_tool_loop_timeout"
        );
        assert_eq!(
            ReviewErrorCode::AiResponseParseFailed.as_str(),
            "ai_response_parse_failed"
        );
        assert_eq!(
            ReviewErrorCode::ReviewRunTimeout.as_str(),
            "review_run_timeout"
        );
    }

    #[test]
    fn review_failure_preserves_code_and_displays_message() {
        let failure = ReviewFailure::new(ReviewErrorCode::AiRequestTimeout, "request timed out");

        assert_eq!(failure.code, ReviewErrorCode::AiRequestTimeout);
        assert_eq!(failure.to_string(), "request timed out");
    }

    #[test]
    fn app_error_exposes_only_structured_review_failures() {
        let error = AppError::ai_review(ReviewErrorCode::AiRequestFailed, "upstream unavailable");

        assert_eq!(
            error.review_failure().map(|failure| failure.code),
            Some(ReviewErrorCode::AiRequestFailed)
        );
        assert!(AppError::Storage("db unavailable".into())
            .review_failure()
            .is_none());
    }
}
