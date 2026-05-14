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
    ScriptTask(String),
    #[error("gitlab api error: {0}")]
    GitLab(String),
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

pub type AppResult<T> = Result<T, AppError>;
