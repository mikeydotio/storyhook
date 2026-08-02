//! The store a test run is allowed to touch.
//!
//! `make test` has always exported an isolated `STORYHOOK_DATA_DIR` and refused
//! to run without one, but a bare `cargo test` routes around the Makefile
//! entirely — and roughly forty-five integration-test files build a fixture
//! directory and run `story` with this process's environment inherited. Before
//! the flip that wrote a `.storyhook/` tree into a temporary directory and cost
//! nothing. After it, the same fixture writes into
//! `~/.local/share/storyhook/store.db`: the developer's real tracker, with their
//! real projects in it.
//!
//! It is not hypothetical. W7 found a project named `.tmpKGBY3a` in this
//! machine's real store, created by exactly that shape.
//!
//! The refusal therefore has to live somewhere `cargo test` cannot skip, which
//! means inside the binary rather than around it — see
//! [`storyhook::env::is_test_build`].

use std::path::Path;

use storyhook_test_support::{TestEnv, daemon_containment, scratch_dir, story_binary};

/// Runs the binary under test with a deliberately incomplete environment.
///
/// `env_clear` is what makes this a real test of the guard rather than of this
/// file's bookkeeping: the process inherits nothing, so `STORYHOOK_DATA_DIR`
/// cannot arrive from the ambient shell, from `make test`, or from a variable a
/// future harness adds. `HOME` and `PATH` are put back because a process with
/// no home has a different complaint.
fn story_without(home: &Path, cwd: &Path, keep_data_dir: Option<&Path>) -> std::process::Output {
    let mut cmd = std::process::Command::new(story_binary());
    cmd.current_dir(cwd)
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .arg("list");
    // Put back only what stops a daemon this command starts from reaching
    // anything real. The cleared environment is the subject of the test; a
    // daemon that inherited it would prefer the developer's dashboard port and
    // outlive the run.
    for (name, value) in daemon_containment() {
        cmd.env(name, value);
    }
    if let Some(dir) = keep_data_dir {
        cmd.env("STORYHOOK_DATA_DIR", dir);
    }
    cmd.output().expect("running the binary under test")
}

/// The guard, stated as the thing it prevents: a test build that was not told
/// where to put its data must not pick a directory for itself.
///
/// Asserted against a scratch `HOME` rather than the developer's, because a red
/// run of this test is precisely a run that creates a store wherever `HOME`
/// points.
#[test]
fn a_test_build_refuses_to_resolve_a_data_home_it_was_not_given() {
    let home = scratch_dir();
    let cwd = scratch_dir();

    let out = story_without(home.path(), cwd.path(), None);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "a test build with no STORYHOOK_DATA_DIR must refuse; it exited {:?} saying {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !home.path().join(".local/share/storyhook/store.db").exists(),
        "the refusal must happen before anything is created — a store at the default \
         location is exactly the accident this guard exists to prevent"
    );
    assert!(
        stderr.contains("STORYHOOK_DATA_DIR"),
        "the message must name the variable to set: {stderr}"
    );
    assert!(
        stderr.contains("make test"),
        "the message must name the supported way to run the suite: {stderr}"
    );
}

/// The other half: the guard must be a refusal to *guess*, not a refusal to run.
/// A named data directory is honoured, and the same command succeeds.
#[test]
fn a_test_build_runs_normally_once_it_is_told_where_the_store_goes() {
    let home = scratch_dir();
    let cwd = scratch_dir();
    let data = home.path().join("elsewhere/storyhook");

    let out = story_without(home.path(), cwd.path(), Some(&data));

    // `story list` outside a project is an ordinary "not initialized" failure,
    // not a refusal to resolve the environment: what matters is that the store
    // was created where it was told to go.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("refusing"),
        "an explicit data directory must be accepted: {stderr}"
    );
    assert!(
        data.join("store.db").exists(),
        "the store must have been created at the directory it was given"
    );
}

/// The harness must satisfy the guard by construction. If a future change to
/// [`TestEnv`] stopped exporting the variable, every fixture in the suite would
/// start failing with the refusal above — this is the test that says why.
#[test]
fn the_harness_always_names_a_data_directory() {
    let env = TestEnv::isolated();
    assert!(
        env.vars()
            .iter()
            .any(|(name, _)| *name == "STORYHOOK_DATA_DIR"),
        "TestEnv must export STORYHOOK_DATA_DIR — the guard refuses a test build without it"
    );

    // Asked of a real child process rather than of the array, because the array
    // is what the harness believes and the child is what the guard sees.
    let project = env.project().build();
    project.run(&["list"]).success();
}

/// The sentinel is the `fault-injection` feature, and this pins the link.
///
/// `cargo build` and `cargo build --release` do not enable it; `cargo test`
/// does, by way of the test-support crate's dependency on `storyhook`. If that
/// ever stops being true the guard silently stops guarding, so the property is
/// asserted from inside a test binary — the one place that can tell.
#[test]
fn this_is_a_test_build() {
    assert!(
        storyhook::env::is_test_build(),
        "a test binary must resolve as a test build, or the data-home guard is inert"
    );
}
