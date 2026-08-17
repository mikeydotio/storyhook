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

use assert_cmd::Command;
use std::collections::BTreeMap;
use std::path::Path;

use storyhook::store::{ReadOps, SqliteStore, Store, StoryQuery};
use storyhook_test_support::{Project, TestEnv, project_id_at};

/// A `story` command in this fixture, carrying the fixture's isolated
/// environment.
///
/// Built from the [`Project`] rather than from a bare path: once story data
/// lives in a global store, a command that inherits the ambient environment
/// reads a *different* database from the one the fixture was created in — and,
/// outside a test harness, the developer's real one.
fn story(project: &Project<'_>) -> Command {
    project.story()
}

fn stdout_of(project: &Project<'_>, args: &[&str]) -> String {
    let out = story(project).args(args).output().unwrap();
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
    &["project"],
    &["project", "new", "--prefix", "SH"],
    &["project", "list"],
    &["project", "settings"],
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
    // `store new` takes a path in the position `new` takes a title, which is
    // the shape SH-52 was: a help request read as data. Here it would create a
    // whole database called `--help`.
    &["store"],
    &["store", "new"],
    &["attachment"],
    &["attachment", "add"],
    &["attachment", "list"],
];

/// A project with one story, so an accidental `story new` is visible in
/// `next-id` as well as in the story files.
fn project() -> Project<'static> {
    TestEnv::shared()
        .project()
        .seed_story("A real story")
        .build()
}

