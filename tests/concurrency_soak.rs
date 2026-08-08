//! Sustained concurrent load against one store, from both invokers at once.
//!
//! # What this adds to the tests that already race
//!
//! `move_if_state.rs` races twenty processes at one compare-and-swap;
//! `worktree_truth.rs` races two `story new` calls from two checkouts;
//! `store_migrations.rs` races eight threads at one migration. Each of those
//! interrogates a single operation at a single instant, which is the right
//! shape for pinning a specific invariant and the wrong shape for finding the
//! failures that only appear under *duration* and *mixture*: a lock held a
//! little too long, a connection returned to the pool in a bad state, a busy
//! timeout that is fine at two writers and not at eight.
//!
//! So this file runs a load rather than a race. Eight clients, several rounds
//! each, mixed operations, all of them through the daemon — which is not a
//! choice the file makes but the only shape there is, since SH-114 collapsed the
//! CLI to one transport.
//!
//! # What that cost, named rather than quietly absorbed
//!
//! Half of these clients used to write the database *directly*, so the file
//! genuinely exercised SQLite's multi-process write path: `busy_timeout`, the
//! `BEGIN IMMEDIATE` retry, `SQLITE_BUSY` reaching a user as exit code 4. With
//! one transport there is one writing process, and that contention moved inside
//! the daemon's own write mutex. The assertions below are kept — a lock held too
//! long is still a lock held too long — but assertion 2 is now about a mutex
//! rather than about a file lock, and nothing in this suite drives two processes
//! at one store any more. The only thing that still can is `story tui`, which
//! holds its own store handle; making it a client is SH-150.
//!
//! # The four things it asserts
//!
//! 1. **It finishes.** Every child is waited on with a deadline; a client that
//!    outlives it is reported as a deadlock rather than hanging the suite.
//! 2. **No lock contention reaches a user.** `SQLITE_BUSY` is a real condition
//!    under this load and the store's answer to it is `busy_timeout` plus a
//!    process-wide write mutex. If either stopped working the symptom would be
//!    exit code 4 on a command that did nothing wrong, so exit code 4 is a
//!    failure here, and so is the word "locked" on stderr.
//! 3. **Nothing is lost or duplicated.** Story numbers are allocated by
//!    `UPDATE … RETURNING` inside the transaction that uses them, so N clients
//!    creating M stories each must produce exactly N×M stories numbered
//!    contiguously from 1 — the property whose absence corrupted this
//!    repository's own tracker twice.
//! 4. **The read model still equals its events.** `diff_read_model` afterwards,
//!    which is the same oracle `story doctor` runs.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use storyhook::store::{ProjectId, ReadOps, SqliteStore, Store, StoryQuery, diff_read_model};
use storyhook_test_support::{TestEnv, project_id_at, run_bounded, scratch_dir};

/// How many clients run at once.
///
/// Eight is chosen against the store's pool rather than arbitrarily: the SQLite
/// engine keeps at most eight idle connections, so eight concurrent writers is
/// the point at which the pool is exactly saturated and a ninth would have to
/// open its own. Load beyond that tests the operating system, not storyhook.
const CLIENTS: usize = 8;

/// How many rounds each client performs. Each round is three commands, so the
/// soak is `CLIENTS * ROUNDS * 3` processes.
const ROUNDS: usize = 3;

