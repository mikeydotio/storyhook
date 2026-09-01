//! Killing the process that owns storyhook's write transaction.
//!
//! The CLI reaches the store only through a daemon, so the process that owns a
//! write transaction **is** the daemon. A crash test that killed the client
//! would be killing a process that holds nothing: it has already handed the work
//! over the wire, and the transaction it is waiting on belongs to somebody else.
//!
//! So every case arms a daemon of its own, gives it exactly one command, and
//! asks what the corpse left behind. The arming is why the daemon is spawned by
//! hand rather than auto-started: choosing a child's environment is something
//! only the process that spawns it can do.
//!
//! # The two hazards that make this shape pass while testing nothing
//!
//! Both are asserted here rather than left to each caller, because a caller that
//! forgets one still looks like a test.
//!
//! 1. **A daemon that was already running.** A client starts one whenever none
//!    is live, so a fixture that built its project through the daemon still has
//!    one — and *it*, not the armed process, would take the work. The corpse
//!    would be the wrong process and the fault would never fire.
//!    [`crash_the_daemon`] stands the incumbent down before arming, and asserts
//!    that it did.
//! 2. **A daemon running at inspection.** A question about bytes on disk is
//!    answered out of a live daemon's page cache, and opening the store
//!    checkpoints the write-ahead log a test may be asking about. Asserted once
//!    the corpse has been reaped, so that whatever the caller does next is
//!    looking at the file.
//!
//! Neither hazard is hypothetical: the first is why the hand-armed daemon case
//! failed when the whole suite was first run with no local transport at all.

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Output, Stdio};
use std::time::Duration;

use storyhook::daemon::lifecycle::{SERVED_DEADLINE, SPAWN_LOCK_DEADLINE};
use storyhook::store::{DELIVERY_BACKSTOP, FaultPoint};

use crate::env::TestEnv;
use crate::server::{ChildGuard, PORTFILE_DEADLINE, port_of, run_bounded, wait_for_server};

// ---------------------------------------------------------------------------
// The three deadlines, and what each one disproves (SH-528)
// ---------------------------------------------------------------------------
//
// Every wait in this file used to be unbounded, and one of them wedged
// `make test` for ten hours and twenty-one minutes while holding this
// machine's `gate` lock. Nothing above a test bounds it — `run-tests.sh`,
// `run-rust-battery.sh`, `leg.sh` and `verify-pr.sh` carry no `timeout`
// between them — so the bound has to live here.
//
// Each constant below is derived from the bound it is meant to prove was not
// reached, never picked (SH-394), and each is deliberately *larger* than that
// bound so the mechanism underneath reports itself first and names its own
// cause. A harness that wins that race reports an anonymous timeout in place
// of a real diagnosis, which is the opposite of the point.

/// How long the client whose daemon is about to die gets.
///
/// The wait is over **a `story` command the daemon may be killed part-way
/// through**. Derived from the two bounds that client enforces on itself: it
/// may spend up to [`SPAWN_LOCK_DEADLINE`] inside `lifecycle::ensure` waiting
/// for the spawn lock, and then up to [`SERVED_DEADLINE`] on a daemon whose
/// in-flight record has stopped moving (`served_deadline()` adds a hook
/// allowance on top, which is zero for every fixture here — none configures a
/// hook). This sits above their sum so a genuinely wedged client reports
/// itself, with its own message, before this fires; what is left for this to
/// catch is the case the client's own clocks structurally cannot see — the
/// parent blocking in `read_to_end` on a pipe some descendant inherited and
/// holds (SH-94, SH-141, SH-142), which is what [`run_bounded`] exists for.
///
/// One limit of that primitive, stated rather than glossed: on the timeout
/// path `run_bounded` neither joins its worker thread nor kills the child —
/// its own doc says why, the worker is blocked in a syscall nothing can
/// interrupt. Here the survivor is a `story` client waiting on the armed
/// daemon, and the panic drops that daemon's [`ChildGuard`], which kills it,
/// so the client ends with it. That is a consequence of this call site rather
/// than a promise of the primitive, which is why it is written down here.
const CLIENT_DEADLINE: Duration = Duration::from_secs(
    SERVED_DEADLINE.as_secs() + SPAWN_LOCK_DEADLINE.as_secs() + MARGIN.as_secs(),
);

