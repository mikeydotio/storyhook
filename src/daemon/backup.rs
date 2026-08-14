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
//! Every snapshot is `VACUUM INTO` plus a reopen, an `integrity_check` and a
//! page count, never `fs::copy`: copying a database whose write-ahead log is
//! hot produces a file that looks fine and is not, which is a backup that fails
//! exactly when it is needed. The page count is there because an empty database
//! passes the integrity check on its own.

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
    let taken = store.snapshot(&dir, "snapshot")?;
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

/// What `story daemon status`/`story web status` say about the daily
/// snapshots.
///
/// Age and count, in a sentence, because a backup nobody has looked at is a
/// belief rather than a backup. Not `story doctor`: its output is pinned
/// byte-for-byte by the golden corpus and its exit code means a *project's*
/// integrity, and how old a copy of the database is is a fact about the
/// machine, not the project. See [`describe_maintenance`] for the second,
/// unpruned directory this daemon never touches.
pub fn describe(env: &Environment) -> String {
    describe_dir(
        &env.backups_dir(),
        "backups",
        "the daemon takes one at startup",
    )
}

/// What `story daemon status`/`story web status` say about maintenance and
/// hand-taken safety backups.
///
/// `story project set-prefix` writes here automatically, and `story store
/// backup` writes here on request — both land in
/// [`Environment::maintenance_backups_dir`], which [`prune`] never scans, so
/// nothing here is ever deleted by the daily schedule. SH-135 is a hand-taken
/// backup evicted by that schedule because it had nowhere else to go; this is
/// the somewhere else.
pub fn describe_maintenance(env: &Environment) -> String {
    describe_dir(
        &env.maintenance_backups_dir(),
        "maintenance backups",
        "none yet — `story store backup` takes one on demand",
    )
}

/// [`describe`] and [`describe_maintenance`], parameterized on which
/// directory and what to say about an empty one.
fn describe_dir(dir: &std::path::Path, noun: &str, empty_hint: &str) -> String {
    let found = snapshots(dir);
    if found.is_empty() {
        return format!("{noun}: none yet in {} — {empty_hint}", dir.display());
    }
    let age = match newest_age(dir) {
        Some(age) if age < Duration::from_secs(3600) => "less than an hour old".to_string(),
        Some(age) => format!("{} hours old", age.as_secs() / 3600),
        None => "of unknown age".to_string(),
    };
    format!("{noun}: {} in {}, newest {age}", found.len(), dir.display())
}

/// The longest a backup label may be.
const MAX_LABEL_LEN: usize = 40;

/// Checks a user-supplied backup label and returns it unchanged.
///
/// The label becomes a filename component
/// (`storyhook-<timestamp>-<label>.db`), so a `/` or a leading `.` here does
/// not stay a label — it becomes a path, and `VACUUM INTO` would write
/// wherever it points. Restricting the alphabet before it reaches
/// [`crate::store::migrate::snapshot`] is what makes that impossible rather
/// than merely unlikely, the same division of responsibility
/// [`crate::domain::prefix::validate`] applies to a project's prefix: one
/// gate, checked before the value is trusted anywhere.
///
/// # Errors
///
/// [`AppError::Validation`] — exit 2 — naming the offending value and the
/// rule.
pub fn validate_label(raw: &str) -> Result<&str, AppError> {
    let refuse = |because: &str| {
        Err(AppError::Validation(format!(
            "invalid backup label `{raw}`: {because}. A label is 1-{MAX_LABEL_LEN} characters \
             of letters, digits, `-` and `_`, and becomes part of the backup's filename."
        )))
    };
    if raw.is_empty() {
        return refuse("it is empty");
    }
    if raw.chars().count() > MAX_LABEL_LEN {
        return refuse("it is too long");
    }
    if let Some(bad) = raw
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return refuse(&format!(
            "`{bad}` is neither a letter, a digit, `-` nor `_`"
        ));
    }
    Ok(raw)
}

