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
        std::os::unix::fs::symlink(
            checkout().join("scripts/with-orphan-postlude.sh"),
            root.path().join("scripts/with-orphan-postlude.sh"),
        )
        .expect("fixture: linking the tracked wrapper");
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn script(&self) -> PathBuf {
        self.path().join("scripts/check-no-orphan-servers.sh")
    }

    fn wrapper(&self) -> PathBuf {
        self.path().join("scripts/with-orphan-postlude.sh")
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
        ChildGuard::new(spawn_matching(&path, &self.live_store()))
    }

    /// A store file that exists, so a shim holding it reads as a live daemon
    /// of this worktree rather than as the abandoned class (SH-493). Inside
    /// the fixture, so it goes when the fixture does.
    fn live_store(&self) -> PathBuf {
        let path = self.path().join("store.db");
        if !path.exists() {
            std::fs::write(
                &path,
                b"never opened: the shim naming this is a shell script",
            )
            .expect("fixture: creating a store file for a live shim");
        }
        path
    }

    /// A store path that does not exist and will not be created — the
    /// abandoned class's whole definition.
    fn missing_store(&self) -> PathBuf {
        self.path().join("gone.db")
    }

    /// Runs the fixture's script for one phase.
    fn run(&self, args: &[&str]) -> Output {
        let script = self.script().display().to_string();
        let mut full = vec![script.as_str()];
        full.extend_from_slice(args);
        run(self.path(), "bash", &full)
    }

    /// Runs a command through the tracked unconditional-postlude wrapper.
    fn run_wrapped(&self, args: &[&str]) -> Output {
        let wrapper = self.wrapper().display().to_string();
        let mut full = vec![wrapper.as_str()];
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
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("fixture: creating {}: {e}", parent.display()));
    }
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
///
/// `store` decides which of the script's two classes the process falls into,
/// and every caller here has to mean one of them (SH-493). A store that
/// **exists** is a live daemon of some worktree — the only thing the
/// checkout-anchored pattern is entitled to reason about, and what every case
/// about that pattern wants. A store that is **gone** is the abandoned class,
/// which is collected in every phase by whoever finds it.
fn spawn_matching(exe: &Path, store: &Path) -> std::process::Child {
    Command::new(exe)
        .args([
            "--store-path",
            &store.display().to_string(),
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

/// The ceiling a "this must not wait out any grace period" assertion checks
/// against — half of `ORPHAN_GRACE_SECS` itself (`orphan_grace_secs`, below),
/// never a bare literal (`tests/timing_assertions.rs`'s own rule, SH-394): the
/// claim being disproved is specifically that the fast path does not consume
/// the grace loop, so the ceiling has to be defined in terms of that loop's
/// own length, not an unrelated guess at "fast."
fn no_grace_period_ceiling() -> std::time::Duration {
    std::time::Duration::from_secs(orphan_grace_secs() / 2)
}

fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether `pid` is a process that is still *running*, as opposed to one that
/// has died and is waiting to be collected.
///
/// [`pid_alive`] cannot answer this and must not be asked to: every shim here
/// is a direct child of the test process, so a shim that exits becomes a
/// **zombie** until this process waits on it — and `kill -0` succeeds on a
/// zombie, because the pid is still there to be signalled. An assertion that a
/// shim was killed, written against `pid_alive`, therefore fails whether or
/// not the kill worked. Asserting a shim is *alive* is safe either way, which
/// is why the cases that predate this one never had to know.
fn pid_running(pid: u32) -> bool {
    let out = Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output()
        .expect("running ps");
    let state = String::from_utf8_lossy(&out.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

// ---------------------------------------------------------------------------
// The provocation
// ---------------------------------------------------------------------------

/// SH-491 itself: a red test body used to make `make` stop before the orphan
/// postlude, so the daemon it leaked poisoned the next run. The wrapper must
/// preserve that red status, but only after the tracked postlude has collected
/// the process and left the next preflight clean.
#[test]
fn a_red_wrapped_body_is_returned_only_after_its_daemon_is_reaped() {
    let fixture = Fixture::new();
    let shim = fixture.shim("sleep 60");
    let store = fixture.live_store();
    let body = fixture.helper(
        "red-body.sh",
        &format!(
            "\"{}\" --store-path \"{}\" daemon --serve --port 0 </dev/null >/dev/null 2>&1 &\nsleep 0.3\nexit 23",
            shim.display(),
            store.display()
        ),
    );
    let body = body.display().to_string();

    let out = fixture.run_wrapped(&["--", "bash", &body]);
    assert_eq!(
        out.status.code(),
        Some(23),
        "the wrapper must preserve the red body's status after cleanup\nstderr: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("reaping"),
        "the red body's daemon must reach the real postlude\nstderr: {}",
        stderr(&out)
    );
    assert!(
        !fixture.anything_still_matches(),
        "the wrapper must not return while the red body's daemon is still alive"
    );

    let next = fixture.run(&["preflight"]);
    assert!(
        next.status.success(),
        "the next preflight must be clean without a manual kill\nstderr: {}",
        stderr(&next)
    );
}

/// Exit-status precedence is the wrapper's receipt-safety contract. A cleanup
/// failure must make a green body red, while an already-red body keeps its own
/// status so the original failing leg remains the primary diagnosis. In both
/// cases the cleanup diagnostic must remain visible.
#[test]
fn the_wrapper_preserves_a_body_failure_and_never_hides_a_cleanup_failure() {
    for (body_status, expected) in [(0, 41), (23, 23)] {
        let fixture = Fixture::new();
        std::fs::remove_file(fixture.script()).expect("fixture: removing the orphan symlink");
        write_executable(&fixture.script(), "echo cleanup-failed >&2\nexit 41");
        let command = format!("exit {body_status}");

        let out = fixture.run_wrapped(&["--", "bash", "-c", &command]);
        assert_eq!(
            out.status.code(),
            Some(expected),
            "body={body_status}, cleanup=41 must return {expected}\nstderr: {}",
            stderr(&out)
        );
        assert!(
            stderr(&out).contains("cleanup-failed"),
            "cleanup diagnostics must remain visible when body={body_status}\nstderr: {}",
            stderr(&out)
        );
    }

    let fixture = Fixture::new();
    std::fs::remove_file(fixture.script()).expect("fixture: removing the orphan symlink");
    write_executable(&fixture.script(), "echo cleanup-must-not-run >&2\nexit 41");
    let out = fixture.run_wrapped(&["--make-no-exec", "--", "bash", "-c", "exit 0"]);
    assert!(
        out.status.success(),
        "a recursive make no-exec operation must return its own status without running cleanup\nstderr: {}",
        stderr(&out)
    );
    assert!(
        !stderr(&out).contains("cleanup-must-not-run"),
        "--make-no-exec must not invoke the real orphan postlude\nstderr: {}",
        stderr(&out)
    );
}

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
        elapsed < no_grace_period_ceiling(),
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
            start.elapsed() < no_grace_period_ceiling(),
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
/// schedule regardless of what gets killed — the worker's own self-expiry
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
    //
    // The lifespan is a knob rather than a constant because this fixture has
    // to satisfy two requirements that pull in opposite directions, and it
    // used to try to satisfy both with one number (SH-493).
    let worker = fixture.shim("trap '' TERM\nsleep \"${SHIM_LIFETIME:-0.6}\"");

    // Requirement one: **something matching is alive at every instant** until
    // SIGKILL. The postlude's grace loop exits 0 on the first empty poll it
    // sees, so a momentary gap is this case passing the run over a clean tree
    // — reported as a failure with an *empty stderr*, having proved nothing.
    // That is what it did, roughly one run in three, once this file grew from
    // 7 tests to 13: on a loaded machine the supervisor's spawn loop can stall
    // for longer than a short-lived worker lives, and the population it is
    // supposed to be maintaining reaches zero underneath it.
    //
    // Tuning the cadence and the lifespan against each other only moves that
    // threshold, and the next test added to this file moves it back. So the
    // requirement is met by construction instead: ONE worker, spawned once,
    // that outlives the whole scenario. It cannot gap, because nothing about
    // it depends on a loop keeping up.
    let guarantor = Command::new(&worker)
        .args([
            "--store-path",
            &fixture.live_store().display().to_string(),
            "daemon",
            "--serve",
            "--port",
            "0",
        ])
        .env(
            "SHIM_LIFETIME",
            (orphan_grace_secs() + 3 * orphan_kill_grace_secs()).to_string(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("fixture: spawning the continuous worker");
    let _guarantor = ChildGuard::new(guarantor);

    // Requirement two: **something matching is alive again within the 0.5s
    // the script settles for after SIGKILL** — the one true race here, and
    // the only thing the supervisor below is still needed for. Its workers
    // keep their short lifespan, so the population stays around a dozen
    // rather than a hundred.
    // A wall-clock deadline rather than a fixed iteration count: what matters
    // is covering the postlude's own worst case, not a specific spawn count,
    // and this must keep respawning regardless of which individual workers the
    // postlude manages to kill.
    //
    // Derived from the script's own two constants rather than written as a
    // number (SH-394), because the worst case moved once already and a literal
    // did not notice: SH-493 put an abandoned-class collection ahead of every
    // phase's own question, which can itself spend a full SIGTERM wait before
    // the grace loop below has even started. Three kill-graces is one for that
    // collection, one for this phase's own SIGTERM wait, and one of margin so
    // the supervisor is provably still respawning at the moment SIGKILL lands
    // — which is the entire condition this case exists to construct.
    // Runs as its OWN process, never under `target/debug/`, so it never
    // matches the pattern itself — only the workers it spawns do. The 0.05s
    // cadence against that 0.5s settle window is deliberately lopsided (~10
    // fresh spawns expected in it, deterministically scheduled rather than
    // left to chance).
    let supervisor = fixture.helper(
        "supervisor.sh",
        &format!(
            "deadline=$((SECONDS + {}))\nwhile [ \"$SECONDS\" -lt \"$deadline\" ]; do\n  \"{}\" --store-path \"{}\" daemon --serve --port 0 &\n  sleep 0.05\ndone",
            orphan_grace_secs() + 3 * orphan_kill_grace_secs(),
            worker.display(),
            fixture.live_store().display()
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

/// `readonly ORPHAN_KILL_GRACE_SECS=<n>` in the tracked script — how long a
/// SIGTERM gets before the script escalates.
fn orphan_kill_grace_secs() -> u64 {
    let src = read_checkout_file("scripts/check-no-orphan-servers.sh");
    let marker = "readonly ORPHAN_KILL_GRACE_SECS=";
    let after = src
        .split(marker)
        .nth(1)
        .unwrap_or_else(|| panic!("scripts/check-no-orphan-servers.sh must declare `{marker}<n>`"));
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("ORPHAN_KILL_GRACE_SECS's value {digits:?} must parse: {e}"))
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

// ---------------------------------------------------------------------------
// SH-493: the class the checkout-anchored pattern structurally cannot see
// ---------------------------------------------------------------------------
//
// `tests/plugin_install.rs` copies the binary into its own fixture and runs
// `<fixture>/package/story`, on purpose, to prove path resolution from an
// installed layout. Every daemon it left behind ran from that copy, so no
// pattern anchored at `${repo_root}/target/debug` could ever match one — 672
// were alive on one machine across three days, while the four siblings that
// happened to run the checkout binary were reaped every run and hid the fact
// that anything was accumulating at all.
//
// The scoping key is what the process IS, not where its binary sits: a daemon
// whose `--store-path` names a file that no longer exists is serving nobody.
// That is checkout-agnostic on purpose, which is the only way to reach a copy,
// and safe precisely because it is unambiguous — unlike a merely looser
// regex, which would refuse this run over a concurrent sibling worktree's LIVE
// suite at the preflight and murder one at the postlude.
//
// # These cases are global, and say so
//
// Everything below spawns a process the running machine's OTHER worktree
// suites can also see — that is the property under test, so it cannot be
// fixtured away the way the rest of this file scopes itself under its own
// checkout path. Two consequences are designed around rather than hoped
// against:
//
// * A sibling's own orphan check may reap this test's abandoned shim first.
//   So the assertions are on **the script's own report naming the pid**, never
//   on the process merely being gone afterwards, which a sibling could satisfy
//   for us and turn the case vacuous.
// * The two negative cases hold shims a sibling must not touch either. The
//   live-store one is safe by construction — its store exists, which is the
//   whole rule. The too-young one is safe for as long as it is too young,
//   which is `ABANDONED_STORE_MIN_AGE_SECS`, and it asserts immediately.

/// A daemon running from a copy of the binary somewhere else entirely, on a
/// store that is gone, is collected — the exact population SH-493 counted.
#[test]
fn a_daemon_on_a_vanished_store_is_collected_wherever_its_binary_lives() {
    let fixture = Fixture::new();
    // Deliberately NOT under `target/debug`: the point of this case is the one
    // place the checkout-anchored pattern cannot reach. `package/story` is the
    // literal layout `tests/plugin_install.rs::Harness::new(true)` builds.
    let packaged = fixture.helper("package/story", SLEEPS_UNTIL_KILLED);
    let child = spawn_matching(&packaged, &fixture.missing_store());
    let pid = child.id();
    let _guard = ChildGuard::new(child);
    wait_until_old_enough();

    let out = fixture.run(&["preflight"]);
    let err = stderr(&out);

    assert!(
        err.contains("no longer exists"),
        "the abandoned class must be named when it is collected — a detector \
         whose subject is a population that accumulated unnoticed for three \
         days does not get to be the next quiet thing (SH-306)\nstderr: {err}"
    );
    assert!(
        !pid_running(pid),
        "a daemon serving a store that is gone is serving nobody and must be \
         collected, even though its binary is at {} — which is exactly where \
         the checkout-anchored pattern cannot look\nstderr: {err}",
        packaged.display()
    );

    fixture.sweep();
}

/// …and collecting it is never a refusal, in any phase.
///
/// The preflight refuses on its own class because a live stranger's daemon
/// makes this run's verification a lie. This class is provably nobody's, and
/// there can be hundreds of them at once from other worktrees — failing a run
/// over a mess it did not make is the SH-306 pressure exactly.
#[test]
fn collecting_an_abandoned_daemon_never_fails_the_phase() {
    let fixture = Fixture::new();
    let packaged = fixture.helper("package/story", SLEEPS_UNTIL_KILLED);
    let child = spawn_matching(&packaged, &fixture.missing_store());
    let _guard = ChildGuard::new(child);
    wait_until_old_enough();

    for phase in ["preflight", "postlude", "check"] {
        let out = fixture.run(&[phase]);
        assert!(
            out.status.success(),
            "`{phase}` must collect an abandoned daemon rather than refuse over \
             one\nstderr: {}",
            stderr(&out)
        );
    }

    fixture.sweep();
}

/// A daemon whose store still exists is never touched — the property that
/// keeps this checkout-agnostic rule from reaching a concurrent sibling
/// worktree's live suite.
#[test]
fn a_daemon_whose_store_still_exists_is_left_alone() {
    let fixture = Fixture::new();
    let packaged = fixture.helper("package/story", SLEEPS_UNTIL_KILLED);
    let child = spawn_matching(&packaged, &fixture.live_store());
    let pid = child.id();
    let _guard = ChildGuard::new(child);
    wait_until_old_enough();

    let out = fixture.run(&["preflight"]);
    let err = stderr(&out);

    assert!(
        out.status.success(),
        "a daemon outside this checkout holding a live store is somebody \
         else's business and not a refusal\nstderr: {err}"
    );
    assert!(
        pid_running(pid),
        "a daemon whose store EXISTS may be a concurrent worktree suite's live \
         daemon — this machine runs three or four at once — and killing one \
         would false-red a suite that was doing nothing wrong\nstderr: {err}"
    );

    fixture.sweep();
}

/// A daemon too young to have opened its store yet is never touched.
///
/// The one shape a bare existence check gets wrong: between `spawn_child` and
/// the store being created there is a real window in which a perfectly healthy
/// daemon has no store file, and reaping there would be this script causing
/// the failure it exists to report.
#[test]
fn a_daemon_too_young_to_have_opened_its_store_is_left_alone() {
    let fixture = Fixture::new();
    let packaged = fixture.helper("package/story", SLEEPS_UNTIL_KILLED);
    let child = spawn_matching(&packaged, &fixture.missing_store());
    let pid = child.id();
    let _guard = ChildGuard::new(child);
    // No wait: the assertion has to happen inside the age floor, which is also
    // what keeps a concurrent sibling's orphan check off this shim.

    let out = fixture.run(&["preflight"]);
    let err = stderr(&out);

    assert!(
        pid_running(pid),
        "a daemon younger than ABANDONED_STORE_MIN_AGE_SECS has not been given \
         up on by anybody yet — its client is still inside SPAWN_DEADLINE — so \
         a missing store means it is starting, not abandoned\nstderr: {err}"
    );

    fixture.sweep();
}

/// A store path containing a space is read whole — the case that decides
/// whether this rule can kill the developer's own daemon.
///
/// macOS hands out home directories like `/Users/Ada Lovelace` without
/// comment, and the real store lives under one. Reading `--store-path`'s
/// argument as the next whitespace-delimited field yields `/Users/Ada`, which
/// does not exist, which classifies a perfectly healthy production daemon as
/// abandoned and kills it — the single worst thing this rule could do, and
/// invisible on any machine whose own paths happen to have no spaces.
/// Delimiting on the verb that follows instead is what makes the read exact;
/// this is the case that fails if anyone ever goes back to field-splitting.
#[test]
fn a_store_path_containing_a_space_is_read_whole_and_its_daemon_left_alone() {
    let fixture = Fixture::new();
    let spaced = fixture.path().join("Ada Lovelace/store.db");
    std::fs::create_dir_all(spaced.parent().unwrap()).expect("fixture: creating a spaced dir");
    std::fs::write(&spaced, b"exists, and is therefore nobody's to collect")
        .expect("fixture: creating the spaced store");

    let packaged = fixture.helper("package/story", SLEEPS_UNTIL_KILLED);
    let child = spawn_matching(&packaged, &spaced);
    let pid = child.id();
    let _guard = ChildGuard::new(child);
    wait_until_old_enough();

    let out = fixture.run(&["preflight"]);
    let err = stderr(&out);

    assert!(
        pid_running(pid),
        "the store at {} EXISTS, so this daemon is not abandoned — a reader \
         that stops at the first space sees a path that does not, and the \
         daemon it kills on that reading is the developer's own\nstderr: {err}",
        spaced.display()
    );

    fixture.sweep();
}

/// The age floor is derived from the deadline it disproves, not picked.
///
/// `SPAWN_DEADLINE` is how long a client waits for a daemon it just spawned to
/// answer; past it, the only process that was waiting has already given up, so
/// a still-storeless daemon is not starting up. Read out of both sources as
/// text rather than imported, the way `the_grace_period_is_derived_from_the_
/// parent_watch_tick_it_disproves` above reads `SHUTDOWN_CHECK` — a shell
/// script cannot import a Rust constant, and widening the constant's
/// visibility to let a test do it would be a production change in service of
/// a test.
#[test]
fn the_abandoned_age_floor_is_derived_from_the_spawn_deadline_it_disproves() {
    let floor = abandoned_store_min_age_secs();
    let spawn_deadline = spawn_deadline_secs();

    assert!(
        floor >= spawn_deadline * MIN_AGE_MULTIPLE_OF_SPAWN_DEADLINE,
        "scripts/check-no-orphan-servers.sh's ABANDONED_STORE_MIN_AGE_SECS \
         ({floor}s) has drifted too close to src/daemon/lifecycle.rs's \
         SPAWN_DEADLINE ({spawn_deadline}s). A daemon still inside \
         SPAWN_DEADLINE has a client actively waiting for it, so a missing \
         store means it is starting rather than abandoned — reaping there \
         would make this script the cause of the failure it exists to report."
    );
}

/// `readonly ABANDONED_STORE_MIN_AGE_SECS=<n>` in the tracked script.
fn abandoned_store_min_age_secs() -> u64 {
    let src = read_checkout_file("scripts/check-no-orphan-servers.sh");
    let marker = "readonly ABANDONED_STORE_MIN_AGE_SECS=";
    let after = src
        .split(marker)
        .nth(1)
        .unwrap_or_else(|| panic!("scripts/check-no-orphan-servers.sh must declare `{marker}<n>`"));
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or_else(|e| {
        panic!("ABANDONED_STORE_MIN_AGE_SECS's value {digits:?} must parse: {e}")
    })
}

/// `pub const SPAWN_DEADLINE: Duration = Duration::from_secs(<n>);` in
/// `src/daemon/lifecycle.rs`.
fn spawn_deadline_secs() -> u64 {
    let src = read_checkout_file("src/daemon/lifecycle.rs");
    let marker = "pub const SPAWN_DEADLINE: Duration = Duration::from_secs(";
    let after = src
        .split(marker)
        .nth(1)
        .unwrap_or_else(|| panic!("src/daemon/lifecycle.rs must declare `{marker}<n>);`"));
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("SPAWN_DEADLINE's value {digits:?} must parse: {e}"))
}

/// The multiple of `SPAWN_DEADLINE` the age floor must clear. Today's actual
/// ratio is 2x (10s / 5s); this floor is the ratio itself rather than half of
/// it, because unlike the grace period there is no margin to give away — one
/// `SPAWN_DEADLINE` is exactly the span in which a storeless daemon is still
/// somebody's live start-up.
const MIN_AGE_MULTIPLE_OF_SPAWN_DEADLINE: u64 = 2;

/// A shim that does nothing but stay alive, so a case can decide what it is
/// purely by the argv `spawn_matching` gives it.
const SLEEPS_UNTIL_KILLED: &str = "while :; do sleep 0.2; done";

/// Sleeps past the age floor, so a storeless shim reads as abandoned rather
/// than as starting up. Derived from the script's own constant, never a bare
/// literal (SH-394), plus one second so the comparison is not decided by
/// where `ps`'s whole-second `etime` happens to round.
fn wait_until_old_enough() {
    std::thread::sleep(std::time::Duration::from_secs(
        abandoned_store_min_age_secs() + 1,
    ));
}
