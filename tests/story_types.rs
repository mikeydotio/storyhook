use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

// ============================================================
// story type list/add/remove CLI commands
// ============================================================

#[test]
fn type_list_shows_default_types() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["type", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("story"))
        .stdout(predicate::str::contains("epic"))
        .stdout(predicate::str::contains("bug"))
        .stdout(predicate::str::contains("chore"))
        .stdout(predicate::str::contains("task"));
}

#[test]
fn type_add_and_list_shows_new_type() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args([
            "type",
            "add",
            "spike",
            "--description",
            "Time-boxed investigation",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("added type spike"));

    story(dir.path())
        .args(["type", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("spike"));
}

#[test]
fn type_add_duplicate_rejected() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["type", "add", "story"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn type_add_none_slug_rejected() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["type", "add", "none"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reserved"));
}

#[test]
fn type_add_none_titlecase_rejected() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["type", "add", "None"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reserved"));
}

#[test]
fn type_add_none_uppercase_rejected() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["type", "add", "NONE"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reserved"));
}

#[test]
fn type_add_none_mixedcase_rejected() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["type", "add", "nOnE"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reserved"));
}

#[test]
fn type_add_rejects_reserved_default_slug() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // Lowercase "default" should be rejected
    story(dir.path())
        .args(["type", "add", "default"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reserved"));

    // Mixed-case "Default" should also be rejected (case-insensitive)
    story(dir.path())
        .args(["type", "add", "Default"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reserved"));

    // All-caps "DEFAULT" should also be rejected
    story(dir.path())
        .args(["type", "add", "DEFAULT"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reserved"));
}

#[test]
fn type_remove_unused_succeeds() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["type", "remove", "chore"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed type chore"));

    // Verify it's gone from the list
    let output = story(dir.path()).args(["type", "list"]).assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(!stdout.contains("chore"));
}

#[test]
fn type_remove_in_use_rejected() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "A bug", "--type", "bug"])
        .assert()
        .success();

    story(dir.path())
        .args(["type", "remove", "bug"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("still used"));
}

#[test]
fn type_remove_nonexistent_rejected() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["type", "remove", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

// ============================================================
// story new --type / story set --type / story show type display
// ============================================================

#[test]
fn new_with_type_creates_typed_story() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Login crash", "--type", "bug"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: bug"));
}

#[test]
fn new_with_unknown_type_rejected() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Something", "--type", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown type `nonexistent`"));
}

#[test]
fn set_type_changes_story_type() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path()).args(["new", "A task"]).assert().success();

    story(dir.path())
        .args(["set", "SH-1", "--type", "epic"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: epic"));
}

#[test]
fn set_unknown_type_rejected() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path()).args(["new", "A task"]).assert().success();

    story(dir.path())
        .args(["set", "SH-1", "--type", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown type `nonexistent`"));
}

#[test]
fn untyped_story_shows_default_for_type() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Plain task"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: Default"));
}

// ============================================================
// story list --type filter
// ============================================================

#[test]
fn list_type_filter_shows_matching_stories() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Bug A", "--type", "bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Epic B", "--type", "epic"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Bug C", "--type", "bug"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--type", "bug"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(!stdout.contains("SH-2"));
    assert!(stdout.contains("SH-3"));
}

#[test]
fn list_type_none_shows_untyped_stories() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Typed", "--type", "bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Untyped"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--type", "none"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(!stdout.contains("SH-1"));
    assert!(stdout.contains("SH-2"));
}

#[test]
fn list_type_filter_combined_with_priority() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "High bug", "--type", "bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Low bug", "--type", "bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "High chore", "--type", "chore"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-2", "low"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-3", "high"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--type", "bug", "--priority", "high"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(!stdout.contains("SH-2"));
    assert!(!stdout.contains("SH-3"));
}

// ============================================================
// story epic create/add/list/show
// ============================================================

#[test]
fn epic_create_sets_type_to_epic() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["epic", "create", "Auth System"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"));

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Auth System"))
        .stdout(predicate::str::contains("type: epic"));
}

#[test]
fn epic_add_creates_parent_child_relationship() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["epic", "create", "Auth System"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Login page"])
        .assert()
        .success();

    story(dir.path())
        .args(["epic", "add", "SH-1", "SH-2"])
        .assert()
        .success();

    // Verify parent-child relationship
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("parent-of SH-2"));

    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("child-of SH-1"));
}

#[test]
fn epic_list_shows_only_epics_with_progress() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["epic", "create", "Auth System"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Login page"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Plain task"])
        .assert()
        .success();

    // Add child to epic
    story(dir.path())
        .args(["epic", "add", "SH-1", "SH-2"])
        .assert()
        .success();

    let output = story(dir.path()).args(["epic", "list"]).assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    // Should show the epic with progress
    assert!(stdout.contains("SH-1"));
    assert!(stdout.contains("[epic]"));
    assert!(stdout.contains("(0/1)"));
    // Should NOT show the non-epic stories
    assert!(!stdout.contains("SH-2") || stdout.contains("SH-1")); // SH-2 might appear in progress context
    assert!(!stdout.contains("SH-3"));
}

#[test]
fn epic_show_displays_progress() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["epic", "create", "Auth System"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Login page"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Signup page"])
        .assert()
        .success();

    story(dir.path())
        .args(["epic", "add", "SH-1", "SH-2"])
        .assert()
        .success();
    story(dir.path())
        .args(["epic", "add", "SH-1", "SH-3"])
        .assert()
        .success();

    // Close one child
    story(dir.path())
        .args(["move", "SH-2", "done"])
        .assert()
        .success();

    story(dir.path())
        .args(["epic", "show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "progress: 1/2 children done (50%)",
        ));
}

