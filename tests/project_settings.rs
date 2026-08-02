//! `story project settings` — the CLI surface for a project's settings (SH-129).
//!
//! The defect this closes is reachability rather than storage. `project_settings`
//! has been written since the store cutover and read since before it, but the
//! only writer was `story migrate`, so a project *created* after the flip could
//! never change either value it holds. `sync.auto_transition` in particular was
//! documented as per-project configurable while being, in fact, permanently on.
//!
//! So the headline test here is not that a value round trips through a column —
//! `tests/service_settings.rs` proves that — but that turning the setting off
//! changes what `story commit-sync` actually does.
//!
//! # What must not be confused with what
//!
//! Three different things wear the word "config" in this project, and the help
//! text has to keep them apart:
//!
//! - **project settings** — this file. Per project, in the store.
//! - **`story set`** — per *story*, and nothing to do with a project.
//! - **`.storyhook.toml`** — per *repository*: `[plugin]` and `[hooks]`,
//!   versioned with the branch and carried by a clone.
//!
//! The last block of tests pins that the shipped help says so.

use std::path::Path;
use std::process::Command;

use storyhook_test_support::{Project, TestEnv};

/// Runs `git <args>` in `cwd` under the fixture's environment.
fn git(env: &TestEnv, cwd: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command.current_dir(cwd);
    env.apply(&mut command);
    command.env("GIT_TERMINAL_PROMPT", "0");
    let output = command.args(args).output().expect("running git");
    assert!(
        output.status.success(),
        "`git {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Runs `story project settings …` and returns the raw output, so a test can
/// assert on a refusal as easily as on an answer.
fn settings(project: &Project<'_>, args: &[&str]) -> std::process::Output {
    let mut cmd = project.story();
    cmd.args(["project", "settings"]);
    cmd.args(args);
    cmd.output().expect("running story project settings")
}

/// The state one story is in, straight from `story show --json`.
fn state_of(project: &Project<'_>, id: &str) -> String {
    project.json(&["show", id])["story"]["story"]["state"]
        .as_str()
        .expect("a story has a state")
        .to_string()
}

/// A git project with one story, and a commit whose body names it.
///
/// The id goes in the **body** rather than the subject, which is how this
/// repository writes them and what `commit-sync` scans.
fn repository_naming_a_story(env: &TestEnv) -> (Project<'_>, String) {
    let project = env.project().git().build();
    let id = project.new_story("Referenced from a commit body");
    std::fs::write(project.path().join("f"), "b\n").expect("changing a tracked file");
    git(env, project.path(), &["add", "f"]);
    git(
        env,
        project.path(),
        &["commit", "-qm", "do the thing", "-m", &format!("Refs {id}")],
    );
    (project, id)
}

// ---------------------------------------------------------------------------
// The defect, closed
// ---------------------------------------------------------------------------

/// The control. Without it, the test below would pass just as well against a
/// `commit-sync` that had stopped transitioning anything at all.
#[test]
fn commit_sync_moves_a_mentioned_story_while_the_setting_is_untouched() {
    let env = TestEnv::shared();
    let (project, id) = repository_naming_a_story(env);

    project.run(&["commit-sync"]).success();

    assert_eq!(
        state_of(&project, &id),
        "in-progress",
        "the default is on, so a mentioned story should have moved"
    );
}

/// The acceptance criterion: the exact case that was impossible before SH-129.
#[test]
fn turning_off_auto_transition_stops_commit_sync_moving_a_mentioned_story() {
    let env = TestEnv::shared();
    let (project, id) = repository_naming_a_story(env);

    project
        .run(&[
            "project",
            "settings",
            "set",
            "sync.auto_transition",
            "false",
        ])
        .success();
    project.run(&["commit-sync"]).success();

    assert_eq!(
        state_of(&project, &id),
        "todo",
        "auto-transition was turned off, so the story must not have moved"
    );
}

/// And turning it back on restores the behaviour — a one-way switch would be a
/// different defect wearing the same fix.
#[test]
fn unsetting_auto_transition_restores_the_default_behaviour() {
    let env = TestEnv::shared();
    let (project, id) = repository_naming_a_story(env);

    project
        .run(&[
            "project",
            "settings",
            "set",
            "sync.auto_transition",
            "false",
        ])
        .success();
    project
        .run(&["project", "settings", "unset", "sync.auto_transition"])
        .success();
    project.run(&["commit-sync"]).success();

    assert_eq!(state_of(&project, &id), "in-progress");
}

/// Settings belong to a project, not to the store. Two projects in one store
/// must not see each other's values.
#[test]
fn one_projects_settings_do_not_reach_another() {
    let env = TestEnv::shared();
    let one = env.project().build();
    let two = env.project().build();

    one.run(&["project", "settings", "set", "doctor.stale_threshold", "3d"])
        .success();

    assert_eq!(
        one.json(&["project", "settings", "get", "doctor.stale_threshold"])["settings"][0]["value"],
        serde_json::json!("3d")
    );
    assert_eq!(
        two.json(&["project", "settings", "get", "doctor.stale_threshold"])["settings"][0]["value"],
        serde_json::Value::Null,
        "the second project inherited the first's setting"
    );
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The listing has to answer "what is in force, and did I choose it?" — which
/// is why the source is reported rather than left to be inferred from a value.
#[test]
fn the_listing_names_every_key_and_says_which_values_are_defaults() {
    let env = TestEnv::shared();
    let project = env.project().build();

    let out = settings(&project, &["list"]);
    assert!(out.status.success(), "listing settings should succeed");
    let text = String::from_utf8_lossy(&out.stdout);

    for key in [
        "sync.auto_transition",
        "doctor.stale_threshold",
        "github.sync",
    ] {
        assert!(
            text.contains(key),
            "`{key}` is missing from the listing:\n{text}"
        );
    }
    assert!(
        text.contains("default"),
        "an unwritten `sync.auto_transition` is `true` by default and must say so:\n{text}"
    );
    assert!(
        text.contains("unset"),
        "an unwritten `doctor.stale_threshold` has no default and must say so:\n{text}"
    );
}

/// `doctor.stale_threshold` is storable user data that nothing consumes yet.
/// The label is the only thing standing between a user and the reasonable
/// belief that writing it changed something, so it ships in the listing.
#[test]
fn the_listing_says_plainly_that_the_stale_threshold_is_not_read_yet() {
    let env = TestEnv::shared();
    let project = env.project().build();

    let out = settings(&project, &["list"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("no command reads this yet"),
        "the inertness of `doctor.stale_threshold` must be visible:\n{text}"
    );
}

/// The read-only key says who owns it, so a user who wants to change it knows
/// where to go rather than only that they may not.
#[test]
fn the_listing_names_the_command_that_owns_the_github_document() {
    let env = TestEnv::shared();
    let project = env.project().build();

    let out = settings(&project, &["list"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("story github-sync"),
        "`github.sync` must name its owning command:\n{text}"
    );
}

/// `--json` carries the source as a field rather than as English inside a
/// message, which is the whole reason this verb got a typed response.
#[test]
fn json_reports_the_source_as_a_field_a_script_can_read() {
    let env = TestEnv::shared();
    let project = env.project().build();

    let listing = project.json(&["project", "settings", "list"]);
    assert_eq!(listing["result"], "ok");
    let settings = listing["settings"]
        .as_array()
        .expect("`--json` must carry a settings array");
    assert_eq!(settings.len(), 3);

    let sync = &settings[0];
    assert_eq!(sync["key"], "sync.auto_transition");
    assert_eq!(sync["kind"], "boolean");
    assert_eq!(sync["source"], "default");
    assert_eq!(sync["value"], "true");
    assert_eq!(sync["settable"], true);

    let doctor = &settings[1];
    assert_eq!(doctor["source"], "unset");
    assert_eq!(doctor["value"], serde_json::Value::Null);
    assert!(
        doctor["note"]
            .as_str()
            .expect("the inert key carries a note")
            .contains("no command reads this yet")
    );

    let github = &settings[2];
    assert_eq!(github["key"], "github.sync");
    assert_eq!(github["settable"], false);
    assert_eq!(github["managed_by"], "story github-sync");
}

/// A write answers with the value it wrote, so the user sees what took effect
/// without a second command.
#[test]
fn set_answers_with_the_setting_it_wrote() {
    let env = TestEnv::shared();
    let project = env.project().build();

    let written = project.json(&[
        "project",
        "settings",
        "set",
        "doctor.stale_threshold",
        "45d",
    ]);
    let view = &written["settings"][0];
    assert_eq!(view["key"], "doctor.stale_threshold");
    assert_eq!(view["value"], "45d");
    assert_eq!(view["source"], "set");
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_key_is_refused_with_exit_two_and_names_the_real_ones() {
    let env = TestEnv::shared();
    let project = env.project().build();

    let out = settings(&project, &["get", "sync.autotransition"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("sync.auto_transition"),
        "the refusal must point at the key the user meant: {stderr}"
    );
}

#[test]
fn writing_the_github_document_is_refused_and_names_its_owner() {
    let env = TestEnv::shared();
    let project = env.project().build();

    let out = settings(&project, &["set", "github.sync", "{}"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("story github-sync"),
        "the refusal must say where the value does come from: {stderr}"
    );
}

/// `parse_duration` reads its count as `i64`, so `0d` and `-14d` both parse.
/// Neither is a threshold.
#[test]
fn a_zero_or_negative_duration_is_refused() {
    let env = TestEnv::shared();
    let project = env.project().build();

    for bad in ["0d", "-14d", "14", "soon"] {
        let out = settings(&project, &["set", "doctor.stale_threshold", bad]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "`{bad}` should have been refused; stdout={}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// Explicit subcommands, so one missing shell word cannot turn a read into a
/// write. A bare `settings` says what the forms are rather than guessing.
#[test]
fn the_grammar_requires_a_subcommand() {
    let env = TestEnv::shared();
    let project = env.project().build();

    for args in [
        vec![],
        vec!["frobnicate"],
        vec!["get"],
        vec!["set", "sync.auto_transition"],
        vec!["unset"],
    ] {
        let out = settings(&project, &args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "`story project settings {}` should be a usage error",
            args.join(" ")
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("list") && stderr.contains("get") && stderr.contains("set"),
            "a usage error must show the forms: {stderr}"
        );
    }
}

// ---------------------------------------------------------------------------
// Help — the three-way disambiguation
// ---------------------------------------------------------------------------

#[test]
fn the_verb_is_discoverable_from_the_top_level_help() {
    let env = TestEnv::shared();
    let project = env.project().build();

    let out = project
        .story()
        .arg("--help")
        .output()
        .expect("story --help");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("story project settings"),
        "`story --help` must list the verb:\n{text}"
    );
}

#[test]
fn the_project_topic_points_at_the_settings_topic() {
    let env = TestEnv::shared();
    let project = env.project().build();

    let out = project
        .story()
        .args(["help", "project"])
        .output()
        .expect("story help project");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("settings"),
        "the project topic must mention its settings verb:\n{text}"
    );
}

/// The topic that has to do the disambiguation the story asked for.
#[test]
fn the_settings_topic_separates_the_three_things_called_config() {
    let env = TestEnv::shared();
    let project = env.project().build();

    let out = project
        .story()
        .args(["help", "project-settings"])
        .output()
        .expect("story help project-settings");
    assert!(
        out.status.success(),
        "`story help project-settings` must exist: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(
        text.contains("story set"),
        "the topic must say what `story set` is instead:\n{text}"
    );
    assert!(
        text.contains(".storyhook.toml"),
        "the topic must say what `.storyhook.toml` is instead:\n{text}"
    );
    for key in [
        "sync.auto_transition",
        "doctor.stale_threshold",
        "github.sync",
    ] {
        assert!(
            text.contains(key),
            "the topic must document `{key}`:\n{text}"
        );
    }
}
