use serde::{Deserialize, Serialize};
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

/// The wire form of [`AppError`] — a 1:1 mirror that carries the *variant*
/// and its structured payload, not a flattened string.
///
/// Why a mirror instead of `#[derive(Serialize, Deserialize)]` on `AppError`
/// itself: `AppError` is a `thiserror` type whose `Display` impls are part of
/// the CLI's user-facing contract, and whose `From` conversions pull in
/// foreign error types (`std::io`, `rusqlite`, `serde_json`) that are not and
/// should not become serializable. Deriving on it would either freeze that
/// enum's shape as a public data format or force `#[serde(skip)]` holes into
/// the error path. The mirror keeps the two concerns apart and makes the
/// conversion a place where the compiler can enforce completeness.
///
/// What survives the hop, and how it is guaranteed:
/// - **variant** — [`From<&AppError>`] is an exhaustive `match`, so an
///   eleventh `AppError` variant stops this file compiling until it is given
///   a wire form.
/// - **fields** — each variant carries its own payload by name, so
///   `StateConflict { expected, actual }` arrives as two strings rather than
///   as prose a receiver would have to parse back apart.
/// - **message and exit code** — deliberately *not* transported. They are
///   recomputed by [`AppError::to_string`] and [`AppError::exit_code`] after
///   the round trip, which makes disagreement between a transported copy and
///   the real value structurally impossible. `tests/wire_envelope.rs` proves
///   the recomputation is exact for every variant.
///
/// The `detail` field is the variant's payload, never its rendered message:
/// `GithubAuth`'s `Display` is `github auth: {0}`, so reconstructing it from
/// the message would double the prefix on every hop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireError {
    Usage { detail: String },
    Validation { detail: String },
    NotFound { detail: String },
    LockTimeout { detail: String },
    Integrity { detail: String },
    Storage { detail: String },
    GithubAuth { detail: String },
    GithubApi { detail: String },
    SyncConflict { detail: String },
    StateConflict { expected: String, actual: String },
}

impl From<&AppError> for WireError {
    fn from(error: &AppError) -> Self {
        match error {
            AppError::Usage(detail) => Self::Usage {
                detail: detail.clone(),
            },
            AppError::Validation(detail) => Self::Validation {
                detail: detail.clone(),
            },
            AppError::NotFound(detail) => Self::NotFound {
                detail: detail.clone(),
            },
            AppError::LockTimeout(detail) => Self::LockTimeout {
                detail: detail.clone(),
            },
            AppError::Integrity(detail) => Self::Integrity {
                detail: detail.clone(),
            },
            AppError::Storage(detail) => Self::Storage {
                detail: detail.clone(),
            },
            AppError::GithubAuth(detail) => Self::GithubAuth {
                detail: detail.clone(),
            },
            AppError::GithubApi(detail) => Self::GithubApi {
                detail: detail.clone(),
            },
            AppError::SyncConflict(detail) => Self::SyncConflict {
                detail: detail.clone(),
            },
            AppError::StateConflict(expected, actual) => Self::StateConflict {
                expected: expected.clone(),
                actual: actual.clone(),
            },
        }
    }
}

impl From<AppError> for WireError {
    fn from(error: AppError) -> Self {
        Self::from(&error)
    }
}

impl From<WireError> for AppError {
    fn from(wire: WireError) -> Self {
        match wire {
            WireError::Usage { detail } => Self::Usage(detail),
            WireError::Validation { detail } => Self::Validation(detail),
            WireError::NotFound { detail } => Self::NotFound(detail),
            WireError::LockTimeout { detail } => Self::LockTimeout(detail),
            WireError::Integrity { detail } => Self::Integrity(detail),
            WireError::Storage { detail } => Self::Storage(detail),
            WireError::GithubAuth { detail } => Self::GithubAuth(detail),
            WireError::GithubApi { detail } => Self::GithubApi(detail),
            WireError::SyncConflict { detail } => Self::SyncConflict(detail),
            WireError::StateConflict { expected, actual } => Self::StateConflict(expected, actual),
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