/// How long an armed daemon has to die once its client has been answered.
///
/// The wait is over **a process that has already been told to die**. By the
/// time [`crash_the_daemon`] reaches it the client has returned, so the fault
/// either fired (and [`DELIVERY_BACKSTOP`] makes that process's death certain
/// within it, by `SIGKILL` or by the `abort()` at the far end) or it never
/// fired at all. So exceeding this does not mean "slow" — it *proves* the
/// second case, which is why the message this bound carries names it as a
/// finding rather than as a timeout.
const ARMED_DEATH_DEADLINE: Duration =
    Duration::from_secs(DELIVERY_BACKSTOP.as_secs() + MARGIN.as_secs());

/// How long a daemon armed at a start-up point has to reach it and die.
///
/// The wait is over **a process coming up, plus a death**. The migration
/// points fire inside `open_store`, which runs strictly before the daemon
/// binds listeners and publishes its portfile — so time-to-reach-the-point is
/// bounded above by time-to-publish, and [`PORTFILE_DEADLINE`] is the bound
/// this crate already stakes on exactly that phase three functions away, in
/// [`port_of`]. Then [`DELIVERY_BACKSTOP`] for the death itself. A hand-spawned
/// `daemon --serve` takes no spawn lock (only `lifecycle::ensure` does), so
/// [`SPAWN_LOCK_DEADLINE`] deliberately does not appear here — importing it
/// would assert a relationship that does not exist (SH-140).
///
/// # Why the start-up term is three times `PORTFILE_DEADLINE` and not one
///
/// `port_of`'s phase and this one are not the same work. Every caller of
/// [`crash_a_starting_daemon`] plants an **old** store, so this daemon
/// additionally takes and *verifies* a pre-migration backup (a `VACUUM INTO`
/// plus a read-back) before it reaches the point — work `port_of` never waits
/// on, because in that fixture the store is already current. Bounding the two
/// identically would be asserting a relationship that does not exist in the
/// other direction (SH-140 both ways round), and on a machine measured at 873s
/// for a suite that runs in 36s idle it is how a rare ten-hour hang becomes a
/// frequent false red — SH-306's pressure with the sign flipped. Three times
/// over, because what is being distinguished here is "never" from "slow", and
/// the cost of being generous is paid only on a run that is already failing.
const STARTING_DEATH_DEADLINE: Duration = Duration::from_secs(
    3 * PORTFILE_DEADLINE.as_secs() + DELIVERY_BACKSTOP.as_secs() + MARGIN.as_secs(),
);

/// The headroom every deadline above adds to the bound it derives from.
///
/// One margin rather than three literals, because all three buy the same
/// thing: the difference between "slow" and "never" on a machine routinely
/// running three or four concurrent worktree suites. It pays for process
/// start, for a reap under contention, and for the scheduling noise a
/// hand-picked ceiling would otherwise be a silent opinion about (SH-394).
const MARGIN: Duration = Duration::from_secs(5);

/// What one armed daemon left behind, and what its client was told.
pub struct Crash {
    armed_pid: u32,
    daemon: ExitStatus,
    client: Output,
}

impl Crash {
    /// How the armed daemon died.
    pub fn daemon(&self) -> ExitStatus {
        self.daemon
    }

    /// The process id this crash armed — and, because a [`Crash`] only exists
    /// once that process has been reaped, the process id that died.
    pub fn armed_pid(&self) -> u32 {
        self.armed_pid
    }

    /// What the client saw. Unlike the daemon, the client survives, and for
    /// several cases what it *said* is the whole subject.
    pub fn client(&self) -> &Output {
        &self.client
    }

    /// The client's stderr, decoded.
    pub fn client_stderr(&self) -> String {
        String::from_utf8_lossy(&self.client.stderr).into_owned()
    }
}

