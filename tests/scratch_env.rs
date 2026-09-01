//! `scripts/scratch-env.sh` — the disposable storyhook a person can type.
//!
//! The property that matters is negative and easy to believe without checking:
//! that a `story` run inside a scratch environment reaches the scratch store
//! and **nothing else**. So it is checked positively — against a decoy "real"
//! store this test builds, plants a project in, and then fingerprints before
//! and after. Never against the developer's actual store: a test that proved
//! this by writing near real data would be the defect it is testing for.
//!
//! Mutation-checked, and worth recording because the two mutations fail
//! DIFFERENTLY. Dropping the isolation entirely is caught by
//! `storyhook::env::is_test_build` before this file ever compares anything —
//! the binary these tests pass with `--binary` is a test build, and a test
//! build refuses to guess a store. Pointing the isolation at the CALLER's own
//! root is not: nothing in the binary can tell that root from a correct one,
//! and only the fingerprint comparison below catches it. That second mutation
//! is why this file compares bytes rather than trusting the refusal.
//!
//! Every run here passes `--binary`, so nothing in this file compiles anything.
//! The build selection is exercised by `--print`, which reports the whole
//! environment without running a command.

use std::path::{Path, PathBuf};

use storyhook_test_support::{TestEnv, scratch_dir, story_binary};

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Runs `scripts/scratch-env.sh` with `args`, under `home` as `$HOME`.
///
/// `$HOME` is redirected on the command rather than in this process: the
/// decoy store hangs off it, and integration tests run as parallel threads in
/// one process where a `set_var` would race every sibling.
fn scratch_env(home: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(repo_root().join("scripts/scratch-env.sh"));
    cmd.args(args);
    cmd.current_dir(repo_root());
    cmd.env("HOME", home);
    // The decoy home's own default store, named the way a real machine's
    // ambient environment would name nothing at all — so if the script failed
    // to isolate, this is where the damage would land, and this test would see
    // it.
    cmd.env_remove("STORYHOOK_STORE_PATH");
    cmd.env_remove("STORYHOOK_DATA_DIR");
    cmd.env_remove("XDG_DATA_HOME");
    cmd.env_remove("XDG_STATE_HOME");
    cmd.output().expect("running scratch-env.sh")
}

