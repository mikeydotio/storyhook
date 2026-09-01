//! `scripts/run-tests.sh` taking the machine-wide `gate` lock — **provoked**,
//! not inspected (SH-457).
//!
//! The style is `tests/machine_lock.rs`'s, which is `tests/orphan_check.rs`'s:
//! real processes, real contention, and the **tracked** script itself reached
//! by symlink from a disposable fixture root, never a copy and never a mock.
//!
//! # Why the gate serializes at all
//!
//! `make test` is 36.375s median warm and idle
//! (`docs/rearch/baseline/timings.md`) and has been measured at 873s under the
//! three-to-four concurrent worktree suites this machine routinely runs —
//! contention that is the documented cause of an open class of load-sensitive
//! failures (SH-347, SH-349, SH-375, SH-378, SH-401, SH-419). Full Auto
//! (SH-452) runs N agent lanes at once and multiplies exactly that, so
//! decision D4 makes every `make test` on the machine queue. SH-456 shipped
//! the primitive; this story takes the first of its two reserved names.
//!
//! # How a case can run the real script safely, and fast
//!
//! Four things make that true, and every one of them is load-bearing:
//!
//! * `STORYHOOK_LOCK_DIR` points at the fixture's own lock root, and
//!   `STORYHOOK_MACHINE_LOCKS` is removed from the environment. Without the
//!   second, every case here would inherit the `gate` lock held by the very
//!   `make test` running this suite, take the reentrancy branch, and prove
//!   nothing while passing.
//! * `--only-no-doc` with **no** names is a real, complete invocation of the
//!   script that runs no tests at all — the whole lock path executes and the
//!   run costs milliseconds.
//! * `scripts/tracked-tree.sh` is stubbed to fail inside the fixture, so
//!   `scripts/test-delta.sh` is never reached and no ledger is written into
//!   the developer's `.git`. The tracked script tolerates an unresolvable
//!   tree oid by design, so this exercises a path it already has.
//! * `cargo` is a fake on the fixture's own `PATH` for the two cases that need
//!   a leg to fail or to be slow. Nothing here builds anything.
//!
//! # Mutation checks (SH-295: a pin that cannot fail is not a pin)
//!
//! Measured by hand, with the script snapshotted by `cp` first — never
//! `git checkout --`, which reverts the fix under test as well and lets the
//! suite pass vacuously. Counts are out of this file's own tests.
//!
//! * The whole lock block deleted → **6 red**. The three that stay green are
//!   the ones whose subject is not the lock: the wiring fence below, the
//!   depth guard, and the "an ordinary run is quiet" control — which is
//!   exactly right, since a script that never locks is indeed quiet.
//! * The bypass taken but left silent → **1 red**:
//!   `the_bypass_runs_without_the_lock_and_says_so_on_stderr`. Nothing else,
//!   which is the point: a working bypass that says nothing is the SH-306
//!   shape no behavioural assertion can see.
//! * The bypass lever misspelled, so `STORYHOOK_GATE_LOCK=0` never fires →
//!   **1 red**, the same test. It reds slowly (the run queues behind the
//!   fixture's own holder for the whole of its hold) rather than instantly,
//!   which is a fact about that mutation, not a weakness of the assertion.
//! * The re-exec handshake never recognised → **6 red**, promptly. Before
//!   the script carried its depth guard this same mutation was not red at
//!   all: it fork-bombed, at roughly two hundred processes a second. See
//!   `a_re_exec_that_comes_back_refuses_instead_of_forking_forever`.
//! * The depth guard's own ceiling raised out of reach → **1 red**: that
//!   test alone, which is the correct blast radius for a guard nothing else
//!   provokes.
//! * The take moved BELOW the isolated data root → **5 red**. Not only the
//!   wiring fence: the block moved past the argument parser too, so the
//!   behavioural cases go with it. The fence is what names the actual
//!   finding.
//!
//! And in the other direction, to prove the suite is not vacuous: with the
//! script as shipped, **all 9 tests pass**.
//!
//! One mutation deliberately has no test and is recorded rather than fenced:
//! deleting `unset STORYHOOK_GATE_LOCK_TAKEN`, which leaks the handshake into
//! every test binary the suite starts. Nothing here observes it, because the
//! damage lands in a *different* suite — a test that itself invoked this
//! script would skip the lock while believing it held one. The `env_remove`
//! calls in this file's own fixture are the same defence applied one level
//! down, and they are load-bearing for exactly that reason.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Constants, derived rather than copied
// ---------------------------------------------------------------------------

