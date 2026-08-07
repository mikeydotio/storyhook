//! `--strategy`/`--mode` and the setup-plan round trip — SH-153's D2.
//!
//! # What is testable through the real `story` subprocess, and what is not
//!
//! `run_initial_setup` still calls `validate_token` and `list_issues` before
//! it can decide plan-vs-proceed, and nothing wires SH-158's `GithubApi` seam
//! into the real `story` binary this file's fixtures run — a fake exercises
//! the engine, not GitHub, so it stays in-process. The
//! `Response::SetupRequired` path itself, end to end, is driven that way now:
//! `tests/github_sync_engine.rs`, calling `run_initial_setup`/`run_sync_with`
//! directly against `FakeGithubApiFactory`. What stays here, reachable
//! offline through the subprocess because both checks fire before any
//! network call:
//!
//! * `--strategy` and `--mode` answer one question and must arrive together —
//!   checked before `sync.load_config()` even runs.
//! * Either flag on an already-configured project is refused — checked right
//!   after `load_config()` returns `Some`, still before any credential or
//!   network use.
//!
//! Both fixtures use [`OFFLINE`] anyway, on the same principle
//! `tests/github_sync_token.rs` states: a refusal that fires before the
//! transport is touched must not depend on there being no network to prove it.

#![cfg(feature = "github-sync")]

use storyhook_test_support::{Project, TestEnv};

/// A proxy that refuses instantly, and a loopback exemption so the client can
/// still reach its own daemon. See `tests/github_sync_token.rs`'s module docs
/// for why both entries are needed.
const OFFLINE: [(&str, &str); 2] = [
    ("ALL_PROXY", "http://127.0.0.1:1"),
    ("NO_PROXY", "127.0.0.1,localhost"),
];

/// Gives `project` a github-sync configuration without going near the wizard —
/// mirrors `tests/github_sync_token.rs`'s helper of the same shape.
fn configure_github_sync(project: &Project<'_>) {
    use storyhook::store::{ReadOps as _, Store as _, WriteOps as _};

    let store = project.open_store();
    let id = project.project_id(&store);
    store
        .write(|tx| {
            let mut settings = tx.settings(id)?;
            settings.github_sync = Some(serde_json::json!({
                "github": { "owner": "acme", "repo": "widgets" },
                "sync": { "mode": "manual" },
            }));
            tx.put_settings(id, &settings)
        })
        .expect("seeding the github-sync configuration");
}

/// `--strategy` alone, with no `--mode`, is refused — and refused before any
/// network call, which the [`OFFLINE`] proxy is what proves: a hang here would
/// mean the refusal did not fire where the doc comment says it does.
#[test]
fn strategy_without_mode_is_refused() {
    let env = TestEnv::isolated();
    let project = env.project().build();

    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    let output = cmd
        .args(["github-sync", "--strategy", "future-only"])
        .output()
        .expect("running the sync");

    assert_eq!(output.status.code(), Some(2), "Usage is exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--strategy"), "{stderr}");
    assert!(stderr.contains("--mode"), "{stderr}");
    assert!(
        stderr.contains("together"),
        "the refusal must say the two flags are a pair: {stderr}"
    );
}

/// The mirror image: `--mode` alone, with no `--strategy`.
#[test]
fn mode_without_strategy_is_refused() {
    let env = TestEnv::isolated();
    let project = env.project().build();

    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    let output = cmd
        .args(["github-sync", "--mode", "manual"])
        .output()
        .expect("running the sync");

    assert_eq!(output.status.code(), Some(2), "Usage is exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--strategy"), "{stderr}");
    assert!(stderr.contains("--mode"), "{stderr}");
}

/// Both flags together on a project that is **already configured** is
/// refused: the flags answer a question only a first-time setup asks.
#[test]
fn both_flags_on_an_already_configured_project_are_refused() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    configure_github_sync(&project);

    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    let output = cmd
        .args([
            "github-sync",
            "--strategy",
            "future-only",
            "--mode",
            "manual",
        ])
        .output()
        .expect("running the sync");

    assert_eq!(output.status.code(), Some(2), "Usage is exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already configured"),
        "the refusal must say why the flags do not apply here: {stderr}"
    );
}

/// `--mode auto` is refused by name, not silently accepted — SH-68 tracks
/// giving it an implementation, and until then offering it is the defect this
/// whole file exists to avoid repeating.
#[test]
fn mode_auto_is_refused_by_name() {
    let env = TestEnv::isolated();
    let project = env.project().build();

    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    let output = cmd
        .args(["github-sync", "--strategy", "future-only", "--mode", "auto"])
        .output()
        .expect("running the sync");

    assert_eq!(output.status.code(), Some(2), "Usage is exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("SH-68"), "{stderr}");
}

/// **`--dry-run` must never write configuration to an unconfigured project.**
///
/// `run_initial_setup(sync, token, Some(answers))` used to call
/// `sync.save_config` itself, unconditionally — so
/// `story github-sync --dry-run --strategy … --mode …` on a project that had
/// never run github-sync before wrote configuration despite `--dry-run`
/// (SH-153).
///
/// This cannot be reproduced through a real `story` subprocess: reaching the
/// write requires `validate_token` and `list_issues` to succeed first, and
/// nothing wires SH-158's `GithubApi` seam into the binary this file drives
/// — the same wall the module docs above name. What is checked here instead
/// is the structural fact that makes the bug impossible: `src/github/initial.rs`
/// does not call `save_config` at all any more. The write moved to
/// `run_sync_with`, gated on `!dry_run` alongside every other write that
/// function makes. The behavior itself — a dry run against a fake GitHub,
/// writing nothing — is now also pinned directly, in-process:
/// `tests/github_sync_engine.rs::dry_run_writes_no_configuration_on_first_setup`.
#[test]
fn initial_setup_never_calls_save_config_itself() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/github/initial.rs"
    ))
    .expect("reading src/github/initial.rs");
    for (number, line) in source.lines().enumerate() {
        let code = line.trim_start();
        if code.starts_with("//") {
            continue;
        }
        assert!(
            !code.contains("save_config"),
            "src/github/initial.rs:{}: {} -- the write belongs in run_sync_with, gated on \
             !dry_run, not here",
            number + 1,
            code.trim()
        );
    }
}