fn combined(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A `$HOME` with a storyhook store in it that this test can compare against.
///
/// Built by running `story` against it directly, so the bytes are a real
/// store's rather than a file this test invented — a hand-made file would
/// compare equal to itself no matter what the script did to a real one.
fn decoy_home(env: &TestEnv, label: &str) -> (tempfile::TempDir, PathBuf) {
    let fixture = scratch_dir();
    let home = fixture.path().join(label);
    let data_dir = home.join(".local/share/storyhook");
    std::fs::create_dir_all(&data_dir).expect("creating the decoy data directory");
    let project = fixture.path().join(format!("{label}-project"));
    std::fs::create_dir_all(&project).expect("creating the decoy project directory");

    let mut cmd = std::process::Command::new(story_binary());
    cmd.current_dir(&project);
    env.apply(&mut cmd);
    // Everything from the harness except where the store lives: this is the
    // store the test wants written, and it must be a real one.
    cmd.env("HOME", &home);
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_STATE_HOME", home.join(".local/state"));
    cmd.env("STORYHOOK_DATA_DIR", &data_dir);
    cmd.env("STORYHOOK_STORE_PATH", data_dir.join("store.db"));
    cmd.args(["project", "new", "--prefix", "DEC"]);
    let out = cmd.output().expect("seeding the decoy store");
    assert!(
        out.status.success(),
        "the decoy store must be real: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let store = data_dir.join("store.db");
    assert!(store.is_file(), "the decoy store was not created");
    (fixture, home)
}

/// A fingerprint of every file that is part of a store.
///
/// All three, because SQLite in write-ahead-logging mode is a database plus two
/// sidecars, and a write that landed only in the log would leave `store.db`
/// itself unchanged.
///
/// A length and a hash rather than the bytes: this comparison is what fails
/// when the script isolates to the wrong root, and a failing `assert_eq!` on a
/// 22-megabyte store prints 22 megabytes of decimal integers, which is a
/// message nobody can read on the way to a diagnosis.
fn store_fingerprint(home: &Path) -> Vec<(String, usize, u64)> {
    let dir = home.join(".local/share/storyhook");
    let mut out = Vec::new();
    for name in ["store.db", "store.db-wal", "store.db-shm"] {
        let bytes = std::fs::read(dir.join(name)).unwrap_or_default();
        // FNV-1a, inline: this needs to detect a change, not resist one, and
        // a dependency for that would be a dependency to justify.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in &bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        out.push((name.to_string(), bytes.len(), hash));
    }
    out
}

/// A `story` run inside a scratch environment writes the scratch store, and
/// leaves the ambient one byte-identical.
///
/// The whole point of the script, asserted from both ends: the story it creates
/// is readable back through the scratch environment, and is nowhere near the
/// store the same `$HOME` would otherwise have resolved.
#[test]
fn a_run_inside_a_scratch_environment_leaves_the_ambient_store_untouched() {
    let env = TestEnv::isolated();
    let (_fixture, home) = decoy_home(&env, "ambient");
    let before = store_fingerprint(&home);

    let name = format!("test-{}", std::process::id());
    let work = scratch_dir();
    let script = work.path().join("inside.sh");
    std::fs::write(
        &script,
        format!(
            "set -eu\ncd '{}'\nstory project new --prefix SCR\nstory new 'inside the scratch env'\nstory list\n",
            work.path().display()
        ),
    )
    .expect("writing the inner script");

    let out = scratch_env(
        &home,
        &[
            "--name",
            &name,
            "--fresh",
            "--binary",
            story_binary().to_str().expect("a UTF-8 binary path"),
            "--",
            "bash",
            script.to_str().expect("a UTF-8 script path"),
        ],
    );
    let text = combined(&out);
    assert!(out.status.success(), "the scratch run failed:\n{text}");
    assert!(
        text.contains("SCR-1") && text.contains("inside the scratch env"),
        "the scratch environment must be usable, not merely safe:\n{text}"
    );

    assert_eq!(
        store_fingerprint(&home),
        before,
        "a scratch run changed the ambient store. All three files — the database \
         and both write-ahead sidecars — must be exactly as they were. This is \
         the assertion that catches a scratch environment rooted at the caller's \
         own home, which no guard inside the binary can distinguish from a \
         correct one."
    );

    // …and the project really did go somewhere else.
    let scratch_store = Path::new("/private/tmp/storyhook-scratch")
        .join(format!("{name}/home/.local/share/storyhook/store.db"));
    assert!(
        scratch_store.is_file(),
        "the scratch store was never created at {}",
        scratch_store.display()
    );
    let _ = std::fs::remove_dir_all(Path::new("/private/tmp/storyhook-scratch").join(&name));
}

/// `--print` reports exactly what the shared isolation would apply, plus the
/// binary's directory on `$PATH`.
///
/// The environment a caller `eval`s must be the environment the script would
/// have entered itself, or `eval "$(scratch-env.sh --print)"` is a fourth
/// rendering of the parameter set.
#[test]
fn print_reports_the_shared_isolation_and_the_binary() {
    let env = TestEnv::isolated();
    let (_fixture, home) = decoy_home(&env, "printed");
    let name = format!("print-{}", std::process::id());

    let out = scratch_env(
        &home,
        &[
            "--name",
            &name,
            "--binary",
            story_binary().to_str().expect("a UTF-8 binary path"),
            "--print",
        ],
    );
    assert!(out.status.success(), "{}", combined(&out));
    let printed = String::from_utf8_lossy(&out.stdout).into_owned();

    let root = PathBuf::from("/private/tmp/storyhook-scratch").join(&name);
    for setting in storyhook::env::test_environment::resolve(
        &root,
        // The pid is the script's own and unknowable from here; every other
        // parameter is compared exactly, and the pid's presence is checked
        // below.
        0,
        storyhook::env::test_environment::Scope::Anywhere,
    ) {
        match setting.value {
            Some(value) if setting.name != "STORYHOOK_PARENT_PID" => {
                let want = format!(
                    "export {}='{}'",
                    setting.name,
                    value.to_str().expect("a UTF-8 path")
                );
                assert!(
                    printed.lines().any(|line| line == want),
                    "--print did not emit {want:?}; it emitted:\n{printed}"
                );
            }
            Some(_) => assert!(
                printed
                    .lines()
                    .any(|line| line.starts_with("export STORYHOOK_PARENT_PID=")),
                "--print must name a parent pid, or a daemon it starts outlives \
                 the caller:\n{printed}"
            ),
            None => assert!(
                printed
                    .lines()
                    .any(|line| line == format!("unset {}", setting.name)),
                "--print did not remove {}; it emitted:\n{printed}",
                setting.name
            ),
        }
    }

    let binary_dir = story_binary()
        .parent()
        .expect("the binary has a parent")
        .display()
        .to_string();
    assert!(
        printed.contains(&binary_dir),
        "--print must put the binary under test on PATH, or the caller runs \
         whatever their own PATH reaches:\n{printed}"
    );

    // `--print` says what it would do and does nothing: no environment, no
    // store, no daemon.
    assert!(
        !root.join("home/.local/share/storyhook/store.db").exists(),
        "--print created a store"
    );
}

/// `--print` without `--isolate-home` leaves `$HOME` alone, and with it does not.
///
/// The asymmetry the whole parameter set turns on, checked where a person meets
/// it. Keeping the real `$HOME` is the default because the store, the daemon
/// and every runtime path are keyed off the other parameters, and losing it
/// costs the caller their shell's rc file, their git identity and their ssh
/// keys for no isolation gained.
#[test]
fn home_is_redirected_only_when_asked_for() {
    let env = TestEnv::isolated();
    let (_fixture, home) = decoy_home(&env, "home-flag");
    let name = format!("home-{}", std::process::id());
    let binary = story_binary().to_str().expect("a UTF-8 binary path");

    let default = scratch_env(&home, &["--name", &name, "--binary", binary, "--print"]);
    assert!(default.status.success(), "{}", combined(&default));
    assert!(
        !String::from_utf8_lossy(&default.stdout).contains("export HOME="),
        "the default must leave $HOME alone"
    );

    let hermetic = scratch_env(
        &home,
        &[
            "--name",
            &name,
            "--binary",
            binary,
            "--isolate-home",
            "--print",
        ],
    );
    assert!(hermetic.status.success(), "{}", combined(&hermetic));
    assert!(
        String::from_utf8_lossy(&hermetic.stdout).contains(&format!(
            "export HOME='/private/tmp/storyhook-scratch/{name}/home'"
        )),
        "--isolate-home must redirect $HOME:\n{}",
        String::from_utf8_lossy(&hermetic.stdout)
    );
}

/// A name that would climb out of the scratch base is refused.
///
/// The name becomes a directory, and `--fresh` deletes that directory. A `..`
/// that reached anywhere else would make this script the most dangerous file in
/// the repository.
#[test]
fn a_name_that_escapes_the_scratch_base_is_refused() {
    let env = TestEnv::isolated();
    let (_fixture, home) = decoy_home(&env, "escape");
    let binary = story_binary().to_str().expect("a UTF-8 binary path");

    for name in ["../elsewhere", "..", ".", "", "a/b"] {
        let out = scratch_env(&home, &["--name", name, "--binary", binary, "--print"]);
        assert!(
            !out.status.success(),
            "[{name}] was accepted as an environment name; --fresh would then \
             delete whatever it resolves to"
        );
        assert!(
            combined(&out).contains("not a usable environment name"),
            "[{name}] must be refused by name, not by accident:\n{}",
            combined(&out)
        );
    }
}

/// An argument that lands nowhere is refused, not dropped.
#[test]
fn an_unknown_argument_is_refused() {
    let env = TestEnv::isolated();
    let (_fixture, home) = decoy_home(&env, "unknown");
    let out = scratch_env(&home, &["--tset-build"]);
    assert!(!out.status.success());
    assert!(
        combined(&out).contains("--tset-build"),
        "the refusal must name the offending word:\n{}",
        combined(&out)
    );
}

/// A binary that is not there is refused rather than silently skipped.
#[test]
fn a_binary_that_does_not_exist_is_refused() {
    let env = TestEnv::isolated();
    let (fixture, home) = decoy_home(&env, "missing-binary");
    let absent = fixture.path().join("no-such-story");
    let out = scratch_env(
        &home,
        &[
            "--binary",
            absent.to_str().expect("a UTF-8 path"),
            "--print",
        ],
    );
    assert!(!out.status.success());
    assert!(
        combined(&out).contains("is not an executable file"),
        "{}",
        combined(&out)
    );
}

/// `make scratch` and `make scratch-clean` run the script this file tests.
///
/// A wiring fence: it proves the Makefile's door leads here, never that the
/// script behaves — which every other test in this file is for. `make -n` so
/// nothing is built, no environment is created and no directory is removed.
#[test]
fn the_makefile_targets_reach_this_script() {
    for (target, expected) in [
        ("scratch", "scripts/scratch-env.sh"),
        ("scratch-clean", "/private/tmp/storyhook-scratch"),
    ] {
        let out = std::process::Command::new("make")
            .args(["-n", target])
            .current_dir(repo_root())
            .output()
            .unwrap_or_else(|e| panic!("running make -n {target}: {e}"));
        let text = combined(&out);
        assert!(out.status.success(), "make -n {target} failed:\n{text}");
        assert!(
            text.contains(expected),
            "`make {target}` does not mention {expected}:\n{text}"
        );
    }
}