/// Runs `story <args>` in `cwd` against a daemon armed to die at `point`.
///
/// For the points inside a write transaction. The daemon starts normally, serves
/// the one request this makes, and is killed part-way through it — which is the
/// experiment, so a daemon that exits any other way is a failure naming the most
/// likely cause.
pub fn crash_the_daemon(env: &TestEnv, cwd: &Path, point: FaultPoint, args: &[&str]) -> Crash {
    let mut armed = arm_a_daemon(env, cwd, point);
    wait_for_server(port_of(env, armed.pid()));

    let armed_pid = armed.pid();
    let mut client_cmd = client_command(env, cwd);
    client_cmd.args(args);
    let client = run_bounded(
        client_cmd,
        &format!("story {}", args.join(" ")),
        CLIENT_DEADLINE,
    );
    let daemon = armed.wait_within(ARMED_DEATH_DEADLINE, || {
        the_fault_never_fired(env, point, args, &client)
    });

    assert_eq!(
        daemon.signal(),
        Some(libc::SIGKILL),
        "{}: the daemon running `story {}` was supposed to be killed at {}; it finished with \
         {daemon:?} instead.\nclient stdout: {}\nclient stderr: {}",
        diagnose(daemon.signal()),
        args.join(" "),
        point.as_str(),
        String::from_utf8_lossy(&client.stdout),
        String::from_utf8_lossy(&client.stderr),
    );
    assert_no_daemon(env, "after the crash");

    Crash {
        armed_pid,
        daemon,
        client,
    }
}

/// Kills a daemon at a point it reaches while *opening* the store, before it has
/// bound anything.
///
/// The migration points live here. They fire inside `open_store`, which runs
/// before the daemon claims its pidfile or binds a port, so there is no client
/// to send and nothing to wait for — the process simply dies on the way up.
pub fn crash_a_starting_daemon(env: &TestEnv, cwd: &Path, point: FaultPoint) -> ExitStatus {
    let mut armed = arm_a_daemon(env, cwd, point);
    let status = armed.wait_within(STARTING_DEATH_DEADLINE, || {
        the_fault_never_fired_on_the_way_up(env, point)
    });
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "{}: {} must be reachable while a daemon carries the store forward; the daemon \
         finished with {status:?} instead",
        diagnose(status.signal()),
        point.as_str(),
    );
    assert_no_daemon(env, "after the crash");
    status
}

/// Spawns `story daemon --serve` in `cwd`, optionally armed to die at `point`.
///
/// The building block the two fixtures above are made of, exposed because the
/// migration race needs several at once and needs to decide for itself what
/// "finished" means for each.
///
/// `--port 0`, not a `reserve_port()`-picked number (SH-195): every caller here
/// learns the daemon's *real* port from its portfile ([`port_of`]) rather
/// than trusting the one it asked for, because `bind_preferred` treats a
/// requested port as a preference and falls back to a kernel-assigned one the
/// moment it is taken. A pre-picked port bought nothing these callers needed
/// and cost a genuine bind-then-release TOCTOU window against whatever else on
/// the machine might claim it in the milliseconds before this daemon actually
/// binds; `--port 0` has no such window, because there is no separate
/// reservation step to race.
///
/// # stderr goes to a file, not to `/dev/null` (SH-528)
///
/// It used to be discarded, which meant a crash fixture that failed had
/// nothing at all from the process it was *about* — including the panic
/// message of a daemon that refused to start. A **file** rather than a pipe,
/// because a daemon outlives the command that started it and a pipe's reader
/// waits for the last writer to close: piping a daemon's stderr into the test
/// process is the exact deadlock SH-94 cost this repository four minutes of
/// `read(2)` to find. Appended rather than truncated, so the eight racers in
/// `crash_matrix.rs`'s migration case all keep their say.
pub fn spawn_daemon(env: &TestEnv, cwd: &Path, point: Option<FaultPoint>) -> ChildGuard {
    let mut serve = env.raw_story(cwd);
    if let Some(point) = point {
        // Before arming, not after failing (SH-528). A binary that cannot fire
        // faults answers this command normally and then serves for ever, and
        // every bound below would sit out its whole deadline discovering that
        // — one door, memoized, so the whole battery pays one spawn.
        crate::env::assert_the_binary_can_fire_faults();
        serve.env("STORYHOOK_FAULT", point.as_str());
    }
    serve
        .args(["daemon", "--serve", "--port", "0"])
        .stdout(Stdio::null())
        .stderr(armed_daemon_log(env));
    ChildGuard::new(serve.spawn().expect("spawning a daemon"))
}