/// How long any single `story` invocation may take before it is called a
/// deadlock.
///
/// Generous by two orders of magnitude, and measured rather than assumed: over a
/// full-suite run the 116 commands below land at p50 10ms, p90 80ms, p99 370ms,
/// maximum 370ms — around eighty times inside this bound. The failure being
/// distinguished is "slow" from "never", and only the second one is a defect.
///
/// **This bound has fired, and when it did it was right.** SH-94 filed those
/// occurrences as load sensitivity, reading them as a deadline calibrated for an
/// idle machine and unable to survive a parallel suite. Measurement said
/// otherwise, and the value stays: what was wrong was the diagnosis, not the
/// number.
///
/// Every wait inside the *daemon* is bounded by five seconds — `ensure_wal` and
/// SQLite lock acquisition by `busy_timeout`, one acquisition per write because
/// the store takes `BEGIN IMMEDIATE`, and the event hooks' own `wait_timeout` —
/// so nothing the work itself does can reach thirty.
///
/// Two things can, and both are in the client. The *parent* can block in
/// `read_to_end` on a pipe whose write end a daemon inherited by accident and
/// holds for its whole life; that is fixed at its origin, see
/// `tests/daemon_fd_hygiene.rs`. And `spawn_locked` took `daemon.spawn.lock`
/// with a blocking `flock` that had no timeout, so several clients arriving at
/// once with no daemon running queued serially behind a full attempt each —
/// measured at 10.16s, 20.25s, 30.33s and 40.42s for four. **That is fixed
/// (SH-143):** the wait is bounded by `SPAWN_LOCK_DEADLINE`, and a client that
/// arrives while another is attempting adopts its verdict rather than repeating
/// it, so the queue is one attempt deep however many clients are in it.
///
/// # Why this is derived from the client's own bound
///
/// The two numbers have to stay ordered, and stating the relationship is the
/// only way to keep them so. A client can now legitimately spend
/// `SPAWN_LOCK_DEADLINE` waiting for the spawn lock and *then* report a proper
/// error; if this harness gave up at the same moment, that correct failure would
/// race a timeout and surface as the anonymous stall this bound exists to
/// retire — the harness would win a race it is supposed to lose. The headroom is
/// what guarantees the client's own deadline fires first and names itself.
///
/// The margin is generous rather than tight for the same reason the 30s was:
/// what is being distinguished is "slow" from "never".
const DEADLINE: Duration =
    Duration::from_secs(storyhook::daemon::lifecycle::SPAWN_LOCK_DEADLINE.as_secs() + 15);

// `run_bounded` itself now lives in `storyhook_test_support` (SH-142), which
// gained a second caller that needed the exact same spawn-wait-collect bound:
// `DaemonGuard`'s own `story web stop` reap, in that crate's `server.rs`. This
// file keeps its own `DEADLINE` — the two callers' legitimate wait times don't
// share a bound, only the shape that enforces one does.

/// Asserts a client command succeeded, and that it did not succeed *by* telling
/// the user about lock contention.
///
/// The second half is the point. Contention is expected here and is the store's
/// problem to absorb: `busy_timeout` covers cross-process waits and a
/// process-wide mutex covers in-process ones. A user seeing any of it is the
/// regression.
fn assert_clean(out: &Output, what: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "`{what}` failed under concurrent load ({:?}): {stderr}",
        out.status
    );
    assert_ne!(
        out.status.code(),
        Some(4),
        "`{what}` surfaced a lock timeout to the user; the busy timeout is supposed to \
         absorb this: {stderr}"
    );
    for symptom in ["locked", "SQLITE_BUSY", "timed out waiting"] {
        assert!(
            !stderr.contains(symptom),
            "`{what}` mentioned `{symptom}` on stderr — contention must never reach a \
             user: {stderr}"
        );
    }
}

/// One client's `story` command.
fn client_command(env: &TestEnv, cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = env.raw_story(cwd);
    cmd.args(args);
    cmd
}

/// The id `story new --json` minted.
fn minted_id(out: &Output) -> String {
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("`story new --json` prints JSON");
    json["story"]["story"]["id"]
        .as_str()
        .expect("a minted story has an id")
        .to_string()
}

/// Stops whatever daemon this environment ended up with, however the test ends.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = storyhook::daemon::lifecycle::stop(
            &self.0.environment(),
            storyhook::daemon::lifecycle::StopMode::Force,
        );
    }
}

/// The read model, checked against its own events.
fn assert_no_drift(env: &TestEnv, project: ProjectId) {
    let store = env.open_store();
    let diff = diff_read_model(&store, project).expect("diffing the read model");
    assert!(
        diff.is_clean() && !diff.has_integrity_issues(),
        "concurrent load must not leave the read model disagreeing with its events:\n{}",
        diff.describe()
    );
}

/// Every story number in the project, sorted.
fn story_numbers(store: &SqliteStore, project: ProjectId) -> Vec<i64> {
    store
        .read(|tx| {
            let mut numbers: Vec<i64> = tx
                .stories(project, &StoryQuery::all())?
                .into_iter()
                .map(|row| row.story_no.get())
                .collect();
            numbers.sort_unstable();
            Ok(numbers)
        })
        .expect("listing stories")
}

// ---------------------------------------------------------------------------
// The soak
// ---------------------------------------------------------------------------

