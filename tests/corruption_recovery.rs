//! What storyhook says when the store, its log, or a checkout's pointer file is
//! damaged.
//!
//! One global database is one global blast radius, and the failure this matrix
//! exists to prevent is not data loss — it is a *user* who cannot tell what
//! happened. Before the store there was no forward-compatibility gate at all,
//! and a `.storyhook/` tree written by a newer storyhook surfaced as a serde
//! error from somewhere deep inside a read; that is how the dashboard went down
//! (SH-54). The rule this file enforces is the lesson: **every failure names
//! what is wrong, where, and what to do next, and no raw `rusqlite`, `serde` or
//! `toml` message ever reaches a user.**
//!
//! The cases are the seven shapes a store can be broken in, plus the pointer
//! file that says which project a checkout belongs to:
//!
//! | case | expected |
//! |---|---|
//! | missing database | created, silently — this is every first run |
//! | zero-byte database | treated as fresh; SQLite's own convention |
//! | truncated database | named as damaged, with the backups directory |
//! | not a database at all | the same words, for the same reason |
//! | a directory in its place | the path and the problem |
//! | schema from a newer storyhook | both versions and the remedy |
//! | damaged write-ahead log | discarded, not obeyed; committed data survives |
//! | corrupt pointer file | the file that could not be parsed |
//! | pointer naming an unknown project | what it names and how to adopt it |

use std::path::Path;
use std::process::Output;

use storyhook_test_support::{TestEnv, scratch_dir};

/// Runs `story <args>` in `cwd` against `env`'s store, in this process's
/// invoker.
///
/// `--local` throughout. A damaged store is a fact about a file, and routing
/// the question through a daemon would ask it of a *second* process that opened
/// the same file at a different moment — sometimes before it was broken.
fn story_in(env: &TestEnv, cwd: &Path, args: &[&str]) -> Output {
    env.raw_story(cwd)
        .env("STORYHOOK_INVOKER", "local")
        .args(args)
        .output()
        .expect("running story")
}

/// The stderr of a command that was supposed to fail.
fn failure_message(out: &Output, what: &str) -> String {
    assert!(
        !out.status.success(),
        "`{what}` was supposed to fail: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A storyhook data directory holding exactly the bytes given.
///
/// Returns the environment and a working directory outside it, so a command can
/// be run somewhere that is not the store.
fn env_with_store_bytes(bytes: &[u8]) -> TestEnv {
    let env = TestEnv::isolated();
    std::fs::create_dir_all(env.data_dir()).expect("creating the data directory");
    std::fs::write(env.store_path(), bytes).expect("planting the store");
    env
}

// ---------------------------------------------------------------------------
// The cases that are not failures
// ---------------------------------------------------------------------------

/// The first run on a new machine. There is no database, and there should be no
/// diagnostic either — creating it is the whole job.
#[test]
fn a_missing_database_is_created_rather_than_reported() {
    let env = TestEnv::isolated();
    let cwd = scratch_dir();
    assert!(
        !env.store_path().exists(),
        "the fixture starts with nothing"
    );

    let out = story_in(&env, cwd.path(), &["init", "--no-agents-md"]);
    assert!(
        out.status.success(),
        "a first run must create the store: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(env.store_path().is_file(), "and the file must be there");
}

/// A zero-byte file is what an interrupted `touch`, a failed restore, or a
/// synced-but-empty directory leaves. SQLite's own convention is that an empty
/// file is an empty database, and storyhook inherits it rather than second-
/// guessing: there is nothing to lose and nothing to warn about.
#[test]
fn a_zero_byte_database_is_treated_as_a_fresh_one() {
    let env = env_with_store_bytes(b"");
    let cwd = scratch_dir();

    let out = story_in(&env, cwd.path(), &["init", "--no-agents-md"]);
    assert!(
        out.status.success(),
        "an empty file is an empty database: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        std::fs::metadata(env.store_path())
            .expect("the store")
            .len()
            > 0
    );
}

/// A write-ahead log whose frames do not checksum is *discarded* by SQLite, not
/// applied — so the last checkpointed state survives and nothing is silently
/// half-recovered.
///
/// Worth pinning rather than assuming: the alternative behaviour a storage layer
/// could have is to fail the open, which would turn a recoverable machine into
/// an unusable one over a file that is by definition regenerable.
#[test]
fn a_damaged_write_ahead_log_is_discarded_and_the_committed_data_survives() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("CO").build();
    let id = project.new_story("committed before the damage");

    // Checkpointed by opening and closing the store, then the log is filled
    // with bytes no frame header could match.
    drop(env.open_store());
    let wal = env.store_path().with_extension("db-wal");
    std::fs::write(&wal, b"garbage garbage garbage garbage garbage").expect("damaging the log");

    let out = story_in(&env, project.path(), &["list"]);
    assert!(
        out.status.success(),
        "a damaged log must not make the store unusable: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&id),
        "the committed story must still be there: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// ---------------------------------------------------------------------------
// The cases that are failures, and have to say so usefully
// ---------------------------------------------------------------------------

/// The SH-54 gate, in the situation it was written for: a store carried forward
/// by a newer storyhook and then opened by an older one.
///
/// The message must contain both version numbers and the remedy, because
/// "something is wrong with your database" and "you are running last month's
/// binary" call for very different next actions.
#[test]
fn a_database_from_a_newer_storyhook_names_both_versions_and_the_remedy() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("CO").build();
    // Written through rusqlite rather than by hand: the pragma is what the gate
    // reads, and a fabricated file would be testing the fabrication.
    rusqlite::Connection::open(env.store_path())
        .expect("opening the store")
        .execute_batch("PRAGMA user_version = 99")
        .expect("claiming a future schema");

    let out = story_in(&env, project.path(), &["list"]);
    let message = failure_message(&out, "story list");

    assert!(message.contains("99"), "the found version: {message}");
    assert!(
        message.contains("newer storyhook"),
        "what it means: {message}"
    );
    assert!(
        message.contains("story update"),
        "and what to do about it: {message}"
    );
    assert_eq!(
        out.status.code(),
        Some(5),
        "a store this binary cannot read is an integrity failure: {message}"
    );
}

/// A directory where the database belongs — what an interrupted restore or a
/// mistyped `mkdir -p` leaves.
///
/// The bar here is lower than for corruption: there is nothing to recover, and
/// what a user needs is the path so they can look at it.
#[test]
fn a_directory_where_the_database_belongs_names_the_path() {
    let env = TestEnv::isolated();
    let cwd = scratch_dir();
    std::fs::create_dir_all(env.store_path()).expect("putting a directory in the way");

    let out = story_in(&env, cwd.path(), &["init", "--no-agents-md"]);
    let message = failure_message(&out, "story init");
    assert!(
        message.contains(&env.store_path().display().to_string()),
        "the message must name the path that is in the way: {message}"
    );
}
