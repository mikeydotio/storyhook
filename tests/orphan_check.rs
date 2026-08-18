//! `scripts/check-no-orphan-servers.sh`, **provoked** — not inspected (SH-412).
//!
//! This gate had no test anywhere in the repo before this file, and its
//! pattern has already been silently broken once (SH-113 moved `--store-path`
//! ahead of the verb; a guard that matches nothing passes). SH-306's own
//! doctrine is that a gate is proven by provoking it — the style
//! `tests/push_gate.rs` and `tests/merge_gate.rs` already use, real processes
//! and the tracked script itself, never a copy or a mock.
//!
//! # The defect this fixes
//!
//! On 2026-08-17, `make test` ran every leg green (rust-suite 873s) and the
//! *postlude* then refused, naming two test-spawned daemons with etimes
//! 2m16s and 7m39s — both gone within a minute. No receipt was minted (the
//! postlude is `make`'s last-but-one line and `make` fail-fasts), so the only
//! sanctioned path was a 22-minute re-run of a suite that had already proved
//! itself. That is exactly the pressure SH-306 was filed for.
//!
//! `SHUTDOWN_CHECK` (`src/daemon/serve.rs`) is 250ms and is the ONLY thing
//! bounding how long a correctly-contained daemon outlives its parent — by
//! postlude time every test binary this run started has already exited, so a
//! survivor of the grace period is provably not "still winding down." The
//! preflight is a hard prerequisite of `make test` and refuses on any match,
//! so anything the postlude still finds was provably spawned by *this* run.
//! The fix: the postlude now reaps a survivor (SIGTERM, bounded wait, then
//! SIGKILL, then verify) instead of failing the whole suite over it, and
//! fails only if something survives SIGKILL — a genuinely different fact.
//!
//! # Fixture shape
//!
//! Every case below builds its own disposable, git-initialized root (never
//! this checkout, never a sibling worktree) with `target/debug/story` as an
//! executable shell shim and a **symlink** to the tracked
//! `scripts/check-no-orphan-servers.sh` — so the artifact under test is the
//! one that ships, while `${BASH_SOURCE[0]}`'s own `repo_root` resolution
//! still lands inside the fixture. That is what keeps `pgrep -f` — which is
//! global on this machine — from ever seeing one of the 3-4 concurrent
//! sibling worktree suites this project's own `HARDENING_PROGRESS.md`
//! documents, or them from seeing this test's shims.
//!
//! Every shim is spawned with the **exact argv shape**
//! [`spawn_child`](../src/daemon/lifecycle.rs) builds for a real daemon:
//! `--store-path <p> daemon --serve --port <n>`. That is deliberate — it
//! makes every case here double as the positive control the plan calls for:
//! if the script's pattern ever stops matching production's actual shape
//! (the SH-113 hazard), a refusal that should fire does not, and the test
//! expecting that refusal fails loudly rather than the suite passing
//! vacuously.

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
/// skipping — a moved or renamed source file is a finding, not a reason for
/// this pin to go quiet.
fn read_checkout_file(relative: &str) -> String {
    let path = checkout().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

/// A worktree-shaped fixture root: its own git repo (needed for the script's
/// own `git rev-parse --git-common-dir` durable-report resolution) with
/// `target/debug/` and a symlink to the tracked script.
struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = scratch_dir();
        run(root.path(), "git", &["init", "-q"]);
        std::fs::create_dir_all(root.path().join("target/debug"))
            .expect("fixture: creating target/debug");
        std::fs::create_dir_all(root.path().join("scripts")).expect("fixture: creating scripts/");
        std::os::unix::fs::symlink(
            checkout().join("scripts/check-no-orphan-servers.sh"),
            root.path().join("scripts/check-no-orphan-servers.sh"),
        )
        .expect("fixture: linking the tracked script");
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn script(&self) -> PathBuf {
        self.path().join("scripts/check-no-orphan-servers.sh")
    }

    /// Writes an executable shell shim at `target/debug/story` — the exact
    /// path, relative to a worktree root, production's daemon binary lives
    /// at.
    fn shim(&self, body: &str) -> PathBuf {
        let path = self.path().join("target/debug/story");
        write_executable(&path, body);
        path
    }

    /// Writes an executable shell script at an arbitrary fixture-relative
    /// path — for a supervisor process that must NOT itself match the
    /// script's pattern (see the SIGKILL-survivor case below).
    fn helper(&self, name: &str, body: &str) -> PathBuf {
        let path = self.path().join(name);
        write_executable(&path, body);
        path
    }

    /// Spawns `target/debug/story` with the exact argv shape
    /// `spawn_child` (`src/daemon/lifecycle.rs`) builds for a real daemon.
    fn spawn_matching_shim(&self) -> ChildGuard {
        let path = self.path().join("target/debug/story");
        ChildGuard::new(spawn_matching(&path))
    }

    /// Runs the fixture's script for one phase.
    fn run(&self, args: &[&str]) -> Output {
        let script = self.script().display().to_string();
        let mut full = vec![script.as_str()];
        full.extend_from_slice(args);
        run(self.path(), "bash", &full)
    }

    /// Whether the pattern this fixture's script would use still matches
    /// anything alive under it — a direct, script-independent liveness
    /// check used to confirm a shim was (or was not) actually killed.
    fn anything_still_matches(&self) -> bool {
        let out = Command::new("pgrep")
            .arg("-f")
            .arg(self.path().join("target/debug").display().to_string())
            .output()
            .expect("running pgrep");
        out.status.success() && !out.stdout.is_empty()
    }

    /// Best-effort sweep: kills anything still matching under this fixture,
    /// so a test whose assertions already ran does not leak a process onto
    /// the machine regardless of what the script itself did or did not
    /// clean up. Bounded, not a loop that could hang a test run.
    fn sweep(&self) {
        for _ in 0..20 {
            if !self.anything_still_matches() {
                return;
            }
            let out = Command::new("pgrep")
                .arg("-f")
                .arg(self.path().join("target/debug").display().to_string())
                .output()
                .expect("running pgrep");
            let pids = String::from_utf8_lossy(&out.stdout);
            for pid in pids.split_whitespace() {
                // A pid already gone between the `pgrep` snapshot above and
                // this call is not a finding here, just a race in a
                // best-effort sweep -- `kill`'s own "no such process" belongs
                // on nobody's screen for that.
                let _ = Command::new("kill")
                    .args(["-9", pid])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.sweep();
    }
}

fn write_executable(path: &Path, body: &str) {
    let mut file = std::fs::File::create(path)
        .unwrap_or_else(|e| panic!("fixture: creating {}: {e}", path.display()));
    write!(file, "#!/usr/bin/env bash\n{body}\n")
        .unwrap_or_else(|e| panic!("fixture: writing {}: {e}", path.display()));
    let mut perms = file
        .metadata()
        .unwrap_or_else(|e| panic!("fixture: stat {}: {e}", path.display()))
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
        .unwrap_or_else(|e| panic!("fixture: chmod {}: {e}", path.display()));
}

/// The argv shape `spawn_child` (`src/daemon/lifecycle.rs`) actually builds:
/// `<exe> --store-path <store> daemon --serve --port <port>`.
fn spawn_matching(exe: &Path) -> std::process::Child {
    Command::new(exe)
        .args([
            "--store-path",
            "/private/tmp/orphan-check-fixture-store",
            "daemon",
            "--serve",
            "--port",
            "0",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("fixture: spawning {}: {e}", exe.display()))
}

fn run(cwd: &Path, program: &str, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("running {program} {args:?}: {e}"))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// The provocation
// ---------------------------------------------------------------------------

/// The defect itself, and its fix, in one test: a shim alive past the grace
/// period that ignores SIGTERM is escalated to SIGKILL and the postlude
/// still succeeds — no refusal, no thrown-away green suite. Doubles as the
/// positive control (see the module doc): if the script's pattern stopped
/// matching this exact argv shape, the shim would never be found at all, and
/// this test would fail on "nothing was reaped" rather than passing
/// vacuously.
#[test]
fn a_survivor_that_ignores_sigterm_is_reaped_by_sigkill_and_the_postlude_still_passes() {
    let fixture = Fixture::new();
    fixture.shim("trap '' TERM\nsleep 60");
    let guard = fixture.spawn_matching_shim();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        fixture.anything_still_matches(),
        "fixture: the shim must be alive before the test begins"
    );

    let out = fixture.run(&["postlude"]);
    let err = stderr(&out);
    assert!(
        out.status.success(),
        "the postlude must reap a leaked, SIGTERM-ignoring test daemon rather than \
         refuse a suite that already went green\nstderr: {err}"
    );
    assert!(
        err.contains("reaping"),
        "the postlude must report what it reaped\nstderr: {err}"
    );
    assert!(
        err.contains("SIGKILL"),
        "a shim that ignores SIGTERM must be escalated to SIGKILL, and the postlude \
         must say so\nstderr: {err}"
    );
    assert!(
        !fixture.anything_still_matches(),
        "the shim must actually be gone once the postlude reports success"
    );
    // The kernel keeps a killed child's exit status pending until its parent
    // reaps it — `kill -0` would still see that pid as "alive" even though
    // `pgrep -f` (what the script itself, and the assertion above, use) has
    // already stopped matching it. Reap it here rather than leaving a zombie
    // for the fixture's own Drop to trip over.
    drop(guard);
}

/// A process that exits *on its own* during the grace window is the defence
/// working, not a leak — the postlude must say nothing about it. Only a
/// process that needed a signal is a leak; a self-exiting shim never needed
/// one.
#[test]
fn a_process_that_exits_on_its_own_within_the_grace_period_is_never_reported() {
    let fixture = Fixture::new();
    fixture.shim("sleep 1");
    let guard = fixture.spawn_matching_shim();
    std::thread::sleep(std::time::Duration::from_millis(200));

    let out = fixture.run(&["postlude"]);
    let err = stderr(&out);
    assert!(
        out.status.success(),
        "a self-exiting shim must not fail the postlude\nstderr: {err}"
    );
    assert!(
        err.trim().is_empty(),
        "the postlude must say nothing about a process that exited on its own — \
         only a process this run actually had to signal is a leak\nstderr: {err}"
    );
    drop(guard);
}

/// The preflight is unchanged: no grace period, no killing, refuse and name
/// it. A pre-existing match makes THIS run's own verification a lie, and it
/// may be a daemon the developer started on purpose — not this script's
/// business to stop. Doubles as the positive control for the preflight path.
#[test]
fn preflight_refuses_a_live_match_immediately_and_does_not_kill_it() {
    let fixture = Fixture::new();
    fixture.shim("sleep 30");
    let guard = fixture.spawn_matching_shim();
    let pid = guard.pid();
    std::thread::sleep(std::time::Duration::from_millis(300));

    let start = std::time::Instant::now();
    let out = fixture.run(&["preflight"]);
    let elapsed = start.elapsed();
    let err = stderr(&out);

    assert!(
        !out.status.success(),
        "the preflight must refuse a pre-existing match\nstderr: {err}"
    );
    assert!(
        err.contains("preflight"),
        "the refusal must name the phase it fired in\nstderr: {err}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "the preflight must refuse immediately, with no grace period — took {elapsed:?}"
    );
    assert!(
        pid_alive(pid),
        "the preflight must never kill what it refuses on — that decision belongs to \
         whoever started it"
    );
}

/// A clean fixture — nothing matching anywhere — passes every phase, fast.
/// This is the ordinary case in real CI, and it must stay a fast path: the
/// postlude's own grace loop checks before it ever sleeps.
#[test]
fn a_clean_fixture_passes_every_phase_quickly() {
    let fixture = Fixture::new();
    for phase in ["preflight", "postlude"] {
        let start = std::time::Instant::now();
        let out = fixture.run(&[phase]);
        assert!(
            out.status.success(),
            "phase {phase} must pass on a clean fixture\nstderr: {}",
            stderr(&out)
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "phase {phase} must not wait out any grace period when nothing matches"
        );
    }
    let out = fixture.run(&["check", "a label"]);
    assert!(
        out.status.success(),
        "the 'check' phase must behave the same as preflight on a clean fixture\nstderr: {}",
        stderr(&out)
    );
}

/// `$1` used to be dual-purpose: `preflight`/`postlude` were phases and
/// anything else fell through to the strict, ungraced check as a free-text
/// label (`scripts/capture-baseline.sh` relied on exactly this). A typo'd
/// phase therefore silently became the strict check — the wrong verdict for
/// the word actually typed, and this story's own failure shape (SH-357's
/// class: an argument that lands nowhere must be refused, not misread).
#[test]
fn an_unrecognized_phase_is_refused_with_a_usage_message() {
    let fixture = Fixture::new();
    for args in [vec!["postlood"], vec![]] {
        let out = fixture.run(&args);
        let err = stderr(&out);
        assert!(
            !out.status.success(),
            "phase {args:?} must be refused, not silently reinterpreted\nstderr: {err}"
        );
        assert!(
            err.contains("usage:"),
            "the refusal must say how the script is actually invoked\nstderr: {err}"
        );
    }
}

/// The one blocking case: something the postlude's single bounded
/// SIGTERM-then-SIGKILL round cannot actually clear must fail the suite
/// loudly, naming what survived — that is a real leak, not the defence
/// working, and certifying a receipt over it would be exactly the silent
/// failure SH-306 was filed against.
///
/// Modeled with a small supervisor (`helper`, never itself matching the
/// script's pattern) that keeps a fresh matching worker alive on a fixed
/// schedule regardless of what gets killed — the worker's own 3s lifespan
/// keeps this self-cleaning even if the fixture's own sweep did not run.
#[test]
fn postlude_fails_when_a_survivor_outlives_sigkill() {
    let fixture = Fixture::new();
    // Ignoring SIGTERM matters here for a subtler reason than it first looks:
    // the postlude's SIGTERM-wait loop `break`s the instant it sees ANY empty
    // poll, so a batch that all died together from an unignored SIGTERM could
    // hand the loop a lucky empty gap before the supervisor's next spawn and
    // let it declare victory without ever reaching SIGKILL. Ignoring TERM
    // (but still self-expiring via `sleep`) keeps each worker alive on its
    // own staggered schedule instead, so a captured batch cannot die as one
    // synchronized event until SIGKILL actually reaches it.
    let worker = fixture.shim("trap '' TERM\nsleep 0.6");
    // A wall-clock deadline rather than a fixed iteration count: what matters
    // is covering the postlude's own worst case (10s grace + 5s SIGTERM wait
    // + settle), not a specific spawn count, and this must keep respawning
    // regardless of which individual workers the postlude manages to kill.
    // Runs as its OWN process, never under `target/debug/`, so it never
    // matches the pattern itself — only the workers it spawns do. The 0.05s
    // cadence against a 0.5s post-SIGKILL settle window is deliberately
    // lopsided (~10 fresh spawns expected in that window, deterministically
    // scheduled rather than left to chance) so the one true race in this
    // fixture — is something new alive the instant the postlude looks again
    // after SIGKILL — resolves the same way every run.
    let supervisor = fixture.helper(
        "supervisor.sh",
        &format!(
            "deadline=$((SECONDS + 18))\nwhile [ \"$SECONDS\" -lt \"$deadline\" ]; do\n  \"{}\" --store-path /private/tmp/orphan-check-fixture-store daemon --serve --port 0 &\n  sleep 0.05\ndone",
            worker.display()
        ),
    );
    let mut supervisor_child = Command::new(&supervisor)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("fixture: spawning the supervisor");
    std::thread::sleep(std::time::Duration::from_millis(600));

    let out = fixture.run(&["postlude"]);
    let err = stderr(&out);

    let _ = supervisor_child.kill();
    let _ = supervisor_child.wait();

    assert!(
        !out.status.success(),
        "a survivor of SIGKILL must fail the postlude rather than certify a tree over \
         a real leak\nstderr: {err}"
    );
    assert!(
        err.contains("SIGKILL") && err.contains("survived"),
        "the failure must say plainly that this is a real leak, not the defence \
         working\nstderr: {err}"
    );

    fixture.sweep();
}

// ---------------------------------------------------------------------------
// The grace period's derivation (SH-394: no bare wall-clock literal)
// ---------------------------------------------------------------------------

/// `readonly ORPHAN_GRACE_SECS=<n>` in the tracked script.
fn orphan_grace_secs() -> u64 {
    let src = read_checkout_file("scripts/check-no-orphan-servers.sh");
    let marker = "readonly ORPHAN_GRACE_SECS=";
    let after = src
        .split(marker)
        .nth(1)
        .unwrap_or_else(|| panic!("scripts/check-no-orphan-servers.sh must declare `{marker}<n>`"));
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("ORPHAN_GRACE_SECS's value {digits:?} must parse: {e}"))
}

/// `pub(crate) const SHUTDOWN_CHECK: Duration = Duration::from_millis(<n>);`
/// in `src/daemon/serve.rs` — the parent-watch poll tick, and the only bound
/// on how long a correctly-contained daemon outlives its parent by the time
/// the postlude runs. Read as source text rather than imported: the constant
/// is deliberately `pub(crate)` (internal to the daemon module), and widening
/// its visibility just so a test could import it would be a production
/// change in service of the test, not the other way around — the same
/// reasoning `tests/timing_assertions.rs` and
/// `tests/dashboard_mutation_deadline.rs` already apply to a cross-language
/// or cross-visibility constant.
fn shutdown_check_ms() -> u64 {
    let src = read_checkout_file("src/daemon/serve.rs");
    let marker = "pub(crate) const SHUTDOWN_CHECK: Duration = Duration::from_millis(";
    let after = src
        .split(marker)
        .nth(1)
        .unwrap_or_else(|| panic!("src/daemon/serve.rs must declare `{marker}<n>);`"));
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("SHUTDOWN_CHECK's value {digits:?} must parse: {e}"))
}