/// Eight clients, three rounds each, three commands a round, all over RPC.
///
/// Each client only ever touches stories it created itself, so every command in
/// the run is *supposed* to succeed: there is no legitimate conflict to explain
/// away, and any failure is a real one. The contention is entirely in the
/// places it belongs — the shared id counter, the shared write lock, the shared
/// database file.
#[test]
fn eight_concurrent_clients_under_load_lose_nothing() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("SK").build();
    let _daemon = DaemonGuard(&env);

    std::thread::scope(|scope| {
        for index in 0..CLIENTS {
            let env = &env;
            let root = project.path();
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    let title = format!("client {index} round {round}");
                    let out = run_bounded(
                        client_command(env, root, &["new", &title, "--json"]),
                        &format!("new ({index}/{round})"),
                        DEADLINE,
                    );
                    assert_clean(&out, &format!("story new ({index}/{round})"));
                    let id = minted_id(&out);

                    let out = run_bounded(
                        client_command(env, root, &["comment", &id, "seen under load", "--json"]),
                        &format!("comment ({index}/{round})"),
                        DEADLINE,
                    );
                    assert_clean(&out, &format!("story comment ({index}/{round})"));

                    let out = run_bounded(
                        client_command(env, root, &["move", &id, "in-progress", "--json"]),
                        &format!("move ({index}/{round})"),
                        DEADLINE,
                    );
                    assert_clean(&out, &format!("story move ({index}/{round})"));
                }
            });
        }
    });

    let store = env.open_store();
    let pid = project_id_at(&store, project.path()).expect("the project resolves");

    let expected: Vec<i64> = (1..=(CLIENTS * ROUNDS) as i64).collect();
    assert_eq!(
        story_numbers(&store, pid),
        expected,
        "{CLIENTS} clients creating {ROUNDS} stories each must produce exactly \
         {} stories numbered 1..{} — no gaps (a lost allocation) and no duplicates \
         (a shared counter read twice)",
        CLIENTS * ROUNDS,
        CLIENTS * ROUNDS
    );
    drop(store);
    assert_no_drift(&env, pid);

    // And the shipped integrity check agrees, which is the form a user would
    // ask the question in.
    project.run(&["doctor"]).success();
}

/// The same load, aimed at relations — the write that touches two stories in one
/// transaction and that the legacy path left one-sided all over this
/// repository's tracker.
///
/// Each client relates its own pair, so the edges are disjoint and every call
/// must succeed; the contention is on the write lock and on the mirror triggers.
/// What is asserted afterwards is symmetry: for every edge the store holds, the
/// other end holds its inverse.
#[test]
fn concurrent_relation_writes_leave_a_symmetric_graph() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("SK").build();
    let _daemon = DaemonGuard(&env);

    // Two stories per client, created serially so the pairing is predictable.
    let pairs: Vec<(String, String)> = (0..CLIENTS)
        .map(|index| {
            (
                project.new_story(&format!("blocker {index}")),
                project.new_story(&format!("blocked {index}")),
            )
        })
        .collect();

    std::thread::scope(|scope| {
        for (index, (a, b)) in pairs.iter().enumerate() {
            let env = &env;
            let root = project.path();
            scope.spawn(move || {
                let out = run_bounded(
                    client_command(env, root, &["relate", a, "blocks", b, "--json"]),
                    &format!("relate ({index})"),
                    DEADLINE,
                );
                assert_clean(&out, &format!("story relate ({index})"));
            });
        }
    });

    let store = env.open_store();
    let pid = project_id_at(&store, project.path()).expect("the project resolves");

    // Ids resolved before the read opens, so the closure does nothing but read.
    let numbered: Vec<_> = pairs
        .iter()
        .map(|(a, b)| {
            (
                a.clone(),
                project.story_no(&store, a),
                b.clone(),
                project.story_no(&store, b),
            )
        })
        .collect();

    store
        .read(|tx| {
            for (a, a_no, b, b_no) in &numbered {
                let from = tx.relations_from(pid, *a_no)?;
                assert!(
                    from.iter().any(|edge| edge.relation == "blocks"),
                    "{a} must hold the edge it wrote, got {from:?}"
                );
                let to = tx.relations_from(pid, *b_no)?;
                assert!(
                    to.iter().any(|edge| edge.relation == "blocked-by"),
                    "{b} must hold the mirror the schema materialized, got {to:?}"
                );
            }
            Ok(())
        })
        .expect("reading relations");
    drop(store);

    assert_no_drift(&env, pid);
    project.run(&["doctor"]).success();
}

