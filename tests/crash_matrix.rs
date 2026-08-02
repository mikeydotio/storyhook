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
//! # Which process
//!
//! **The daemon**, because the daemon is the process that owns the write
//! transaction. This file used to kill the CLI, which was the same process while
//! `--local` existed; it is not any more. A client killed today is a process that
//! has already handed its work over a socket and holds nothing, so killing it
//! would prove something about the socket and nothing about the store.
//!
//! [`crash_the_daemon`] is where that is arranged — one fixture rather than ten
//! copies, and the place the two hazards of this shape are guarded against (a
//! daemon already running when the armed one starts, and a daemon running again
//! when the corpse is inspected). `STORYHOOK_FAULT` delivers `SIGKILL` to the
//! process itself — uncatchable, unblockable, no handler, no flush — which is
//! `kill -9` timed to the instruction rather than to a stopwatch.
//!
//! # The four questions asked of every crash
//!
//! 1. **Did the process actually die where it was told to?** A case whose daemon
//!    exits normally is a case that proved nothing, and the most likely reason
//!    is a binary built without the `fault-injection` feature. Asserted by the
//!    fixture, never assumed.
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

use rusqlite::Connection;
use storyhook::store::{
    FaultPoint, ProjectId, ReadOps, SqliteStore, Store, StoryQuery, diff_read_model,
};
use storyhook_test_support::{
    Crash, Project, TestEnv, assert_no_daemon, crash_a_starting_daemon, crash_the_daemon,
    project_id_at, scratch_dir, spawn_daemon,
};

// ---------------------------------------------------------------------------
// Running a command whose daemon is going to die
// ---------------------------------------------------------------------------

/// Runs `story <args>` in `project` against a daemon armed at `point`, and hands
/// back the corpse and what the client was told.
fn crash(project: &Project<'_>, point: FaultPoint, args: &[&str]) -> Crash {
    crash_the_daemon(project.env(), project.path(), point, args)
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
///
/// The daemon is stood down first. Several cases run a `story` command between
/// the crash and this call — to prove the store still works — and each of those
/// starts a daemon, which would then be holding the database these four
/// questions are about.
fn assert_store_is_sound(env: &TestEnv, project: ProjectId) {
    env.stop_daemon();
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
    let project = env.project().prefix("CR").build();
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
    let project = env.project().prefix("CR").build();
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
    let project = env.project().prefix("CR").build();
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
/// either direction: nothing may exit claiming success it cannot guarantee, and
/// nothing may exit claiming failure a subsequent read contradicts without any
/// way for the caller to find out.
///
/// This case owns the store's half of that: **the write is there, and the number
/// it took is not reissued.** The client's half — that it says it cannot know
/// rather than guessing — is the case further down, and the two are kept apart
/// because they can fail independently.
///
/// The dead daemon says nothing at all, which is the honest thing for a dead
/// process to say and is pinned here as the absence of an exit code.
#[test]
fn sigkill_after_commit_before_ack_does_not_report_false_success() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("CR").build();

    let crashed = crash(
        &project,
        FaultPoint::AfterCommitBeforeAck,
        &["new", "durable"],
    );

    // No success claim from either process: the daemon was killed by a signal,
    // so it has no exit code at all, let alone a zero one — and the client did
    // not invent one on its behalf.
    assert_eq!(
        crashed.daemon().code(),
        None,
        "a process killed by a signal must not also be reporting an exit code"
    );
    assert!(!crashed.daemon().success());
    assert!(
        !crashed.client().status.success(),
        "the client must not report success for an answer it never received: {}",
        String::from_utf8_lossy(&crashed.client().stdout)
    );

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

    // The exit status and a subsequent read must agree in the sense that
    // matters: the next allocation does not reuse the number the committed
    // write took.
    assert_eq!(project.new_story("the next one"), "CR-2");
    assert_store_is_sound(&env, pid);
}

/// The same window, for the two other shapes of write — so that "durable after
/// COMMIT" is a property of the store rather than of one code path.
#[test]
fn sigkill_after_commit_before_ack_keeps_a_move_and_a_relation() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("CR").build();
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
/// Asserted in that order deliberately, and the order is now load-bearing twice
/// over: the `-wal` file is checked *before* anything opens the store, because
/// opening it is what checkpoints it away — and every `story` command starts a
/// daemon, which opens it. [`crash_the_daemon`] guarantees none is running when
/// this line is reached.
#[test]
fn a_commit_that_survives_a_crash_is_replayed_from_the_write_ahead_log() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("CR").build();

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

    // A brand-new daemon, which has to recover the log to see the row at all.
    let listed = project.json(&["list"]).to_string();
    assert!(
        listed.contains("in the wal"),
        "the committed story must be recovered from the log: {listed}"
    );
    env.stop_daemon();
    assert_eq!(integrity_of(env.store_path()), "ok");
}

