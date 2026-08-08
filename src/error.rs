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
    #[error("sync errors: {0}")]
    SyncErrors(String),
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
            Self::SyncErrors(_) => 10,
        }
    }

    /// The same error, told where it was met.
    ///
    /// A layer that catches an error and re-raises a summary of it has thrown
    /// away the only sentence that named the actual problem. This adds without
    /// removing: the variant, and therefore the exit code, is unchanged, and
    /// the original message survives verbatim below the context.
    ///
    /// `StateConflict` is returned untouched. Its payload is two slugs a caller
    /// compares programmatically, not prose, and prepending to either would
    /// corrupt a value rather than annotate a message.
    ///
    /// The context joins the variant's *detail*, so for a variant whose
    /// `Display` carries a prefix — `github auth: {0}` — the prefix stays
    /// outermost. That is the right order: it names the subsystem, and the
    /// context names the operation within it.
    #[must_use]
    pub fn with_context(self, context: &str) -> Self {
        let joined = |detail: String| format!("{context}\n\n{detail}");
        match self {
            Self::Usage(detail) => Self::Usage(joined(detail)),
            Self::Validation(detail) => Self::Validation(joined(detail)),
            Self::NotFound(detail) => Self::NotFound(joined(detail)),
            Self::LockTimeout(detail) => Self::LockTimeout(joined(detail)),
            Self::Integrity(detail) => Self::Integrity(joined(detail)),
            Self::Storage(detail) => Self::Storage(joined(detail)),
            Self::GithubAuth(detail) => Self::GithubAuth(joined(detail)),
            Self::GithubApi(detail) => Self::GithubApi(joined(detail)),
            Self::SyncConflict(detail) => Self::SyncConflict(joined(detail)),
            Self::SyncErrors(detail) => Self::SyncErrors(joined(detail)),
            conflict @ Self::StateConflict(..) => conflict,
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
    SyncErrors { detail: String },
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
            AppError::SyncErrors(detail) => Self::SyncErrors {
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
            WireError::SyncErrors { detail } => Self::SyncErrors(detail),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Context is added; nothing is taken away. The original message has to
    /// survive verbatim, because it is the only part that names the actual
    /// problem — a layer that summarised it would be the defect this method
    /// exists to prevent.
    #[test]
    fn context_is_added_without_losing_the_original_message() {
        let error = AppError::Storage("the store is damaged".to_string())
            .with_context("the daemon could not start");
        let rendered = error.to_string();

        assert!(rendered.contains("the daemon could not start"));
        assert!(rendered.contains("the store is damaged"));
    }

    /// The variant survives, and therefore so does the exit code. A caller that
    /// branches on `NotFound` must keep branching on it after a layer has
    /// annotated the message.
    #[test]
    fn every_variant_keeps_its_own_exit_code_through_a_context() {
        for error in [
            AppError::Usage("u".into()),
            AppError::Validation("v".into()),
            AppError::NotFound("n".into()),
            AppError::LockTimeout("l".into()),
            AppError::Integrity("i".into()),
            AppError::Storage("s".into()),
            AppError::GithubAuth("ga".into()),
            AppError::GithubApi("gp".into()),
            AppError::SyncConflict("sc".into()),
            AppError::SyncErrors("se".into()),
            AppError::StateConflict("todo".into(), "done".into()),
        ] {
            let before = error.exit_code();
            let after = error.with_context("while doing the thing");
            assert_eq!(
                before,
                after.exit_code(),
                "annotating an error must not change what a script sees: {after}"
            );
        }
    }

    /// `StateConflict`'s payload is two slugs a caller compares, not prose.
    /// Prepending to either would corrupt a value while appearing to annotate a
    /// message — and `story move --if-state` reads them back.
    #[test]
    fn a_state_conflicts_two_slugs_are_left_alone() {
        let error =
            AppError::StateConflict("todo".into(), "done".into()).with_context("some context");
        match error {
            AppError::StateConflict(expected, actual) => {
                assert_eq!(expected, "todo");
                assert_eq!(actual, "done");
            }
            other => panic!("the variant must survive: {other}"),
        }
    }

    /// A variant whose `Display` carries a prefix keeps it outermost: the
    /// prefix names the subsystem, the context names the operation inside it.
    #[test]
    fn a_prefixed_variant_keeps_its_prefix_in_front() {
        let rendered = AppError::GithubAuth("no token".into())
            .with_context("while syncing")
            .to_string();
        assert!(
            rendered.starts_with("github auth: while syncing"),
            "got: {rendered}"
        );
    }
}
