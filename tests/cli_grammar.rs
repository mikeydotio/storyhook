use assert_cmd::Command;
use predicates::prelude::*;
use storyhook::cli::{EngineAction, Invocation, parse_invocation};
use storyhook::store::{EngineAgent, EngineSpeed};
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment's private `HOME`, XDG directories and store — so nothing
/// here can reach the developer's own storyhook state, with or without a
/// wrapper script supplying one.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

fn init_and_create(dir: &std::path::Path) {
    story(dir)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir).args(["new", "Test story"]).assert().success();
}

#[test]
fn show_displays_story() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Test story"));
}

#[test]
fn comment_adds_comment() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["comment", "SH-1", "my note"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("my note"));
}

#[test]
fn assign_sets_assignee() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["member", "add", "Test User <test@test.com>"])
        .assert()
        .success();

    story(dir.path())
        .args(["assign", "SH-1", "test-user"])
        .assert()
        .success();
}

#[test]
fn move_changes_state() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["move", "SH-1", "in-progress"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: in-progress"));
}

#[test]
fn move_to_done_archives() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success()
        .stdout(predicate::str::contains("closed_at:"));

    // SH-409: closed stories are excluded from `list` by default now —
    // `--include-closed` is what still finds it in `[done]` state.
    story(dir.path())
        .args(["list", "--include-closed"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[done]"));

    // JSONL file is moved out of open/stories/
    assert!(
        !dir.path()
            .join(".storyhook/open/stories/SH-1.jsonl")
            .exists()
    );
}

#[test]
fn block_sets_awaiting() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["block", "SH-1", "waiting for API"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("waiting for API"));
}

#[test]
fn unblock_clears_awaiting() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["block", "SH-1", "waiting for API"])
        .assert()
        .success();

    story(dir.path())
        .args(["unblock", "SH-1"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("awaiting:").not());
}

#[test]
fn prioritize_sets_priority() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: high"));
}

#[test]
fn label_adds_labels() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["label", "SH-1", "bug,frontend"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("bug"))
        .stdout(predicate::str::contains("frontend"));
}

#[test]
fn unlabel_removes_labels() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["label", "SH-1", "bug,frontend"])
        .assert()
        .success();

    story(dir.path())
        .args(["unlabel", "SH-1", "bug"])
        .assert()
        .success();
}

#[test]
fn reopen_reopens() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    story(dir.path())
        .args(["reopen", "SH-1"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Test story"));
}

#[test]
fn delete_permanently_removes_the_story() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["delete", "SH-1", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("deleted SH-1 — Test story"));

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("story `SH-1` not found"));
}

#[test]
fn relate_adds_relationship() {
    let dir = scratch_dir();
    init_and_create(dir.path());
    story(dir.path())
        .args(["new", "Second story"])
        .assert()
        .success();

    story(dir.path())
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("blocks"))
        .stdout(predicate::str::contains("SH-2"));
}

