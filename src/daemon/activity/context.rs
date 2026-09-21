//! Immutable verification ownership, scoped to the executing thread.

use crate::service::verification::VerificationCandidate;
use std::{cell::RefCell, marker::PhantomData, path::PathBuf, rc::Rc};

thread_local! {
    static CURRENT: RefCell<Option<LogContext>> = const { RefCell::new(None) };
}

/// Routing evidence copied into each asynchronous output observer.
#[derive(Clone, Debug)]
pub(crate) struct LogContext {
    /// Registered checkout journal, independent of the speculative worktree.
    pub(crate) directory: PathBuf,
    /// Human-readable ownership accompanying every record.
    pub(crate) label: String,
}

impl LogContext {
    /// Constructs ownership from the authoritative candidate and attempt.
    pub(crate) fn candidate(candidate: &VerificationCandidate, attempt: &str) -> Option<Self> {
        if !candidate.checkout.is_absolute() || !candidate.checkout.is_dir() {
            return None;
        }
        Some(Self {
            directory: candidate.checkout.join(".storyhook/logs"),
            label: format!(
                "project={} {} attempt={attempt}",
                candidate.project_slug, candidate.story_id
            ),
        })
    }
}

/// Records a failure before its explicit project ownership is lost on return.
/// Catalog failures keep their diagnostic in the store journal.
pub(crate) fn project_error(
    store: &impl crate::store::Store,
    project: crate::store::ProjectId,
    source: &str,
    message: &str,
) {
    use crate::store::ReadOps;
    let route = store.read(|tx| {
        Ok(match (tx.project(project)?, tx.checkout_path(project)?) {
            (Some(project), Some(checkout)) if checkout.is_absolute() && checkout.is_dir() => {
                Some(LogContext {
                    directory: checkout.join(".storyhook/logs"),
                    label: format!("project={} supervisor", project.slug),
                })
            }
            _ => None,
        })
    });
    match route {
        Ok(route) => {
            let _scope = enter(route);
            super::emit(
                "ERROR",
                source,
                "event",
                &format!("project_id={project}"),
                message,
            );
        }
        Err(error) => super::emit(
            "ERROR",
            source,
            "event",
            &format!("project_id={project}"),
            &format!("{message}; cannot resolve journal ownership: {error}"),
        ),
    }
}

/// A non-Send guard must restore the same thread on which it was installed.
pub(crate) struct Scope(Option<LogContext>, PhantomData<Rc<()>>);

/// Snapshots the current candidate for an owned observer or child command.
pub(crate) fn current() -> Option<LogContext> {
    CURRENT.with_borrow(Clone::clone)
}

/// Installs an explicit snapshot, including an empty scope for daemon work.
pub(crate) fn enter(context: Option<LogContext>) -> Scope {
    Scope(CURRENT.replace(context), PhantomData)
}

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.set(self.0.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn supervisor_errors_use_catalog_ownership_and_restore_the_callers_scope() {
        use crate::store::{ProjectId, SqliteStore, Store, WriteOps};
        let fixture = storyhook_test_support::ServiceFixture::new();
        let store = SqliteStore::open(fixture.store().path()).unwrap();
        let project = ProjectId::new(fixture.project().get());
        store
            .write(|tx| tx.set_checkout_path(project, Some(fixture.cwd())))
            .unwrap();
        let unrelated = storyhook_test_support::scratch_dir();
        let _scope = enter(Some(LogContext {
            directory: unrelated.path().join("logs"),
            label: "unrelated".into(),
        }));
        project_error(&store, project, "verification-progress", "publish failed");
        let directory = fixture.cwd().join(".storyhook/logs");
        let text = std::fs::read_to_string(super::super::day_path(&directory, chrono::Utc::now()))
            .unwrap();
        let row: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(row["message"], "publish failed");
        assert_eq!(row["source"], "verification-progress");
        assert!(!row["context"].as_str().unwrap().contains("unrelated"));
        assert!(!unrelated.path().join("logs").exists());
        assert_eq!(current().unwrap().label, "unrelated");
    }

    #[test]
    fn capture_routes_each_threads_streams_with_story_identity() {
        let root = storyhook_test_support::scratch_dir();
        std::thread::scope(|threads| {
            for project in ["one", "two"] {
                let directory = root.path().join(project);
                threads.spawn(move || {
                    let _scope = enter(Some(LogContext {
                        directory: directory.clone(),
                        label: format!("project={project} SH-1 attempt=test"),
                    }));
                    let mut command = Command::new("sh");
                    command.args(["-c", "printf output; printf error >&2"]);
                    let output =
                        crate::process::run_captured(command, std::time::Duration::from_secs(5))
                            .unwrap_or_else(|error| panic!("{}", error.detail()));
                    assert!(output.status.success());
                    let text = std::fs::read_to_string(super::super::day_path(
                        &directory,
                        chrono::Utc::now(),
                    ))
                    .unwrap();
                    let records: Vec<serde_json::Value> = text
                        .lines()
                        .map(|line| serde_json::from_str(line).unwrap())
                        .collect();
                    for (stream, message) in [("stdout", "output"), ("stderr", "error")] {
                        let record = records
                            .iter()
                            .find(|r| r["stream"] == stream && r["message"] == message)
                            .unwrap();
                        assert!(
                            record["context"]
                                .as_str()
                                .unwrap()
                                .contains(&format!("project={project} SH-1 attempt=test"))
                        );
                    }
                });
            }
        });
        assert!(current().is_none());
    }

    #[test]
    fn configure_preserves_an_explicit_destination_and_scopes_restore_on_panic() {
        let _scope = enter(Some(LogContext {
            directory: "outer".into(),
            label: "outer".into(),
        }));
        let _ = std::panic::catch_unwind(|| {
            let _scope = enter(Some(LogContext {
                directory: "inner".into(),
                label: "inner".into(),
            }));
            panic!("fixture unwind");
        });
        assert_eq!(current().unwrap().label, "outer");
        let mut command = Command::new("sh");
        command.env("STORYHOOK_ACTIVITY_LOG_DIR", "explicit");
        super::super::configure(&mut command);
        let value = command
            .get_envs()
            .find(|(key, _)| *key == "STORYHOOK_ACTIVITY_LOG_DIR")
            .unwrap()
            .1
            .unwrap();
        assert_eq!(value, "explicit");
    }
}
