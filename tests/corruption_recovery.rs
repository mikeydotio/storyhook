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
//! The cases are the shapes a store can be broken in, plus the pointer file that
//! says which project a checkout belongs to. Three of them are deliberately
//! *not* failures, and are tested for exactly that reason: a store that refused
//! to open on a zero-byte file, or on a write-ahead log it could not make sense
//! of, would turn a recoverable machine into an unusable one over a file that is
//! by definition regenerable.
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

/// Runs `story <args>` in `cwd` against `env`'s store, through the daemon.
///
/// This used to be `--local` throughout, on the reasoning that a damaged store
/// is a fact about a file and routing the question through a daemon would ask it
/// of a *second* process that opened the same file at a different moment. The
/// reasoning was sound and its remedy is now the wrong one: there is one
/// transport, so what a user meets when their store is damaged is exactly this
/// second process, and a test that stepped around it would be testing a path
/// nobody can take.
///
/// What the reasoning was actually about survives as a rule the cases keep:
/// **whoever asks about the bytes must first make sure nobody is holding them**
/// — `env.stop_daemon()` before the file is read or written, never a different
/// transport.
fn story_in(env: &TestEnv, cwd: &Path, args: &[&str]) -> Output {
    env.raw_story(cwd)
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

/// A project to ask the damaged-store questions from.
///
/// Building it starts a daemon, because building it runs `story project init`.
/// Every test here that then goes on to read or write the store's *bytes* has to
/// stand that daemon down first: a live one answers reads from memory, and its
/// `-shm` keeps alive a write-ahead log that would otherwise be discarded. The
/// daemon leg caught two tests in this file doing exactly that, back when the
/// fixture avoided the problem by never starting one.
fn project_in<'a>(env: &'a TestEnv, prefix: &str) -> storyhook_test_support::Project<'a> {
    env.project().prefix(prefix).build()
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

    let out = story_in(
        &env,
        cwd.path(),
        &["project", "new", "--prefix", "SH", "--no-agents-md"],
    );
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

    let out = story_in(
        &env,
        cwd.path(),
        &["project", "new", "--prefix", "SH", "--no-agents-md"],
    );
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
    let project = project_in(&env, "CO");
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

    // The daemon holds its own write-ahead-log handle, so a log damaged while it
    // is running is a log it goes on reading out of memory. Stood down first, and
    // then checkpointed by opening and closing the store, before the log is
    // filled with bytes no frame header could match.
    env.stop_daemon();
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
    let project = project_in(&env, "CO");
    // The daemon the fixture started has already opened this store and passed
    // the gate, so it would go on serving happily whatever the pragma says.
    env.stop_daemon();
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

    let out = story_in(
        &env,
        cwd.path(),
        &["project", "new", "--prefix", "SH", "--no-agents-md"],
    );
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
    let seed = project_in(&source, "CO");
    assert!(
        story_in(&source, seed.path(), &["new", "real data"])
            .status
            .success()
    );
    // Nothing may be holding the file this test is about to read: a daemon's
    // uncheckpointed log would leave the copy missing the story just written.
    source.stop_daemon();
    drop(source.open_store());
    let whole = std::fs::read(source.store_path()).expect("reading a real store");
    assert!(whole.len() > 8192, "the fixture must be worth truncating");

    let env = env_with_store_bytes(&whole[..4096]);
    let cwd = scratch_dir();
    let out = story_in(
        &env,
        cwd.path(),
        &["project", "new", "--prefix", "SH", "--no-agents-md"],
    );
    let message = failure_message(&out, "story init");

    assert_actionable_corruption(&env, &message);
}

/// The same claim for a file that was never a database at all — a text file
/// restored over the store, or a sync conflict copy renamed into place.
#[test]
fn a_file_that_is_not_a_database_says_so_in_the_same_words() {
    let env = env_with_store_bytes(b"this is not a database, it is a note to self\n");
    let cwd = scratch_dir();
    let out = story_in(
        &env,
        cwd.path(),
        &["project", "new", "--prefix", "SH", "--no-agents-md"],
    );
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
    let project = project_in(&env, "CO");
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
    let project = project_in(&env, "CO");
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

    let out = story_in(
        &env,
        clone.path(),
        &["project", "new", "--prefix", "SH", "--no-agents-md"],
    );
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
        message.contains("story project init"),
        "and the command that adopts it here: {message}"
    );
}

