use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    LockTimeout(String),
    #[error("{0}")]
    Integrity(String),
    #[error("{0}")]
    Storage(String),
    #[error("github auth: {0}")]
    GithubAuth(String),
    #[error("github api: {0}")]
    GithubApi(String),
    #[error("sync conflict: {0}")]
    SyncConflict(String),
    #[error("state conflict: expected `{0}`, was `{1}`")]
    StateConflict(String, String), // (expected, actual)
}

impl AppError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) | Self::Validation(_) => 2,
            Self::NotFound(_) => 3,
            Self::LockTimeout(_) => 4,
            Self::Integrity(_) | Self::Storage(_) => 5,
            Self::GithubAuth(_) => 6,
            Self::GithubApi(_) => 7,
            Self::SyncConflict(_) => 8,
            Self::StateConflict(..) => 9,
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(value: std::io::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(value: serde_json::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<toml::de::Error> for AppError {
    fn from(value: toml::de::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<toml::ser::Error> for AppError {
    fn from(value: toml::ser::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<serde_yml::Error> for AppError {
    fn from(value: serde_yml::Error) -> Self {
        Self::Storage(value.to_string())
    }
}
