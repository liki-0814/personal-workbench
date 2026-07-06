use thiserror::Error;

pub type Result<T> = std::result::Result<T, PwError>;

#[derive(Debug, Error)]
pub enum PwError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("invalid skill at {path}: {message}")]
    InvalidSkill { path: String, message: String },

    #[error("tool not found: {0}")]
    ToolNotFound(String),

    #[error("tool execution failed: {0}")]
    ToolExecution(String),

    #[error("policy denied tool call: {0}")]
    PolicyDenied(String),

    #[error("{0}")]
    Message(String),
}
