//! Daily snapshots of the store.
//!
//! One global database is one global blast radius. Everything a user has ever
//! tracked, on every project, is in one file — so something has to be keeping
//! copies of it that nobody has to remember to make.
//!
//! The daemon takes one at startup if the newest is more than a day old, which
//! is a schedule without a scheduler: the daemon restarts whenever the binary
//! changes and whenever the machine boots, and a machine that is never used for
//! a week has nothing to back up anyway. Seven are kept, which is a week of
//! daily use and about 70MB for a store the size of this project's.
//!
//! Every snapshot is `VACUUM INTO` plus a reopen and an `integrity_check`,
//! never `fs::copy`: copying a database whose write-ahead log is hot produces a
//! file that looks fine and is not, which is a backup that fails exactly when it
//! is needed.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::env::Environment;
use crate::error::AppError;
use crate::store::Store;

/// How old the newest snapshot may be before another is taken.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// How many snapshots are kept.
pub const RETAIN: usize = 7;

/// Takes a snapshot if the newest is older than [`MAX_AGE`], then prunes to
/// [`RETAIN`].
///
/// Returns the snapshot it took, if it took one. Called at daemon startup, and
/// deliberately not fatal to it: a machine whose backup directory is read-only
/// should still get a tracker, loudly short of a backup rather than silently
/// short of both.
pub fn run_if_due<S: Store>(store: &S, env: &Environment) -> Result<Option<PathBuf>, AppError> {
    let dir = env.backups_dir();
    if let Some(age) = newest_age(&dir)
        && age < MAX_AGE
    {
        return Ok(None);
    }
    let taken = store.snapshot(&dir)?;
    prune(&dir, RETAIN);
    Ok(Some(taken))
}

/// How long ago the newest snapshot was taken, or `None` if there is not one.
pub fn newest_age(dir: &std::path::Path) -> Option<Duration> {
    snapshots(dir)
        .last()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
        .and_then(|at| SystemTime::now().duration_since(at).ok())
}

/// Every snapshot in `dir`, oldest first.
///
/// Ordered by *name* rather than by mtime, because the name carries an RFC3339
/// timestamp and sorts correctly, while an mtime can be rewritten by a backup
/// tool copying the directory around.
pub fn snapshots(dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "db")
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("storyhook-"))
        })
        .collect();
    found.sort();
    found
}

/// Deletes all but the newest `keep` snapshots.
///
/// A failed delete is ignored: the point of pruning is to bound disk use, and
/// failing a daemon's startup because a stale backup could not be removed would
/// trade a small problem for a large one.
fn prune(dir: &std::path::Path, keep: usize) {
    let found = snapshots(dir);
    let excess = found.len().saturating_sub(keep);
    for path in found.iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

/// What `story doctor` says about the backups.
///
/// Age and count, in a sentence, because a backup nobody has looked at is a
/// belief rather than a backup.
pub fn describe(env: &Environment) -> String {
    let dir = env.backups_dir();
    let found = snapshots(&dir);
    if found.is_empty() {
        return format!(
            "backups: none yet in {} — the daemon takes one at startup",
            dir.display()
        );
    }
    let age = match newest_age(&dir) {
        Some(age) if age < Duration::from_secs(3600) => "less than an hour old".to_string(),
        Some(age) => format!("{} hours old", age.as_secs() / 3600),
        None => "of unknown age".to_string(),
    };
    format!(
        "backups: {} in {}, newest {age}",
        found.len(),
        dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ReadOps, SqliteStore};

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("storyhook-backup-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory")
    }

    fn store_in(env: &Environment) -> SqliteStore {
        let store = SqliteStore::open(env.store_path()).expect("opening the store");
        store.migrate().expect("migrating");
        store
    }

    #[test]
    fn the_first_run_takes_a_snapshot() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = store_in(&env);

        let taken = run_if_due(&store, &env).expect("taking a snapshot");
        assert!(taken.is_some(), "an empty backup directory is overdue");
        assert_eq!(snapshots(&env.backups_dir()).len(), 1);
    }

    /// A daemon that restarts five times in an afternoon must not leave five
    /// copies of the database behind.
    #[test]
    fn a_recent_snapshot_stops_another_being_taken() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = store_in(&env);

        run_if_due(&store, &env).expect("the first");
        assert_eq!(
            run_if_due(&store, &env).expect("the second"),
            None,
            "a snapshot taken moments ago is not a day old"
        );
        assert_eq!(snapshots(&env.backups_dir()).len(), 1);
    }

    /// The bound is on the count, and the *oldest* are what go.
    #[test]
    fn pruning_keeps_the_newest_and_no_more_than_the_limit() {
        let dir = scratch();
        let backups = dir.path().join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        // Zero-padded, so the names sort the way the dates do — which is the
        // property `snapshots` relies on instead of trusting an mtime.
        for day in 1..=12 {
            std::fs::write(
                backups.join(format!("storyhook-202601{day:02}T000000.000Z-snapshot.db")),
                "not really a database",
            )
            .unwrap();
        }
        prune(&backups, RETAIN);

        let left = snapshots(&backups);
        assert_eq!(left.len(), RETAIN);
        assert!(
            left[0].to_string_lossy().contains("20260106"),
            "the five oldest must be the ones deleted; kept {left:?}"
        );
        assert!(left[RETAIN - 1].to_string_lossy().contains("20260112"));
    }

    /// A snapshot is a real, openable database — not a byte copy that happens to
    /// have the right size.
    #[test]
    fn a_snapshot_is_a_database_that_opens() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = store_in(&env);
        let taken = run_if_due(&store, &env)
            .expect("a snapshot")
            .expect("a path");

        let reopened = SqliteStore::open(&taken).expect("the snapshot must open");
        reopened
            .read(|tx| Ok(tx.projects()?.len()))
            .expect("the snapshot must be readable");
    }

    #[test]
    fn the_doctor_says_something_useful_before_and_after() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = store_in(&env);

        let before = describe(&env);
        assert!(before.contains("none yet"), "{before}");

        run_if_due(&store, &env).expect("a snapshot");
        let after = describe(&env);
        assert!(after.contains("newest"), "{after}");
        assert!(after.contains('1'), "{after}");
    }

    /// Nothing in `backups/` that is not one of ours is counted, and nothing
    /// that is not one of ours is deleted.
    #[test]
    fn foreign_files_are_neither_counted_nor_pruned() {
        let dir = scratch();
        let backups = dir.path().join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        std::fs::write(backups.join("README.md"), "why this directory exists").unwrap();
        std::fs::write(backups.join("somebody-elses.db"), "not ours").unwrap();

        assert!(snapshots(&backups).is_empty());
        prune(&backups, 0);
        assert!(backups.join("README.md").exists());
        assert!(backups.join("somebody-elses.db").exists());
    }
}