/// How many of the lock's own poll cycles a fixture may spend waiting for a
/// process to reach an expected state. Generous on purpose — this machine runs
/// several suites at once, and the thing under test is never the latency.
const WAIT_POLLS_ALLOWED: u64 = 30;

/// The checkout under test — the tracked scripts live here.
fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Reads a repository file, failing with the path rather than silently
/// skipping — a moved or renamed source file is a finding, not a reason for a
/// pin to go quiet.
fn read_checkout_file(relative: &str) -> String {
    let path = checkout().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

/// `machine-lock.sh`'s own poll period, parsed out of the artifact rather than
/// copied into this file — a copy is a second place to disagree (SH-136, four
/// times over in this project).
fn lock_poll_secs() -> u64 {
    let src = read_checkout_file("scripts/machine-lock.sh");
    let after = src
        .split("readonly LOCK_POLL_SECS=")
        .nth(1)
        .expect("scripts/machine-lock.sh must declare `readonly LOCK_POLL_SECS=<n>`");
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("LOCK_POLL_SECS's value {digits:?} must parse: {e}"))
}

/// How long a fixture's own lock holder must stay alive.
///
/// SH-493's lesson one subsystem over: a test's own population is a deadline
/// like any other — derive how long it must hold from the window it has to
/// cover, never from a bare literal. The window is [`wait_for`]'s own ceiling,
/// doubled, so a holder outlives the longest wait any case here may
/// legitimately spend before that case gets to decide the outcome itself.
fn hold_secs() -> u64 {
    lock_poll_secs() * WAIT_POLLS_ALLOWED * 2
}

/// Long enough for a spawned run to have reached the lock and been refused.
///
/// Not a speed assertion: the run reaches the lock within a handful of bash
/// statements, and this is two whole observation periods of the lock it is
/// being refused by — the smallest multiple that still tolerates one lost
/// cycle on a loaded machine.
fn time_to_reach_the_lock() -> Duration {
    Duration::from_secs(lock_poll_secs() * 2)
}

/// Blocks until `path` exists, or panics. The ceiling derives from the lock's
/// own poll period rather than being a bare literal (SH-394).
fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(lock_poll_secs() * WAIT_POLLS_ALLOWED);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "{} never appeared within the allowed poll cycles",
        path.display()
    );
}

/// Blocks until `path` no longer exists, or panics.
fn wait_for_gone(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(lock_poll_secs() * WAIT_POLLS_ALLOWED);
    while Instant::now() < deadline {
        if !path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "{} was still there after the allowed poll cycles",
        path.display()
    );
}

fn signal(pid: u32, name: &str) {
    let status = Command::new("kill")
        .args([&format!("-{name}"), &pid.to_string()])
        .status()
        .expect("running kill");
    assert!(
        status.success() || status.code() == Some(1),
        "kill -{name} {pid}: {status:?}"
    );
}