// ---------------------------------------------------------------------------
// The commands that must survive a store they cannot open (SH-149)
// ---------------------------------------------------------------------------

/// A `story` invocation with nothing said about the transport at all.
///
/// [`story_in`] still names one while the variable that names it exists. These
/// cases must not: the whole claim is that the answer arrives *before* any
/// invoker is chosen, so a test that chose one would be testing a narrower thing
/// than the defect. When the variable goes, the difference between the two
/// helpers goes with it — and this is the one whose meaning does not change.
fn story_however_it_runs(env: &TestEnv, cwd: &Path, args: &[&str]) -> Output {
    env.raw_story(cwd)
        .args(args)
        .output()
        .expect("running story")
}

/// Every command that is not about the data must survive a store that will not
/// open.
///
/// This is the defect that made storyhook's own advice impossible to follow. The
/// corruption diagnostic three tests above says, in as many words, *"to restore
/// one: run `story daemon stop`, delete store.db …"* — and `story daemon stop`
/// exited 5 with that same message, because `main` opened the store before
/// dispatching anything. So did `story --help`, which is what every other error
/// in the program tells the reader to run.
///
/// Each of the five is checked for two separate things, because they fail
/// differently: the exit status, and the absence of the corruption text. A
/// command that started reporting "damaged" while exiting 0 would be a
/// regression this test would otherwise miss.
#[test]
fn a_store_that_will_not_open_does_not_take_down_the_commands_that_never_needed_it() {
    let env = env_with_store_bytes(b"this is not a database at all\n");
    let cwd = scratch_dir();

    for args in [
        vec!["--version"],
        vec!["--help"],
        vec!["daemon", "status"],
        vec!["web", "status"],
        vec!["help", "web"],
    ] {
        let out = story_however_it_runs(&env, cwd.path(), &args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`story {}` needs no store, so a damaged one must not take it down; \
             it exited {:?} saying: {stderr}",
            args.join(" "),
            out.status.code(),
        );
        assert!(
            !stderr.contains("is damaged"),
            "`story {}` must not report a store it never opened: {stderr}",
            args.join(" "),
        );
    }
}

/// The remedy the corruption diagnostic prints, executed against a corrupt
/// store.
///
/// Written as the drill rather than as a list of commands, because the claim is
/// not "these five verbs work" — it is that a reader who does what the message
/// says gets somewhere. `story daemon stop` is step one, and it was step one
/// that failed.
#[test]
fn the_remedy_a_damaged_store_prints_can_actually_be_run() {
    let env = env_with_store_bytes(b"not a database\n");
    let cwd = scratch_dir();

    let broken = story_in(&env, cwd.path(), &["list"]);
    let message = failure_message(&broken, "story list");
    assert!(
        message.contains("story daemon stop"),
        "the fixture assumes the diagnostic still names this step: {message}"
    );

    let step_one = story_however_it_runs(&env, cwd.path(), &["daemon", "stop"]);
    assert!(
        step_one.status.success(),
        "the first step of storyhook's own remedy must run on the store it is a \
         remedy for; it exited {:?} saying: {}",
        step_one.status.code(),
        String::from_utf8_lossy(&step_one.stderr)
    );

    // And the rest of the remedy then works, which is what makes step one worth
    // fixing rather than merely worth reporting.
    std::fs::remove_file(env.store_path()).expect("deleting the damaged store");
    let after = story_in(
        &env,
        cwd.path(),
        &["project", "new", "--prefix", "SH", "--no-agents-md"],
    );
    assert!(
        after.status.success(),
        "a fresh store must open once the damaged one is gone: {}",
        String::from_utf8_lossy(&after.stderr)
    );
}

// ---------------------------------------------------------------------------
// What the client says when the daemon cannot start (SH-114)
// ---------------------------------------------------------------------------