/// A fingerprint of **every piece of mutable state a command could touch**,
/// on either storage model.
///
/// This is the whole power of `every_verb_answers_a_help_flag_with_help_and_
/// changes_nothing`: SH-52 was a help request that quietly created a story and
/// burned an id, and the only way to catch that class rather than the one
/// reported instance is to compare *all* the state before and after.
///
/// Two halves, and both are taken every time:
///
/// * **The legacy tree** — every file under `.storyhook`, as before. Empty for
///   a project that lives in the store, which is why it cannot be the only
///   half.
/// * **The store** — the project's row and its stories. `next_story_no` is in
///   there deliberately: it is the counter SH-52 burned, and a story created
///   and rolled back would be invisible without it.
///
/// **A green result here means nothing unless the fingerprint is watching
/// something**, so `the_fingerprint_notices_a_real_change` proves it moves when
/// a story is created. Without that, an empty fingerprint over a directory that
/// no longer exists would re-open SH-52 with a passing test.
fn snapshot(project: &Project<'_>) -> BTreeMap<String, String> {
    fn walk(root: &Path, at: &Path, into: &mut BTreeMap<String, String>) {
        let Ok(entries) = std::fs::read_dir(at) else {
            return;
        };
        for entry in entries {
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

    let mut state = BTreeMap::new();
    walk(
        project.path(),
        &project.path().join(".storyhook"),
        &mut state,
    );
    state.extend(store_state(project));
    state
}

/// The store half of [`snapshot`], scoped to this project's own rows.
///
/// Scoped, not global: the store is shared by every fixture in this binary, so
/// a sibling test creating a story would otherwise read as a change this test's
/// help sweep made.
fn store_state(project: &Project<'_>) -> BTreeMap<String, String> {
    let store: SqliteStore = project.open_store();
    let Some(id) = project_id_at(&store, project.path()) else {
        return BTreeMap::new();
    };
    store
        .read(|tx| {
            let mut state = BTreeMap::new();
            let record = tx.project(id)?.expect("the project exists");
            state.insert(
                "store/next_story_no".to_string(),
                record.next_story_no.to_string(),
            );
            state.insert("store/prefix".to_string(), record.prefix.clone());
            state.insert("store/states".to_string(), format!("{:?}", tx.states(id)?));
            state.insert("store/types".to_string(), format!("{:?}", tx.types(id)?));
            state.insert(
                "store/members".to_string(),
                format!("{:?}", tx.members(id)?),
            );
            for row in tx.stories(id, &StoryQuery::all())? {
                state.insert(
                    format!("store/story/{}", row.story_no.get()),
                    format!(
                        "head={} {:?}",
                        row.head_seq.get(),
                        serde_json::to_string(&row.snapshot).unwrap_or_default()
                    ),
                );
            }
            Ok(state)
        })
        .expect("reading the store")
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
fn sanctioned_help_texts(project: &Project<'_>) -> Vec<String> {
    let unknown = story(project)
        .args(["help", "no-such-topic"])
        .output()
        .unwrap();
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

    let mut texts = vec![stdout_of(project, &["--help"])];
    texts.extend(
        topics
            .iter()
            .map(|topic| stdout_of(project, &["help", topic])),
    );
    texts
}

#[test]
fn every_verb_answers_a_help_flag_with_help_and_changes_nothing() {
    let dir = project();
    let before = snapshot(&dir);
    let sanctioned = sanctioned_help_texts(&dir);

    for verb in VERBS {
        for flag in ["--help", "-h"] {
            let mut args = verb.to_vec();
            args.push(flag);
            let out = story(&dir).args(&args).output().unwrap();
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
        &snapshot(&dir),
        "a help request must not touch the project: no story created, no id burned, \
         no file rewritten",
    );
}

/// The reported case, asserted precisely: `story new --help` prints exactly
/// what `story help new` prints, and creates nothing.
#[test]
fn new_help_prints_the_new_topic_instead_of_creating_a_story() {
    let dir = project();

    let listed_before = stdout_of(&dir, &["list"]);
    let help = stdout_of(&dir, &["new", "--help"]);

    assert_eq!(
        help,
        stdout_of(&dir, &["help", "new"]),
        "`story new --help` must print the `new` help topic"
    );
    assert_eq!(
        stdout_of(&dir, &["list"]),
        listed_before,
        "`story new --help` must not create a story"
    );
    assert!(
        !stdout_of(&dir, &["list"]).contains("--help"),
        "no story titled `--help` may exist"
    );
}

/// A help flag is a help request wherever it appears in the verb's argument
/// list, not only immediately after the verb — the swallowing parsers read
/// every position the same way.
#[test]
fn a_help_flag_after_other_arguments_is_still_a_help_request() {
    let dir = project();
    let before = snapshot(&dir);

    for args in [
        vec!["new", "Some title", "--help"],
        vec!["comment", "SH-1", "--help"],
        vec!["move", "SH-1", "done", "--help"],
        vec!["search", "query", "--help"],
    ] {
        let out = story(&dir).args(&args).output().unwrap();
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
        &snapshot(&dir),
        "none of those requests may change the project",
    );
}

/// The fingerprint is not vacuous.
///
/// `every_verb_answers_a_help_flag_with_help_and_changes_nothing` is only worth
/// anything if [`snapshot`] would *notice*. Before the flip the fingerprint
/// walked `.storyhook/`; after it, that directory does not exist, and a
/// fingerprint over a directory that is not there is an empty map that compares
/// equal to itself forever — a green test that has stopped testing, and SH-52
/// reopened without a single failing assertion.
///
/// So this asserts the thing the sweep depends on: creating a story moves the
/// fingerprint, and it moves in the two places SH-52 damaged — a new story row
/// and an advanced id counter.
#[test]
fn the_fingerprint_notices_a_real_change() {
    let dir = project();
    let before = snapshot(&dir);
    assert!(
        !before.is_empty(),
        "the fingerprint must be watching something: {before:?}"
    );

    story(&dir)
        .args(["new", "A story the sweep did not ask for"])
        .assert()
        .success();
    let after = snapshot(&dir);

    assert_ne!(before, after, "creating a story must move the fingerprint");

    // The id counter, wherever this build keeps it: `.storyhook/next-id` under
    // the legacy tree, `store/next_story_no` in the store. One of the two is
    // always present, and burning an id is what SH-52 did — a fingerprint that
    // did not include the counter would miss the defect it exists to catch.
    let counters = ["store/next_story_no", ".storyhook/next-id"];
    let counter_moved = counters
        .iter()
        .any(|key| before.contains_key(*key) && before.get(*key) != after.get(*key));
    assert!(
        counter_moved,
        "the fingerprint must include the id counter and see it advance; \
         {counters:?} read {:?} before and {:?} after",
        counters.map(|key| before.get(key)),
        counters.map(|key| after.get(key)),
    );

    let new_keys: Vec<&String> = after
        .keys()
        .filter(|key| !before.contains_key(*key))
        .collect();
    assert!(
        !new_keys.is_empty(),
        "the new story must appear in the fingerprint as state that was not there before"
    );
}
