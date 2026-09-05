//! `scripts/machine-lock.sh`, **provoked** — not inspected (SH-456).
//!
//! The style is `tests/orphan_check.rs`'s, which the design of record names as
//! the precedent: real processes, real contention, and the **tracked** script
//! itself reached by symlink from a disposable fixture root, never a copy and
//! never a mock. `STORYHOOK_LOCK_DIR` points every case inside its own
//! fixture, so no test here can see — or be seen by — the developer's real
//! locks or one of the three-to-four concurrent worktree suites this machine
//! runs.
//!
//! # What this lock is for
//!
//! Full Auto (SH-452) runs N agent lanes at once. `make test` is 36.375s
//! median warm and idle (`docs/rearch/baseline/timings.md`) and has been
//! measured at 873s under that concurrency, which is the documented cause of
//! an open class of load-sensitive failures. D4 and D5 therefore serialize
//! every `make test` and every lane merge behind two machine-wide locks;
//! SH-456 ships the primitive, SH-457 and SH-458 take the two names.
//!
//! # Mutation checks (SH-295: a pin that cannot fail is not a pin)
//!
//! Measured by hand, with the script snapshotted by `cp` first — never
//! `git checkout --`, which reverts the fix under test as well and lets the
//! suite pass vacuously. Counts are out of this file's own tests.
//!
//! * The `kill -0` staleness check deleted, so a recorded holder is always
//!   believed → **1 red**: `a_lock_left_by_a_dead_pid_is_reclaimed`. Nothing
//!   else, which is the right blast radius: every other case's holder is
//!   either genuinely alive or genuinely this process.
//! * The start-time comparison deleted, so a pid alone is the identity →
//!   **1 red**: `a_lock_whose_pid_was_reused_is_reclaimed`. The live-holder
//!   control stays green, which is what proves the two are separable. This is
//!   the mutation that forced `reclaim_deadline` (below): unbounded, the wrong
//!   implementation waits on a live pid forever and hangs `cargo test` instead
//!   of turning one test red.
//! * The lock-name validation deleted → **1 red**:
//!   `a_name_that_would_escape_the_lock_root_is_refused`.
//! * The waiter's `note` calls deleted, leaving the wait silent → **1 red**:
//!   `a_waiter_names_the_holder_and_how_long_it_waited`. Mutual exclusion
//!   stays green, which is the point: a correct lock that says nothing is
//!   exactly the SH-306 shape a behavioural test cannot see.
//! * The three signal traps deleted, leaving only `trap ... EXIT` → **1 red**:
//!   `a_signal_releases_the_lock_and_takes_the_command_with_it`. This is the
//!   one that cannot be reviewed by eye: with the command in the foreground
//!   the EXIT trap alone looks sufficient and is not, because bash defers a
//!   trap until the foreground command returns.
//! * `lock_root` changed to read `$XDG_STATE_HOME` — the plausible, wrong
//!   implementation — → **1 red**:
//!   `the_lock_root_ignores_xdg_state_home_because_the_gate_rewrites_it`.
//!
//! And in the other direction, to prove the suite is not vacuous: with the
//! script as shipped, **all 26 tests pass**.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use storyhook_test_support::{ChildGuard, scratch_dir};
use tempfile::TempDir;

/// The checkout under test — the tracked script lives here.
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