/// The pid the fake `cargo` recorded for itself. It `exec`s its sleep, so the
/// pid it printed before that is the pid of the process actually running — a
/// shell that merely started one would leave a child this test could not name.
fn fake_cargo_pid(pidfile: &Path) -> u32 {
    std::fs::read_to_string(pidfile)
        .expect("reading the fake cargo's pid")
        .trim()
        .parse()
        .expect("the fake cargo's pid must parse")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or_else(|| {
        panic!(
            "the script must exit, not die of a signal: {:?}",
            out.status
        )
    })
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// A live child that is killed if a case panics before collecting it.
///
/// `storyhook_test_support::ChildGuard` cannot serve here: collecting a
/// spawned run's **stderr** needs `wait_with_output`, which consumes the
/// `Child` that guard owns privately. Repeated rather than shared, the way
/// `tests/machine_lock.rs` repeats `orphan_check.rs`'s zombie helper — the two
/// suites are deliberately independent of each other.
struct Runner(Option<Child>);

impl Runner {
    fn child(&mut self) -> &mut Child {
        self.0.as_mut().expect("the runner was already collected")
    }

    fn pid(&mut self) -> u32 {
        self.child().id()
    }

    fn finished(&mut self) -> bool {
        self.child()
            .try_wait()
            .expect("asking after a child this test started")
            .is_some()
    }

    fn collect(&mut self) -> Output {
        self.0
            .take()
            .expect("the runner was already collected")
            .wait_with_output()
            .expect("collecting a child this test started")
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Fixture {
    root: TempDir,
}

/// The one tracked script this fixture replaces rather than links.
///
/// Named as a constant because the link loop above must skip exactly it: a
/// symlink written through would put the stub into the developer's own
/// checkout, and a copy left in place would reach `test-delta.sh` and write a
/// ledger into the real `.git`.
const STUBBED_SCRIPT: &str = "tracked-tree.sh";

impl Fixture {
    fn new() -> Self {
        let root = scratch_dir();
        let path = root.path();
        for dir in ["scripts", "locks", "bin", "tests"] {
            std::fs::create_dir_all(path.join(dir))
                .unwrap_or_else(|e| panic!("fixture: creating {dir}/: {e}"));
        }
        // Every tracked `scripts/*.sh` except the one this fixture replaces,
        // derived rather than listed. The list used to name three — the ones
        // `run-tests.sh` needed on the day this file was written — and a
        // fourth dependency (`test-env.sh`, once the isolation moved into a
        // sourced function) turned every test in this file red with a `No such
        // file or directory` from inside the fixture, which reads as a broken
        // lock rather than a stale list. The same hand-kept-list shape SH-136,
        // SH-198, SH-258, SH-260/276 and SH-360 have already cost this project.
        //
        // Symlinks, never copies: the point is to run the TRACKED script.
        for entry in std::fs::read_dir(checkout().join("scripts"))
            .expect("fixture: reading the checkout's scripts/")
        {
            let entry = entry.expect("fixture: a scripts/ entry");
            let name = entry.file_name();
            let name = name.to_str().expect("a UTF-8 script name");
            if !name.ends_with(".sh") || name == STUBBED_SCRIPT {
                continue;
            }
            std::os::unix::fs::symlink(entry.path(), path.join("scripts").join(name))
                .unwrap_or_else(|e| panic!("fixture: linking the tracked {name}: {e}"));
        }
        let fixture = Self { root };
        // Refusing to answer is a path the tracked script already tolerates —
        // a tarball or a corrupt index produces it — and it is what keeps
        // `test-delta.sh` from writing a ledger into the real `.git`.
        fixture.executable(
            &format!("scripts/{STUBBED_SCRIPT}"),
            "#!/bin/sh\n# tests/gate_lock.rs: no tree oid, so no ledger is written\nexit 1\n",
        );
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn lock_root(&self) -> PathBuf {
        self.path().join("locks")
    }

    /// The directory `machine-lock.sh` would use for `name`.
    fn lock(&self, name: &str) -> PathBuf {
        self.lock_root().join(format!("{name}.lock"))
    }

    fn executable(&self, relative: &str, body: &str) -> PathBuf {
        let path = self.path().join(relative);
        let mut file = std::fs::File::create(&path)
            .unwrap_or_else(|e| panic!("creating {}: {e}", path.display()));
        file.write_all(body.as_bytes()).expect("writing a script");
        drop(file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("making a script executable");
        path
    }

    /// Installs a fake `cargo` on the fixture's own `PATH`.
    fn fake_cargo(&self, body: &str) {
        self.executable("bin/cargo", body);
    }

    /// Creates `tests/<name>.rs` inside the fixture, which is how
    /// `run-tests.sh` decides a `--only` name is an integration binary.
    fn integration_test(&self, name: &str) {
        std::fs::write(self.path().join("tests").join(format!("{name}.rs")), "")
            .expect("fixture: creating an integration test file");
    }

    fn base_command(&self, program: &str) -> Command {
        let inherited = std::env::var("PATH").unwrap_or_default();
        let mut cmd = Command::new("bash");
        cmd.arg(self.path().join("scripts").join(program))
            .current_dir(self.path())
            .env(
                "PATH",
                format!("{}:{inherited}", self.path().join("bin").display()),
            )
            .env("STORYHOOK_LOCK_DIR", self.lock_root())
            // A value inherited from the `make test` running this suite would
            // send every case down the reentrancy branch.
            .env_remove("STORYHOOK_MACHINE_LOCKS")
            // The re-exec handshake. The script consumes it, but a stray one
            // inherited from an outer run would send every case straight past
            // the lock while reporting nothing.
            .env_remove("STORYHOOK_GATE_LOCK_TAKEN")
            .env_remove("STORYHOOK_GATE_LOCK_DEPTH")
            .env_remove("STORYHOOK_GATE_LOCK");
        cmd
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = self.base_command("run-tests.sh");
        cmd.args(args);
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args)
            .output()
            .unwrap_or_else(|e| panic!("running run-tests.sh with {args:?}: {e}"))
    }

    fn spawn(&self, args: &[&str]) -> Runner {
        Runner(Some(
            self.command(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap_or_else(|e| panic!("spawning run-tests.sh with {args:?}: {e}")),
        ))
    }

    /// A real, live holder of the `gate` lock that is nothing to do with
    /// `run-tests.sh` — the other suite in the "two concurrent runs" case.
    fn spawn_gate_holder(&self) -> Runner {
        let mut cmd = self.base_command("machine-lock.sh");
        cmd.args(["gate", "--", "sleep", &hold_secs().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd.spawn().expect("spawning a gate holder");
        let runner = Runner(Some(child));
        wait_for(&self.lock("gate").join("pid"));
        runner
    }

    fn holder_pid(&self) -> String {
        std::fs::read_to_string(self.lock("gate").join("pid"))
            .expect("reading the gate lock's recorded pid")
            .trim()
            .to_string()
    }
}

// ---------------------------------------------------------------------------
// The lock is taken, and it serializes
// ---------------------------------------------------------------------------

/// The acceptance criterion, end to end: a second run does not start while a
/// first holds the gate, and it says **who** it is waiting for.
///
/// SH-306 is why the second half is asserted separately from the first: a lock
/// that serializes silently makes a wedged machine look identical to a slow
/// one, and no purely behavioural assertion can see the difference.
#[test]
fn a_second_run_waits_for_the_gate_lock_and_names_its_holder() {
    let fixture = Fixture::new();

    let mut holder = fixture.spawn_gate_holder();
    let holder_pid = fixture.holder_pid();

    let mut runner = fixture.spawn(&["--only-no-doc"]);
    std::thread::sleep(time_to_reach_the_lock());
    assert!(
        !runner.finished(),
        "the run must not have completed while the gate lock was held -- it did not serialize"
    );

    signal(holder.pid(), "TERM");
    holder.collect();

    let out = runner.collect();
    let err = stderr(&out);
    assert_eq!(code(&out), 0, "the queued run must then succeed: {err}");
    assert!(
        err.contains("waiting for the 'gate' lock"),
        "the waiter must say what it is waiting for\nstderr: {err}"
    );
    assert!(
        err.contains(&format!("pid {holder_pid}")),
        "the waiter must name the pid actually holding the lock ({holder_pid}), so a wedged machine can be traced to a process\nstderr: {err}"
    );
}

/// The positive control for "the lock is taken at all", and for whose name is
/// on it: the recorded holder is the run's **own** process.
#[test]
fn a_running_suite_is_itself_the_recorded_holder_of_the_gate_lock() {
    let fixture = Fixture::new();
    fixture.integration_test("slowleg");
    let pidfile = fixture.path().join("cargo.pid");
    fixture.fake_cargo(&format!(
        "#!/bin/sh\nprintf '%s\\n' \"$$\" > {}\nexec sleep {}\n",
        pidfile.display(),
        hold_secs()
    ));

    let mut runner = fixture.spawn(&["--only-no-doc", "slowleg"]);
    wait_for(&fixture.lock("gate").join("pid"));

    assert_eq!(
        fixture.holder_pid(),
        runner.pid().to_string(),
        "the gate lock must be recorded to the run that took it, not to some intermediate process -- a holder nobody can identify is a wedge nobody can clear"
    );

    wait_for(&pidfile);
    let cargo_pid = fake_cargo_pid(&pidfile);
    signal(runner.pid(), "TERM");
    // The leg is collected BEFORE the run is, because it inherited the run's
    // own stderr: `wait_with_output` reads that pipe to EOF, and every process
    // still holding the write end keeps it open — so collecting first would
    // block for the whole of the fake's sleep rather than for the run.
    signal(cargo_pid, "KILL");
    runner.collect();
}

/// The suite is interrupted mid-leg. A lock left behind by a killed run is a
/// machine that never runs the gate again until somebody notices.
#[test]
fn an_interrupted_run_releases_the_gate_lock() {
    let fixture = Fixture::new();
    fixture.integration_test("slowleg");
    let pidfile = fixture.path().join("cargo.pid");
    fixture.fake_cargo(&format!(
        "#!/bin/sh\nprintf '%s\\n' \"$$\" > {}\nexec sleep {}\n",
        pidfile.display(),
        hold_secs()
    ));

    let mut runner = fixture.spawn(&["--only-no-doc", "slowleg"]);
    wait_for(&fixture.lock("gate").join("pid"));
    wait_for(&pidfile);
    let cargo_pid = fake_cargo_pid(&pidfile);

    signal(runner.pid(), "TERM");
    // See the sibling case above: the leg holds the run's stderr, so it is
    // collected first or `collect` waits out the fake's whole sleep.
    signal(cargo_pid, "KILL");
    runner.collect();
    wait_for_gone(&fixture.lock("gate"));
}

/// The suite fails. `make test` failing is the ordinary case, not the
/// exceptional one, so the release path that matters most is this one.
#[test]
fn a_failing_run_releases_the_gate_lock() {
    let fixture = Fixture::new();
    // Every `cargo` invocation fails, which is what makes `--only`'s lib-target
    // lookup refuse the name below.
    fixture.fake_cargo("#!/bin/sh\nexit 1\n");

    let out = fixture.run(&["--only-no-doc", "no-such-binary"]);
    let err = stderr(&out);
    assert_ne!(code(&out), 0, "the refusal must fail the run: {err}");
    assert!(
        err.contains("no-such-binary"),
        "the refusal must name the offending word (SH-357)\nstderr: {err}"
    );
    assert!(
        !fixture.lock("gate").exists(),
        "a failing run must not leave the gate lock behind at {}",
        fixture.lock("gate").display()
    );
}

// ---------------------------------------------------------------------------
// The escape hatch, and the one case that must not wait
// ---------------------------------------------------------------------------

/// `STORYHOOK_GATE_LOCK=0` runs against a live holder — and **says so**. The
/// story asks for the message to be asserted, not merely the behaviour: this
/// project has already paid for a silent bypass once, with
/// `SKIP_PREPUSH_TESTS` (SH-306).
#[test]
fn the_bypass_runs_without_the_lock_and_says_so_on_stderr() {
    let fixture = Fixture::new();
    let mut holder = fixture.spawn_gate_holder();
    let holder_pid = fixture.holder_pid();

    let out = fixture
        .command(&["--only-no-doc"])
        .env("STORYHOOK_GATE_LOCK", "0")
        .output()
        .expect("running the bypass");
    let err = stderr(&out);

    assert_eq!(code(&out), 0, "the bypass must run the suite: {err}");
    assert!(
        err.contains("STORYHOOK_GATE_LOCK=0"),
        "the bypass must name the lever that caused it, so a reader can undo it\nstderr: {err}"
    );
    assert!(
        err.contains("gate"),
        "the bypass must name what was bypassed\nstderr: {err}"
    );
    assert!(
        !err.contains("waiting for the 'gate' lock"),
        "the bypass must not wait\nstderr: {err}"
    );
    assert!(
        !stdout(&out).contains("STORYHOOK_GATE_LOCK=0"),
        "the notice belongs on stderr, where it cannot corrupt a caller reading this script's output\nstdout: {}",
        stdout(&out)
    );
    assert_eq!(
        fixture.holder_pid(),
        holder_pid,
        "a bypassing run must leave the real holder's lock exactly as it found it"
    );

    signal(holder.pid(), "TERM");
    holder.collect();
}

/// The outer-wrap idiom `machine-lock.sh` was built for —
/// `machine-lock.sh gate -- make test` — must not deadlock on a lock its own
/// process tree already holds, and must not take a second one.
#[test]
fn a_run_whose_process_tree_already_holds_the_gate_does_not_re_take_it() {
    let fixture = Fixture::new();
    let mut holder = fixture.spawn_gate_holder();
    let holder_pid = fixture.holder_pid();

    let out = fixture
        .command(&["--only-no-doc"])
        .env("STORYHOOK_MACHINE_LOCKS", "gate")
        .output()
        .expect("running under an inherited gate lock");
    let err = stderr(&out);

    assert_eq!(code(&out), 0, "the nested run must proceed: {err}");
    assert!(
        err.contains("already held by this process tree"),
        "running without re-taking is a decision, and a decision nobody can see is the SH-306 shape\nstderr: {err}"
    );
    assert!(
        !err.contains("waiting for the 'gate' lock"),
        "a run whose own tree holds the lock must never wait on it\nstderr: {err}"
    );
    assert_eq!(
        fixture.holder_pid(),
        holder_pid,
        "the nested run must not have disturbed the recorded holder"
    );

    signal(holder.pid(), "TERM");
    holder.collect();
}

/// The common path stays quiet. Three notices exist in this script and every
/// one of them means something unusual happened; a run that printed one
/// routinely would train a reader to ignore all three.
#[test]
fn an_ordinary_run_says_nothing_about_waiting_bypassing_or_nesting() {
    let fixture = Fixture::new();
    let out = fixture.run(&["--only-no-doc"]);
    let err = stderr(&out);

    assert_eq!(code(&out), 0, "an empty selection must succeed: {err}");
    for unexpected in [
        "waiting for the 'gate' lock",
        "already held by this process tree",
        "STORYHOOK_GATE_LOCK=0",
    ] {
        assert!(
            !err.contains(unexpected),
            "an uncontended run must not report {unexpected:?}\nstderr: {err}"
        );
    }
    assert!(
        !fixture.lock("gate").exists(),
        "a completed run must leave no lock behind"
    );
}

/// A broken re-exec handshake must refuse, not loop.
///
/// This is the one mutation in this file that does **not** produce a red on
/// its own, and the reason is worth stating rather than discovering: the
/// wrapper runs its command in a background child — it has to, or a signal
/// could not reach a fifteen-minute suite — so a re-exec that keeps coming
/// back leaves a live process waiting on the next one. Measured at roughly two
/// hundred processes a second before the guard existed, on a machine that
/// routinely runs three or four other suites. `machine_lock.rs` records the
/// same lesson from the other direction: a mutation that turns a bounded wait
/// unbounded hangs `cargo test` instead of failing one test.
///
/// The guard sits in the `else` branch and the handshake it checks is
/// consumed in the `if`, so breaking either one is caught by the other
/// (SH-365's two-mechanism shape). This provokes it directly, by handing the
/// script a depth it could only have got from a previous take.
#[test]
fn a_re_exec_that_comes_back_refuses_instead_of_forking_forever() {
    let fixture = Fixture::new();
    let out = fixture
        .command(&["--only-no-doc"])
        .env("STORYHOOK_GATE_LOCK_DEPTH", "1")
        .output()
        .expect("running with a depth already recorded");
    let err = stderr(&out);

    assert_eq!(
        code(&out),
        2,
        "a handshake that is not landing must be refused, not absorbed: {err}"
    );
    assert!(
        err.contains("refusing rather than forking"),
        "the refusal must say what it is refusing and why, or the next reader will simply raise the ceiling\nstderr: {err}"
    );
    assert!(
        !fixture.lock("gate").exists(),
        "a refused run must take no lock"
    );
}

// ---------------------------------------------------------------------------
// Where the lock sits in the script
// ---------------------------------------------------------------------------

/// A wiring fence, and it says so (SH-360's sense): it proves the take is
/// written **above** the isolated data root's creation, never that a waiting
/// run holds no resources.
///
/// Behaviourally this cannot be provoked here. The evidence would be the
/// absence of a `/private/tmp/storyhook-gate.*` directory while a run waits,
/// and that directory is not attributable: this machine runs three or four
/// concurrent worktree suites, every one of which creates and removes its own.
/// A count taken across a wait would be measuring the neighbours.
///
/// Comment lines are stripped first, because this script's own header
/// discusses the lock at length — the `tests/dashboard_focus_coverage.rs`
/// lesson, one language over.
#[test]
fn the_lock_is_taken_before_the_run_builds_its_isolated_data_root() {
    let src = read_checkout_file("scripts/run-tests.sh");
    let code_only: String = src
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    let take = code_only.find("machine-lock.sh").expect(
        "scripts/run-tests.sh must reach scripts/machine-lock.sh in code, not only in prose",
    );
    let data_root = code_only
        .find("mktemp -d /private/tmp/storyhook-gate")
        .expect("scripts/run-tests.sh must still build its isolated data root");

    assert!(
        take < data_root,
        "the gate lock must be taken before the run builds anything it would have to clean up -- a queued run should hold no temp directory and no EXIT trap it has not installed yet"
    );
}
