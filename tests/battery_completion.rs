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

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

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
    printf '%s\n' '{{"packages":[{{"name":"storyhook-test-support","targets":[{{"kind":["lib"],"name":"storyhook_test_support"}}]}}]}}'
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

    // The integration, lib and doctest paths of `--only`.
    let targeted = fixture
        .run_tests(&["--only", "first", "storyhook_test_support"])
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
        5,
        "expected the integration, lib, doctest, workspace and journalled executions\n{calls}"
    );
    assert!(
        discovering.len() >= 2,
        "expected at least the journalled run's default and --ignored discoveries\n{calls}"
    );
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