#[test]
fn unrelate_removes_relationship() {
    let dir = scratch_dir();
    init_and_create(dir.path());
    story(dir.path())
        .args(["new", "Second story"])
        .assert()
        .success();

    story(dir.path())
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();

    story(dir.path())
        .args(["unrelate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();
}

#[test]
fn link_is_alias_for_relate() {
    let dir = scratch_dir();
    init_and_create(dir.path());
    story(dir.path())
        .args(["new", "Second story"])
        .assert()
        .success();

    story(dir.path())
        .args(["link", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();
}

#[test]
fn unlink_is_alias_for_unrelate() {
    let dir = scratch_dir();
    init_and_create(dir.path());
    story(dir.path())
        .args(["new", "Second story"])
        .assert()
        .success();

    story(dir.path())
        .args(["link", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();

    story(dir.path())
        .args(["unlink", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();
}

#[test]
fn set_batch_updates() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["set", "SH-1", "--title", "New title", "--priority", "high"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("New title"))
        .stdout(predicate::str::contains("priority: high"));
}

#[test]
fn set_json_patch_applies_fields() {
    // `story set <id> --json '...'` applies a JSON patch to the story.
    // split_global_flags detects that --json is followed by a value arg
    // and keeps it for the subcommand parser instead of consuming it as
    // the global JSON-output flag.
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args([
            "set",
            "SH-1",
            "--json",
            r#"{"title":"JSON title","priority":"critical"}"#,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("title -> JSON title"))
        .stdout(predicate::str::contains("priority -> critical"));
}

// Regression: #39 — the `--json` patch's "assignee" key wrote the raw input
// as `member_id` with no membership check, unlike the `--assignee` flag
// (parsed by the same `set` command) right next to it, which already
// validated via `storage::find_member`. See also `story_new_fields.rs`'s
// `new_with_unknown_assignee_is_rejected_and_creates_no_story`.

#[test]
fn set_json_patch_unknown_assignee_is_rejected_and_does_not_set() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["set", "SH-1", "--json", r#"{"assignee":"nobody"}"#])
        .assert()
        .failure()
        .stderr(predicate::str::contains("member `nobody` not found"));

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("assignee: -"));
}

#[test]
fn set_json_patch_assignee_by_github_handle_normalizes_to_member_id() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    // `id` is a lowercased slug of the handle, so this member's id
    // ("mikeyward") differs in case from its github handle ("MikeyWard").
    story(dir.path())
        .args(["member", "add", "-g", "MikeyWard"])
        .assert()
        .success();

    story(dir.path())
        .args(["set", "SH-1", "--json", r#"{"assignee":"MikeyWard"}"#])
        .assert()
        .success()
        .stdout(predicate::str::contains("assignee -> MikeyWard"));

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("assignee: mikeyward"));
}

#[test]
fn set_blocked_via_set() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["set", "SH-1", "--blocked", "waiting for design"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("waiting for design"));
}

#[test]
fn set_unblocked_via_set() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    story(dir.path())
        .args(["block", "SH-1", "waiting for design"])
        .assert()
        .success();

    story(dir.path())
        .args(["set", "SH-1", "--unblocked"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("awaiting:").not());
}

#[test]
fn old_syntax_errors() {
    let dir = scratch_dir();
    init_and_create(dir.path());

    // "story SH-1 is done" should fail — SH-1 is not a valid command
    story(dir.path())
        .args(["SH-1", "is", "done"])
        .assert()
        .failure();

    // "story SH-1 assign mikey" should fail
    story(dir.path())
        .args(["SH-1", "assign", "mikey"])
        .assert()
        .failure();

    // "story SH-1 reopen" should fail
    story(dir.path())
        .args(["SH-1", "reopen"])
        .assert()
        .failure();
}

#[test]
fn new_with_state() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "My story", "--state", "in-progress"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: in-progress"));
}

// -----------------------------------------------------------------------
// Phase tests
// -----------------------------------------------------------------------

#[test]
fn phase_list_empty() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "A task"]).assert().success();

    story(dir.path())
        .args(["phase", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no phases found"));
}

#[test]
fn phase_add_and_list() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();
    story(dir.path()).args(["new", "Task C"]).assert().success();

    // Assign stories to phases
    story(dir.path())
        .args(["phase", "add", "SH-1", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("assigned SH-1 to phase 1"));
    story(dir.path())
        .args(["phase", "add", "SH-2", "1"])
        .assert()
        .success();
    story(dir.path())
        .args(["phase", "add", "SH-3", "2"])
        .assert()
        .success();

    // List phases
    let output = story(dir.path()).args(["phase", "list"]).assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Phase 1"), "should show Phase 1");
    assert!(stdout.contains("Phase 2"), "should show Phase 2");
}

#[test]
fn phase_show() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();

    story(dir.path())
        .args(["phase", "add", "SH-1", "1"])
        .assert()
        .success();
    story(dir.path())
        .args(["phase", "add", "SH-2", "2"])
        .assert()
        .success();

    // Show phase 1 should only have Task A
    story(dir.path())
        .args(["phase", "show", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task A"))
        .stdout(predicate::str::contains("Task B").not());
}

#[test]
fn phase_create() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["phase", "create", "1", "Foundation"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Phase 1: Foundation"))
        .stdout(predicate::str::contains("phase:1"));
}

#[test]
fn phase_remove() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();

    story(dir.path())
        .args(["phase", "add", "SH-1", "1"])
        .assert()
        .success();

    story(dir.path())
        .args(["phase", "remove", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed phase assignment"));

    // Verify no phase label remains
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("phase:").not());
}

#[test]
fn list_with_phase_filter() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();

    story(dir.path())
        .args(["phase", "add", "SH-1", "1"])
        .assert()
        .success();

    // List with --phase 1 should only show Task A
    story(dir.path())
        .args(["list", "--phase", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task A"))
        .stdout(predicate::str::contains("Task B").not());
}

#[test]
fn next_with_phase_filter() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();

    story(dir.path())
        .args(["phase", "add", "SH-2", "2"])
        .assert()
        .success();

    // Next with --phase 2 should only return Task B
    story(dir.path())
        .args(["next", "--phase", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task B"));
}

#[test]
fn decompose_sets_phase_labels() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let spec = "### Wave 1\n- [ ] Task A\n- [ ] Task B\n### Wave 2\n- [ ] Task C\n";
    let spec_path = dir.path().join("spec.md");
    std::fs::write(&spec_path, spec).unwrap();

    story(dir.path())
        .args(["decompose", "spec.md"])
        .assert()
        .success();

    // Task A and B should have phase:1 label
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("phase:1"));

    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("phase:1"));

    // Task C should have phase:2 label
    story(dir.path())
        .args(["show", "SH-3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("phase:2"));
}

#[test]
fn load_context_shows_phases() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();

    story(dir.path())
        .args(["phase", "add", "SH-1", "1"])
        .assert()
        .success();
    story(dir.path())
        .args(["phase", "add", "SH-2", "2"])
        .assert()
        .success();

    story(dir.path())
        .args(["load-context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Phase Progress"))
        .stdout(predicate::str::contains("Phase 1"))
        .stdout(predicate::str::contains("Phase 2"));
}

#[test]
fn old_context_alias_works() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();

    // "context" should still work as an alias for "load-context"
    story(dir.path())
        .args(["context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Project Status"));
}

#[test]
fn load_context_basic() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();

    story(dir.path())
        .args(["load-context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Project Status"))
        .stdout(predicate::str::contains("1 open"));
}

fn invocation(args: &[&str]) -> Result<Invocation, storyhook::error::AppError> {
    parse_invocation(
        &args
            .iter()
            .map(|word| (*word).to_string())
            .collect::<Vec<_>>(),
    )
}

#[test]
fn engine_parser_covers_every_action_and_default() {
    assert_eq!(
        invocation(&["engine", "start"]).unwrap(),
        Invocation::Engine {
            action: EngineAction::Start {
                epic: None,
                lanes: 1,
                agent: EngineAgent::Claude,
                model: None,
                effort: None,
                speed: None,
            },
        }
    );
    assert_eq!(
        invocation(&[
            "engine",
            "start",
            "--agent",
            "codex",
            "--lanes",
            "3",
            "--epic",
            "SH-9",
            "--model",
            "gpt-5.6-sol",
            "--effort",
            "xhigh",
            "--speed",
            "fast",
        ])
        .unwrap(),
        Invocation::Engine {
            action: EngineAction::Start {
                epic: Some("SH-9".to_string()),
                lanes: 3,
                agent: EngineAgent::Codex,
                model: Some("gpt-5.6-sol".to_string()),
                effort: Some("xhigh".to_string()),
                speed: Some(EngineSpeed::Fast),
            },
        }
    );
    for (args, expected) in [
        (
            vec!["engine", "status", "--run", "run-1"],
            EngineAction::Status {
                run: Some("run-1".to_string()),
            },
        ),
        (vec!["engine", "pause"], EngineAction::Pause { run: None }),
        (
            vec!["engine", "resume", "--run", "run-1"],
            EngineAction::Resume {
                run: Some("run-1".to_string()),
            },
        ),
        (
            vec!["engine", "stop", "--now", "--run", "run-1"],
            EngineAction::Stop {
                run: Some("run-1".to_string()),
                now: true,
            },
        ),
        (vec!["engine", "ack"], EngineAction::Ack { run: None }),
    ] {
        assert_eq!(
            invocation(&args).unwrap(),
            Invocation::Engine { action: expected }
        );
    }
}

#[test]
fn engine_parser_refuses_bad_values_scoped_flags_and_trailing_words() {
    for args in [
        vec!["engine", "start", "--lanes", "0"],
        vec!["engine", "start", "--lanes", "256"],
        vec!["engine", "start", "--lanes", "many"],
        vec!["engine", "start", "--agent", "other"],
        vec!["engine", "start", "--model", "gpt;unsafe"],
        vec!["engine", "start", "--effort", "high effort"],
        vec!["engine", "start", "--speed", "turbo"],
    ] {
        assert!(
            invocation(&args).is_err(),
            "story {} must fail",
            args.join(" ")
        );
    }

    let wrong_flag = invocation(&["engine", "status", "--now"]).unwrap_err();
    assert!(wrong_flag.to_string().contains("unknown flag `--now`"));

    let trailing = invocation(&["engine", "ack", "extra"]).unwrap_err();
    assert!(trailing.to_string().contains("unexpected argument `extra`"));
    assert!(trailing.to_string().contains("usage: story engine ack"));

    assert!(invocation(&["engine"]).is_err());
    assert!(invocation(&["engine", "launch"]).is_err());
}

#[test]
fn engine_cli_runs_the_lifecycle_and_reports_no_auto_work() {
    let dir = scratch_dir();
    let path = dir.path();
    story(path)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(path)
        .args(["new", "Approve the rollout", "--labels", "no-auto"])
        .assert()
        .success();

    let started = story(path)
        .args([
            "engine",
            "start",
            "--lanes",
            "2",
            "--agent",
            "codex",
            "--model",
            "gpt-5.6-sol",
            "--effort",
            "xhigh",
            "--speed",
            "fast",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let started: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    let run_id = started["run"]["id"].as_str().unwrap().to_string();
    assert_eq!(started["run"]["scope"]["kind"], "project");
    assert_eq!(started["run"]["agent"], "codex");
    assert_eq!(started["run"]["model"], "gpt-5.6-sol");
    assert_eq!(started["run"]["effort"], "xhigh");
    assert_eq!(started["run"]["speed"], "fast");
    assert_eq!(started["run"]["lane_count"], 2);
    assert_eq!(started["run"]["lanes"].as_array().unwrap().len(), 2);
    assert_eq!(started["run"]["needs_human"][0]["id"], "SH-1");

    story(path)
        .args(["engine", "status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(r#""id": "{run_id}""#)))
        .stdout(predicate::str::contains(r#""model": "gpt-5.6-sol""#))
        .stdout(predicate::str::contains(r#""effort": "xhigh""#))
        .stdout(predicate::str::contains(r#""speed": "fast""#));
    story(path)
        .args(["engine", "pause"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: paused"));
    story(path)
        .args(["engine", "resume"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: running"));
    story(path)
        .args(["engine", "stop"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: finished"))
        .stdout(predicate::str::contains("stop reason: operator-stopped"));

    story(path)
        .args(["engine", "status"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no live engine run"));
    story(path)
        .args(["engine", "ack", "--run", &run_id, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"acknowledged_at\""));
    story(path)
        .args(["engine", "status", "--run", &run_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("acknowledged: 20"));

    story(path)
        .args(["engine", "start"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: running"));
    story(path)
        .args(["engine", "stop", "--now"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: finished"))
        .stdout(predicate::str::contains(
            "stop reason: operator-stopped-now",
        ));
}

#[test]
fn engine_start_canonicalizes_a_bare_epic_id() {
    let dir = scratch_dir();
    let path = dir.path();
    story(path)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(path)
        .args(["new", "Release epic", "--type", "epic"])
        .assert()
        .success();

    story(path)
        .args(["engine", "start", "--epic", "1", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"epic\": \"SH-1\""));
}
