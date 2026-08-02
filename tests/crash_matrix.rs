//! The crash matrix: `kill -9` at every named point in the store, against
//! every shape of write the CLI can make.
//!
//! # Why out of process
//!
//! `tests/store_rebuild.rs` already arms the same points *in* process, with
//! [`FaultAction::Fail`], and proves the transaction rolls back. That is a test
//! of the error path, and it is not the same claim. An error unwinds: `Drop`
//! runs, the transaction issues its `ROLLBACK`, the connection goes back to the
//! pool. A crash does none of that. Whether the database is still consistent
//! afterwards is then a property of SQLite's write-ahead log and of *where*
//! storyhook chose to put its commit, and the only way to find out is to kill a
//! real process holding a real file and open it again.
//!
//! So every case here spawns `story`, arms one point through `STORYHOOK_FAULT`,
//! and inspects what the corpse left behind. `STORYHOOK_FAULT` delivers
//! `SIGKILL` to the process itself — uncatchable, unblockable, no handler, no
//! flush — which is `kill -9` timed to the instruction rather than to a
//! stopwatch.
//!
//! # The four questions asked of every crash
//!
//! 1. **Did the process actually die where it was told to?** A case whose child
//!    exits normally is a case that proved nothing, and the most likely reason
//!    is a binary built without the `fault-injection` feature. Asserted, never
//!    assumed.
//! 2. **Is the database sound?** `PRAGMA integrity_check` on reopen.
//! 3. **Is the operation all-or-nothing?** Present and complete, or absent
//!    entirely — never a story with no labels, an edge with one end, or a
//!    burned story number.
//! 4. **Does the read model still equal a fold of its own events?**
//!    `diff_read_model` is the oracle `story doctor` runs, and a crash is
//!    exactly the situation it exists for.
//!
//! [`FaultAction::Fail`]: storyhook::store::fault::FaultAction::Fail

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;

use rusqlite::Connection;
use storyhook::store::{
    FaultPoint, ProjectId, ReadOps, SqliteStore, Store, StoryQuery, diff_read_model,
};
use storyhook_test_support::{Project, TestEnv, project_id_at, scratch_dir};

/// The command the migration cases run: the cheapest one that opens the store,
/// and therefore migrates it.
///
/// It was `help`, on the premise — written into this file — that *every*
/// storyhook command migrates on open. That premise stopped being true when a
/// command needing no store stopped opening one (SH-149): `story help` is
/// answered from a string constant before any database is touched, so a fault
/// armed inside the migration would never fire and these tests would pass while
/// proving nothing.
///
/// `project list` rather than `list`, and the difference is load-bearing:
/// the racer case below requires every survivor to *succeed*, and `story list`
/// in a directory with no project is an ordinary failure. `project list` needs
/// the store, resolves no project, and answers anywhere.
const MIGRATING_COMMAND: [&str; 2] = ["project", "list"];

// ---------------------------------------------------------------------------
// Running a command that is going to die
// ---------------------------------------------------------------------------