/// Where [`spawn_daemon`] sends a hand-spawned daemon's stderr.
///
/// Beside the store, because that is the one directory every fixture in this
/// module has already created by the time it spawns anything, and because a
/// daemon's runtime state is keyed by its store path anyway (SH-113).
fn armed_daemon_log_path(env: &TestEnv) -> std::path::PathBuf {
    env.store_path().with_extension("armed-daemon.log")
}

/// The log above, opened for appending — or `/dev/null` if it cannot be, since
/// a fixture must not fail over its own diagnostics.
fn armed_daemon_log(env: &TestEnv) -> Stdio {
    if let Some(parent) = env.store_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(armed_daemon_log_path(env))
        .map_or_else(|_| Stdio::null(), Stdio::from)
}

/// The tail of what the hand-spawned daemons in this environment have said.
///
/// Bounded rather than dumped whole: what a reader needs is the last thing the
/// process said before it stopped saying anything.
fn armed_daemon_stderr(env: &TestEnv) -> String {
    let path = armed_daemon_log_path(env);
    let Ok(whole) = std::fs::read_to_string(&path) else {
        return format!("(no armed-daemon stderr at {})", path.display());
    };
    if whole.trim().is_empty() {
        return format!(
            "(the armed daemon said nothing; {} is empty)",
            path.display()
        );
    }
    let tail: Vec<&str> = whole.lines().rev().take(STDERR_TAIL_LINES).collect();
    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// How much of the armed daemon's stderr a diagnostic carries.
const STDERR_TAIL_LINES: usize = 40;

/// Fails unless this environment has no daemon holding its store.
///
/// The lock rather than the portfile: a portfile outlives the process that wrote
/// it, and the question here is whether anything is holding the database *now*.
pub fn assert_no_daemon(env: &TestEnv, when: &str) {
    assert!(
        !env.daemon_is_live(),
        "a daemon is holding the store {when}. It answers reads from its own page cache \
         and keeps the write-ahead log alive, so every question this test asks about the \
         file would be asked of memory instead."
    );
}

/// What a corpse that is not a `SIGKILL` most likely means.
///
/// Two answers, and telling them apart is the difference between a five-minute
/// fix and an afternoon:
///
/// * **`SIGABRT`** — the fault *fired*. `kill(getpid(), SIGKILL)` posts a signal
///   rather than stopping the calling thread, so the instruction after it can
///   still run; when that instruction was an `abort`, the process died of the
///   wrong signal and a crash test read "the fault did not fire". Pinned by
///   `tests/fault_injection.rs`.
/// * **anything else, including a clean exit** — the fault did not fire, and the
///   usual reason is a binary built without the `fault-injection` feature.
fn diagnose(signal: Option<i32>) -> &'static str {
    match signal {
        Some(libc::SIGABRT) => {
            "the daemon aborted, which means the fault fired and then lost the race with the \
             instruction after it — see `process_env_fault` in src/store/fault.rs"
        }
        _ => {
            "the fault never fired. The usual cause is a binary built without the \
             `fault-injection` feature — which is every binary `cargo build` produces, and \
             none that `cargo test` does"
        }
    }
}