/// A disposable root holding a symlink to the tracked script and its own lock
/// directory. The symlink rather than a copy is the `tests/orphan_check.rs`
/// rule: the artifact under test is the one that ships.
struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = scratch_dir();
        std::fs::create_dir_all(root.path().join("scripts")).expect("fixture: creating scripts/");
        std::os::unix::fs::symlink(
            checkout().join("scripts/machine-lock.sh"),
            root.path().join("scripts/machine-lock.sh"),
        )
        .expect("fixture: linking the tracked script");
        std::fs::create_dir_all(root.path().join("locks")).expect("fixture: creating locks/");
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn script(&self) -> String {
        self.path()
            .join("scripts/machine-lock.sh")
            .display()
            .to_string()
    }

    fn lock_root(&self) -> PathBuf {
        self.path().join("locks")
    }

    /// The directory the script would use for `name`.
    fn lock(&self, name: &str) -> PathBuf {
        self.lock_root().join(format!("{name}.lock"))
    }

    /// A `Command` for the script with this fixture's lock root bound. Every
    /// case goes through here, so no test can reach a real lock.
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("bash");
        cmd.arg(self.script())
            .args(args)
            .current_dir(self.path())
            .env("STORYHOOK_LOCK_DIR", self.lock_root())
            // A stray value inherited from an outer run would make the
            // reentrancy branch swallow a case that means to contend.
            .env_remove("STORYHOOK_MACHINE_LOCKS");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args)
            .output()
            .unwrap_or_else(|e| panic!("running the script with {args:?}: {e}"))
    }

    fn spawn(&self, args: &[&str]) -> ChildGuard {
        let mut command = self.command(args);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        ChildGuard::spawn(&mut command)
            .unwrap_or_else(|e| panic!("spawning the script with {args:?}: {e}"))
    }

    /// Plants a lock directory with a chosen identity — the only way to build
    /// the dead-pid and reused-pid cases deterministically, rather than
    /// waiting for a machine to produce one (SH-420's posture: construct the
    /// straddle).
    fn plant(&self, name: &str, pid: &str, started: &str) {
        let lock = self.lock(name);
        std::fs::create_dir_all(&lock).expect("fixture: planting a lock directory");
        // The script's own write order: `pid` last, so a reader that sees a
        // pid is guaranteed to see the start time beside it.
        std::fs::write(lock.join("started"), format!("{started}\n")).expect("fixture: started");
        std::fs::write(lock.join("meta"), "planted by tests/machine_lock.rs\n")
            .expect("fixture: meta");
        std::fs::write(lock.join("pid"), format!("{pid}\n")).expect("fixture: pid");
    }

    /// Writes an executable shell script inside the fixture and returns its
    /// path — a command for the lock to wrap.
    fn helper(&self, name: &str, body: &str) -> PathBuf {
        let path = self.path().join(name);
        let mut file = std::fs::File::create(&path)
            .unwrap_or_else(|e| panic!("creating {}: {e}", path.display()));
        file.write_all(body.as_bytes()).expect("writing a helper");
        drop(file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("making a helper executable");
        path
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or_else(|| {
        panic!(
            "the script must exit, not die of a signal: {:?}",
            out.status
        )
    })
}

/// Whether `pid` is a process that is still *running*, as opposed to one that
/// has died and is waiting to be collected.
///
/// `kill -0` cannot answer this and must not be asked to: a spawned child of
/// this test process becomes a **zombie** when it exits, and `kill -0`
/// succeeds on a zombie because the pid is still there to be signalled. An
/// assertion that something was killed, written against `kill -0`, passes
/// whether or not the kill worked. (`tests/orphan_check.rs` records the same
/// trap; it is repeated rather than shared because the two suites are
/// deliberately independent of each other.)
fn pid_running(pid: u32) -> bool {
    let out = Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output()
        .expect("running ps");
    let state = String::from_utf8_lossy(&out.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

/// `ps -o lstart=` for a live pid, normalized exactly the way the script
/// normalizes it. Used to plant a *matching* identity for the live-holder
/// control.
fn started_of(pid: u32) -> String {
    let out = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .expect("running ps");
    let raw = String::from_utf8_lossy(&out.stdout);
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `--max-wait` a reclamation case gives the script.
///
/// Not a speed assertion and not a bare literal (SH-394): reclaiming is decided
/// on the **first** observation of a lock, so any positive multiple of the
/// script's own poll period is enough for a correct implementation, and two is
/// the smallest that still tolerates one lost cycle on a loaded machine. Its
/// real job is to make a *wrong* implementation FAIL rather than hang — a
/// script that believes a holder it should have reclaimed waits on a live pid
/// forever, and an unbounded case would hang the whole suite instead of
/// reporting the mutation. Measured: without this, deleting the start-time
/// comparison hung `cargo test` indefinitely rather than turning one test red.
fn reclaim_deadline() -> String {
    (lock_poll_secs() * 2).to_string()
}

/// A pid that is not in use. Spawning and reaping is the only way to be sure:
/// a number picked out of the air can belong to something.
fn a_dead_pid() -> u32 {
    let mut command = Command::new("true");
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = ChildGuard::spawn(&mut command).expect("spawning a process to reap");
    let pid = child.pid();
    child.wait_within(poll_ceiling(), || {
        "the short-lived pid fixture did not exit".to_string()
    });
    pid
}

/// Blocks until `path` exists, or panics. The ceiling derives from the
/// script's own poll period (below) rather than being a bare literal
/// (SH-394): what is being waited for is one observation cycle of the lock,
/// so the bound is a multiple of that cycle.
fn wait_for(path: &Path) {
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(lock_poll_secs() * WAIT_POLLS_ALLOWED);
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!(
        "{} never appeared within the allowed poll cycles",
        path.display()
    );
}

/// Blocks until `pid` is no longer running, bounded by the lock's observation
/// cycle so a failed process-group reap turns one case red instead of hanging
/// this test binary.
fn wait_for_process_exit(pid: u32) {
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(lock_poll_secs() * WAIT_POLLS_ALLOWED);
    while std::time::Instant::now() < deadline {
        if !pid_running(pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("process {pid} survived the machine-lock cleanup ceiling");
}

/// How many of the script's own poll cycles a fixture may spend waiting for a
/// process to reach an expected state. Generous on purpose — this machine runs
/// several suites at once, and the thing under test is never the latency.
const WAIT_POLLS_ALLOWED: u64 = 30;

/// The same quantity [`wait_for`] spends, for a fixture waiting on a *process*
/// to exit rather than on a path to appear (SH-528).
///
/// One derivation rather than two: both are "N observation cycles of the
/// lock", and stating that once is what stops the two drifting apart (SH-136).
///
/// **The inequality below it is load-bearing** at the one site whose subject is
/// how a wrapper dies: that fixture wraps `sleep 60`, so a ceiling *under* 60s
/// is what makes a deleted signal trap fail as a named timeout instead of
/// running the sleep to completion and failing on the exit code a whole minute
/// later. Raising `WAIT_POLLS_ALLOWED` past 60 would quietly cost that.
fn poll_ceiling() -> std::time::Duration {
    std::time::Duration::from_secs(lock_poll_secs() * WAIT_POLLS_ALLOWED)
}

// ---------------------------------------------------------------------------
// The script's own derived constants, read back out of it
// ---------------------------------------------------------------------------

/// Reads `readonly <name>=<digits>` out of the tracked script.
///
/// The `tests/orphan_check.rs` idiom: a constant that a test needs is parsed
/// out of the artifact rather than copied into the test, because a copy is a
/// second place to disagree (SH-136, four times over in this project).
fn script_constant(name: &str) -> u64 {
    let src = read_checkout_file("scripts/machine-lock.sh");
    let marker = format!("readonly {name}=");
    let after = src
        .split(&marker)
        .nth(1)
        .unwrap_or_else(|| panic!("scripts/machine-lock.sh must declare `{marker}<n>`"));
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("{name}'s value {digits:?} must parse: {e}"))
}

fn lock_poll_secs() -> u64 {
    script_constant("LOCK_POLL_SECS")
}

/// `make test`'s measured warm median, read out of the baseline document the
/// script says it derives from.
fn measured_gate_median_secs() -> u64 {
    let src = read_checkout_file("docs/rearch/baseline/timings.md");
    let after = src
        .split("## The whole gate")
        .nth(1)
        .expect("docs/rearch/baseline/timings.md must have a `## The whole gate` section");
    let bolded = after
        .split("**")
        .nth(1)
        .expect("that section must carry a **bolded** median");
    let seconds: String = bolded
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    seconds
        .parse::<f64>()
        .unwrap_or_else(|e| panic!("the median {seconds:?} must parse: {e}")) as u64
}

// ---------------------------------------------------------------------------
// Running the command at all
// ---------------------------------------------------------------------------

/// The whole contract of a wrapper: the command runs, and its status is the
/// script's. A wrapper that swallowed a failure would make every caller's gate
/// a lie.
#[test]
fn the_commands_exit_status_is_the_scripts_exit_status() {
    let fixture = Fixture::new();

    let ok = fixture.run(&["gate", "--", "sh", "-c", "echo ran; exit 0"]);
    assert_eq!(code(&ok), 0, "a zero exit must pass through: {ok:?}");
    assert!(
        String::from_utf8_lossy(&ok.stdout).contains("ran"),
        "the command's own stdout must reach the caller: {ok:?}"
    );

    let bad = fixture.run(&["gate", "--", "sh", "-c", "exit 7"]);
    assert_eq!(
        code(&bad),
        7,
        "a nonzero exit must pass through unchanged, not be flattened to 1: {bad:?}"
    );
}

/// The lock is gone afterwards on both paths. A failing command that left the
/// lock behind would wedge the machine on the first red suite.
#[test]
fn the_lock_is_released_on_both_a_zero_and_a_nonzero_exit() {
    let fixture = Fixture::new();

    fixture.run(&["gate", "--", "true"]);
    assert!(
        !fixture.lock("gate").exists(),
        "a zero exit must leave no lock behind"
    );

    fixture.run(&["gate", "--", "false"]);
    assert!(
        !fixture.lock("gate").exists(),
        "a nonzero exit must leave no lock behind either"
    );
}

/// SH-536: the ceiling is over silence, never over the command's total wall
/// clock. Every append to the gate journal buys a fresh full idle budget.
#[test]
fn journal_progress_resets_the_idle_ceiling() {
    let fixture = Fixture::new();
    let journal = fixture.path().join("progress.ndjson");
    let helper = fixture.helper(
        "progressing.sh",
        "#!/bin/sh\nfor step in 1 2 3 4; do\n  printf '{\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\",\"step\":%s}\\n' \"$step\" >> \"$STORYHOOK_GATE_PROGRESS\"\n  sleep 1\ndone\n",
    );

    let out = fixture
        .command(&[
            "--max-idle",
            "2",
            "gate",
            "--",
            &helper.display().to_string(),
        ])
        .env("STORYHOOK_GATE_PROGRESS", &journal)
        .output()
        .expect("running a progressing holder");

    assert_eq!(
        code(&out),
        0,
        "four seconds of progress must outlive a two-second silence ceiling: {out:?}"
    );
    assert!(
        !stderr(&out).contains("made no progress"),
        "progress must reset the watchdog, not merely be reported: {}",
        stderr(&out)
    );
}

/// SH-536's incident shape, with the harder descendant case included: a live
/// holder and its child both ignore TERM and emit no further progress. The
/// watchdog must diagnose them, escalate to KILL, reap, and release the lock.
#[test]
fn a_silent_holder_is_diagnosed_and_its_process_group_is_reaped() {
    let fixture = Fixture::new();
    let journal = fixture.path().join("progress.ndjson");
    let descendant = fixture.path().join("descendant.pid");
    let helper = fixture.helper(
        "stubborn-holder.sh",
        "#!/bin/sh\nprintf '{\"kind\":\"item\",\"path\":\"release gate/rust-suite\",\"status\":\"running\",\"at\":\"fixture-time\"}\\n' >> \"$STORYHOOK_GATE_PROGRESS\"\nsh -c 'trap \"\" TERM; while :; do sleep 1; done' &\nprintf '%s\\n' \"$!\" > \"$1\"\ntrap '' TERM\nwhile :; do sleep 1; done\n",
    );

    let out = fixture
        .command(&[
            "--max-idle",
            "2",
            "gate",
            "--",
            &helper.display().to_string(),
            &descendant.display().to_string(),
        ])
        .env("STORYHOOK_GATE_PROGRESS", &journal)
        .output()
        .expect("running a silent holder");

    assert_eq!(
        code(&out),
        124,
        "a watchdog expiry has a distinct status: {out:?}"
    );
    let err = stderr(&out);
    for expected in [
        "made no progress for 2s",
        "last gate progress",
        "rust-suite",
        "active process group",
        "stubborn-holder.sh",
        "SIGTERM",
        "SIGKILL",
    ] {
        assert!(
            err.contains(expected),
            "the failure must report {expected:?}\nstderr: {err}"
        );
    }
    let descendant_pid: u32 = std::fs::read_to_string(&descendant)
        .expect("reading the stubborn descendant pid")
        .trim()
        .parse()
        .expect("the stubborn descendant pid must parse");
    wait_for_process_exit(descendant_pid);
    assert!(
        !fixture.lock("gate").exists(),
        "the lock must be released only after the stubborn group is gone"
    );
    assert_eq!(
        code(&fixture.run(&["--max-idle", "2", "gate", "--", "true"])),
        0,
        "the next holder must be able to enter after cleanup"
    );
}

/// An interactive gate has no daemon-provided journal, so the lock must mint
/// one and hand it to the holder rather than silently disabling SH-536.
#[test]
fn a_gate_without_an_external_journal_exports_a_private_one() {
    let fixture = Fixture::new();
    let seen = fixture.path().join("seen-journal");
    let helper = fixture.helper(
        "record-journal.sh",
        "#!/bin/sh\nprintf '%s\\n' \"$STORYHOOK_GATE_PROGRESS\" > \"$1\"\nprintf '{\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\"}\\n' >> \"$STORYHOOK_GATE_PROGRESS\"\n",
    );

    let out = fixture.run(&[
        "--max-idle",
        "2",
        "gate",
        "--",
        &helper.display().to_string(),
        &seen.display().to_string(),
    ]);

    assert_eq!(
        code(&out),
        0,
        "the private journal path must be usable: {out:?}"
    );
    let path = std::fs::read_to_string(&seen).expect("reading the exported journal path");
    assert!(
        path.trim()
            .starts_with(&fixture.lock("gate").display().to_string()),
        "the private journal must be owned by the lock directory: {path:?}"
    );
    assert!(
        !Path::new(path.trim()).exists(),
        "releasing the lock must remove its private journal"
    );
}

/// Losing the safety signal must end the holder, not merely end the watchdog
/// and leave the machine wedged behind an unobserved live process.
#[test]
fn a_journal_that_disappears_fails_the_holder_loudly() {
    let fixture = Fixture::new();
    let journal = fixture.path().join("progress.ndjson");
    let helper = fixture.helper(
        "remove-journal.sh",
        "#!/bin/sh\nprintf '{\"kind\":\"item\",\"path\":\"release gate/rust-suite\",\"status\":\"running\"}\\n' >> \"$STORYHOOK_GATE_PROGRESS\"\nsleep 1\nrm \"$STORYHOOK_GATE_PROGRESS\"\nwhile :; do sleep 1; done\n",
    );

    let out = fixture
        .command(&[
            "--max-idle",
            "5",
            "gate",
            "--",
            &helper.display().to_string(),
        ])
        .env("STORYHOOK_GATE_PROGRESS", &journal)
        .output()
        .expect("running a holder that removes its journal");

    assert_eq!(
        code(&out),
        124,
        "losing the journal is a watchdog failure: {out:?}"
    );
    let err = stderr(&out);
    assert!(err.contains("lost access"), "stderr: {err}");
    assert!(
        err.contains(&journal.display().to_string()),
        "stderr: {err}"
    );
    assert!(
        !fixture.lock("gate").exists(),
        "journal loss must still reap and release the holder"
    );
}

// ---------------------------------------------------------------------------
// Exclusion
// ---------------------------------------------------------------------------

/// Mutual exclusion, proved by an **interleaving trace** the two commands
/// append to rather than by timing: the second command's first line must come
/// after the first command's last, whatever the machine's load. A timing
/// assertion here would be measuring this machine, not the lock.
#[test]
fn two_holders_of_one_name_serialize() {
    let fixture = Fixture::new();
    let trace = fixture.path().join("trace");
    let trace_arg = trace.display().to_string();

    let slow = fixture.helper(
        "slow.sh",
        "#!/bin/sh\nprintf 'A-in\\n' >> \"$1\"\nsleep 2\nprintf 'A-out\\n' >> \"$1\"\n",
    );
    let quick = fixture.helper(
        "quick.sh",
        "#!/bin/sh\nprintf 'B-in\\n' >> \"$1\"\nprintf 'B-out\\n' >> \"$1\"\n",
    );

    let mut first = fixture.spawn(&["gate", "--", &slow.display().to_string(), &trace_arg]);
    wait_for(&fixture.lock("gate").join("pid"));

    let second = fixture.run(&["gate", "--", &quick.display().to_string(), &trace_arg]);
    assert_eq!(
        code(&second),
        0,
        "the waiter must eventually run: {second:?}"
    );
    first.wait_within(poll_ceiling(), || {
        "the first `gate` holder (slow.sh) never exited, so the lock it holds was never \
         released"
            .to_string()
    });

    let seen = std::fs::read_to_string(&trace).expect("reading the trace");
    assert_eq!(
        seen, "A-in\nA-out\nB-in\nB-out\n",
        "the second holder must not enter before the first left; interleaved lines mean the lock did not exclude"
    );
}

/// The positive control for the case above: exclusion must be **per name**. A
/// script that serialized everything would pass `two_holders_of_one_name_
/// serialize` while being useless, and this is what separates the two.
#[test]
fn two_different_names_do_not_serialize() {
    let fixture = Fixture::new();
    let trace = fixture.path().join("trace");
    let trace_arg = trace.display().to_string();

    let slow = fixture.helper(
        "slow.sh",
        "#!/bin/sh\nprintf 'A-in\\n' >> \"$1\"\nsleep 2\nprintf 'A-out\\n' >> \"$1\"\n",
    );
    let quick = fixture.helper(
        "quick.sh",
        "#!/bin/sh\nprintf 'B-in\\n' >> \"$1\"\nprintf 'B-out\\n' >> \"$1\"\n",
    );

    let mut first = fixture.spawn(&["gate", "--", &slow.display().to_string(), &trace_arg]);
    wait_for(&fixture.lock("gate").join("pid"));

    let second = fixture.run(&["merge", "--", &quick.display().to_string(), &trace_arg]);
    assert_eq!(code(&second), 0, "the other name must not wait: {second:?}");

    let seen = std::fs::read_to_string(&trace).expect("reading the trace");
    assert_eq!(
        seen, "A-in\nB-in\nB-out\n",
        "`merge` must run while `gate` is held -- it ran either before A entered or after A left, so the names are not independent"
    );
    first.wait_within(poll_ceiling(), || {
        "the `gate` holder (slow.sh) never exited".to_string()
    });
}

/// SH-306: a gate that goes quiet reads as an all-clear. A waiter must say
/// **who** it is waiting for and **how long** it waited, or a wedged machine
/// looks identical to a slow one.
#[test]
fn a_waiter_names_the_holder_and_how_long_it_waited() {
    let fixture = Fixture::new();

    let mut first = fixture.spawn(&["gate", "--", "sleep", "2"]);
    wait_for(&fixture.lock("gate").join("pid"));
    let holder = std::fs::read_to_string(fixture.lock("gate").join("pid"))
        .expect("reading the holder's pid");
    let holder = holder.trim().to_string();

    let second = fixture.run(&["gate", "--", "true"]);
    first.wait_within(poll_ceiling(), || {
        "the first `gate` holder never exited, though the waiter below it already got the lock"
            .to_string()
    });

    let err = stderr(&second);
    assert!(
        err.contains(&format!("pid {holder}")),
        "the waiter must name the pid actually holding the lock ({holder}), so a wedge can be traced to a process\nstderr: {err}"
    );
    assert!(
        err.contains("waiting for the 'gate' lock"),
        "the waiter must say what it is waiting for\nstderr: {err}"
    );
    assert!(
        err.contains("after waiting"),
        "having waited must be reported on the way out too -- a caller that only ever sees the command's own output cannot otherwise tell a queued run from a prompt one\nstderr: {err}"
    );
}

// ---------------------------------------------------------------------------
// Holder identity
// ---------------------------------------------------------------------------

/// A lock left by a process that is simply gone — the ordinary case after a
/// crash or a `kill -9` — is reclaimed rather than wedging the machine
/// forever, and the reclamation is reported.
#[test]
fn a_lock_left_by_a_dead_pid_is_reclaimed() {
    let fixture = Fixture::new();
    let dead = a_dead_pid();
    fixture.plant("gate", &dead.to_string(), "Sat Aug 29 00:00:00 2026");

    let out = fixture.run(&[
        "--max-wait",
        &reclaim_deadline(),
        "gate",
        "--",
        "echo",
        "ran",
    ]);

    assert_eq!(
        code(&out),
        0,
        "a lock with a dead holder must be taken: {out:?}"
    );
    let err = stderr(&out);
    assert!(
        err.contains(&format!("left by pid {dead}")),
        "reclaiming must be reported, naming the pid it belonged to -- a lock silently stolen is the same SH-306 shape as a silent wait\nstderr: {err}"
    );
}

/// The constructed straddle (SH-420's posture — build the case rather than
/// wait for a machine to produce one). A pid alone is **not** an identity:
/// pids are reused, this lock root outlives a reboot, and a reused pid is
/// exactly how a live holder's lock gets stolen. The holder here is alive and
/// its pid matches; only the recorded start time disagrees, and that alone
/// must be enough to judge the recorded holder gone.
#[test]
fn a_lock_whose_pid_was_reused_is_reclaimed() {
    let fixture = Fixture::new();
    let mut command = Command::new("sleep");
    command
        .arg("30")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let victim = ChildGuard::spawn(&mut command)
        .expect("spawning a live process to impersonate a reused pid");
    fixture.plant(
        "gate",
        &victim.pid().to_string(),
        "Sat Aug 29 00:00:00 2026",
    );

    let out = fixture.run(&[
        "--max-wait",
        &reclaim_deadline(),
        "gate",
        "--",
        "echo",
        "ran",
    ]);

    assert_eq!(
        code(&out),
        0,
        "an alive pid whose start time does not match the record is not the recorded holder, so the lock must be taken: {out:?}"
    );
    let err = stderr(&out);
    assert!(
        err.contains("pid was reused"),
        "the reclamation must say WHY it judged the holder gone, since 'the pid is alive' and 'the holder is alive' are different claims\nstderr: {err}"
    );
    assert!(
        pid_running(victim.pid()),
        "reclaiming a lock must never touch the process that happens to hold the reused pid"
    );
    // Dropped rather than waited on (SH-528). `victim` is `sleep 30` and the
    // assertion above has just proved it is deliberately still running, so
    // waiting here would spend the whole remaining sleep for nothing — and
    // *bounding* that wait would be racing a 30-second sleep with a
    // 30-second ceiling, which is a coin toss by construction.
    // `ChildGuard::Drop` already kills and reaps.
    drop(victim);
}

/// The positive control for both reclamation cases. A holder that is alive
/// **and** whose start time still matches is the real thing, and must be
/// waited for, not stolen from. Without this, a script that reclaimed
/// unconditionally would pass every case above.
#[test]
fn a_live_holder_whose_identity_matches_is_never_reclaimed() {
    let fixture = Fixture::new();
    let mut command = Command::new("sleep");
    command
        .arg("30")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let victim = ChildGuard::spawn(&mut command).expect("spawning a live holder");
    fixture.plant("gate", &victim.pid().to_string(), &started_of(victim.pid()));

    let out = fixture.run(&["--max-wait", "1", "gate", "--", "echo", "MUST-NOT-RUN"]);

    assert_eq!(
        code(&out),
        75,
        "a genuinely held lock must be waited for and then given up on, never reclaimed: {out:?}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("MUST-NOT-RUN"),
        "the command must not run when the lock was never taken: {out:?}"
    );
    assert!(
        fixture.lock("gate").join("pid").exists(),
        "the live holder's lock must still be there afterwards"
    );
    // Dropped rather than waited on — see the sibling case above (SH-528).
    drop(victim);
}

// ---------------------------------------------------------------------------
// Giving up
// ---------------------------------------------------------------------------

/// `--max-wait` elapsing is a *distinct* outcome from the command failing, and
/// has to read that way: EX_TEMPFAIL, a line saying the command did not run,
/// and the holder's lock left exactly where it was.
#[test]
fn max_wait_elapsing_refuses_without_running_or_stealing() {
    let fixture = Fixture::new();

    let mut first = fixture.spawn(&["gate", "--", "sleep", "5"]);
    wait_for(&fixture.lock("gate").join("pid"));

    let second = fixture.run(&["--max-wait", "0", "gate", "--", "echo", "MUST-NOT-RUN"]);

    assert_eq!(
        code(&second),
        75,
        "giving up must be EX_TEMPFAIL: {second:?}"
    );
    assert!(
        !String::from_utf8_lossy(&second.stdout).contains("MUST-NOT-RUN"),
        "giving up must not run the command: {second:?}"
    );
    assert!(
        stderr(&second).contains("The command did not run"),
        "the caller must be told that nothing happened, in words -- an exit code alone is the ambiguity SH-312 is about\nstderr: {}",
        stderr(&second)
    );
    assert!(
        fixture.lock("gate").exists(),
        "giving up must leave the holder's lock alone"
    );
    first.wait_within(poll_ceiling(), || {
        "the `gate` holder (sleep 5) never exited, so the waiter above gave up against a lock \
         nothing was ever going to release"
            .to_string()
    });
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// The case that cannot be reviewed by eye. `trap ... EXIT` alone looks
/// sufficient and is not: bash defers a trap until the current foreground
/// command returns, so a SIGTERM arriving during a fourteen-minute suite would
/// not release the lock until that suite finished — which is the exact wedge
/// this script exists to prevent. Running the command in the background with
/// an explicit `wait` is what makes the trap fire; nothing but a real signal
/// mid-command can tell the two implementations apart.
#[test]
fn a_signal_releases_the_lock_and_takes_the_command_with_it() {
    let fixture = Fixture::new();

    let mut wrapper = fixture.spawn(&["gate", "--", "sleep", "60"]);
    wait_for(&fixture.lock("gate").join("pid"));

    let held = std::fs::read_to_string(fixture.lock("gate").join("pid")).expect("reading the pid");
    assert_eq!(
        held.trim(),
        wrapper.pid().to_string(),
        "the lock must record the wrapper's own pid"
    );

    let children = Command::new("pgrep")
        .args(["-P", &wrapper.pid().to_string()])
        .output()
        .expect("running pgrep");
    let command_pid: u32 = String::from_utf8_lossy(&children.stdout)
        .split_whitespace()
        .next()
        .expect("the wrapped command must be a child of the wrapper")
        .parse()
        .expect("a pid");

    Command::new("kill")
        .args(["-TERM", &wrapper.pid().to_string()])
        .status()
        .expect("signalling the wrapper");
    // Bounded, and the ceiling is what makes this case a mutation detector
    // rather than a formality (SH-528). The wrapper wraps `sleep 60`; with the
    // signal traps deleted, bash defers the EXIT trap until that foreground
    // command returns, so an unbounded wait here sat out the whole minute and
    // only then failed on the exit code. `poll_ceiling()` is 30s, comfortably
    // under 60, so the same mutation now fails at half the time, naming the
    // wrapper rather than its status.
    let status = wrapper.wait_within(poll_ceiling(), || {
        "the wrapper did not die of the SIGTERM just sent to it. A trap that is deferred until \
         the wrapped `sleep 60` returns looks identical to a correct one until exactly this \
         wait, which is why it is bounded below that sleep."
            .to_string()
    });

    assert_eq!(
        status.code(),
        None,
        "the wrapper must die OF the signal, so its status is a truthful 128+signal rather than a fabricated one: {status:?}"
    );

    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(lock_poll_secs() * WAIT_POLLS_ALLOWED);
    while fixture.lock("gate").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        !fixture.lock("gate").exists(),
        "a signalled wrapper must release the lock -- otherwise one Ctrl-C wedges the machine until somebody finds the directory by hand"
    );
    assert!(
        !pid_running(command_pid),
        "the signal must be forwarded to the wrapped command; leaving it running is the SH-493 leak one layer up"
    );
}

// ---------------------------------------------------------------------------
// Refusals (SH-357: an argument that lands nowhere is refused, not dropped)
// ---------------------------------------------------------------------------

/// A name is validated rather than sanitized, and the refusal is also what
/// keeps the lock directory inside the lock root.
#[test]
fn a_name_that_would_escape_the_lock_root_is_refused() {
    let fixture = Fixture::new();
    let escape = fixture.path().join("escaped").display().to_string();

    for name in ["../escape", "a/b", "with space", "_leading"] {
        let out = fixture.run(&[name, "--", "touch", &escape]);
        assert_eq!(code(&out), 2, "'{name}' must be refused: {out:?}");
        assert!(
            stderr(&out).contains(name),
            "the refusal must name the offending word (SH-357)\nstderr: {}",
            stderr(&out)
        );
    }
    assert!(
        !Path::new(&escape).exists(),
        "a refused invocation must not have run the command"
    );
}

/// The separator and the command are both required. A missing `--` used to be
/// the shape that silently dropped a word.
#[test]
fn a_malformed_invocation_is_refused_and_names_the_problem() {
    let fixture = Fixture::new();

    let cases: [(&[&str], &str); 4] = [
        (&["gate", "true"], "literal '--'"),
        (&["gate", "--"], "followed by a command"),
        (&["--", "true"], "no lock name"),
        (&["--nope", "gate", "--", "true"], "unknown option"),
    ];
    for (args, expected) in cases {
        let out = fixture.run(args);
        assert_eq!(code(&out), 2, "{args:?} must be refused: {out:?}");
        assert!(
            stderr(&out).contains(expected),
            "{args:?} must be refused with a message naming the problem, not a bare usage dump\nstderr: {}",
            stderr(&out)
        );
        assert!(
            stderr(&out).contains("usage: machine-lock.sh"),
            "every refusal carries the usage line\nstderr: {}",
            stderr(&out)
        );
    }
    assert!(
        !fixture.lock("gate").exists(),
        "a refusal must happen before any lock is taken"
    );
}

/// `--max-wait` takes seconds, and a word that is not a number lands nowhere.
#[test]
fn max_wait_refuses_a_value_that_is_not_a_whole_number() {
    let fixture = Fixture::new();
    for bad in ["soon", "-1", "1.5"] {
        let out = fixture.run(&["--max-wait", bad, "gate", "--", "true"]);
        assert_eq!(code(&out), 2, "--max-wait {bad} must be refused: {out:?}");
    }
}

/// An idle budget of zero cannot observe progress even once, and a malformed
/// word must not be accepted as if it disabled the watchdog.
#[test]
fn max_idle_requires_a_positive_whole_number() {
    let fixture = Fixture::new();
    for bad in ["0", "soon", "-1", "1.5"] {
        let out = fixture.run(&["--max-idle", bad, "gate", "--", "true"]);
        assert_eq!(code(&out), 2, "--max-idle {bad} must be refused: {out:?}");
        assert!(
            stderr(&out).contains("--max-idle"),
            "stderr: {}",
            stderr(&out)
        );
    }
}

// ---------------------------------------------------------------------------
// --plan, and the root derivation it makes testable
// ---------------------------------------------------------------------------

/// `--plan` is what lets the root derivation below be tested against the REAL
/// default without ever creating a real lock — `browser-watch.sh --plan`'s own
/// reason for existing.
#[test]
fn plan_reports_the_resolved_lock_and_runs_nothing() {
    let fixture = Fixture::new();
    let sentinel = fixture.path().join("ran").display().to_string();

    let out = fixture.run(&["--plan", "gate", "--", "touch", &sentinel]);
    let printed = String::from_utf8_lossy(&out.stdout).to_string();

    assert_eq!(code(&out), 0, "--plan must succeed: {out:?}");
    assert!(
        printed.contains(&format!("lock={}", fixture.lock("gate").display())),
        "--plan must print the path it WOULD take, or it cannot be used to check the derivation\nstdout: {printed}"
    );
    assert!(
        printed.contains("command=touch"),
        "--plan must print the command it would run\nstdout: {printed}"
    );
    assert!(
        printed.contains("max_idle=288"),
        "the reserved gate's derived watchdog default must be inspectable\nstdout: {printed}"
    );
    assert!(!Path::new(&sentinel).exists(), "--plan must run nothing");
    assert!(
        !fixture.lock("gate").exists(),
        "--plan must take nothing -- a planner that created the directory would be a lock nobody released"
    );
}

#[test]
fn non_gate_locks_have_no_implicit_idle_ceiling() {
    let fixture = Fixture::new();
    let out = fixture.run(&["--plan", "merge", "--", "true"]);
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(printed.contains("max_idle=none"), "stdout: {printed}");
}

/// **The load-bearing derivation.** `scripts/run-tests.sh` exports
/// `XDG_STATE_HOME` into a fresh per-run `mktemp -d` directory, and SH-457
/// takes the `gate` lock inside that script. A lock root read from
/// `XDG_STATE_HOME` would therefore be unique per run: every concurrent suite
/// on the machine would take a *different* lock, nothing would serialize, and
/// the gate would pass having proved nothing — the SH-364 shape, a harness
/// lying to the gate running under it.
///
/// The decoy is constructed rather than waited for: with no
/// `STORYHOOK_LOCK_DIR`, an `XDG_STATE_HOME` pointing somewhere obvious must
/// have no effect on where the lock lands.
#[test]
fn the_lock_root_ignores_xdg_state_home_because_the_gate_rewrites_it() {
    let fixture = Fixture::new();
    let fake_home = fixture.path().join("home");
    let decoy = fixture.path().join("decoy-state");

    let out = Command::new("bash")
        .arg(fixture.script())
        .args(["--plan", "gate", "--", "true"])
        .current_dir(fixture.path())
        .env_remove("STORYHOOK_LOCK_DIR")
        .env_remove("STORYHOOK_MACHINE_LOCKS")
        .env("HOME", &fake_home)
        .env("XDG_STATE_HOME", &decoy)
        .output()
        .expect("running the script with a decoy state home");

    let printed = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        printed.contains(&format!(
            "lock={}/.local/state/storyhook/locks/gate.lock",
            fake_home.display()
        )),
        "the default lock root must derive from $HOME\nstdout: {printed}"
    );
    assert!(
        !printed.contains(&decoy.display().to_string()),
        "$XDG_STATE_HOME must not reach the lock root: `scripts/run-tests.sh` rewrites that variable per run, so a root read from it would serialize nothing\nstdout: {printed}"
    );
}

/// With neither lever available there is no per-user place to put a
/// machine-wide lock, and inventing one (a fixed `/tmp` path, say) is the
/// SH-263 collision. Refusing names both levers.
#[test]
fn a_lock_root_that_cannot_be_resolved_is_refused_naming_both_levers() {
    let fixture = Fixture::new();

    let out = Command::new("bash")
        .arg(fixture.script())
        .args(["gate", "--", "true"])
        .current_dir(fixture.path())
        .env_remove("STORYHOOK_LOCK_DIR")
        .env_remove("STORYHOOK_MACHINE_LOCKS")
        .env_remove("HOME")
        .output()
        .expect("running the script with no home");

    assert_eq!(code(&out), 2, "an unresolvable root must refuse: {out:?}");
    let err = stderr(&out);
    assert!(
        err.contains("STORYHOOK_LOCK_DIR") && err.contains("HOME"),
        "the refusal must name both levers rather than leave them to be discovered by reading source\nstderr: {err}"
    );
}

// ---------------------------------------------------------------------------
// Reentrancy
// ---------------------------------------------------------------------------

/// `make test` reaches `run-tests.sh` twice per run, and a caller who wraps a
/// whole `make test` in `machine-lock.sh gate --` would otherwise wait on a
/// lock its own process tree already holds — forever, since that holder is
/// provably alive. The nested call runs directly, and says so.
#[test]
fn a_nested_take_of_a_held_name_runs_instead_of_deadlocking() {
    let fixture = Fixture::new();

    let out = fixture.run(&[
        "gate",
        "--",
        "bash",
        &fixture.script(),
        "gate",
        "--",
        "echo",
        "nested-ran",
    ]);

    assert_eq!(code(&out), 0, "a nested take must not deadlock: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("nested-ran"),
        "the nested command must still run: {out:?}"
    );
    assert!(
        stderr(&out).contains("already held by this process tree"),
        "running without re-taking is a decision, and a decision nobody can see is the SH-306 shape\nstderr: {}",
        stderr(&out)
    );
}

// ---------------------------------------------------------------------------
// The derived constants stay derived
// ---------------------------------------------------------------------------

/// SH-394: a wall-clock number in this script must derive from the budget it
/// is about, and stay derived. `GATE_MEDIAN_SECS` is `make test`'s own
/// measured warm median from `docs/rearch/baseline/timings.md`, and this is
/// what fails when the two drift apart — the class SH-136 has already cost
/// this project four times over.
#[test]
fn the_wait_report_cadence_still_matches_the_measured_gate_median() {
    let declared = script_constant("GATE_MEDIAN_SECS");
    let measured = measured_gate_median_secs();

    assert_eq!(
        declared, measured,
        "scripts/machine-lock.sh declares GATE_MEDIAN_SECS={declared}, but docs/rearch/baseline/timings.md now measures `make test` at {measured}s. Re-derive the constant rather than leaving the script asserting a median nobody measures any more."
    );
    let src = read_checkout_file("scripts/machine-lock.sh");
    assert!(
        src.contains("readonly WAIT_REPORT_SECS=$GATE_MEDIAN_SECS"),
        "the report cadence must be spelled as the derived constant rather than re-typed as the same digits -- a second copy is a second place to disagree"
    );
}

/// The watchdog ceiling is a formula over three named facts, not a literal
/// that happens to equal today's result.
#[test]
fn the_gate_idle_ceiling_stays_derived() {
    let src = read_checkout_file("scripts/machine-lock.sh");
    assert!(
        src.contains(
            "readonly GATE_IDLE_CEILING_SECS=$((GATE_MEDIAN_SECS * GATE_CONCURRENT_RUNS * GATE_IDLE_MARGIN))"
        ),
        "the ceiling must preserve its derivation in source"
    );
    assert_eq!(script_constant("GATE_CONCURRENT_RUNS"), 4);
    assert_eq!(script_constant("GATE_IDLE_MARGIN"), 2);
    assert_eq!(
        measured_gate_median_secs()
            * script_constant("GATE_CONCURRENT_RUNS")
            * script_constant("GATE_IDLE_MARGIN"),
        288
    );

    let dispatch = read_checkout_file("src/api/dispatch.rs");
    assert!(
        dispatch.contains("const MAX_RUNNING: usize = 4;"),
        "GATE_CONCURRENT_RUNS must be re-derived when MAX_RUNNING changes"
    );
}

/// The poll period is the resolution of the observation, not a guess about
/// speed: `date +%s` and `ps -o lstart=` are both whole-second granular, so a
/// faster re-check cannot observe a different answer. This pins the claim that
/// the two are the same number.
#[test]
fn the_poll_period_is_the_granularity_of_what_it_observes() {
    assert_eq!(
        lock_poll_secs(),
        1,
        "LOCK_POLL_SECS is one second because that is the granularity of both clocks the script reads; a different value needs a different derivation written beside it"
    );

    let out = Command::new("ps")
        .args(["-o", "lstart=", "-p", &std::process::id().to_string()])
        .output()
        .expect("running ps");
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(
        !printed.contains('.'),
        "the derivation assumes `ps -o lstart=` carries no sub-second field; it now prints {printed:?}, so the poll period needs re-deriving"
    );
}
