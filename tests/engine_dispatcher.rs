use std::path::{Path, PathBuf};

use storyhook::domain::{CLEANUP_LEASE_VERSION, StoryCleanupLease, TmuxCleanupTarget};
use storyhook::env::Environment;
use storyhook::service::engine::{
    DispatchOutcome, DispatchOutcomeState, DispatchRequest, Dispatcher, ShellDispatcher,
    UnclaimRequest, WindowProbe,
};
use storyhook::store::{EngineAgent, EngineSpeed};
use storyhook_test_support::{DispatcherCall, DispatcherStep, FakeDispatcher, scratch_dir};

fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write dispatcher fixture");
}

/// The `XDG_STATE_HOME` a child must be handed to resolve `env`'s own state
/// home: the parent of `<...>/storyhook`, which is how `Environment` reads the
/// variable back (`$XDG_STATE_HOME/storyhook`).
fn xdg_state_home_of(env: &Environment) -> String {
    env.state_home()
        .parent()
        .expect("a state home has a parent")
        .to_string_lossy()
        .to_string()
}

fn request() -> DispatchRequest {
    DispatchRequest {
        project: "alpha".to_string(),
        story: "ALPHA-7".to_string(),
        agent: EngineAgent::Codex,
        model: None,
        effort: None,
        speed: None,
    }
}

#[test]
fn shell_dispatcher_appends_the_run_configuration_to_every_lane() {
    let root = scratch_dir();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let script = root.path().join("story.sh");
    write_script(
        &script,
        r#"printf '{"ok":true,"argv":"%s","cleanup_lease":{"version":1,"project_slug":"alpha","story_id":"ALPHA-7","repository_path":"/repos/original","worktree_path":"/repos/original/.codex/worktrees/ALPHA-7","branch":"worktree-ALPHA-7","tmux":{"socket_path":"/tmp/tmux-original/default"}}}\n' "$*""#,
    );
    let configured = DispatchRequest {
        model: Some("gpt-5.6-sol".to_string()),
        effort: Some("xhigh".to_string()),
        speed: Some(EngineSpeed::Fast),
        ..request()
    };

    let outcome = ShellDispatcher::new(&script, Environment::at(home))
        .dispatch(configured)
        .unwrap();

    assert_eq!(
        outcome.payload["argv"],
        "--project alpha dispatch ALPHA-7 --agent=codex --auto --full-auto --force --model=gpt-5.6-sol --effort=xhigh --speed=fast"
    );

    let standard = DispatchRequest {
        speed: Some(EngineSpeed::Standard),
        ..request()
    };
    let outcome = ShellDispatcher::new(&script, Environment::at(root.path().join("home")))
        .dispatch(standard)
        .unwrap();
    assert_eq!(
        outcome.payload["argv"],
        "--project alpha dispatch ALPHA-7 --agent=codex --auto --full-auto --force",
        "explicit standard keeps the helper's legacy argv"
    );
}

fn unclaim_request() -> UnclaimRequest {
    UnclaimRequest {
        project: "alpha".to_string(),
        story: "ALPHA-7".to_string(),
        cleanup_lease: cleanup_lease(),
    }
}

fn cleanup_lease() -> StoryCleanupLease {
    StoryCleanupLease {
        version: CLEANUP_LEASE_VERSION,
        project_slug: "alpha".to_string(),
        story_id: "ALPHA-7".to_string(),
        repository_path: PathBuf::from("/repos/original"),
        worktree_path: PathBuf::from("/repos/original/.codex/worktrees/ALPHA-7"),
        branch: "worktree-ALPHA-7".to_string(),
        tmux: TmuxCleanupTarget {
            socket_path: PathBuf::from("/tmp/tmux-original/default"),
        },
    }
}

