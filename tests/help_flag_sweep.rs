//! `story <verb> --help` prints help — for every verb, without exception.
//!
//! Verbs that take a positional used to read a flag-looking token as data:
//! `story new --help` created a story titled `--help`, burning an id and
//! incrementing `next-id`, leaving a storyhook-tracked repo dirty (SH-52).
//! `--help` is the conventional way to ask what a command does, so the users
//! who hit it are exactly the ones least able to recognize what happened.
//!
//! The sweep is table-driven over the whole verb list because this defect
//! regresses one verb at a time: a new positional-taking verb inherits the
//! bug unless the fix lives ahead of all of them.

// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use std::collections::BTreeMap;
use std::path::Path;

fn story(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn stdout_of(dir: &Path, args: &[&str]) -> String {
    let out = story(dir).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Every verb that reaches a parser, including the aliases and the
/// subcommand forms — `story phase create --help` must behave like `story
/// phase --help`, since the swallowed token sits after the subcommand.
const VERBS: &[&[&str]] = &[
    &["new"],
    &["search"],
    &["phase"],
    &["phase", "create"],
    &["member"],
    &["member", "add"],
    &["import"],
    &["decompose"],
    &["web"],
    &["web", "register"],
    &["web", "deregister"],
    &["init"],
    &["list"],
    &["next"],
    &["summary"],
    &["report"],
    &["export"],
    &["context"],
    &["load-context"],
    &["import-project"],
    &["type"],
    &["type", "add"],
    &["epic"],
    &["epic", "create"],
    &["state"],
    &["state", "add"],
    &["handoff"],
    &["graph"],
    &["doctor"],
    &["hooks"],
    &["scaffold"],
    &["commit-sync"],
    &["sync-git"],
    &["github-sync"],
    &["plugin"],
    &["show"],
    &["comment"],
    &["assign"],
    &["move"],
    &["block"],
    &["unblock"],
    &["prioritize"],
    &["label"],
    &["unlabel"],
    &["reopen"],
    &["delete"],
    &["set"],
    &["relate"],
    &["link"],
    &["unrelate"],
    &["unlink"],
    &["update"],
    &["session-start"],
    &["tui"],
];

/// A project with one story, so an accidental `story new` is visible in
/// `next-id` as well as in the story files.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "A real story"])
        .assert()
        .success();
    dir
}

/// Every file under `.storyhook`, so any mutation at all shows up as a diff.
/// SQLite's sidecar files are excluded: merely reading the archive rewrites
/// them, which says nothing about whether a command changed state.
fn snapshot(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, at: &Path, into: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(at).expect("reading the project tree") {
            let path = entry.expect("reading a directory entry").path();
            if path.is_dir() {
                walk(root, &path, into);
                continue;
            }
            let name = path.to_string_lossy();
            if name.ends_with("-wal") || name.ends_with("-shm") || name.ends_with(".lock") {
                continue;
            }
            let key = path
                .strip_prefix(root)
                .expect("a path under the project root")
                .to_string_lossy()
                .into_owned();
            let bytes = std::fs::read(&path).expect("reading a project file");
            // Text, so a failure reads as a diff a human can act on; the
            // SQLite archive is summarized rather than dumped.
            let value = match std::str::from_utf8(&bytes) {
                Ok(text) => text.to_string(),
                Err(_) => format!("<{} binary bytes>", bytes.len()),
            };
            into.insert(key, value);
        }
    }

    let mut files = BTreeMap::new();
    walk(dir, &dir.join(".storyhook"), &mut files);
    files
}

/// Fails with the paths that changed, and how, rather than with two whole
/// project trees.
fn assert_unchanged(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
    why: &str,
) {
    let mut differences = Vec::new();
    for (path, old) in before {
        match after.get(path) {
            None => differences.push(format!("  deleted: {path}")),
            Some(new) if new != old => differences.push(format!(
                "  changed: {path}\n    before: {old}\n    after:  {new}"
            )),
            Some(_) => {}
        }
    }
    for path in after.keys() {
        if !before.contains_key(path) {
            differences.push(format!("  created: {path}"));
        }
    }
    assert!(differences.is_empty(), "{why}\n{}", differences.join("\n"));
}

/// Every rendering the CLI is allowed to answer a help request with: the
/// general help, or any one of its help topics. Read from the binary itself
/// rather than restated here, so the verb-to-topic mapping can't drift out of
/// sync with a copy in this file.
fn sanctioned_help_texts(dir: &Path) -> Vec<String> {
    let unknown = story(dir).args(["help", "no-such-topic"]).output().unwrap();
    let listing = String::from_utf8_lossy(&unknown.stderr).into_owned();
    let topics = listing
        .split_once("Available: ")
        .expect("the unknown-topic error must list the available topics")
        .1
        .trim()
        .split(", ")
        .map(|topic| topic.trim().to_string())
        .collect::<Vec<_>>();
    assert!(
        topics.len() > 10,
        "expected a real topic list, got {topics:?}"
    );

    let mut texts = vec![stdout_of(dir, &["--help"])];
    texts.extend(topics.iter().map(|topic| stdout_of(dir, &["help", topic])));
    texts
}

#[test]
fn every_verb_answers_a_help_flag_with_help_and_changes_nothing() {
    let dir = project();
    let before = snapshot(dir.path());
    let sanctioned = sanctioned_help_texts(dir.path());

    for verb in VERBS {
        for flag in ["--help", "-h"] {
            let mut args = verb.to_vec();
            args.push(flag);
            let out = story(dir.path()).args(&args).output().unwrap();
            let invocation = format!("story {}", args.join(" "));

            assert_eq!(
                out.status.code(),
                Some(0),
                "`{invocation}` must exit 0; stdout: {}, stderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                out.stderr.is_empty(),
                "`{invocation}` must print help to stdout, not stderr: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            assert!(
                sanctioned.contains(&stdout),
                "`{invocation}` printed something other than help:\n{stdout}"
            );
        }
    }

    assert_unchanged(
        &before,
        &snapshot(dir.path()),
        "a help request must not touch the project: no story created, no id burned, \
         no file rewritten",
    );
}

/// The reported case, asserted precisely: `story new --help` prints exactly
/// what `story help new` prints, and creates nothing.
#[test]
fn new_help_prints_the_new_topic_instead_of_creating_a_story() {
    let dir = project();

    let listed_before = stdout_of(dir.path(), &["list"]);
    let help = stdout_of(dir.path(), &["new", "--help"]);

    assert_eq!(
        help,
        stdout_of(dir.path(), &["help", "new"]),
        "`story new --help` must print the `new` help topic"
    );
    assert_eq!(
        stdout_of(dir.path(), &["list"]),
        listed_before,
        "`story new --help` must not create a story"
    );
    assert!(
        !stdout_of(dir.path(), &["list"]).contains("--help"),
        "no story titled `--help` may exist"
    );
}

/// A help flag is a help request wherever it appears in the verb's argument
/// list, not only immediately after the verb — the swallowing parsers read
/// every position the same way.
#[test]
fn a_help_flag_after_other_arguments_is_still_a_help_request() {
    let dir = project();
    let before = snapshot(dir.path());

    for args in [
        vec!["new", "Some title", "--help"],
        vec!["comment", "SH-1", "--help"],
        vec!["move", "SH-1", "done", "--help"],
        vec!["search", "query", "--help"],
    ] {
        let out = story(dir.path()).args(&args).output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "`story {}` must exit 0, got stderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    assert_unchanged(
        &before,
        &snapshot(dir.path()),
        "none of those requests may change the project",
    );
}
