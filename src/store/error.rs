//! The store's error type, and its conversion into the application's.
//!
//! [`StoreError`] is deliberately *not* [`AppError`]: the store is the layer
//! that knows what went wrong at the storage level, and the application is the
//! layer that knows what a user should be told. But the two must agree on
//! *semantics*, because the CLI's exit codes and the dashboard's HTTP statuses
//! both switch on `AppError`'s variants and are part of a contract this
//! rearchitecture promises to preserve byte-for-byte. [`From<StoreError> for
//! AppError`] is written here, next to the variants, so that adding a variant
//! without deciding its user-visible meaning is impossible.

use thiserror::Error;

use crate::error::AppError;
use crate::store::ids::{EventSeq, ExpectedSeq};

/// Everything the store can fail with.
#[derive(Debug, Error)]
pub enum StoreError {
    /// A row the caller named does not exist.
    #[error("{0}")]
    NotFound(String),

    /// A compare-and-swap append lost: the story's head moved.
    ///
    /// Carries both halves so the caller can say what happened without
    /// re-reading. Maps to [`AppError::StateConflict`] — exit code 9, HTTP 409.
    #[error("expected {expected}, but the story is at seq {actual}")]
    Conflict {
        /// What the caller required the head to be.
        expected: ExpectedSeq,
        /// What the head actually was.
        actual: EventSeq,
    },

    /// The database was locked by another writer past the busy timeout.
    ///
    /// Maps to [`AppError::LockTimeout`] — exit code 4 — preserving the
    /// contract the 5-second `.storyhook/lock` deadline used to provide.
    #[error("{0}")]
    Busy(String),

    /// A write would have left the database in a state the schema forbids:
    /// a second parent, an asymmetric relation, a self-relation.
    #[error("{0}")]
    Invariant(String),

    /// The caller's input is malformed — a story id from the wrong project, a
    /// state slug that is not in the project's set.
    #[error("{0}")]
    Validation(String),

    /// The database was written by a newer storyhook than this one.
    ///
    /// The single clear message that SH-54 was missing. Without this gate a
    /// forward-incompatible database surfaces as a serde or rusqlite error
    /// somewhere deep in a read, which is how the dashboard went down.
    #[error(
        "this database is at schema version {found}, but this storyhook only understands up to \
         version {supported} — it was written by a newer storyhook; upgrade with `story update`"
    )]
    SchemaTooNew {
        /// The version recorded in the database.
        found: u32,
        /// The newest version this binary can apply.
        supported: u32,
    },

    /// A migration failed. The database is unchanged: migrations run one
    /// transaction each, and a pre-migration backup was taken first.
    #[error("migration {version} ({name}) failed: {detail}")]
    Migration {
        /// The version being applied.
        version: u32,
        /// That migration's name.
        name: String,
        /// What went wrong.
        detail: String,
    },

    /// The pre-migration backup could not be written or could not be verified.
    ///
    /// Migration refuses to proceed: a backup that fails exactly when it is
    /// needed is worse than no backup, because it was counted on.
    #[error("pre-migration backup failed, so no migration was attempted: {0}")]
    Backup(String),

    /// The database contains a value the schema should have made impossible.
    /// Distinct from [`StoreError::Invariant`], which is a rejected *write*.
    #[error("the store is corrupt: {0}")]
    Corrupt(String),

    /// A stored JSON payload could not be parsed.
    #[error("{0}")]
    Serde(#[from] serde_json::Error),

    /// Anything SQLite reported that is not one of the cases above.
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A filesystem operation failed — opening the database, creating the
    /// backup directory.
    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// The caller's own closure rejected the write.
    ///
    /// [`crate::store::Store::write`] decides commit or rollback from the
    /// closure's `Result`, so a service that wants to abort a transaction
    /// because *its* rules were broken — a closed story, a state slug the
    /// project does not define — has to say so as a `StoreError`. Wrapping the
    /// application's own error rather than flattening it into
    /// [`StoreError::Validation`] is what keeps the CLI's error contract exact:
    /// `Usage` and `Validation` share an exit code but not a wire form, and
    /// [`AppError::StateConflict`] carries two state slugs that no store-level
    /// variant has anywhere to put.
    ///
    /// [`From<StoreError> for AppError`] unwraps it, so a service's rejection
    /// arrives at the caller as the error it raised, unchanged.
    #[error("{0}")]
    Rejected(Box<AppError>),
}

impl From<AppError> for StoreError {
    fn from(error: AppError) -> Self {
        Self::Rejected(Box::new(error))
    }
}

impl StoreError {
    /// Classifies a raw rusqlite error, promoting the ones the store gives
    /// dedicated meanings.
    ///
    /// `SQLITE_BUSY` after `busy_timeout` has elapsed is the store's version of
    /// the old lock timeout, and constraint failures are invariant violations
    /// rather than anonymous storage errors — which is what makes "a second
    /// parent was rejected" a message a user can act on.
    pub(crate) fn from_sqlite(error: rusqlite::Error, context: &str) -> Self {
        use rusqlite::ErrorCode;

        let code = match &error {
            rusqlite::Error::SqliteFailure(inner, _) => Some(inner.code),
            _ => None,
        };
        match code {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Self::Busy(format!(
                "timed out waiting for the project write lock ({context})"
            )),
            Some(ErrorCode::ConstraintViolation) => {
                Self::Invariant(format!("{context}: {}", constraint_detail(&error)))
            }
            // No `context`, deliberately. Every other arm names the operation
            // because it tells the caller something: *which* write was rejected,
            // *what* was waiting for the lock. Corruption is different — the
            // store runs half a dozen statements on open and which of them
            // tripped over the bad bytes is an accident of ordering, so
            // prefixing it produces "setting synchronous mode: file is not a
            // database", which is the least useful sentence available. The
            // caller that has the path and the backups directory composes the
            // message a user sees; see `SqliteStore::explain_unopenable`.
            Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => {
                Self::Corrupt(error.to_string())
            }
            _ => Self::Sqlite(error),
        }
    }
}