/// Runs `story <args>` in `project` with `point` armed, and returns how it died.
///
/// `STORYHOOK_INVOKER=local` is not optional. The default invoker hands the work
/// to a daemon, and a daemon that crashed would be a different experiment with
/// a different subject: this file is about the process that owns the write
/// transaction, so the process that owns the write transaction has to be the one
/// holding the knife. (The daemon-side crash has its own case below, which sets
/// its experiment up explicitly.)
///
/// The fixtures are built with [`ProjectBuilder::local`] for the same reason
/// one step further out: the *questions* asked after the crash are about the
/// file, and a daemon holding the store answers them from its own page cache.
///
/// [`ProjectBuilder::local`]: storyhook_test_support::ProjectBuilder::local
fn crash(project: &Project<'_>, point: FaultPoint, args: &[&str]) -> ExitStatus {
    let mut cmd = project.env().raw_story(project.path());
    cmd.env("STORYHOOK_INVOKER", "local")
        .env("STORYHOOK_FAULT", point.as_str())
        .args(args);
    let output = cmd.output().expect("running the command that is to crash");

    assert_eq!(
        output.status.signal(),
        Some(libc::SIGKILL),
        "`story {}` was supposed to be killed at {}; it finished with {:?} instead. \
         The usual cause is a binary built without the `fault-injection` feature — \
         which is every binary `cargo build` produces, and none that `cargo test` does.\n\
         stdout: {}\nstderr: {}",
        args.join(" "),
        point.as_str(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output.status
}

// ---------------------------------------------------------------------------
// Asking the corpse's database what happened
// ---------------------------------------------------------------------------

/// `PRAGMA integrity_check` on the database at `path`, as a fresh connection.
///
/// Opened with rusqlite directly rather than through [`SqliteStore`]: a store
/// runs migrations and pragmas on open, and the question here is what SQLite
/// thinks of the bytes a killed process left, before anything has had a chance
/// to tidy them.
fn integrity_of(path: &Path) -> String {
    let conn = Connection::open(path).expect("reopening the database a crash left behind");
    conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("running integrity_check")
}

/// Every assertion that must hold after *any* crash, whatever it interrupted.
///
/// Factored out because the interesting part of each case is the one thing it
/// asserts about *its* operation; these four are the floor beneath all of them,
/// and a case that quietly stopped checking them would still look like a test.
fn assert_store_is_sound(env: &TestEnv, project: ProjectId) {
    assert_eq!(
        integrity_of(env.store_path()),
        "ok",
        "the database must survive a kill -9 intact"
    );

    let store = env.open_store();
    let diff = diff_read_model(&store, project).expect("diffing the read model");
    assert!(
        diff.is_clean(),
        "a crash must not leave the read model disagreeing with its events:\n{}",
        diff.describe()
    );
    assert!(
        !diff.has_integrity_issues(),
        "a crash must not leave an integrity issue behind:\n{}",
        diff.describe()
    );
}

/// The ids of every story in `project`, as the read model holds them.
fn story_ids(store: &SqliteStore, project: ProjectId) -> Vec<String> {
    store
        .read(|tx| {
            Ok(tx
                .stories(project, &StoryQuery::all())?
                .into_iter()
                .map(|row| row.snapshot.id)
                .collect())
        })
        .expect("listing stories")
}

/// The state slug of one story, as the read model holds it.
fn state_of(project: &Project<'_>, id: &str) -> String {
    project.json(&["show", id])["story"]["story"]["state"]
        .as_str()
        .expect("a story has a state")
        .to_string()
}

/// The relation targets `id` claims, as the read model holds them.
fn relations_of(project: &Project<'_>, id: &str) -> String {
    project.json(&["show", id])["story"]["story"]["relationships"].to_string()
}

// ---------------------------------------------------------------------------
// before_commit — everything the closure did must vanish
// ---------------------------------------------------------------------------

/// The strongest form of the claim, because a story number is the one piece of
/// state the legacy path leaked on *every* rejected write: `create_story_with_
/// events` incremented the on-disk counter and validated afterwards, so
/// `story new --state nonsense` burned a number permanently.
///
/// Here the allocation happens inside the transaction that uses it, so a crash
/// before COMMIT returns the number to the pool — and the next story gets it.
#[test]
fn sigkill_before_commit_creates_nothing_and_burns_no_story_number() {
    let env = TestEnv::isolated();
    let project = env.project().local().prefix("CR").build();
    let first = project.new_story("survives");

    crash(&project, FaultPoint::BeforeCommit, &["new", "vanishes"]);

    let store = env.open_store();
    let id = project_id_at(&store, project.path()).expect("the project still resolves");
    assert_eq!(
        story_ids(&store, id),
        vec![first.clone()],
        "a write killed before COMMIT must leave nothing behind"
    );
    drop(store);

    assert_eq!(
        project.new_story("the next one"),
        "CR-2",
        "the number the killed write allocated must have gone back to the pool"
    );
    assert_store_is_sound(&env, id);
}

/// A compare-and-swap is the operation a crash could most plausibly half-apply:
/// it reads a state, decides, and writes. Killed before COMMIT, the story has to
/// still be where it was — and the guard has to still fire on the old value.
#[test]
fn sigkill_before_commit_leaves_a_compare_and_swap_unapplied() {
    let env = TestEnv::isolated();
    let project = env.project().local().prefix("CR").build();
    let id = project.new_story("to be moved");
    let before = state_of(&project, &id);

    crash(
        &project,
        FaultPoint::BeforeCommit,
        &["move", &id, "in-progress", "--if-state", &before],
    );

    assert_eq!(
        state_of(&project, &id),
        before,
        "a compare-and-swap killed before COMMIT must not have moved the story"
    );
    // And the CAS is still armed on the *old* value, which is the property a
    // half-applied move would have destroyed.
    project
        .run(&["move", &id, "in-progress", "--if-state", &before])
        .success();
    assert_eq!(state_of(&project, &id), "in-progress");

    let store = env.open_store();
    let pid = project_id_at(&store, project.path()).expect("the project resolves");
    drop(store);
    assert_store_is_sound(&env, pid);
}

/// A relation is two events on two stories in one transaction — the write whose
/// legacy implementation left one-sided edges all over this repository's own
/// tracker (SH-60, fifteen live violations at migration time). Killed before
/// COMMIT, neither end may claim it.
#[test]
fn sigkill_before_commit_leaves_neither_end_of_a_relation() {
    let env = TestEnv::isolated();
    let project = env.project().local().prefix("CR").build();
    let a = project.new_story("blocker");
    let b = project.new_story("blocked");

    crash(
        &project,
        FaultPoint::BeforeCommit,
        &["relate", &a, "blocks", &b],
    );

    assert_eq!(relations_of(&project, &a), "[]", "{a} must claim no edge");
    assert_eq!(relations_of(&project, &b), "[]", "{b} must claim no edge");

    let store = env.open_store();
    let pid = project_id_at(&store, project.path()).expect("the project resolves");
    drop(store);
    assert_store_is_sound(&env, pid);
}

// ---------------------------------------------------------------------------
// after_commit_before_ack — durable, unacknowledged
// ---------------------------------------------------------------------------

/// **The no-lost-acks case, and the one the design named.**
///
/// A crash between `COMMIT` returning and the caller being told is the one
/// window where "the write failed" and "the write happened" are both true from
/// somebody's point of view. What must never happen is a *false* report in
/// either direction: the process must not exit claiming success it cannot
/// guarantee, and it must not exit claiming failure a subsequent read
/// contradicts without any way for the caller to find out.
///
/// storyhook's answer is that there is no report at all — the process dies, and
/// a dead process has said nothing. That is the honest outcome and it is what
/// this pins: **the exit status carries no success claim, and the write is
/// there.** The client-side contract that follows is written down in
/// `docs/rearch/hardening.md`: a caller that sees `story` die by signal knows
/// nothing about whether the write landed and must *read* before it retries.
/// Retrying blind is what turns this window into a duplicate.
#[test]
fn sigkill_after_commit_before_ack_does_not_report_false_success() {
    let env = TestEnv::isolated();
    let project = env.project().local().prefix("CR").build();

    let status = crash(
        &project,
        FaultPoint::AfterCommitBeforeAck,
        &["new", "durable"],
    );

    // No success claim: killed by a signal, so there is no exit code at all,
    // let alone a zero one.
    assert_eq!(
        status.code(),
        None,
        "a process killed by a signal must not also be reporting an exit code"
    );
    assert!(!status.success());

    // And the write is durable, which is the half that makes a blind retry a
    // duplicate rather than a repair.
    let store = env.open_store();
    let pid = project_id_at(&store, project.path()).expect("the project resolves");
    assert_eq!(
        story_ids(&store, pid),
        vec!["CR-1".to_string()],
        "a write killed *after* COMMIT is durable — that is what COMMIT means"
    );
    drop(store);
    assert_store_is_sound(&env, pid);

    // The exit status and a subsequent read must agree in the sense that
    // matters: the next allocation does not reuse the number the committed
    // write took.
    assert_eq!(project.new_story("the next one"), "CR-2");
}

/// The same window, for the two other shapes of write — so that "durable after
/// COMMIT" is a property of the store rather than of one code path.
#[test]
fn sigkill_after_commit_before_ack_keeps_a_move_and_a_relation() {
    let env = TestEnv::isolated();
    let project = env.project().local().prefix("CR").build();
    let a = project.new_story("blocker");
    let b = project.new_story("blocked");
    let before = state_of(&project, &a);

    crash(
        &project,
        FaultPoint::AfterCommitBeforeAck,
        &["move", &a, "in-progress", "--if-state", &before],
    );
    assert_eq!(
        state_of(&project, &a),
        "in-progress",
        "a move killed after COMMIT is durable"
    );

    crash(
        &project,
        FaultPoint::AfterCommitBeforeAck,
        &["relate", &a, "blocks", &b],
    );
    let from_a = relations_of(&project, &a);
    let from_b = relations_of(&project, &b);
    assert!(from_a.contains(&b), "{a} must claim the edge: {from_a}");
    assert!(
        from_b.contains(&a),
        "and so must {b} — a relation is two events in one transaction: {from_b}"
    );

    let store = env.open_store();
    let pid = project_id_at(&store, project.path()).expect("the project resolves");
    drop(store);
    assert_store_is_sound(&env, pid);
}

/// The commit that survived the crash was in the write-ahead log, not in the
/// main database file — so reading it back is a WAL replay, performed by a
/// process that has never seen this database before.
///
/// Asserted in that order deliberately: the `-wal` file is checked *before*
/// anything opens the store, because opening it is what checkpoints it away.
#[test]
fn a_commit_that_survives_a_crash_is_replayed_from_the_write_ahead_log() {
    let env = TestEnv::isolated();
    let project = env.project().local().prefix("CR").build();

    crash(
        &project,
        FaultPoint::AfterCommitBeforeAck,
        &["new", "in the wal"],
    );

    let wal = env.store_path().with_extension("db-wal");
    let wal_len = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    assert!(
        wal_len > 0,
        "the killed process's commit must still be in {} — if the write-ahead log is \
         empty there was nothing to replay and this test proves nothing",
        wal.display()
    );

    // A brand-new process, which has to recover the log to see the row at all.
    let listed = project.json(&["list"]).to_string();
    assert!(
        listed.contains("in the wal"),
        "the committed story must be recovered from the log: {listed}"
    );
    assert_eq!(integrity_of(env.store_path()), "ok");
}

/// The same window, one process further away: the **daemon** dies between
/// `COMMIT` and the reply, and the client is left holding a socket that closed
/// mid-answer.
///
/// This is the case the client-side contract exists for, and it is the only one
/// where storyhook has to *say* something rather than merely die. A dropped
/// connection after the request was sent is indistinguishable from a dropped
/// connection before the write — from the client's side — so the honest answer
/// is the one `HttpInvoker` gives: this may or may not have run, it will not be
/// repeated, go and look. Repeating it is what would turn a survived commit into
/// two stories.
#[test]
fn a_daemon_killed_after_commit_tells_the_client_it_cannot_know() {
    let env = TestEnv::isolated();
    let cwd = scratch_dir();

    // Initialized `--local`, so the fixture does not depend on the transport
    // that is about to be broken.
    let mut init = env.raw_story(cwd.path());
    init.env("STORYHOOK_INVOKER", "local")
        .args(["project", "init", "--prefix", "DM", "--no-agents-md"])
        .stdout(std::process::Stdio::null());
    assert!(
        init.status().expect("initializing").success(),
        "the fixture project must exist before the daemon starts"
    );

    // A daemon of our own rather than an auto-spawned one: arming a fault means
    // choosing the child's environment, which only the process that spawns it
    // can do.
    let port = storyhook_test_support::reserve_port();
    let mut serve = env.raw_story(cwd.path());
    serve
        .env("STORYHOOK_FAULT", FaultPoint::AfterCommitBeforeAck.as_str())
        .args(["daemon", "--serve", "--port", &port.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let daemon = storyhook_test_support::ChildGuard::new(serve.spawn().expect("spawning a daemon"));
    storyhook_test_support::wait_for_server(port);

    let out = env
        .story(cwd.path())
        .env("STORYHOOK_INVOKER", "daemon")
        .args(["new", "written once"])
        .output()
        .expect("running the command whose daemon is about to die");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "the client must not report success for an answer it never received: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("may or may not have run"),
        "the client must say it cannot know: {stderr}"
    );
    assert!(
        stderr.contains("will not repeat it"),
        "and it must say it is not going to retry — a blind retry is how this window \
         becomes a duplicate story: {stderr}"
    );

    drop(daemon);

    // And the write did land, which is what makes the refusal to retry correct
    // rather than merely cautious.
    let store = env.open_store();
    let pid = project_id_at(&store, cwd.path()).expect("the project resolves");
    assert_eq!(
        story_ids(&store, pid),
        vec!["DM-1".to_string()],
        "the daemon committed before it died, so the story is there — exactly once"
    );
    drop(store);
    assert_store_is_sound(&env, pid);
}

// ---------------------------------------------------------------------------
// mid_read_model_update — a partial row must never be observable
// ---------------------------------------------------------------------------

/// The point fires inside `put_story`, between the story row and the labels and
/// relations derived from it. It is inside the write transaction, so the whole
/// thing rolls back — and the test that matters is that *nothing* of it is
/// visible, not merely that the row is absent.
#[test]
fn sigkill_mid_read_model_update_leaves_no_half_written_story() {
    let env = TestEnv::isolated();
    let project = env.project().local().prefix("CR").build();

    crash(
        &project,
        FaultPoint::MidReadModelUpdate,
        &["new", "half a story", "--label", "one", "--label", "two"],
    );

    let store = env.open_store();
    let pid = project_id_at(&store, project.path()).expect("the project resolves");
    assert!(
        story_ids(&store, pid).is_empty(),
        "a story killed between its row and its labels must not exist at all"
    );
    // Asked of the tables rather than of the API, because a row with no labels
    // and a row that is absent look the same through `story show`.
    let conn = Connection::open(env.store_path()).expect("reopening the database");
    let labels: i64 = conn
        .query_row("SELECT COUNT(*) FROM story_labels", [], |row| row.get(0))
        .expect("counting labels");
    assert_eq!(
        labels, 0,
        "no label row may outlive the story it belonged to"
    );
    drop(conn);
    drop(store);
    assert_store_is_sound(&env, pid);
}

/// The same point during a relation write: the mirror edge is materialized by a
/// schema trigger, so a half-applied `put_story` is exactly how the store could
/// grow an asymmetric edge if the transaction boundary were wrong.
#[test]
fn sigkill_mid_read_model_update_leaves_no_one_sided_edge() {
    let env = TestEnv::isolated();
    let project = env.project().local().prefix("CR").build();
    let a = project.new_story("blocker");
    let b = project.new_story("blocked");

    crash(
        &project,
        FaultPoint::MidReadModelUpdate,
        &["relate", &a, "blocks", &b],
    );

    let conn = Connection::open(env.store_path()).expect("reopening the database");
    let edges: i64 = conn
        .query_row("SELECT COUNT(*) FROM story_relations", [], |row| row.get(0))
        .expect("counting relation rows");
    assert_eq!(
        edges, 0,
        "a killed relation write must leave no edge at all"
    );
    drop(conn);

    let store = env.open_store();
    let pid = project_id_at(&store, project.path()).expect("the project resolves");
    drop(store);
    assert_store_is_sound(&env, pid);
}

// ---------------------------------------------------------------------------
// mid_migration and backup_verify — crashing during an upgrade
// ---------------------------------------------------------------------------

/// A store at schema v1, the shape every machine's store had before migration 2
/// existed, placed where `story` will find it.
///
/// The committed fixture is used rather than a generated one: it is the only
/// *old* database in the tree, and it exists so that a migration has something
/// real to migrate. See `tests/store_schema_fixture.rs`.
fn env_with_a_v1_store() -> TestEnv {
    let env = TestEnv::isolated();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema/v1.db");
    std::fs::create_dir_all(env.data_dir()).expect("creating the data directory");
    std::fs::copy(&fixture, env.store_path()).expect("planting the v1 fixture");
    env
}

/// `PRAGMA user_version` of the database at `path`.
fn user_version(path: &Path) -> u32 {
    Connection::open(path)
        .expect("opening the database")
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map(|v| u32::try_from(v).expect("a non-negative schema version"))
        .expect("reading user_version")
}

/// Every storyhook command that opens the store migrates it, so an upgrade
/// happens under whatever the user happened to type — including, on a busy
/// machine, under a git hook. Killed mid-migration, the recorded version has to
/// be *true*: the next process must be able to tell what is left to do.
///
/// The qualifier is new and it is the point of [`MIGRATING_COMMAND`]: since
/// SH-149 the commands that need no store do not open one, so they no longer
/// carry a schema forward either.
#[test]
fn sigkill_mid_migration_leaves_a_version_that_is_true_and_a_backup_that_is_not() {
    let env = env_with_a_v1_store();
    let cwd = scratch_dir();
    assert_eq!(user_version(env.store_path()), 1, "the fixture is at v1");

    let mut cmd = env.raw_story(cwd.path());
    cmd.env("STORYHOOK_INVOKER", "local")
        .env("STORYHOOK_FAULT", FaultPoint::MidMigration.as_str())
        .args(MIGRATING_COMMAND);
    let status = cmd.status().expect("running the doomed migration");
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "the migration must have been killed, not completed"
    );

    assert_eq!(
        user_version(env.store_path()),
        1,
        "a migration killed before it applied anything must leave the version it started at"
    );
    assert_eq!(integrity_of(env.store_path()), "ok");

    // The backup is taken *before* the first migration runs, so the killed
    // process must have left one — and it must be a database, not a byte copy.
    let backups = storyhook::daemon::backup::snapshots(&env.state_home().join("storyhook/backups"));
    assert_eq!(
        backups.len(),
        1,
        "the pre-migration backup is taken before any statement runs, so a crash \
         during the migration must leave exactly one: {backups:?}"
    );
    assert_eq!(integrity_of(&backups[0]), "ok");

    // And the upgrade is simply resumable: nothing was half-applied, so the
    // next process finishes the job.
    let store = env.open_store();
    assert_eq!(
        user_version(store.path()),
        storyhook::store::current_schema_version(),
        "a second process must be able to complete the migration the first one died in"
    );
}

/// The fifth point, which exists because the alternative way to test "refuse to
/// migrate when the backup cannot be verified" is to hand-corrupt a SQLite file
/// — and that tests the corruption, not the refusal.
///
/// Killed during verification, no migration may have been attempted, and the
/// half-written backup must not be left lying around looking like a good one.
#[test]
fn sigkill_during_backup_verification_migrates_nothing() {
    let env = env_with_a_v1_store();
    let cwd = scratch_dir();

    let mut cmd = env.raw_story(cwd.path());
    cmd.env("STORYHOOK_INVOKER", "local")
        .env("STORYHOOK_FAULT", FaultPoint::BackupVerify.as_str())
        .args(MIGRATING_COMMAND);
    let status = cmd.status().expect("running the doomed migration");
    assert_eq!(status.signal(), Some(libc::SIGKILL));

    assert_eq!(
        user_version(env.store_path()),
        1,
        "a backup that never verified must leave the database at its old version"
    );
    assert_eq!(integrity_of(env.store_path()), "ok");

    // The snapshot is written under a staging name and renamed only once it has
    // verified, so a crash during verification leaves nothing that `snapshots`
    // will ever hand back as a backup.
    let backups = storyhook::daemon::backup::snapshots(&env.state_home().join("storyhook/backups"));
    assert!(
        backups.is_empty(),
        "an unverified copy must not be visible as a backup: {backups:?}"
    );
}

/// The concurrency case, with a crash in the middle of it.
///
/// A machine looks like this the first time a new storyhook runs on it: a shell,
/// a git hook and a daemon all reaching for one un-migrated store at once. This
/// adds the participant threads cannot model — one that **dies holding its
/// turn** — and then asks whether the seven that arrive afterwards finish the
/// job exactly once between them.
///
/// The two phases are deliberately ordered rather than raced. An armed process
/// only fires `mid_migration` if it finds a migration still pending, so throwing
/// it into the race with seven others means it sometimes arrives to find the
/// work done and exits cleanly — the test would then pass while proving nothing,
/// or fail on a machine that scheduled it late. Observed at
/// `--test-threads=2`. Killing it first makes the crash a *precondition* of the
/// race instead of a lottery.
///
/// The in-process sibling
/// (`store_migrations.rs::concurrent_migrations_of_one_store_apply_exactly_once`)
/// proves the `BEGIN IMMEDIATE` re-read serializes eight threads. This proves it
/// across eight processes, after a crash.
#[test]
fn concurrent_daemon_starts_migrate_exactly_once_even_when_one_is_killed() {
    let env = env_with_a_v1_store();
    let cwd = scratch_dir();

    // Phase one: a process dies inside the migration loop, alone, so that it
    // certainly finds work pending.
    let mut doomed = env.raw_story(cwd.path());
    doomed
        .env("STORYHOOK_INVOKER", "local")
        .env("STORYHOOK_FAULT", FaultPoint::MidMigration.as_str())
        .args(MIGRATING_COMMAND)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    assert_eq!(
        doomed.status().expect("running the doomed racer").signal(),
        Some(libc::SIGKILL),
        "the crash this test is about has to actually happen"
    );
    assert_eq!(
        user_version(env.store_path()),
        1,
        "and it must have died before applying anything"
    );

    // Phase two: eight processes arrive at the store the corpse left.
    let mut children: Vec<std::process::Child> = Vec::new();
    for _ in 0..8 {
        let mut cmd = env.raw_story(cwd.path());
        cmd.env("STORYHOOK_INVOKER", "local")
            .args(MIGRATING_COMMAND)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        children.push(cmd.spawn().expect("spawning a racer"));
    }
    for child in children {
        let out = child.wait_with_output().expect("waiting for a racer");
        assert!(
            out.status.success(),
            "every racer after the crash must succeed: {:?}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    assert_eq!(integrity_of(env.store_path()), "ok");
    assert_eq!(
        user_version(env.store_path()),
        storyhook::store::current_schema_version(),
        "eight survivors racing one crash must land on the current schema"
    );

    // `schema_migrations` is the history, and it is where a doubly-applied
    // migration would show: the version pragma would look identical.
    let conn = Connection::open(env.store_path()).expect("reopening the database");
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("counting applied migrations");
    assert_eq!(
        u32::try_from(rows).expect("a small count"),
        storyhook::store::current_schema_version(),
        "each migration must be recorded exactly once, however many processes tried"
    );
}

// ---------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------

/// Which two worlds a point can be crashed in.
///
/// The migration points only fire while a store is being carried forward, and
/// the transaction points only fire once there is a project to write to — so
/// there is no single command that reaches all five, and pretending otherwise
/// is how a sweep ends up asserting nothing about half of them.
enum Driver {
    /// A store at v1, crashed while [`MIGRATING_COMMAND`] carries it forward.
    Upgrading,
    /// A migrated store with a project, crashed during a write.
    Writing,
}

/// The world each point lives in.
///
/// Written as an exhaustive `match` rather than a lookup table on purpose: a
/// sixth [`FaultPoint`] makes this function stop compiling, which is a better
/// reminder than a test that silently skips it.
const fn driver_for(point: FaultPoint) -> Driver {
    match point {
        FaultPoint::MidMigration | FaultPoint::BackupVerify => Driver::Upgrading,
        FaultPoint::BeforeCommit
        | FaultPoint::AfterCommitBeforeAck
        | FaultPoint::MidReadModelUpdate => Driver::Writing,
    }
}

/// Every point, killed in the world it belongs to, with one assertion: the
/// database opens, passes `integrity_check`, and still answers.
///
/// The cases above each interrogate one point closely; this one is the guard
/// against a *new* point being added and never being crashed at. It iterates
/// [`FaultPoint::all`], so the day a sixth point appears this test starts
/// covering it — and [`driver_for`] makes forgetting to say where it lives a
/// compile error rather than a silent gap.
#[test]
fn every_named_point_survives_a_kill_with_the_database_intact() {
    for point in FaultPoint::all() {
        let context = point.as_str();
        match driver_for(point) {
            Driver::Upgrading => {
                let env = env_with_a_v1_store();
                let cwd = scratch_dir();
                let mut cmd = env.raw_story(cwd.path());
                cmd.env("STORYHOOK_INVOKER", "local")
                    .env("STORYHOOK_FAULT", context)
                    .args(MIGRATING_COMMAND)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                let status = cmd.status().expect("running the doomed command");
                assert_eq!(
                    status.signal(),
                    Some(libc::SIGKILL),
                    "{context} must be reachable while a store is being migrated"
                );
                assert_survives(&env, context);
            }
            Driver::Writing => {
                let env = TestEnv::isolated();
                let project = env.project().local().prefix("SW").build();
                crash(
                    &project,
                    point,
                    &["new", "the doomed one", "--label", "l", "--label", "m"],
                );
                assert_survives(&env, context);
            }
        }
    }
}

/// The floor: intact bytes *and* a store that still works. `integrity_check`
/// alone does not say the second — a file can be structurally sound and still
/// refuse to migrate or read.
fn assert_survives(env: &TestEnv, context: &str) {
    assert_eq!(
        integrity_of(env.store_path()),
        "ok",
        "the database must be intact after a kill at {context}"
    );
    let store = env.open_store();
    store.read(|tx| tx.projects()).unwrap_or_else(|e| {
        panic!("the store must still be readable after a kill at {context}: {e}")
    });
}