/// The minimum multiple of `SHUTDOWN_CHECK` the grace period must clear.
/// Today's actual ratio is 40x (10s / 250ms); this floor is set at half that
/// — generous headroom against a small, deliberate change to either
/// constant, while still catching the shape of drift that matters: the grace
/// period being cut without anyone re-deriving it from what it is supposed to
/// disprove.
const MIN_GRACE_MULTIPLE_OF_SHUTDOWN_CHECK: u64 = 20;

#[test]
fn the_grace_period_is_derived_from_the_parent_watch_tick_it_disproves() {
    let grace_ms = orphan_grace_secs() * 1000;
    let tick_ms = shutdown_check_ms();
    assert!(
        grace_ms >= tick_ms * MIN_GRACE_MULTIPLE_OF_SHUTDOWN_CHECK,
        "scripts/check-no-orphan-servers.sh's ORPHAN_GRACE_SECS ({grace_ms}ms) has drifted \
         too close to src/daemon/serve.rs's SHUTDOWN_CHECK ({tick_ms}ms) — by postlude time \
         every test binary has already exited, so SHUTDOWN_CHECK is the only bound left on \
         how long a correctly-contained daemon can still be shutting down; a grace period \
         that is not comfortably larger than it stops being able to tell 'still winding down' \
         apart from 'leaked', which is the SH-412 defect this test exists to catch."
    );
}