/// The diagnosis the daemon computed is the one the user reads.
///
/// This is the failure mode for the entire CLI once there is no in-process
/// fallback, and it was the worst message in the program: the daemon worked out
/// that the store was damaged, named the file, the damage, the snapshots and the
/// restore procedure — wrote all of it to a log — and the client replied *"the
/// storyhook daemon did not start within 5s"* and pointed at the log.
///
/// Asserted on the *content* rather than on the two messages being equal,
/// because the client adds to it: what it tried, how the daemon ended, and where
/// its files are. Adding is the point; replacing was the defect.
#[test]
fn a_client_reports_the_daemon_s_own_reason_for_not_starting() {
    let env = env_with_store_bytes(b"not a database\n");
    let cwd = scratch_dir();

    let out = story_in(&env, cwd.path(), &["list"]);
    let message = failure_message(&out, "story list");

    assert!(
        message.contains("is damaged: file is not a database"),
        "the client must report what the daemon found, not that it gave up: {message}"
    );
    assert!(
        message.contains("story doctor"),
        "including the remedy the daemon computed: {message}"
    );
    assert!(
        message.contains("could not start"),
        "and its own context, because the command did not run either: {message}"
    );
    assert!(
        !message.contains("--local"),
        "and it must not offer a flag that no longer exists: {message}"
    );
    assert_eq!(
        out.status.code(),
        Some(5),
        "the daemon's exit code survives the hop, because the variant does"
    );
}

/// A daemon that has already exited is not waited for.
///
/// The client polled a portfile for the full five-second spawn deadline without
/// ever asking whether the process it started was still alive, so every startup
/// failure cost five seconds and then reported a *timeout* — which names the
/// symptom and nothing else. Measured before the fix: 5.0s. After: under a
/// tenth of that.
///
/// The bound is generous on purpose. What is being asserted is that the deadline
/// is not being spent, not that the machine is fast; a threshold near the true
/// figure would be a speed assertion in a suite that runs at core-count
/// parallelism, which is a known way to build a flaky test (SH-140).
#[test]
fn a_client_does_not_wait_out_the_deadline_for_a_daemon_that_already_died() {
    let env = env_with_store_bytes(b"not a database\n");
    let cwd = scratch_dir();

    let started = std::time::Instant::now();
    let out = story_in(&env, cwd.path(), &["list"]);
    let elapsed = started.elapsed();

    failure_message(&out, "story list");
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "a daemon that exited must be noticed rather than waited out; the whole \
         command took {elapsed:?} against a 5s spawn deadline"
    );
}

/// The failure record belongs to the daemon this client started, never to an
/// older one.
///
/// A file that outlived its cause would be worse than no file: the client would
/// report a stale reason with complete confidence. The spawner deletes it in the
/// same place it truncates the log, and this proves that deletion happens by
/// planting a failure that is a lie and watching a successful start ignore it.
#[test]
fn a_stale_failure_record_is_not_blamed_on_a_daemon_that_started_fine() {
    let env = TestEnv::isolated();
    let cwd = scratch_dir();

    // A working store, so the daemon this test starts will succeed.
    let init = story_in(
        &env,
        cwd.path(),
        &["project", "new", "--prefix", "SH", "--no-agents-md"],
    );
    assert!(
        init.status.success(),
        "the fixture needs a project: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let environment = env.environment();
    let failure = environment.daemon_failure();
    std::fs::create_dir_all(failure.parent().expect("a parent directory"))
        .expect("creating the daemon state directory");

    // Stop the daemon so the next command has to start one, then plant a lie
    // where its predecessor's failure would have been.
    story_in(&env, cwd.path(), &["daemon", "stop"]);
    std::fs::write(
        &failure,
        r#"{"kind":"storage","detail":"a failure from some previous life"}"#,
    )
    .expect("planting a stale failure record");

    let out = story_in(&env, cwd.path(), &["list"]);
    assert!(
        out.status.success(),
        "a daemon that starts must not be blamed for an older one's failure: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !failure.exists(),
        "and the stale record must be gone, not merely ignored"
    );
}
