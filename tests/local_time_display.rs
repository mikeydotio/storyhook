//! Human CLI output shows stored instants in the process's timezone (SH-679);
//! `--json` keeps the stored UTC string.
//!
//! The zone is `TZ` on the *child* `story` process: chrono's `Local` reads it
//! before `/etc/localtime`, and setting it on the child leaves this test
//! runner's own zone alone. Two zones are used: `Asia/Tokyo` (+09:00, no
//! DST) proves conversion and offset, and `UTC` proves the output is then
//! byte-identical to the stored string, which is the escape hatch for anyone
//! who wants the old text back. The rendering under test happens in the CLI
//! process (`src/main.rs`), not the daemon, which is why a per-command `TZ`
//! reaches it at all.

use assert_cmd::Command;
use chrono::{DateTime, FixedOffset, SecondsFormat};
use storyhook::domain::StoryEvent;
use storyhook::store::test_support::inject_events;
use storyhook_test_support::{Project, TestEnv};

/// A stored instant late on the 1st (UTC): the morning of the 2nd in Tokyo.
const STORED: &str = "2026-03-01T23:30:00Z";
const TOKYO: &str = "2026-03-02T08:30:00+09:00";
const COMMENT: &str = "SH-679 pinned comment";

fn story(dir: &std::path::Path, zone: &str) -> Command {
    let mut cmd = TestEnv::shared().story(dir);
    cmd.env("TZ", zone);
    cmd
}

/// A project with one story carrying a comment at [`STORED`].
fn project_with_pinned_comment() -> (Project<'static>, String) {
    let project = TestEnv::shared().project().prefix("LTZ").build();
    let id = project.new_story("Local time story");
    let store = project.open_store();
    let project_id = project.project_id(&store);
    inject_events(
        &store,
        project_id,
        project.story_no(&store, &id),
        &[StoryEvent::StoryCommentAdded {
            at: STORED.to_string(),
            text: COMMENT.to_string(),
        }],
    )
    .expect("inject the pinned comment");
    (project, id)
}

fn stdout(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "story failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn story_show_renders_comments_in_the_process_zone() {
    let (project, id) = project_with_pinned_comment();

    let tokyo = stdout(story(project.path(), "Asia/Tokyo").args(["show", &id]));
    assert!(
        tokyo.contains(&format!("- {TOKYO} {COMMENT}")),
        "expected the Tokyo-local comment line in:\n{tokyo}"
    );
    assert!(
        !tokyo.contains(STORED),
        "the stored UTC string must not leak into Tokyo-local text:\n{tokyo}"
    );

    let utc = stdout(story(project.path(), "UTC").args(["show", &id]));
    assert!(
        utc.contains(&format!("- {STORED} {COMMENT}")),
        "TZ=UTC must reproduce the stored string byte for byte:\n{utc}"
    );
}

#[test]
fn story_show_json_keeps_the_stored_utc_string_whatever_the_zone() {
    let (project, id) = project_with_pinned_comment();

    let json: serde_json::Value = serde_json::from_str(&stdout(
        story(project.path(), "Asia/Tokyo").args(["show", &id, "--json"]),
    ))
    .unwrap();
    let comments = json["story"]["story"]["comments"].as_array().unwrap();
    let pinned = comments
        .iter()
        .find(|c| c["text"] == COMMENT)
        .expect("the pinned comment is in the JSON");
    assert_eq!(pinned["at"], STORED);
    for key in ["created_at", "updated_at"] {
        let value = json["story"]["story"][key].as_str().unwrap();
        assert!(
            value.ends_with('Z'),
            "{key} must stay a UTC wire string, got {value}"
        );
    }
}

#[test]
fn closed_at_renders_in_the_process_zone() {
    let (project, id) = project_with_pinned_comment();
    stdout(story(project.path(), "UTC").args(["close", &id, "done with it"]));

    let shown = stdout(story(project.path(), "Asia/Tokyo").args(["show", &id]));
    let line = shown
        .lines()
        .find(|line| line.starts_with("closed_at: "))
        .unwrap_or_else(|| panic!("no closed_at line in:\n{shown}"));
    assert!(
        line.ends_with("+09:00"),
        "closed_at must carry the Tokyo offset, got: {line}"
    );
    DateTime::parse_from_rfc3339(line.trim_start_matches("closed_at: "))
        .expect("closed_at keeps the RFC3339 grammar");
}

#[test]
fn story_log_renders_the_trail_in_the_process_zone() {
    let (project, id) = project_with_pinned_comment();

    let tokyo = stdout(story(project.path(), "Asia/Tokyo").args(["log", &id]));
    assert!(
        tokyo.lines().any(|line| line.starts_with(TOKYO)),
        "expected a trail row starting with {TOKYO} in:\n{tokyo}"
    );
    let utc = stdout(story(project.path(), "UTC").args(["log", &id]));
    assert!(
        utc.lines().any(|line| line.starts_with(STORED)),
        "expected a trail row starting with {STORED} in:\n{utc}"
    );

    let json: serde_json::Value = serde_json::from_str(&stdout(
        story(project.path(), "Asia/Tokyo").args(["log", &id, "--json"]),
    ))
    .unwrap();
    let entries = json["log"].as_array().expect("log --json has a log array");
    assert!(
        entries.iter().any(|entry| entry["at"] == STORED),
        "log --json must keep the stored UTC string:\n{json}"
    );
}

#[test]
fn the_html_report_dates_its_generation_and_rows_in_the_process_zone() {
    let (project, id) = project_with_pinned_comment();
    let tokyo_zone = FixedOffset::east_opt(9 * 3600).unwrap();

    let json: serde_json::Value = serde_json::from_str(&stdout(
        story(project.path(), "Asia/Tokyo").args(["show", &id, "--json"]),
    ))
    .unwrap();
    let updated_at = json["story"]["story"]["updated_at"].as_str().unwrap();
    let expected_day = DateTime::parse_from_rfc3339(updated_at)
        .unwrap()
        .with_timezone(&tokyo_zone)
        .format("%Y-%m-%d")
        .to_string();

    let html = stdout(story(project.path(), "Asia/Tokyo").args(["report", "--html"]));
    let generated = html
        .split("Generated ")
        .nth(1)
        .and_then(|rest| rest.split(" &middot;").next())
        .unwrap_or_else(|| panic!("no Generated subtitle in:\n{html}"));
    let generated_at = DateTime::parse_from_rfc3339(generated)
        .unwrap_or_else(|e| panic!("Generated {generated:?} is not RFC3339: {e}"));
    assert_eq!(
        generated_at.offset().local_minus_utc(),
        9 * 3600,
        "the report must be generated in the Tokyo offset, got {generated}"
    );
    assert!(
        !html.contains(" UTC"),
        "the subtitle no longer claims UTC:\n{generated}"
    );
    assert!(
        html.contains(&format!("<td class=\"col-date\">{expected_day}</td>")),
        "the Updated column must show the Tokyo date {expected_day} for {updated_at} in:\n{html}"
    );

    let utc_html = stdout(story(project.path(), "UTC").args(["report", "--html"]));
    let utc_generated = utc_html
        .split("Generated ")
        .nth(1)
        .and_then(|rest| rest.split(" &middot;").next())
        .unwrap();
    assert!(
        utc_generated.ends_with('Z'),
        "under TZ=UTC the subtitle ends in Z, got {utc_generated}"
    );
    assert_eq!(
        DateTime::parse_from_rfc3339(utc_generated)
            .unwrap()
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        utc_generated
    );
}
