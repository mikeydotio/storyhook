//! `--strategy`/`--mode` and the setup-plan round trip — SH-153's D2.
//!
//! `--mode` is also, since SH-68/SH-201, the repair path for a project stuck
//! on a mode this build refuses to run under: a stored `auto` (never
//! implemented, SH-68) or a chosen `off` (otherwise permanent, SH-201). Both
//! are the same defect — a refusal whose only advice named a command that
//! could not run — fixed the same way: `--mode` alone, on an already-
//! configured project, changes the stored mode instead of being refused.
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
//! offline through the subprocess because every check below fires before any
//! network call:
//!
//! * `--strategy` without `--mode` is refused, on any project — checked
//!   before `sync.load_config()` even runs.
//! * `--strategy` at all, on an already-configured project, is refused —
//!   checked right after `load_config()` returns `Some`.
//! * `--mode` without `--strategy`, on an *unconfigured* project, is refused
//!   — checked right after `load_config()` returns `None`.
//! * `--mode` without `--strategy`, on a *configured* project, changes the
//!   stored mode and returns — reachable with **no token and no network**,
//!   since it never builds a `GithubApi` client at all.
//!
//! Every fixture uses [`OFFLINE`] anyway, on the same principle
//! `tests/github_sync_token.rs` states: a refusal — or, here, a repair — that
//! fires before the transport is touched must not depend on there being no
//! network to prove it.

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
    configure_github_sync_with_mode(project, "manual");
}

/// The same seeding, with the stored mode named explicitly — what
/// [`stored_mode`] reads back. Used to put a project into `auto` or `off`
/// without going through the CLI grammar that refuses one of them as input
/// and the wizard that refuses both.
fn configure_github_sync_with_mode(project: &Project<'_>, mode: &str) {
    use storyhook::store::{ReadOps as _, Store as _, WriteOps as _};

    let store = project.open_store();
    let id = project.project_id(&store);
    store
        .write(|tx| {
            let mut settings = tx.settings(id)?;
            settings.github_sync = Some(serde_json::json!({
                "github": { "owner": "acme", "repo": "widgets" },
                "sync": { "mode": mode },
            }));
            tx.put_settings(id, &settings)
        })
        .expect("seeding the github-sync configuration");
}

/// Reads the stored sync mode directly from the store — never through a
/// `story` subprocess, which would need a daemon stood down first (a live
/// daemon answers from its own page cache, not the file just replaced).
fn stored_mode(env: &TestEnv, project: &Project<'_>) -> String {
    use storyhook::store::{ReadOps as _, Store as _};

    env.stop_daemon();
    let store = project.open_store();
    let id = project.project_id(&store);
    let settings = store.read(|tx| tx.settings(id)).expect("reading settings");
    settings
        .github_sync
        .expect("a github-sync document")
        .get("sync")
        .and_then(|s| s.get("mode"))
        .and_then(|m| m.as_str())
        .expect("a sync.mode field")
        .to_string()
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
        stderr.contains("alongside"),
        "the refusal must say --strategy needs --mode with it: {stderr}"
    );
}

/// The mirror image on an **unconfigured** project: `--mode` alone answers
/// only half the first-setup question, so it is refused too — but the
/// refusal must say the project has never synced, not that the flags are a
/// pair, because on a *configured* project `--mode` alone is legal (it
/// changes the mode; see `an_off_project_is_freed_by_mode_manual` and
/// neighbours below).
#[test]
fn mode_without_strategy_is_refused_on_an_unconfigured_project() {
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
    assert!(
        stderr.contains("never synced"),
        "the refusal must say why this differs from the configured case: {stderr}"
    );
}