#[test]
fn shell_dispatcher_invokes_the_autonomous_project_contract_and_relays_success() {
    let root = scratch_dir();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let script = root.path().join("story.sh");
    write_script(
        &script,
        r#"printf '{"ok":true,"argv":"%s","session":"%s","create":"%s","store":"%s","state_home":"%s","future":{"nested":true},"cleanup_lease":{"version":1,"project_slug":"alpha","story_id":"ALPHA-7","repository_path":"/repos/original","worktree_path":"/repos/original/.codex/worktrees/ALPHA-7","branch":"worktree-ALPHA-7","tmux":{"socket_path":"/tmp/tmux-original/default"}}}\n' "$*" "$STORY_TARGET_SESSION" "$STORY_CREATE_SESSION" "$STORYHOOK_STORE_PATH" "${XDG_STATE_HOME:-unset}""#,
    );
    let env = Environment::at(&home);
    let expected_store = env.store_path().to_string_lossy().to_string();
    let expected_state_home = xdg_state_home_of(&env);
    let outcome = ShellDispatcher::new(&script, env)
        .dispatch(request())
        .unwrap();

    assert_eq!(outcome.state, DispatchOutcomeState::Ok);
    assert_eq!(
        outcome.payload["argv"],
        "--project alpha dispatch ALPHA-7 --agent=codex --auto --full-auto --force"
    );
    assert_eq!(outcome.payload["session"], "alpha");
    assert_eq!(outcome.payload["create"], "1");
    assert_eq!(outcome.payload["store"], expected_store);
    // SH-633: a child told the store but not the state home found no daemon
    // under its own state home and started a second one for the same store.
    assert_eq!(outcome.payload["state_home"], expected_state_home);
    assert_eq!(outcome.payload["future"]["nested"], true);
}

#[test]
fn shell_dispatcher_relays_a_nonzero_refusal_instead_of_reclassifying_it_as_failure() {
    let root = scratch_dir();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let script = root.path().join("story.sh");
    write_script(
        &script,
        "printf '%s\\n' '{\"ok\":false,\"reason\":\"future-refusal\",\"detail\":{\"kept\":true}}'; exit 17",
    );
    let outcome = ShellDispatcher::new(&script, Environment::at(home))
        .dispatch(request())
        .unwrap();

    assert_eq!(outcome.state, DispatchOutcomeState::Refused);
    assert_eq!(outcome.payload["reason"], "future-refusal");
    assert_eq!(outcome.payload["detail"]["kept"], true);
}

#[test]
fn shell_dispatcher_rejects_success_without_a_cleanup_lease() {
    let root = scratch_dir();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let script = root.path().join("story.sh");
    write_script(
        &script,
        "printf '%s\\n' '{\"ok\":true,\"window_name\":\"ALPHA-7\"}'",
    );

    let error = ShellDispatcher::new(&script, Environment::at(home))
        .dispatch(request())
        .unwrap_err()
        .to_string();

    assert!(error.contains("omitted cleanup_lease"), "{error}");
}

#[test]
fn shell_dispatcher_rejects_success_json_from_a_failed_process() {
    let root = scratch_dir();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let script = root.path().join("story.sh");
    write_script(
        &script,
        "printf '%s\\n' '{\"ok\":true,\"display\":\"not actually dispatched\"}'; exit 17",
    );

    let error = ShellDispatcher::new(&script, Environment::at(home))
        .dispatch(request())
        .unwrap_err()
        .to_string();

    assert!(error.contains("reported success"), "{error}");
    assert!(error.contains("17"), "{error}");
}

#[test]
fn shell_dispatcher_invokes_the_non_destructive_unclaim_contract() {
    let root = scratch_dir();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let script = root.path().join("story.sh");
    write_script(
        &script,
        r#"printf '{"ok":true,"argv":"%s","store":"%s","state_home":"%s","closed_window":true,"worktree_status":"dirty","lease_env":%s,"cleanup":{"lease":%s,"postconditions":{"tmux_story_windows_absent":true}}}\n' "$*" "$STORYHOOK_STORE_PATH" "${XDG_STATE_HOME:-unset}" "$STORYHOOK_REAP_LEASE_V1" "$STORYHOOK_REAP_LEASE_V1""#,
    );
    let env = Environment::at(&home);
    let expected_store = env.store_path().to_string_lossy().to_string();
    let expected_state_home = xdg_state_home_of(&env);

    let outcome = ShellDispatcher::new(&script, env)
        .unclaim(unclaim_request())
        .unwrap();

    assert_eq!(outcome.state, DispatchOutcomeState::Ok);
    assert_eq!(outcome.payload["argv"], "--project alpha unclaim ALPHA-7");
    assert_eq!(outcome.payload["store"], expected_store);
    assert_eq!(outcome.payload["state_home"], expected_state_home);
    assert_eq!(outcome.payload["closed_window"], true);
    assert_eq!(outcome.payload["worktree_status"], "dirty");
    assert_eq!(
        serde_json::from_value::<StoryCleanupLease>(outcome.payload["lease_env"].clone()).unwrap(),
        cleanup_lease()
    );
}