/// Takes an on-demand, verified backup for a human or agent about to run a
/// risky operation — `story store backup`'s implementation.
///
/// The caller supplies `dir`; every production caller passes
/// [`Environment::maintenance_backups_dir`], so the result survives the daily
/// prune by construction rather than by naming convention, which is the
/// failure SH-135 filed. Goes through [`Store::snapshot`] like every other
/// backup in this module — `VACUUM INTO` plus a reopen, an `integrity_check`
/// and a page count, never a raw file copy — so a hand-taken backup carries
/// the same guarantee a scheduled one does. Unconfirmed by design: this only
/// ever creates a file, so it carries none of the risk that gates `story
/// project delete`/`set-prefix` behind `--force`.
pub fn take_manual<S: Store>(
    store: &S,
    dir: &std::path::Path,
    label: &str,
) -> Result<PathBuf, AppError> {
    let label = validate_label(label)?;
    Ok(store.snapshot(dir, label)?)
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
    fn describe_says_something_useful_before_and_after() {
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

    // -------------------------------------------------------------------
    // `validate_label`
    // -------------------------------------------------------------------

    #[test]
    fn validate_label_accepts_letters_digits_hyphen_and_underscore() {
        assert_eq!(
            validate_label("pre-sh130-purge").unwrap(),
            "pre-sh130-purge"
        );
        assert_eq!(validate_label("manual").unwrap(), "manual");
        assert_eq!(validate_label("v2_final").unwrap(), "v2_final");
    }

    #[test]
    fn validate_label_refuses_empty() {
        let error = validate_label("").unwrap_err();
        assert!(matches!(error, AppError::Validation(_)));
        assert!(error.to_string().contains("empty"), "{error}");
    }

    #[test]
    fn validate_label_refuses_a_path_separator() {
        // The load-bearing case: a label that could walk `VACUUM INTO` outside
        // the backup directory must never reach `migrate::snapshot`.
        let error = validate_label("../../etc/passwd").unwrap_err();
        assert!(matches!(error, AppError::Validation(_)));
    }

    #[test]
    fn validate_label_refuses_a_space() {
        let error = validate_label("pre sh130 purge").unwrap_err();
        assert!(matches!(error, AppError::Validation(_)));
    }

    #[test]
    fn validate_label_refuses_over_the_length_limit() {
        let too_long = "a".repeat(MAX_LABEL_LEN + 1);
        let error = validate_label(&too_long).unwrap_err();
        assert!(matches!(error, AppError::Validation(_)));
        assert!(error.to_string().contains("too long"), "{error}");
    }

    // -------------------------------------------------------------------
    // `take_manual` / `describe_maintenance`
    // -------------------------------------------------------------------

    #[test]
    fn take_manual_writes_a_database_that_opens_and_names_the_label() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = store_in(&env);
        let maintenance = env.maintenance_backups_dir();

        let taken = take_manual(&store, &maintenance, "pre-risky-op").expect("a manual backup");

        assert!(
            taken
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("pre-risky-op"),
            "the label should be part of the filename: {taken:?}"
        );
        let reopened = SqliteStore::open(&taken).expect("the manual backup must open");
        reopened
            .read(|tx| Ok(tx.projects()?.len()))
            .expect("the manual backup must be readable");
    }

    #[test]
    fn take_manual_refuses_an_invalid_label_before_touching_disk() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = store_in(&env);
        let maintenance = env.maintenance_backups_dir();

        let error = take_manual(&store, &maintenance, "../escape").unwrap_err();
        assert!(matches!(error, AppError::Validation(_)));
        assert!(
            !maintenance.exists() || snapshots(&maintenance).is_empty(),
            "a refused label must not create a directory or a file"
        );
    }

    /// The regression test for SH-135 itself: a hand-taken backup that lands
    /// in the maintenance directory survives a daily prune that would have
    /// evicted it had it shared `backups_dir` with the scheduled snapshots.
    #[test]
    fn a_manual_backup_survives_a_daily_prune_that_would_have_evicted_it() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = store_in(&env);

        let manual = take_manual(&store, &env.maintenance_backups_dir(), "pre-sh130-purge")
            .expect("a manual backup");

        // Overfill the *daily* directory well past RETAIN and prune it, the
        // way seven days of daemon restarts would.
        let daily = env.backups_dir();
        std::fs::create_dir_all(&daily).unwrap();
        for day in 1..=12 {
            std::fs::write(
                daily.join(format!("storyhook-202601{day:02}T000000.000Z-snapshot.db")),
                "not really a database",
            )
            .unwrap();
        }
        prune(&daily, RETAIN);

        assert!(
            manual.exists(),
            "a manual backup in the maintenance directory must survive a daily prune"
        );
    }

    #[test]
    fn describe_maintenance_says_something_useful_before_and_after() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = store_in(&env);

        let before = describe_maintenance(&env);
        assert!(before.contains("none yet"), "{before}");

        take_manual(&store, &env.maintenance_backups_dir(), "manual").expect("a manual backup");
        let after = describe_maintenance(&env);
        assert!(after.contains("newest"), "{after}");
        assert!(after.contains('1'), "{after}");
    }

    /// The two directories [`describe`] and [`describe_maintenance`] report on
    /// must never be the same one — that would silently defeat the whole
    /// point of the split SH-135 exists to make.
    #[test]
    fn describe_and_describe_maintenance_report_on_different_directories() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        assert_ne!(env.backups_dir(), env.maintenance_backups_dir());
    }
}