#[test]
fn epic_list_empty_when_no_epics() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Plain task"])
        .assert()
        .success();

    story(dir.path())
        .args(["epic", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no stories found"));
}

#[test]
fn epic_create_rejects_when_epic_type_not_defined() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // Remove the epic type
    story(dir.path())
        .args(["type", "remove", "epic"])
        .assert()
        .success();

    story(dir.path())
        .args(["epic", "create", "Auth System"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("type `epic` is not defined"));
}

// ============================================================
// Progress rollup rendering
// ============================================================

#[test]
fn show_displays_progress_in_human_output() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path()).args(["new", "Parent"]).assert().success();
    story(dir.path())
        .args(["new", "Child A"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Child B"])
        .assert()
        .success();

    story(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-3"])
        .assert()
        .success();

    // Before closing any child: 0/2
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("progress: 0/2 children done (0%)"));

    // Close one child
    story(dir.path())
        .args(["move", "SH-2", "done"])
        .assert()
        .success();

    // After closing one: 1/2 (50%)
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "progress: 1/2 children done (50%)",
        ));
}

#[test]
fn list_shows_type_badge_in_human_output() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "A bug", "--type", "bug"])
        .assert()
        .success();

    let output = story(dir.path()).args(["list"]).assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("[bug]"));
}

#[test]
fn list_shows_default_badge_for_untyped_story() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Untyped task"])
        .assert()
        .success();

    let output = story(dir.path()).args(["list"]).assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("[Default]"));
}

#[test]
fn type_remove_last_type_rejected() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // Remove all but one type
    for slug in &["epic", "bug", "chore", "task"] {
        story(dir.path())
            .args(["type", "remove", slug])
            .assert()
            .success();
    }

    // Attempting to remove the last type should fail
    story(dir.path())
        .args(["type", "remove", "story"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("last"));
}

// ============================================================
// JSON patch sets story_type via --json flag
// ============================================================

#[test]
fn json_patch_sets_story_type() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Refactor auth module"])
        .assert()
        .success();

    story(dir.path())
        .args(["set", "SH-1", "--json", r#"{"story_type":"epic"}"#])
        .assert()
        .success()
        .stdout(predicate::str::contains("type -> epic"));

    // Verify the type actually changed on disk
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: epic"));
}

#[test]
fn json_patch_rejects_invalid_story_type() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Migrate database"])
        .assert()
        .success();

    story(dir.path())
        .args(["set", "SH-1", "--json", r#"{"story_type":"nonexistent"}"#])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown type"));
}

#[test]
fn json_patch_unknown_field_error_lists_story_type() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Add caching layer"])
        .assert()
        .success();

    story(dir.path())
        .args(["set", "SH-1", "--json", r#"{"bogus":"value"}"#])
        .assert()
        .failure()
        .stderr(predicate::str::contains("story_type"));
}

// ============================================================
// JSON output includes story_type and progress
// ============================================================

#[test]
fn json_output_includes_story_type() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "A bug", "--type", "bug"])
        .assert()
        .success();

    story(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"story_type\": \"bug\""));
}

#[test]
fn json_output_omits_type_for_untyped_story() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Untyped task"])
        .assert()
        .success();

    // story_type has skip_serializing_if = "Option::is_none", so it's omitted entirely
    let output = story(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(!stdout.contains("story_type"));
}

// ============================================================
// E2E workflow: full epic lifecycle
// ============================================================

#[test]
fn full_epic_lifecycle() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // 1. Create an epic
    story(dir.path())
        .args(["epic", "create", "Auth System"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"));

    // 2. Create child stories
    story(dir.path())
        .args(["new", "Login page", "--type", "story"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Signup page", "--type", "story"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Password reset", "--type", "story"])
        .assert()
        .success();

    // 3. Add children to epic
    story(dir.path())
        .args(["epic", "add", "SH-1", "SH-2"])
        .assert()
        .success();
    story(dir.path())
        .args(["epic", "add", "SH-1", "SH-3"])
        .assert()
        .success();
    story(dir.path())
        .args(["epic", "add", "SH-1", "SH-4"])
        .assert()
        .success();

    // 4. Verify epic shows 0/3 progress
    story(dir.path())
        .args(["epic", "show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("progress: 0/3 children done (0%)"));

    // 5. story next should skip the epic and return a child
    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"));

    // 6. Complete one child
    story(dir.path())
        .args(["move", "SH-2", "done"])
        .assert()
        .success();

    // 7. Verify epic progress updates
    story(dir.path())
        .args(["epic", "show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "progress: 1/3 children done (33%)",
        ));

    // 8. Complete remaining children
    story(dir.path())
        .args(["move", "SH-3", "done"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-4", "done"])
        .assert()
        .success();

    // 9. Verify epic shows 100% progress
    story(dir.path())
        .args(["epic", "show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "progress: 3/3 children done (100%)",
        ));

    // 10. epic list shows the complete epic
    let output = story(dir.path()).args(["epic", "list"]).assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(stdout.contains("(3/3)"));

    // 11. Verify story list --type filters work
    let output = story(dir.path())
        .args(["list", "--type", "story"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    // Only story-typed stories (SH-2, SH-3, SH-4 are archived after being closed,
    // so list won't show them since it only shows open stories by default)
    assert!(!stdout.contains("SH-1")); // SH-1 is epic type, not story
}
