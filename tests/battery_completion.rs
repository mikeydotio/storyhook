//! A Rust battery finishes after its first red test binary — SH-697.
//!
//! `scripts/run-tests.sh` hands Cargo every integration binary of a battery
//! in one `cargo test -p storyhook --test a --test b …` invocation. Cargo's
//! default is fail-fast *across binaries*: the first one with a failing case
//! ends the run and every later binary never executes, so the verifier's RED
//! summary carried one binary's failures and the agent found the next
//! binary's on resubmission (`tests/store_isolation.rs` records three of four
//! sibling defects hidden exactly this way). The runner now asks Cargo to run
//! every binary regardless.
//!
//! Two proofs, because they cover different holes. The first drives the
//! tracked runner with the REAL `cargo` over a disposable crate whose first
//! binary is red and whose second is green, and asserts the second ran — a
//! fake cargo that "stops after the first `--test`" would be modelling the
//! very behaviour under test. The second pins the argv every executing
//! invocation receives through a recording fake, because the real-cargo case
//! reaches only the integration path and the flag has to survive on the
//! workspace, lib and doctest paths too, while the `--list` discovery calls —
//! which execute nothing — must stay exactly as they are.
//!
//! The workspace regressions cover package resolution and ambiguous target names.
//!
//! The pooled cases (SH-783) drive the same fixture with
//! `STORYHOOK_TEST_THREAD_BUDGET` set: binaries that can only pass side by side
//! prove the pool runs them together, the per-binary blocks and the ledger
//! prove each one's output still arrives whole and in order, and a cancelled
//! run proves the pool's binaries go with it.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

#[path = "battery_completion/workspace.rs"]
mod workspace;

fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// A git repository holding a two-binary crate and symlinks to every tracked
/// helper under `scripts/`, so the runner under test is the tracked one and
/// nothing here can drift from it.
struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = scratch_dir();
        let path = root.path();
        for dir in ["scripts", "bin", "locks", "src", "tests"] {
            fs::create_dir_all(path.join(dir))
                .unwrap_or_else(|e| panic!("fixture: creating {dir}/: {e}"));
        }
        // Every tracked shell, Python and awk helper, derived rather than
        // listed: `run-tests.sh` reaches a dozen of them, and a hand-kept
        // list is the shape that turned `tests/gate_lock.rs` red when a
        // thirteenth appeared.
        for entry in fs::read_dir(checkout().join("scripts"))
            .expect("fixture: reading the checkout's scripts/")
        {
            let entry = entry.expect("fixture: a scripts/ entry");
            let name = entry.file_name();
            let name = name.to_str().expect("a UTF-8 script name");
            if !(name.ends_with(".sh") || name.ends_with(".py") || name.ends_with(".awk")) {
                continue;
            }
            std::os::unix::fs::symlink(entry.path(), path.join("scripts").join(name))
                .unwrap_or_else(|e| panic!("fixture: linking the tracked {name}: {e}"));
        }
        // The package is named `storyhook` because `--only` addresses
        // integration binaries as `cargo test -p storyhook --test <name>`.
        // `[workspace]` keeps Cargo from adopting the crate into any
        // enclosing workspace the scratch root might sit under.
        let fixture = Self { root };
        fixture.write(
            "Cargo.toml",
            "[package]\nname = \"storyhook\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
        );
        fixture.write("src/lib.rs", "//! SH-697 battery fixture\n");
        fixture.write(
            "tests/first.rs",
            "#[test]\nfn first_fails() {\n    panic!(\"the first binary of the battery is red on purpose\");\n}\n",
        );
        fixture.write("tests/second.rs", "#[test]\nfn second_passes() {}\n");
        fixture.git(&["init", "-q", "-b", "main"]);
        fixture.git(&["config", "user.email", "battery@example.test"]);
        fixture.git(&["config", "user.name", "Battery Completion Test"]);
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-q", "-m", "fixture"]);
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn write(&self, relative: &str, contents: &str) {
        fs::write(self.path().join(relative), contents)
            .unwrap_or_else(|e| panic!("fixture: writing {relative}: {e}"));
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .output()
            .expect("running git");
        assert!(
            out.status.success(),
            "git {args:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Installs a fake `cargo` ahead of the real one on the fixture's PATH.
    fn fake_cargo(&self, body: &str) {
        let path = self.path().join("bin/cargo");
        fs::write(&path, body).expect("writing the fake cargo");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("making the fake cargo executable");
    }

    /// The tracked runner, with the same containment `tests/gate_lock.rs`
    /// gives it: a private lock root, a private build directory, and none of
    /// the outer run's lock, journal or diagnostics state — each of which
    /// would otherwise change which branch the script takes.
    fn run_tests(&self, args: &[&str]) -> Command {
        let inherited = std::env::var("PATH").unwrap_or_default();
        let mut cmd = Command::new("bash");
        cmd.arg(self.path().join("scripts/run-tests.sh"))
            .args(args)
            .current_dir(self.path())
            .env(
                "PATH",
                format!("{}:{inherited}", self.path().join("bin").display()),
            )
            .env("STORYHOOK_LOCK_DIR", self.path().join("locks"))
            .env("CARGO_TARGET_DIR", self.path().join("target"))
            .env_remove("STORYHOOK_MACHINE_LOCKS")
            .env_remove("STORYHOOK_GATE_LOCK_TAKEN")
            .env_remove("STORYHOOK_GATE_LOCK_DEPTH")
            .env_remove("STORYHOOK_GATE_LOCK")
            .env_remove("STORYHOOK_GATE_PROGRESS")
            .env_remove("STORYHOOK_GATE_PROGRESS_PATH")
            .env_remove("STORYHOOK_COMPILER_DIAGNOSTICS")
            .env_remove("STORYHOOK_TEST_THREAD_BUDGET")
            // A jobserver inherited from the `make` running this suite names
            // descriptors this child does not have.
            .env_remove("MAKEFLAGS")
            .env_remove("MFLAGS")
            .env_remove("CARGO_MAKEFLAGS");
        cmd
    }

    /// The per-test ledger `scripts/test-delta.sh` wrote for this run, which
    /// lands in the fixture's own `.git` because the fixture is its own
    /// common dir.
    fn ledger(&self) -> String {
        let dir = self.path().join(".git/storyhook/test-results");
        let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("reading the ledger directory {}: {e}", dir.display()))
            .map(|entry| entry.expect("a ledger entry").path())
            .filter(|path| path.is_file())
            .collect();
        assert_eq!(entries.len(), 1, "exactly one ledger expected: {entries:?}");
        fs::read_to_string(entries.remove(0)).expect("reading the ledger")
    }
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The real tool, end to end: a red first binary must not stop the second.
#[test]
fn a_red_test_binary_does_not_stop_the_rest_of_its_battery() {
    let fixture = Fixture::new();

    let out = fixture
        .run_tests(&["--only-no-doc", "first", "second", "--", "--test-threads=1"])
        .output()
        .expect("running the battery");
    let output = combined(&out);

    assert!(
        !out.status.success(),
        "a battery with a red binary must still fail\n{output}"
    );
    assert!(
        output.contains("test first_fails ... FAILED"),
        "the red binary must have run and failed\n{output}"
    );
    assert!(
        output.contains("test second_passes ... ok"),
        "the binary after the red one must still run (cargo's default fail-fast across binaries)\n{output}"
    );
    let ledger = fixture.ledger();
    assert!(
        ledger.contains("first\tfirst_fails\tFAIL")
            && ledger.contains("second\tsecond_passes\tPASS"),
        "the ledger must record both binaries, not `not re-run` for the second\n{ledger}"
    );
}

/// Every executing `cargo test` the runner issues carries the flag, on every
/// path; the discovery calls that execute nothing do not.
#[test]
fn every_executing_cargo_test_invocation_asks_cargo_not_to_stop() {
    let fixture = Fixture::new();
    let record = fixture.path().join("cargo-calls.log");
    fixture.fake_cargo(&format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> "{record}"
case " $* " in
(*" metadata "*)
    printf '%s\n' '{{"packages":[{{"name":"storyhook-test-support","targets":[{{"kind":["lib"],"name":"storyhook_test_support"}}]}},{{"name":"auxiliary-checks","targets":[{{"kind":["test"],"name":"lint"}}]}}]}}'
    ;;
(*" --list "*)
    case " $* " in
    (*" --ignored "*) ;;
    (*) printf 'recorded: test\n' ;;
    esac
    ;;
(*)
    printf '     Running tests/first.rs (target/debug/deps/first-fixture)\n' >&2
    printf 'test first_fails ... FAILED\n'
    exit 101
    ;;
esac
"#,
        record = record.display()
    ));
    let journal = fixture.path().join("gate-progress.ndjson");

    // Both integration packages, libraries and doctests must run after a failure.
    let targeted = fixture
        .run_tests(&["--only", "first", "lint", "storyhook_test_support"])
        .output()
        .expect("running the targeted battery");
    assert!(!targeted.status.success(), "{}", combined(&targeted));
    // The whole-workspace path.
    let workspace = fixture
        .run_tests(&[])
        .output()
        .expect("running the workspace");
    assert!(!workspace.status.success(), "{}", combined(&workspace));
    // A caller-supplied journal, the verifier's shape; the lock wrapper
    // supplies one of its own otherwise, so discovery runs either way.
    let journalled = fixture
        .run_tests(&["--only-no-doc", "first"])
        .env("STORYHOOK_GATE_PROGRESS", &journal)
        .output()
        .expect("running the journalled battery");
    assert!(!journalled.status.success(), "{}", combined(&journalled));

    let calls = fs::read_to_string(&record).expect("reading the recorded cargo calls");
    let test_calls: Vec<&str> = calls
        .lines()
        .filter(|line| line.starts_with("test "))
        .collect();
    let executing: Vec<&str> = test_calls
        .iter()
        .copied()
        .filter(|line| !line.contains(" --list"))
        .collect();
    let discovering: Vec<&str> = test_calls
        .iter()
        .copied()
        .filter(|line| line.contains(" --list"))
        .collect();
    assert_eq!(
        executing.len(),
        6,
        "expected both integrations, lib, doctest, workspace and journalled executions\n{calls}"
    );
    assert!(
        discovering.len() >= 2,
        "expected at least the journalled run's default and --ignored discoveries\n{calls}"
    );
    for phase in [&executing, &discovering] {
        assert!(
            phase
                .iter()
                .any(|line| line.contains("-p auxiliary-checks --test lint")),
            "discovery and execution must select the integration test's owning package\n{calls}"
        );
    }
    for line in &executing {
        let cargo_options = line.split(" -- ").next().unwrap_or(line);
        assert!(
            cargo_options
                .split(' ')
                .any(|word| word == "--no-fail-fast"),
            "an executing cargo test must ask cargo to run every binary, before the `--` separator: {line}"
        );
    }
    for line in &discovering {
        assert!(
            !line.contains("--no-fail-fast"),
            "a discovery call executes nothing and must not change: {line}"
        );
    }
}