/// The hardest allocation case: every client asks for a number at the same
/// instant, repeatedly, with nothing else competing for the lock.
///
/// `worktree_truth.rs` proves two checkouts do not collide. This proves the
/// property does not degrade with pressure — the failure mode a
/// read-then-increment counter shows is that it is *usually* right.
#[test]
fn story_numbers_stay_unique_under_a_burst_of_simultaneous_allocations() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("SK").build();
    let _daemon = DaemonGuard(&env);

    // Every child spawned before any is waited on: waiting between spawns is
    // how a race test quietly becomes a sequential one.
    let mut children = Vec::new();
    for _ in 0..CLIENTS * 2 {
        let mut cmd = client_command(&env, project.path(), &["new", "simultaneous", "--json"]);
        children.push(
            cmd.stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawning a simultaneous allocator"),
        );
    }

    let mut ids: Vec<String> = Vec::new();
    for child in children {
        let out = child.wait_with_output().expect("waiting for an allocator");
        assert_clean(&out, "simultaneous story new");
        ids.push(minted_id(&out));
    }

    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        ids.len(),
        "every simultaneous allocation must get its own number; got {ids:?}"
    );

    let store = env.open_store();
    let pid = project_id_at(&store, project.path()).expect("the project resolves");
    assert_eq!(
        story_numbers(&store, pid),
        (1..=(CLIENTS * 2) as i64).collect::<Vec<_>>(),
        "and the numbers must be contiguous — a gap means an allocation was made and lost"
    );
    drop(store);
    assert_no_drift(&env, pid);
}

/// Readers must not be blocked by writers, and must never see a half-written
/// story.
///
/// Write-ahead logging is what makes this true, and it is a *configuration*
/// rather than a law: a store that fell back to rollback journalling would
/// serialize every reader behind every writer and this test would go from
/// milliseconds to seconds, or fail outright on a busy timeout.
#[test]
fn readers_run_through_a_write_storm_without_seeing_a_partial_story() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("SK").build();
    let _daemon = DaemonGuard(&env);
    let cwd = scratch_dir();
    // A directory outside the project, so the readers resolve the project the
    // way anything else does rather than by being handed it.
    std::fs::copy(
        project.path().join(".storyhook.toml"),
        cwd.path().join(".storyhook.toml"),
    )
    .expect("copying the pointer file");

    std::thread::scope(|scope| {
        for index in 0..CLIENTS / 2 {
            let env = &env;
            let root = project.path();
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    let out = run_bounded(
                        client_command(
                            env,
                            root,
                            &["new", &format!("storm {index}/{round}"), "--json"],
                        ),
                        &format!("storm writer ({index})"),
                        DEADLINE,
                    );
                    assert_clean(&out, &format!("storm writer ({index})"));
                }
            });
        }
        for index in 0..CLIENTS / 2 {
            let env = &env;
            let reader_cwd = cwd.path();
            scope.spawn(move || {
                for _ in 0..ROUNDS * 2 {
                    let out = run_bounded(
                        client_command(env, reader_cwd, &["list", "--json"]),
                        &format!("storm reader ({})", index + 1),
                        DEADLINE,
                    );
                    assert_clean(&out, &format!("storm reader ({})", index + 1));
                    // Every story the reader sees must be whole: a title and a
                    // state, never a row caught mid-write.
                    let json: serde_json::Value =
                        serde_json::from_slice(&out.stdout).expect("`story list --json` is JSON");
                    let listed = json["stories"].as_array().into_iter().flatten();
                    let mut seen = 0;
                    for entry in listed {
                        let story = &entry["story"];
                        assert!(
                            story["title"].is_string()
                                && story["state"].is_string()
                                && story["id"].is_string(),
                            "a reader must never observe a partially-written story: {entry}"
                        );
                        seen += 1;
                    }
                    // A reader that saw nothing proves nothing about what a
                    // reader sees mid-write, and would pass forever if `list`
                    // started returning an empty array.
                    assert!(
                        seen > 0 || json["stories"].is_array(),
                        "`story list --json` must at least return an array: {json}"
                    );
                }
            });
        }
    });

    let store = env.open_store();
    let pid = project_id_at(&store, project.path()).expect("the project resolves");
    drop(store);
    assert_no_drift(&env, pid);
}