/// Turns SQLite's terse constraint text into something that names the rule.
///
/// The schema's invariants are enforced by named indexes, triggers, and CHECK
/// constraints; SQLite reports the object's name, so translating here is the
/// difference between "UNIQUE constraint failed" and "a story may have at most
/// one parent".
fn constraint_detail(error: &rusqlite::Error) -> String {
    let raw = error.to_string();
    // SQLite names the *columns* a unique index covers, not the index. The
    // single-parent rule is the only unique constraint on exactly
    // `(project_id, story_no)` of `story_relations`, so those columns identify
    // it unambiguously.
    if raw.contains("story_relations.project_id, story_relations.story_no") {
        "a story may have at most one parent".to_string()
    } else if raw.contains("story_relations") && raw.contains("FOREIGN KEY") {
        "that relation names a story that does not exist, or a relation type storyhook does not \
         know"
            .to_string()
    } else if raw.contains("events are append-only") {
        "events are append-only".to_string()
    } else if raw.contains("project_paths.path") {
        "that path is already registered to a different project".to_string()
    } else if raw.contains("project_remotes.normalized") {
        // The backstop, not the ordinary message. `link_remote` reads the
        // holder first so it can name it; reaching this text means the index
        // caught a write that did not come through there.
        "that git origin is already registered to a different project".to_string()
    } else if raw.contains("archived = (closed_at IS NOT NULL)") {
        "a story is archived exactly when it has a close timestamp".to_string()
    } else if raw.contains("story_relations") && raw.contains("story_no <> other_no") {
        "a story cannot relate to itself".to_string()
    } else {
        raw
    }
}

impl From<StoreError> for AppError {
    fn from(error: StoreError) -> Self {
        match error {
            // A rejection the caller raised itself travels back out unchanged;
            // anything else would make a round trip through the transaction
            // boundary lossy.
            StoreError::Rejected(inner) => *inner,
            // Semantics preserved deliberately, variant by variant: `exit_code`
            // and the dashboard's `status_for` both switch on these, and the
            // characterization suite asserts the results.
            StoreError::NotFound(detail) => Self::NotFound(detail),
            StoreError::Conflict { expected, actual } => {
                Self::StateConflict(expected.to_string(), format!("seq {actual}"))
            }
            StoreError::Busy(detail) => Self::LockTimeout(detail),
            StoreError::Invariant(detail) | StoreError::Corrupt(detail) => Self::Integrity(detail),
            StoreError::Validation(detail) => Self::Validation(detail),
            other @ (StoreError::SchemaTooNew { .. }
            | StoreError::Migration { .. }
            | StoreError::Backup(_)
            | StoreError::Serde(_)
            | StoreError::Sqlite(_)
            | StoreError::Io(_)) => Self::Storage(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit_code_of(error: StoreError) -> i32 {
        AppError::from(error).exit_code()
    }

    #[test]
    fn conflict_becomes_a_state_conflict_carrying_both_sequences() {
        let app = AppError::from(StoreError::Conflict {
            expected: ExpectedSeq::Exact(EventSeq::new(3)),
            actual: EventSeq::new(5),
        });
        match app {
            AppError::StateConflict(expected, actual) => {
                assert_eq!(expected, "seq 3");
                assert_eq!(actual, "seq 5");
            }
            other => panic!("expected StateConflict, got {other:?}"),
        }
    }

    #[test]
    fn the_exit_codes_the_cli_contract_depends_on_are_preserved() {
        assert_eq!(exit_code_of(StoreError::NotFound("x".into())), 3);
        assert_eq!(exit_code_of(StoreError::Busy("x".into())), 4);
        assert_eq!(exit_code_of(StoreError::Invariant("x".into())), 5);
        assert_eq!(exit_code_of(StoreError::Corrupt("x".into())), 5);
        assert_eq!(exit_code_of(StoreError::Validation("x".into())), 2);
        assert_eq!(exit_code_of(StoreError::Backup("x".into())), 5);
        assert_eq!(
            exit_code_of(StoreError::SchemaTooNew {
                found: 9,
                supported: 1
            }),
            5
        );
        assert_eq!(
            exit_code_of(StoreError::Conflict {
                expected: ExpectedSeq::Any,
                actual: EventSeq::ZERO
            }),
            9
        );
    }

    #[test]
    fn a_future_schema_names_the_versions_and_the_remedy() {
        let message = StoreError::SchemaTooNew {
            found: 7,
            supported: 1,
        }
        .to_string();
        assert!(message.contains("schema version 7"), "{message}");
        assert!(message.contains("version 1"), "{message}");
        assert!(message.contains("newer storyhook"), "{message}");
    }
}