/// What [`ARMED_DEATH_DEADLINE`] elapsing means, and every scrap of evidence
/// that names which case it was.
///
/// # Why this is a finding rather than a timeout
///
/// [`DELIVERY_BACKSTOP`] makes an armed process's death certain once the fault
/// fires. So a daemon still running after that has not died slowly — **it
/// never reached `point` at all**, and no `SIGKILL` delivery race can account
/// for it. That is the whole diagnosis, and it is stated first, because the
/// first hypothesis a reader forms about a crash test that did not crash is
/// the wrong one (SH-528 was filed on it).
///
/// Everything after it is the evidence the unbounded wait used to destroy: the
/// client's own answer, which is where a command that failed before ever
/// reaching a write transaction says so; whether anything is still holding the
/// store; which pid the portfile names *now*, since [`port_of`] already
/// checked it once before the command was sent and a different answer here
/// means the client's work went somewhere else; and the armed daemon's own
/// stderr.
///
/// The single most common cause is named explicitly, because it is invisible
/// from inside the test: `story` carries the store's fault points only when it
/// is built with the `fault-injection` feature, which reaches it through the
/// test-support dev-dependency and therefore through `cargo test` and never
/// through `cargo build`. Both write `target/debug/story`. This was confirmed
/// by toggle, not inferred: `cargo build --bin story` over a green tree
/// reproduces this exact hang every time, and rebuilding with the feature
/// clears it.
fn the_fault_never_fired(
    env: &TestEnv,
    point: FaultPoint,
    args: &[&str],
    client: &Output,
) -> String {
    format!(
        "the daemon armed at {point} is still running, which proves the fault never fired \
         rather than that it died slowly: `process_env_fault` bounds an armed process's \
         life at {DELIVERY_BACKSTOP:?} (SIGKILL, then abort), so there is no delivery race \
         that ends here.\n\n\
         The likeliest cause is a `story` binary built WITHOUT the `fault-injection` \
         feature, which makes `store::fault::fire` an inlined `Ok(())`. That feature \
         reaches the binary only through the test-support dev-dependency, so `cargo test` \
         builds one that can fire and `cargo build` builds one that cannot — to the same \
         path. Check it directly: `strings target/debug/story | grep -c STORYHOOK_FAULT` \
         is 1 on a binary that can fire and 0 on one that cannot.\n\n\
         command:        story {command}\n\
         client status:  {status:?}\n\
         client stdout:  {stdout}\n\
         client stderr:  {stderr}\n\
         store held now: {held}\n\
         portfile names: {portfile:?}\n\
         armed daemon stderr (last {STDERR_TAIL_LINES} lines):\n{daemon_stderr}",
        point = point.as_str(),
        command = args.join(" "),
        status = client.status,
        stdout = String::from_utf8_lossy(&client.stdout),
        stderr = String::from_utf8_lossy(&client.stderr),
        held = env.daemon_is_live(),
        portfile = env.daemon().map(|info| info.pid),
        daemon_stderr = armed_daemon_stderr(env),
    )
}

/// [`the_fault_never_fired`], for the start-up points.
///
/// There is no client here, so the evidence is thinner by construction — which
/// is itself worth saying, since a reader who expects a client's stderr and
/// finds none should know that is the fixture's shape rather than a lost
/// message.
///
/// One extra cause belongs on this path and not the other: the migration
/// points only fire when a migration is actually pending, so an armed daemon
/// that opened an already-current store reaches nothing and serves normally.
/// `crash_matrix.rs`'s own migration race documents that behaviour; a fixture
/// that forgot to plant an old store gets it by accident.
fn the_fault_never_fired_on_the_way_up(env: &TestEnv, point: FaultPoint) -> String {
    format!(
        "the daemon armed at {point} is still running, which proves the fault never fired \
         rather than that it died slowly: `process_env_fault` bounds an armed process's \
         life at {DELIVERY_BACKSTOP:?} (SIGKILL, then abort), so there is no delivery race \
         that ends here. No client is involved on this path, so there is no client output \
         to read.\n\n\
         Two causes reach here. Either the `story` binary was built WITHOUT the \
         `fault-injection` feature, which makes `store::fault::fire` an inlined `Ok(())` \
         — `cargo test` builds one that can fire and `cargo build` builds one that cannot, \
         to the same path, so check `strings target/debug/story | grep -c \
         STORYHOOK_FAULT` — or this store had no migration pending, so `open_store` never \
         reached {point} and the daemon went on to serve.\n\n\
         store held now: {held}\n\
         portfile names: {portfile:?}\n\
         armed daemon stderr (last {STDERR_TAIL_LINES} lines):\n{daemon_stderr}",
        point = point.as_str(),
        held = env.daemon_is_live(),
        portfile = env.daemon().map(|info| info.pid),
        daemon_stderr = armed_daemon_stderr(env),
    )
}

/// Stands down whatever daemon is holding the store, then spawns one armed at
/// `point`.
fn arm_a_daemon(env: &TestEnv, cwd: &Path, point: FaultPoint) -> ChildGuard {
    env.stop_daemon();
    assert_no_daemon(env, "before the armed one could start");
    spawn_daemon(env, cwd, Some(point))
}

