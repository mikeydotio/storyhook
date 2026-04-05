use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn report_text_matches_summary() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();
    story(dir.path())
        .args(["prioritize", "SH-2", "high"])
        .assert()
        .success();

    let summary_output = story(dir.path()).arg("summary").output().unwrap().stdout;
    let report_output = story(dir.path()).arg("report").output().unwrap().stdout;

    assert_eq!(
        String::from_utf8_lossy(&summary_output),
        String::from_utf8_lossy(&report_output)
    );
}

#[test]
fn report_html_basic() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["report", "--html"])
        .assert()
        .success()
        .stdout(predicate::str::contains("<!DOCTYPE html>"))
        .stdout(predicate::str::contains("<html lang=\"en\">"))
        .stdout(predicate::str::contains("</html>"))
        .stdout(predicate::str::contains("Storyhook Report"));
}

#[test]
fn report_html_contains_stories() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Implement login"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Write tests"])
        .assert()
        .success();

    story(dir.path())
        .args(["report", "--html"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Implement login"))
        .stdout(predicate::str::contains("Write tests"))
        .stdout(predicate::str::contains("SH-1"))
        .stdout(predicate::str::contains("SH-2"));
}

#[test]
fn report_html_xss_title() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "<script>alert(1)</script>"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["report", "--html"])
        .output()
        .unwrap()
        .stdout;
    let html = String::from_utf8_lossy(&output);

    assert!(
        html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "title should be HTML-escaped"
    );
    // The raw <script> tag should NOT appear in the HTML body (outside our CSS/structure)
    // Count occurrences of literal <script> - there should be zero
    let body_start = html.find("<body>").unwrap_or(0);
    let body_html = &html[body_start..];
    assert!(
        !body_html.contains("<script>alert(1)</script>"),
        "raw <script> tag must not appear in HTML body"
    );
}

#[test]
fn report_html_xss_comment() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Normal title"])
        .assert()
        .success();
    story(dir.path())
        .args(["comment", "SH-1", "<img onerror=alert(1)>"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["report", "--html"])
        .output()
        .unwrap()
        .stdout;
    let html = String::from_utf8_lossy(&output);

    // The report contains story data; ensure no raw img tags from user input
    assert!(
        !html.contains("<img onerror=alert(1)>"),
        "raw <img> tag must not appear in HTML output"
    );
}

#[test]
fn report_html_xss_label() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Label test"])
        .assert()
        .success();
    story(dir.path())
        .args(["label", "SH-1", "\"><script>"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["report", "--html"])
        .output()
        .unwrap()
        .stdout;
    let html = String::from_utf8_lossy(&output);

    assert!(
        html.contains("&quot;&gt;&lt;script&gt;"),
        "label should be HTML-escaped, got: {}",
        html
    );
    let body_start = html.find("<body>").unwrap_or(0);
    let body_html = &html[body_start..];
    assert!(
        !body_html.contains("\"><script>"),
        "raw label XSS payload must not appear in HTML body"
    );
}

#[test]
fn report_html_empty_project() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["report", "--html"])
        .assert()
        .success()
        .stdout(predicate::str::contains("<!DOCTYPE html>"))
        .stdout(predicate::str::contains("0 stories"))
        .stdout(predicate::str::contains("No stories in this project"));
}

#[test]
fn report_html_shows_priority() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Critical bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Low priority cleanup"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "critical"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-2", "low"])
        .assert()
        .success();

    story(dir.path())
        .args(["report", "--html"])
        .assert()
        .success()
        .stdout(predicate::str::contains("critical"))
        .stdout(predicate::str::contains("low"))
        .stdout(predicate::str::contains("Critical bug"))
        .stdout(predicate::str::contains("Low priority cleanup"));
}

#[test]
fn report_html_shows_type_breakdown() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // Create stories: 1 untyped (Default), 1 bug, 1 story
    // "bug" and "story" are default types from init
    story(dir.path())
        .args(["new", "Untyped task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Fix crash", "--type", "bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Add feature", "--type", "story"])
        .assert()
        .success();

    story(dir.path())
        .args(["report", "--html"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Type Breakdown"))
        .stdout(predicate::str::contains("Default: 1"))
        .stdout(predicate::str::contains("bug: 1"))
        .stdout(predicate::str::contains("story: 1"));
}