/// The client's half of the same window: it is left holding a socket that closed
/// mid-answer, and has to say so.
///
/// This is the only case where storyhook has to *say* something rather than
/// merely die. A dropped connection after the request was sent is
/// indistinguishable from a dropped connection before the write — from the
/// client's side — so the honest answer is the one `HttpInvoker` gives: this may
/// or may not have run, it will not be repeated, go and look. Repeating it is
/// what would turn a survived commit into two stories.
#[test]
fn a_daemon_killed_after_commit_tells_the_client_it_cannot_know() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("DM").build();

    let crashed = crash(
        &project,
        FaultPoint::AfterCommitBeforeAck,
        &["new", "written once"],
    );

    let stderr = crashed.client_stderr();
    assert!(
        !crashed.client().status.success(),
        "the client must not report success for an answer it never received: {}",
        String::from_utf8_lossy(&crashed.client().stdout)
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

    // And the write did land, which is what makes the refusal to retry correct
    // rather than merely cautious.
    let store = env.open_store();
    let pid = project_id_at(&store, project.path()).expect("the project resolves");
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
    let project = env.project().prefix("CR").build();

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
    let project = env.project().prefix("CR").build();
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

/// A daemon carries the store forward the moment it opens it, which is before it
/// claims a pidfile or binds a port. Killed there, the recorded version has to
/// be *true*: the next process must be able to tell what is left to do.
///
/// No client is involved and none is needed — which is the shape the two
/// migration points take now, and simpler than what they took when a CLI command
/// migrated on its own behalf.
#[test]
fn sigkill_mid_migration_leaves_a_version_that_is_true_and_a_backup_that_is_not() {
    let env = env_with_a_v1_store();
    let cwd = scratch_dir();
    assert_eq!(user_version(env.store_path()), 1, "the fixture is at v1");

    crash_a_starting_daemon(&env, cwd.path(), FaultPoint::MidMigration);

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

    crash_a_starting_daemon(&env, cwd.path(), FaultPoint::BackupVerify);

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
/// A machine looks like this the first time a new storyhook runs on it: a
/// dashboard, a launchd agent and a hand-started daemon all reaching for one
/// un-migrated store at once. This adds the participant threads cannot model —
/// one that **dies holding its turn** — and then asks whether the eight that
/// arrive afterwards finish the job exactly once between them.
///
/// **Eight daemons rather than eight clients, and that is the whole reason this
/// test still says anything.** Eight *clients* would be funnelled by the spawn
/// lock into starting one daemon between them, so exactly one process would open
/// the store and the cross-process claim would evaporate while the test stayed
/// green. Eight daemons genuinely race, because `open_store` — and therefore the
/// migration — runs before `lifecycle::run` claims the pidfile. Seven then lose
/// that claim and exit, which is asserted rather than tolerated.
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
    crash_a_starting_daemon(&env, cwd.path(), FaultPoint::MidMigration);
    assert_eq!(
        user_version(env.store_path()),
        1,
        "and it must have died before applying anything"
    );

    // Phase two: eight daemons arrive at the store the corpse left.
    let mut racers: Vec<_> = (0..8)
        .map(|_| spawn_daemon(&env, cwd.path(), None))
        .collect();

    // Whichever wins the pidfile serves forever, so every round asks the
    // incumbent to stand down. Repeatedly, and not once at the end: a racer
    // still inside `open_store` when the first winner is stopped goes on to
    // claim the vacated pidfile and serve in its turn.
    //
    // Which is also why "exactly one served" is not the assertion. How many
    // racers get as far as serving is a fact about scheduling; what this test is
    // about is that however many did, the migration happened once. That at
    // *least* one served is asserted, because a run where none did never reached
    // the race at all.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut served = 0;
    let mut done = vec![false; racers.len()];
    while done.iter().any(|finished| !finished) {
        for (racer, finished) in racers.iter_mut().zip(done.iter_mut()) {
            if *finished {
                continue;
            }
            let Some(status) = racer.try_wait() else {
                continue;
            };
            *finished = true;
            assert_eq!(
                status.signal(),
                None,
                "a racer that merely lost the pidfile must exit, not die: {status:?}"
            );
            if status.code() == Some(0) {
                served += 1;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a racer never exited; {} of 8 still running",
            done.iter().filter(|finished| !**finished).count()
        );
        env.stop_daemon();
    }
    assert!(
        served >= 1,
        "no racer ever got as far as serving, so none of them finished opening the store"
    );

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
    /// A store at v1, crashed while a starting daemon carries it forward.
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
                crash_a_starting_daemon(&env, cwd.path(), point);
                assert_survives(&env, context);
            }
            Driver::Writing => {
                let env = TestEnv::isolated();
                let project = env.project().prefix("SW").build();
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
    assert_no_daemon(env, "before the bytes are inspected");
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