/// A `story` command, which is a client of whatever daemon is holding the store
/// — and, by the time this is called, of the one this test armed.
fn client_command(env: &TestEnv, cwd: &Path) -> std::process::Command {
    env.raw_story(cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::AssertUnwindSafe;
    use std::time::Instant;

    /// **The regression test for SH-528**, and the reproduction the story was
    /// filed without.
    ///
    /// # What is being provoked, and why it is deterministic
    ///
    /// The reported hang was an armed daemon that never fired its fault at
    /// all: it served its client normally and then served forever, while
    /// `crash_the_daemon` blocked in what was then an unbounded wait. That is
    /// the *only* branch an indefinite hang can reach — [`DELIVERY_BACKSTOP`]
    /// makes a fault that fires fatal within it — so reproducing the class
    /// means arming a point the command will not reach.
    ///
    /// [`FaultPoint::MidMigration`] fires inside `open_store`'s migration
    /// loop, and this fixture's store is already at the current schema (which
    /// the assertion below states rather than assumes, so a future change that
    /// makes the point reachable here turns this red with a comprehensible
    /// message instead of quietly changing the subject). So the armed daemon
    /// opens, finds nothing pending, reaches nothing, and lives — with no
    /// race, no sleep, and no dependence on scheduling.
    ///
    /// # The original mechanism, for the record
    ///
    /// In the wild the same branch was reached a different way: a `story`
    /// binary built without the `fault-injection` feature, which makes
    /// `store::fault::fire` an inlined `Ok(())`. Confirmed by toggle against
    /// this very test binary — `cargo build --bin story` over a green tree
    /// reproduced the hang every time, and rebuilding with the feature cleared
    /// it. That cause cannot be provoked from inside a test, because every
    /// `cargo test` build carries the feature, which is exactly why the
    /// *class* is pinned here through the branch that can be.
    #[test]
    fn an_armed_daemon_that_never_reaches_its_point_is_reported_rather_than_waited_out() {
        let env = TestEnv::isolated();
        let project = env.project().prefix("NF").build();
        assert_eq!(
            schema_version_of(&env),
            storyhook::store::current_schema_version(),
            "this case needs a store with NO migration pending, so that an armed \
             `mid_migration` is unreachable and the fault genuinely cannot fire"
        );

        let started = Instant::now();
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            crash_the_daemon(
                &env,
                project.path(),
                FaultPoint::MidMigration,
                &["new", "the daemon will never die for this"],
            )
        }));

        let Err(payload) = outcome else {
            panic!("an armed daemon that never reaches its point must be REPORTED, not waited out")
        };
        let message = panic_message(payload.as_ref());
        assert!(
            message.contains("proves the fault never fired"),
            "the failure must name the finding rather than merely time out, since the first \
             hypothesis a reader forms about a crash test that did not crash is the wrong \
             one:\n{message}"
        );
        assert!(
            message.contains("STORYHOOK_FAULT"),
            "and it must name the check that settles the likeliest cause:\n{message}"
        );
        assert!(
            message.contains("client status"),
            "and it must carry the client's own answer, which is the evidence the unbounded \
             wait used to destroy:\n{message}"
        );

        // Derived from the bound it is meant to prove was not exceeded, never
        // a bare literal (SH-394). Three times over is headroom for a machine
        // running several suites at once; what is being distinguished is
        // "reported" from "waited out", not "fast" from "slow".
        let give_up_ceiling = ARMED_DEATH_DEADLINE * 3;
        let elapsed = started.elapsed();
        assert!(
            elapsed < give_up_ceiling,
            "the bound must end this, not the harness above it: took {elapsed:?} against a \
             {ARMED_DEATH_DEADLINE:?} deadline"
        );

        assert_no_daemon(
            &env,
            "after the bound fired — a failure that leaks the very daemon it is about is the \
             other half of what SH-528 cost (SH-493)",
        );
    }

    /// The `PRAGMA user_version` of this environment's store.
    ///
    /// The daemon is stood down first: a live one answers out of its own page
    /// cache rather than out of the file.
    fn schema_version_of(env: &TestEnv) -> u32 {
        env.stop_daemon();
        storyhook::store::SqliteStore::open(env.store_path())
            .expect("opening the fixture store")
            .schema_version()
            .expect("reading the schema version")
    }

    /// The text of a caught panic, whichever of the two shapes it took.
    fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .unwrap_or_else(|| "a panic carrying neither a String nor a &str".to_string())
    }
}
