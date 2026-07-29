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

use storyhook::store::{ReadOps, Store};
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

/// A project whose every command runs in its own process.
///
/// [`ProjectBuilder::local`] rather than the default, because every test in this
/// file is about the bytes on disk and a live daemon changes what those bytes
/// mean: it answers reads from memory, and its `-shm` keeps alive a
/// write-ahead log that would otherwise be discarded. The daemon leg caught two
/// tests here doing exactly that.
fn local_project<'a>(env: &'a TestEnv, prefix: &str) -> storyhook_test_support::Project<'a> {
    env.project().local().prefix(prefix).build()
}

/// A storyhook data directory holding exactly the bytes given./// A storyhook data directory holding exactly the bytes given.
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
    let project = local_project(&env, "CO");
    let created = story_in(
        &env,
        project.path(),
        &["new", "committed before the damage", "--json"],
    );
    assert!(created.status.success());
    let id = serde_json::from_slice::<serde_json::Value>(&created.stdout)
        .expect("`story new --json` prints JSON")["story"]["story"]["id"]
        .as_str()
        .expect("a minted id")
        .to_string();

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
    let project = local_project(&env, "CO");
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

/// A database cut in half: a valid SQLite header over pages that are not there.
///
/// This is what a full disk, an interrupted `cp`, or a sync client that copied a
/// file while it was being written leaves behind. What a user needs is not the
/// name of the statement that happened to notice — they need to know their
/// tracker is damaged, and that verified snapshots of it exist.
#[test]
fn a_truncated_database_says_it_is_damaged_and_where_the_backups_are() {
    let source = TestEnv::isolated();
    let seed = local_project(&source, "CO");
    assert!(
        story_in(&source, seed.path(), &["new", "real data"])
            .status
            .success()
    );
    drop(source.open_store());
    let whole = std::fs::read(source.store_path()).expect("reading a real store");
    assert!(whole.len() > 8192, "the fixture must be worth truncating");

    let env = env_with_store_bytes(&whole[..4096]);
    let cwd = scratch_dir();
    let out = story_in(&env, cwd.path(), &["init", "--no-agents-md"]);
    let message = failure_message(&out, "story init");

    assert_actionable_corruption(&env, &message);
}

/// The same claim for a file that was never a database at all — a text file
/// restored over the store, or a sync conflict copy renamed into place.
#[test]
fn a_file_that_is_not_a_database_says_so_in_the_same_words() {
    let env = env_with_store_bytes(b"this is not a database, it is a note to self\n");
    let cwd = scratch_dir();
    let out = story_in(&env, cwd.path(), &["init", "--no-agents-md"]);
    let message = failure_message(&out, "story init");

    assert_actionable_corruption(&env, &message);
}

/// What "actionable" means for a damaged store, stated once.
///
/// Three things, and the absence of a fourth: the file, the fact, the way back —
/// and no leaked implementation detail about which statement noticed. The store
/// runs half a dozen pragmas on open and which of them tripped over the bad
/// bytes is an accident of ordering; a user told "setting synchronous mode:
/// database disk image is malformed" has been given the least useful sentence
/// available.
fn assert_actionable_corruption(env: &TestEnv, message: &str) {
    assert!(
        message.contains(&env.store_path().display().to_string()),
        "the message must name the damaged file: {message}"
    );
    assert!(
        message.contains("damaged"),
        "the message must say what is wrong in a word a user knows: {message}"
    );
    assert!(
        message.contains("backups") || message.contains("snapshot"),
        "the message must point at the backups, which is the whole reason they \
         are taken: {message}"
    );
    for leaked in [
        "synchronous",
        "journal_mode",
        "busy timeout",
        "foreign_keys",
    ] {
        assert!(
            !message.contains(leaked),
            "the message must not name the pragma that happened to notice ({leaked}): \
             {message}"
        );
    }
}

// ---------------------------------------------------------------------------
// The pointer file
// ---------------------------------------------------------------------------

/// `.storyhook.toml` is committed to the repository and hand-editable — it
/// carries the user's `[plugin]` and `[hooks]` tables as well as the project's
/// identity — so a syntax error in it is an ordinary mistake, not an exotic one.
///
/// It must therefore say *which file*. A bare `TOML parse error at line 1,
/// column 6` in the middle of `story list` gives a user nothing to open.
#[test]
fn a_corrupt_pointer_file_names_the_file_that_could_not_be_read() {
    let env = TestEnv::isolated();
    let project = local_project(&env, "CO");
    std::fs::write(
        project.path().join(".storyhook.toml"),
        "this is not toml {{{\n",
    )
    .expect("corrupting the pointer file");

    let out = story_in(&env, project.path(), &["list"]);
    let message = failure_message(&out, "story list");

    assert!(
        message.contains(".storyhook.toml"),
        "the message must name the file: {message}"
    );
    assert!(
        message.contains(&project.path().display().to_string()),
        "and where it is, because a checkout is not always the working \
         directory: {message}"
    );
}