/// `--strategy` on a project that is **already configured** is refused —
/// with or without `--mode` alongside it — because the flag answers a
/// question only a first-time setup asks. The refusal must point at the
/// working alternative, `--mode` alone, or a user stuck on `auto`/`off` has
/// nowhere to go from this message.
#[test]
fn strategy_on_an_already_configured_project_is_refused() {
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
        "the refusal must say why --strategy does not apply here: {stderr}"
    );
    assert!(
        stderr.contains("--mode manual|off"),
        "and must point at the flag that does work: {stderr}"
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

/// A project whose *stored* document still says `auto` — impossible to reach
/// through today's grammar, but reachable by a project migrated from before
/// the rearchitecture — is refused, not silently treated as manual. SH-68:
/// the refusal is what replaces the old `eprintln!` notice, which fired
/// inside the daemon and was never seen by anyone.
///
/// The message must name both working repairs, or a user reading only the
/// error has no way out.
#[test]
fn a_stored_auto_mode_is_refused_and_names_both_repairs() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    configure_github_sync_with_mode(&project, "auto");

    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    let output = cmd
        .args(["github-sync"])
        .output()
        .expect("running the sync");

    assert_eq!(output.status.code(), Some(2), "Usage is exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("auto"), "{stderr}");
    assert!(stderr.contains("--mode manual"), "{stderr}");
    assert!(stderr.contains("--mode off"), "{stderr}");
}

/// The repair itself: `--mode manual` on a project stuck on `auto` writes the
/// new mode and says so — checked by reading the stored document directly,
/// never through another `story` invocation, so the assertion cannot be
/// satisfied by a response message that lied.
#[test]
fn a_stored_auto_mode_is_repaired_by_mode_manual() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    configure_github_sync_with_mode(&project, "auto");

    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    let output = cmd
        .args(["github-sync", "--mode", "manual"])
        .output()
        .expect("running the sync");

    assert!(
        output.status.success(),
        "the repair itself must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("auto"), "{stdout}");
    assert!(stdout.contains("manual"), "{stdout}");

    assert_eq!(stored_mode(&env, &project), "manual");
}

/// SH-201's regression test: a project configured `off` — otherwise
/// permanent, since `story github-sync` refuses to run under it and
/// `story project settings` holds the document read-only — is freed by the
/// same `--mode manual` repair.
#[test]
fn an_off_project_is_freed_by_mode_manual() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    configure_github_sync_with_mode(&project, "off");

    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    let output = cmd
        .args(["github-sync", "--mode", "manual"])
        .output()
        .expect("running the sync");

    assert!(
        output.status.success(),
        "SH-201: an off project must be repairable: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stored_mode(&env, &project), "manual");
}

/// The `off` refusal itself must name a command that actually runs — the
/// defect SH-201 was filed about. Before the fix this message said "re-run
/// `story github-sync`", which was refused right alongside it.
#[test]
fn the_off_refusal_names_a_command_that_works() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    configure_github_sync_with_mode(&project, "off");

    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    let output = cmd
        .args(["github-sync"])
        .output()
        .expect("running the sync");

    assert_eq!(output.status.code(), Some(2), "Usage is exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--mode manual"),
        "the refusal must name a command that works, not the one that is refused: {stderr}"
    );
}

/// A mode change touches only the stored document — no `GithubApi` client is
/// ever built, so it needs no credential. `TestEnv` already strips
/// `STORYHOOK_GITHUB_TOKEN` from every child (`crates/storyhook-test-support`,
/// so no other proof is needed), and this run succeeds anyway.
#[test]
fn changing_the_mode_needs_no_token() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    configure_github_sync_with_mode(&project, "off");

    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    let output = cmd
        .args(["github-sync", "--mode", "manual"])
        .output()
        .expect("running the sync");

    assert!(
        output.status.success(),
        "a mode change needs no GitHub token: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `--dry-run` on a mode change writes nothing, matching every other write
/// this function makes.
#[test]
fn changing_the_mode_under_dry_run_writes_nothing() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    configure_github_sync_with_mode(&project, "auto");

    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    let output = cmd
        .args(["github-sync", "--mode", "manual", "--dry-run"])
        .output()
        .expect("running the sync");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Dry run"),
        "the response must say nothing was written: {stdout}"
    );

    assert_eq!(
        stored_mode(&env, &project),
        "auto",
        "a dry run must not change the stored mode"
    );
}

/// A mode change is project-level and takes no story — naming one is refused
/// rather than silently ignored, since a per-story mode is not a thing this
/// command has ever supported.
#[test]
fn changing_the_mode_refuses_a_story_id() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    configure_github_sync(&project);
    let story_id = project.new_story("a story that should not scope a mode change");

    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    let output = cmd
        .args(["github-sync", &story_id, "--mode", "off"])
        .output()
        .expect("running the sync");

    assert_eq!(output.status.code(), Some(2), "Usage is exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--mode"), "{stderr}");
    assert!(stderr.contains("takes no story"), "{stderr}");
}

/// Setting a project to the mode it already has is not an error — it is the
/// obvious idempotent case, and refusing it would make a script that always
/// names the desired mode (rather than checking first) fail half the time.
#[test]
fn changing_to_the_mode_already_set_succeeds() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    configure_github_sync(&project); // already "manual"

    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    let output = cmd
        .args(["github-sync", "--mode", "manual"])
        .output()
        .expect("running the sync");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("already"), "{stdout}");
    assert_eq!(stored_mode(&env, &project), "manual");
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