#[test]
fn shell_dispatcher_rejects_success_json_from_a_failed_unclaim_process() {
    let root = scratch_dir();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let script = root.path().join("story.sh");
    write_script(
        &script,
        "printf '%s\\n' '{\"ok\":true,\"display\":\"not actually unclaimed\"}'; exit 23",
    );

    let error = ShellDispatcher::new(&script, Environment::at(home))
        .unclaim(unclaim_request())
        .unwrap_err()
        .to_string();

    assert!(error.contains("reported success"), "{error}");
    assert!(error.contains("23"), "{error}");
}

#[test]
fn shell_dispatcher_fails_only_when_the_helper_does_not_answer_with_json() {
    let root = scratch_dir();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let script = root.path().join("story.sh");
    write_script(&script, "printf 'helper exploded\\n' >&2; exit 19");
    let error = ShellDispatcher::new(&script, Environment::at(home))
        .dispatch(request())
        .unwrap_err();

    assert!(error.to_string().contains("helper exploded"));
}

#[test]
fn fake_dispatcher_scripts_calls_in_order_and_records_them() {
    let refused = DispatchOutcome::from_payload(serde_json::json!({
        "ok": false,
        "reason": "claim-conflict"
    }));
    let fake = FakeDispatcher::new([
        DispatcherStep::Dispatch(refused.clone()),
        DispatcherStep::Unclaim(refused.clone()),
        DispatcherStep::WindowAlive {
            window: "@7".to_string(),
            alive: false,
        },
        DispatcherStep::KillWindow {
            window: "@7".to_string(),
            result: Ok(()),
        },
    ]);

    assert_eq!(fake.dispatch(request()).unwrap(), refused);
    assert_eq!(fake.unclaim(unclaim_request()).unwrap(), refused);
    assert!(matches!(fake.probe_window("@7"), WindowProbe::Gone { .. }));
    fake.kill_window("@7").unwrap();
    assert_eq!(
        fake.calls(),
        vec![
            DispatcherCall::Dispatch(request()),
            DispatcherCall::Unclaim(unclaim_request()),
            DispatcherCall::WindowAlive("@7".to_string()),
            DispatcherCall::KillWindow("@7".to_string()),
        ]
    );
}

/// SH-657: an engine lane runs under the tool-call ceiling its own stall
/// clock is derived from. The daemon tells the helper the number in
/// milliseconds; the helper puts it on the lane's window as
/// `BASH_MAX_TIMEOUT_MS`. One constant, two consumers — the alternative is a
/// ceiling that cites a host default the operator's own settings can raise.
#[test]
fn shell_dispatcher_hands_every_engine_lane_the_tool_call_ceiling() {
    use storyhook::service::engine::{HOST_TOOL_CALL_CEILING_SECS, LANE_TOOL_CEILING_ENV};
    let root = scratch_dir();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let script = root.path().join("story.sh");
    write_script(
        &script,
        &format!(
            r#"printf '{{"ok":true,"ceiling":"%s","cleanup_lease":{{"version":1,"project_slug":"alpha","story_id":"ALPHA-7","repository_path":"/repos/original","worktree_path":"/repos/original/.codex/worktrees/ALPHA-7","branch":"worktree-ALPHA-7","tmux":{{"socket_path":"/tmp/tmux-original/default"}}}}}}\n' "${{{LANE_TOOL_CEILING_ENV}:-unset}}""#
        ),
    );

    let outcome = ShellDispatcher::new(&script, Environment::at(home))
        .dispatch(request())
        .unwrap();

    assert_eq!(
        outcome.payload["ceiling"],
        (HOST_TOOL_CALL_CEILING_SECS * 1000).to_string(),
        "the helper receives the engine's own ceiling, in milliseconds"
    );
}
