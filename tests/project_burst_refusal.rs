//! Volume, not path, catches the caller who never opts into isolation at all
//! (SH-122).
//!
//! `temp_project_refusal.rs` (SH-95) refuses a creation whose *target* is a
//! temporary path in a store that is not. That guard has two blind spots,
//! both live today: `--no-attach` creates nothing at a path for it to judge,
//! and a fixture directory that is not itself under a temp root — a CI
//! workspace, say — is not temporary by that guard's rule even though it is
//! exactly as throwaway. The suite that caused SH-95's incident was rewritten
//! from `story init` (a directory) to `story project new --prefix X` (a
//! switch, at all 164 of its fixture sites) — the exact rename the epic's
//! acceptance of this gap assumed would not happen.
//!
//! This guard asks a different question: how many, how fast. A person or a
//! script that means to create a project creates one; a test suite creates
//! one per test case, and did so at roughly 15 a minute on 2026-07-30.
//! Counting closes both blind spots without asking who is calling.

use std::path::{Path, PathBuf};

use storyhook_test_support::{
    TestEnv, daemon_containment, non_temporary_dir, scratch_dir, story_binary,
};

/// A data home that is **not** under any temporary directory.
///
/// Same helper as `temp_project_refusal.rs`'s: [`non_temporary_dir`] resolves
/// this independently of the checkout (SH-258), rather than inferring it from
/// `CARGO_TARGET_TMPDIR`, which is silently false whenever the checkout itself
/// is temp-rooted — exactly the case this guard exists for.
fn non_temporary_data_home(label: &str) -> PathBuf {
    non_temporary_dir(label)
}

/// Runs `story project new --prefix SH --name <name> --no-attach` against
/// `data_home`, with nothing inherited.
///
/// `--no-attach` throughout: it is one of the two blind spots this guard
/// exists for, and it also sidesteps needing a fresh, real target directory
/// per call — the burst gate counts store rows, not paths.
fn new_detached_project(name: &str, data_home: &Path, allow_burst: bool) -> std::process::Output {
    let cwd = scratch_dir();
    let mut cmd = std::process::Command::new(story_binary());
    cmd.current_dir(cwd.path())
        .env_clear()
        .env("HOME", cwd.path())
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("STORYHOOK_DATA_DIR", data_home)
        .args([
            "project",
            "new",
            "--prefix",
            "SH",
            "--name",
            name,
            "--no-attach",
        ]);
    for (var, value) in daemon_containment() {
        cmd.env(var, value);
    }
    if allow_burst {
        cmd.env("STORYHOOK_ALLOW_PROJECT_BURST", "1");
    }
    cmd.output().expect("running the binary under test")
}

/// Same as [`new_detached_project`], but `--attach <target>` a real,
/// non-temporary directory instead of `--no-attach` — the other blind spot:
/// a target that exists and is not temporary walks straight through the
/// SH-95 guard regardless of volume.
fn new_attached_project(
    name: &str,
    target: &Path,
    data_home: &Path,
    allow_burst: bool,
) -> std::process::Output {
    let cwd = scratch_dir();
    let mut cmd = std::process::Command::new(story_binary());
    cmd.current_dir(cwd.path())
        .env_clear()
        .env("HOME", cwd.path())
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("STORYHOOK_DATA_DIR", data_home)
        .args([
            "project",
            "new",
            "--prefix",
            "SH",
            "--name",
            name,
            "--attach",
            target.to_str().unwrap(),
        ]);
    for (var, value) in daemon_containment() {
        cmd.env(var, value);
    }
    if allow_burst {
        cmd.env("STORYHOOK_ALLOW_PROJECT_BURST", "1");
    }
    cmd.output().expect("running the binary under test")
}