/// A test file whose one case passes only while the binary named `other`
/// runs at the same time: it announces itself in `$BARRIER_DIR`, then waits
/// for `other` to do the same.
fn meets(me: &str, other: &str) -> String {
    format!(
        r#"#[test]
fn {me}_meets_{other}() {{
    let dir = std::path::PathBuf::from(std::env::var("BARRIER_DIR").unwrap());
    std::fs::write(dir.join("{me}"), "").unwrap();
    let give_up = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !dir.join("{other}").exists() {{
        assert!(std::time::Instant::now() < give_up, "{me} never ran beside {other}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }}
}}
"#
    )
}

/// Each binary's `test … ok|FAILED` lines, keyed by the `Running` line they
/// follow, in the order the `Running` lines appeared.
fn blocks(output: &str) -> Vec<(String, Vec<String>)> {
    let mut blocks: Vec<(String, Vec<String>)> = Vec::new();
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("     Running tests/") {
            let name = rest.split(".rs").next().unwrap_or(rest).to_string();
            blocks.push((name, Vec::new()));
        } else if (line.starts_with("test ")
            && (line.ends_with(" ok") || line.ends_with(" FAILED")))
            && let Some((_, cases)) = blocks.last_mut()
        {
            cases.push(line.to_string());
        }
    }
    blocks
}

/// SH-783: with a thread budget the battery's binaries run at the same time,
/// and each binary's output still reaches the terminal and the ledger whole,
/// in the order the battery named them, whatever order they finished in.
#[test]
fn a_pooled_battery_runs_its_binaries_together_and_keeps_each_ones_output_whole() {
    let fixture = Fixture::new();
    fixture.write("tests/alpha.rs", &meets("alpha", "beta"));
    fixture.write("tests/beta.rs", &meets("beta", "alpha"));
    // Finishes last, so a report in completion order would move its block.
    fixture.write(
        "tests/first.rs",
        "#[test]\nfn first_fails() {\n    std::thread::sleep(std::time::Duration::from_secs(2));\n    \
         panic!(\"the first binary of the battery is red on purpose\");\n}\n",
    );
    let barrier = fixture.path().join("barrier");
    fs::create_dir(&barrier).expect("fixture: the barrier dir");

    let out = fixture
        .run_tests(&[
            "--only-no-doc",
            "alpha",
            "beta",
            "first",
            "second",
            "--",
            "--test-threads=1",
        ])
        .env("STORYHOOK_TEST_THREAD_BUDGET", "4")
        .env("BARRIER_DIR", &barrier)
        .output()
        .expect("running the battery");
    let output = combined(&out);

    assert_eq!(
        out.status.code(),
        Some(101),
        "a red binary makes the pooled battery fail as cargo would\n{output}"
    );
    assert_eq!(
        blocks(&String::from_utf8_lossy(&out.stdout)),
        [
            ("alpha".into(), vec!["test alpha_meets_beta ... ok".into()]),
            ("beta".into(), vec!["test beta_meets_alpha ... ok".into()]),
            ("first".into(), vec!["test first_fails ... FAILED".into()]),
            ("second".into(), vec!["test second_passes ... ok".into()]),
        ],
        "every binary ran, alpha and beta at the same time, and each one's \
         output is whole and in the battery's order\n{output}"
    );
    let ledger = fixture.ledger();
    for row in [
        "alpha\talpha_meets_beta\tPASS",
        "beta\tbeta_meets_alpha\tPASS",
        "first\tfirst_fails\tFAIL",
        "second\tsecond_passes\tPASS",
    ] {
        assert!(ledger.contains(row), "ledger lacks {row:?}\n{ledger}");
    }
}

/// Whether `pid` is still running rather than gone or a zombie.
fn pid_running(pid: &str) -> bool {
    let out = Command::new("ps")
        .args(["-o", "state=", "-p", pid])
        .output()
        .expect("running ps");
    let state = String::from_utf8_lossy(&out.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

/// SH-783: cancelling a pooled battery through its gate lock takes the test
/// binaries it is running with it, as the serial run's cancellation does
/// (`tests/gate_lock.rs`).
#[test]
fn terminating_a_pooled_battery_reaps_the_binaries_it_is_running() {
    let fixture = Fixture::new();
    let pids = fixture.path().join("pids");
    fs::create_dir(&pids).expect("fixture: the pid dir");
    for name in ["slow_a", "slow_b"] {
        fixture.write(
            &format!("tests/{name}.rs"),
            &format!(
                r#"#[test]
fn {name}_waits() {{
    let dir = std::path::PathBuf::from(std::env::var("PID_DIR").unwrap());
    std::fs::write(dir.join("{name}"), std::process::id().to_string()).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(300));
}}
"#
            ),
        );
    }
    let mut runner = storyhook_test_support::ChildGuard::spawn(
        fixture
            .run_tests(&[
                "--only-no-doc",
                "slow_a",
                "slow_b",
                "--",
                "--test-threads=1",
            ])
            .env("STORYHOOK_TEST_THREAD_BUDGET", "4")
            .env("PID_DIR", &pids)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .expect("spawning the battery");

    let give_up = std::time::Instant::now() + std::time::Duration::from_secs(240);
    let files = [pids.join("slow_a"), pids.join("slow_b")];
    while !files.iter().all(|file| file.exists()) {
        assert!(
            std::time::Instant::now() < give_up,
            "both pooled binaries never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let test_pids: Vec<String> = files
        .iter()
        .map(|file| fs::read_to_string(file).expect("a pid"))
        .collect();

    let status = Command::new("kill")
        .args(["-TERM", &runner.pid().to_string()])
        .status()
        .expect("signalling the battery");
    assert!(status.success());
    runner.wait_within(std::time::Duration::from_secs(120), || {
        "the pooled battery did not exit after SIGTERM".into()
    });

    let survivors: Vec<&String> = test_pids.iter().filter(|pid| pid_running(pid)).collect();
    assert!(
        survivors.is_empty(),
        "test binaries outlived their cancelled battery: {survivors:?}"
    );
}