/// The same, for a pointer that parses but is missing the identity itself.
/// serde's `missing field `uuid`` is accurate and unattributed.
#[test]
fn a_pointer_file_missing_its_identity_names_the_file_too() {
    let env = TestEnv::isolated();
    let project = local_project(&env, "CO");
    std::fs::write(
        project.path().join(".storyhook.toml"),
        "schema = 1\nprefix = \"CO\"\n",
    )
    .expect("removing the identity");

    let out = story_in(&env, project.path(), &["list"]);
    let message = failure_message(&out, "story list");
    assert!(
        message.contains(".storyhook.toml"),
        "the message must name the file: {message}"
    );
    assert!(
        message.contains("uuid"),
        "and keep serde's account of what is missing: {message}"
    );
}

/// The pointer file's whole reason to exist is that it survives being copied to
/// another machine: a uuid rather than a path or a counter, committed to the
/// repository, so a clone knows which project it is.
///
/// A clone on a second machine therefore arrives with a pointer naming a
/// project that machine's store has never seen, and `story init` — which is
/// what the scaffolded `AGENTS.md` tells the next person to run — has to adopt
/// that identity. Minting a fresh one instead leaves the committed file naming
/// a project that does not exist anywhere, and the repository resolves by path
/// alone from then on: move the checkout, or clone it again, and it stops
/// resolving at all.
#[test]
fn a_clone_whose_pointer_names_an_unknown_project_adopts_that_identity() {
    let env = TestEnv::isolated();
    let clone = scratch_dir();
    let committed =
        "schema = 1\nuuid = \"11111111-2222-3333-4444-555555555555\"\nprefix = \"ZZ\"\n";
    std::fs::write(clone.path().join(".storyhook.toml"), committed)
        .expect("writing the committed pointer");

    let out = story_in(&env, clone.path(), &["init", "--no-agents-md"]);
    assert!(
        out.status.success(),
        "init on a clone must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The committed file is untouched — it is user-authored and carries their
    // `[plugin]`/`[hooks]` tables.
    assert_eq!(
        std::fs::read_to_string(clone.path().join(".storyhook.toml")).expect("the pointer"),
        committed,
        "init must not rewrite a pointer file the repository already committed"
    );

    // And the store now holds exactly the project that file names.
    let store = env.open_store();
    let project = store
        .read(|tx| tx.project_by_uuid("11111111-2222-3333-4444-555555555555"))
        .expect("reading the store")
        .expect(
            "the project the committed pointer names must exist after init — otherwise the \
             file names nothing and the checkout resolves by path alone",
        );
    assert_eq!(
        project.prefix, "ZZ",
        "and it must carry the prefix the clone's ids already use; a project that mints \
         SH-1 for a repository whose history is full of ZZ-* is a second tracker wearing \
         the first one's name"
    );
    drop(store);

    assert!(
        String::from_utf8_lossy(&story_in(&env, clone.path(), &["new", "first here"]).stdout)
            .contains("ZZ-1"),
        "the adopted prefix must reach the ids"
    );
}

/// Before that `init`, the same checkout has to be told what is going on.
///
/// "not initialized in this directory" is true of a bare directory and
/// misleading here: this repository *is* initialized, it says so in a committed
/// file, and what is missing is the project on this machine.
#[test]
fn a_checkout_naming_a_project_this_store_does_not_have_says_which_one() {
    let env = TestEnv::isolated();
    let clone = scratch_dir();
    std::fs::write(
        clone.path().join(".storyhook.toml"),
        "schema = 1\nuuid = \"11111111-2222-3333-4444-555555555555\"\nprefix = \"ZZ\"\n",
    )
    .expect("writing the committed pointer");

    let out = story_in(&env, clone.path(), &["list"]);
    let message = failure_message(&out, "story list");

    assert!(
        message.contains("11111111-2222-3333-4444-555555555555"),
        "the message must name the project the checkout claims: {message}"
    );
    assert!(
        message.contains("story init"),
        "and the command that adopts it here: {message}"
    );
}
