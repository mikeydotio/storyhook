use std::path::Path;

use storyhook::env::Environment;
use storyhook::service::engine::{
    DispatchOutcome, DispatchOutcomeState, DispatchRequest, Dispatcher, ShellDispatcher,
    UnclaimRequest,
};
use storyhook::store::EngineAgent;
use storyhook_test_support::{DispatcherCall, DispatcherStep, FakeDispatcher, scratch_dir};

fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write dispatcher fixture");
}

fn request() -> DispatchRequest {
    DispatchRequest {
        project: "alpha".to_string(),
        story: "ALPHA-7".to_string(),
        agent: EngineAgent::Codex,
    }
}

fn unclaim_request() -> UnclaimRequest {
    UnclaimRequest {
        project: "alpha".to_string(),
        story: "ALPHA-7".to_string(),
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
        r#"printf '{"ok":true,"argv":"%s","session":"%s","create":"%s","store":"%s","future":{"nested":true}}\n' "$*" "$STORY_TARGET_SESSION" "$STORY_CREATE_SESSION" "$STORYHOOK_STORE_PATH""#,
    );
    let env = Environment::at(&home);
    let expected_store = env.store_path().to_string_lossy().to_string();
    let outcome = ShellDispatcher::new(&script, env)
        .dispatch(request())
        .unwrap();

    assert_eq!(outcome.state, DispatchOutcomeState::Ok);
    assert_eq!(
        outcome.payload["argv"],
        "--project alpha dispatch ALPHA-7 --agent=codex --auto --full-auto"
    );
    assert_eq!(outcome.payload["session"], "alpha");
    assert_eq!(outcome.payload["create"], "1");
    assert_eq!(outcome.payload["store"], expected_store);
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
fn shell_dispatcher_invokes_the_non_destructive_unclaim_contract() {
    let root = scratch_dir();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let script = root.path().join("story.sh");
    write_script(
        &script,
        r#"printf '{"ok":true,"argv":"%s","store":"%s","closed_window":true,"worktree_status":"dirty"}\n' "$*" "$STORYHOOK_STORE_PATH""#,
    );
    let env = Environment::at(&home);
    let expected_store = env.store_path().to_string_lossy().to_string();

    let outcome = ShellDispatcher::new(&script, env)
        .unclaim(unclaim_request())
        .unwrap();

    assert_eq!(outcome.state, DispatchOutcomeState::Ok);
    assert_eq!(outcome.payload["argv"], "--project alpha unclaim ALPHA-7");
    assert_eq!(outcome.payload["store"], expected_store);
    assert_eq!(outcome.payload["closed_window"], true);
    assert_eq!(outcome.payload["worktree_status"], "dirty");
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
    assert!(!fake.window_alive("@7"));
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
