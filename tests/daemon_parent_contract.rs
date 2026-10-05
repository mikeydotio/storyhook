//! A `story` whose test-harness owner has gone starts nothing, and a daemon
//! follows the owner incarnation its spawner saw.
//!
//! Plugin tests left daemons listening after their temporary home was deleted.
//! A helper or hook still running after its test ended ran `story`, which
//! started a fresh daemon for the deleted fixture and recreated its home. A
//! shell harness names its owner by pid alone, so that daemon could also
//! follow whatever process later reused the pid. Both are closed where they
//! begin: the client refuses to start a daemon for a gone owner, and a client
//! that sees its owner alive pins the owner's start token for the daemon.

use std::process::Command;

use storyhook::daemon::lifecycle;
use storyhook_test_support::{ChildGuard, DaemonGuard, TestEnv, scratch_dir};

/// A pid whose process has exited and been reaped.
fn a_pid_that_has_exited() -> u32 {
    let mut gone =
        ChildGuard::spawn(Command::new("sleep").arg("30")).expect("spawning a stand-in owner");
    let pid = gone.pid();
    gone.kill_and_reap();
    pid
}

/// The incident's shape: the owner is gone, and a straggler asks for a
/// daemon. Every command that would start one is refused, and nothing is
/// written: no store, no daemon state, and so no recreated home.
#[test]
fn a_story_whose_parent_has_exited_starts_no_daemon_and_writes_nothing() {
    let env = TestEnv::isolated();
    let cwd = scratch_dir();
    let _daemon = DaemonGuard::new(&env, cwd.path());
    let owner = a_pid_that_has_exited();
    let state = env.environment().daemon_state_dir();

    for args in [
        &["daemon", "start"][..],
        &["list"][..],
        &["daemon", "restart"][..],
    ] {
        let out = env
            .story(cwd.path())
            .env("STORYHOOK_PARENT_PID", owner.to_string())
            .env("STORYHOOK_PARENT_START_TIME", "")
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("running `story {}`: {e}", args.join(" ")));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "`story {}` succeeded for an owner that has exited",
            args.join(" ")
        );
        assert!(
            stderr.contains("STORYHOOK_PARENT_PID") && stderr.contains(&owner.to_string()),
            "`story {}` must say its owner has gone; it said: {stderr}",
            args.join(" ")
        );
        assert!(
            !env.store_path().exists(),
            "`story {}` created the store {} for a finished run",
            args.join(" "),
            env.store_path().display()
        );
        assert!(
            !state.exists(),
            "`story {}` created daemon state at {} for a finished run",
            args.join(" "),
            state.display()
        );
    }
}

/// A shell harness declares an empty start token. The client that starts the
/// daemon samples the owner's token while the owner is alive, and the daemon
/// and everything it runs carry that exact token, so a later process that
/// reuses the pid is never mistaken for the owner.
///
/// The daemon's own environment is read through an event hook, which inherits
/// it whole.
#[test]
fn a_daemon_follows_the_incarnation_its_spawner_saw() {
    let env = TestEnv::isolated();
    let mut owner =
        ChildGuard::spawn(Command::new("sleep").arg("30")).expect("spawning a stand-in owner");
    let token = lifecycle::process_start_time(owner.pid())
        .expect("the stand-in owner's native start token");
    let project = env.project().build();
    env.stop_daemon();
    let _daemon = DaemonGuard::new(&env, project.path());

    env.story(project.path())
        .env("STORYHOOK_PARENT_PID", owner.pid().to_string())
        .env("STORYHOOK_PARENT_START_TIME", "")
        .args(["daemon", "start"])
        .assert()
        .success();

    let seen = project.path().join("what-the-hook-saw");
    let pointer = project.path().join(".storyhook.toml");
    let identity = std::fs::read_to_string(&pointer).expect("the project's pointer file");
    std::fs::write(
        &pointer,
        format!(
            "{identity}\n[hooks.on_create]\ncommand = \"printf '%s' \\\"${{STORYHOOK_PARENT_START_TIME-unset}}\\\" > {}\"\n",
            seen.display()
        ),
    )
    .expect("appending the hook to the pointer file");

    env.story(project.path())
        .args(["new", "a story whose creation fires the hook"])
        .assert()
        .success();

    let reported = std::fs::read_to_string(&seen)
        .expect("the on_create hook must have run and written its file");
    assert_eq!(
        reported, token,
        "the daemon watches the owner's pid without the incarnation its spawner \
         saw, so a process that reuses the pid would keep it alive"
    );

    owner.kill_and_reap();
}