/// The case `--no-attach` walks straight past SH-95: no path is ever judged,
/// so the only thing that can catch a burst of these is volume.
#[test]
fn a_burst_of_detached_projects_is_refused_at_the_fifth() {
    let data_home = non_temporary_data_home("sh122-detached-burst");

    for i in 0..4 {
        let out = new_detached_project(&format!("burst-{i}"), &data_home, false);
        assert_eq!(
            out.status.code(),
            Some(0),
            "project {i} of 4 must succeed; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let out = new_detached_project("burst-4", &data_home, false);
    assert_eq!(
        out.status.code(),
        Some(2),
        "the fifth project inside the window must be refused; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--store-path") || stderr.contains("STORYHOOK_STORE_PATH"),
        "the message must lead with a lever that actually wins the precedence contest: {stderr}"
    );
    assert!(
        stderr.contains("STORYHOOK_ALLOW_PROJECT_BURST"),
        "the message must name the override, so the refusal is never a dead end: {stderr}"
    );
}

/// The other SH-95 blind spot: a target that is real and not temporary is
/// not temporary by that guard's own rule, even run five times in ten
/// seconds against the same store.
#[test]
fn a_burst_of_attached_real_targets_is_refused_at_the_fifth() {
    let data_home = non_temporary_data_home("sh122-attached-burst");

    for i in 0..4 {
        let target = non_temporary_data_home(&format!("sh122-attached-burst-target-{i}"));
        let out = new_attached_project(&format!("burst-{i}"), &target, &data_home, false);
        assert_eq!(
            out.status.code(),
            Some(0),
            "project {i} of 4 must succeed; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let target = non_temporary_data_home("sh122-attached-burst-target-4");
    let out = new_attached_project("burst-4", &target, &data_home, false);
    assert_eq!(
        out.status.code(),
        Some(2),
        "the fifth project inside the window must be refused even though every target is a \
         real, non-temporary directory; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The positive control. If this ever fails, the gate has broken every
/// correctly isolated suite on the machine — including this project's own,
/// whose fixtures build a store under `/private/tmp` in exactly this shape.
#[test]
fn a_burst_of_detached_projects_in_a_throwaway_store_is_allowed() {
    let data_home = scratch_dir();

    for i in 0..6 {
        let out = new_detached_project(&format!("burst-{i}"), data_home.path(), false);
        assert_eq!(
            out.status.code(),
            Some(0),
            "a throwaway store must never trip the burst gate; project {i}; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The refusal is never a wall with no way past.
#[test]
fn the_override_permits_a_burst_in_a_real_store() {
    let data_home = non_temporary_data_home("sh122-override");

    for i in 0..6 {
        let out = new_detached_project(&format!("burst-{i}"), &data_home, true);
        assert_eq!(
            out.status.code(),
            Some(0),
            "an explicit override must be honoured; project {i}; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// `ImportProject` is a bulk verb a person or a script chose deliberately by
/// typing that command, so it stays under the path-based SH-95 guard alone
/// and is exempt from this one — the burst gate exists for the caller who
/// never chose to create anything explicitly enough to name a path or a
/// batch. Four detached projects already sit in the window when this import
/// runs; if the burst gate applied to it too, this would be refused.
#[test]
fn a_bulk_import_is_not_limited_by_the_burst_gate() {
    let export_json = {
        let env = TestEnv::shared();
        let dir = env.project().build();
        env.story(dir.path())
            .args(["new", "a story to export"])
            .assert()
            .success();
        let output = env.story(dir.path()).args(["export"]).assert().success();
        String::from_utf8(output.get_output().stdout.clone()).unwrap()
    };

    let data_home = non_temporary_data_home("sh122-import-exempt");
    for i in 0..4 {
        let out = new_detached_project(&format!("burst-{i}"), &data_home, false);
        assert_eq!(
            out.status.code(),
            Some(0),
            "project {i} of 4 must succeed; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let import_cwd = non_temporary_data_home("sh122-import-exempt-cwd");
    let export_file = import_cwd.join("export.json");
    std::fs::write(&export_file, &export_json).expect("writing the export file");

    let mut cmd = std::process::Command::new(story_binary());
    cmd.current_dir(&import_cwd)
        .env_clear()
        .env("HOME", &import_cwd)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("STORYHOOK_DATA_DIR", &data_home)
        .args(["import-project", export_file.to_str().unwrap()]);
    for (var, value) in daemon_containment() {
        cmd.env(var, value);
    }
    let out = cmd.output().expect("running the binary under test");

    assert_eq!(
        out.status.code(),
        Some(0),
        "a bulk import must not be refused by the burst gate; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
