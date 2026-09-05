//! SH-61: `commit-sync` no longer feeds itself.
//!
//! # The loop this file exists to rule out
//!
//! The managed `post-commit` hook runs `story commit-sync`, which appends a
//! `[git]` link comment to every story a new commit names. Under `.storyhook/`
//! that comment dirtied a **version-controlled file**, so recording it required
//! *another* commit — whose message, being a useful one, named the same
//! stories. Scan bodies as well as subjects and that second commit gets linked
//! too, dirtying the file again, requiring a third commit, and so on:
//!
//! ```text
//! commit X mentions SH-1  ->  link comment  ->  SH-1.jsonl dirty
//! commit Y "record the link for SH-1"  ->  link comment  ->  SH-1.jsonl dirty
//! commit Z "record the link for SH-1"  ->  ...
//! ```
//!
//! It terminated before the flip only by accident: the scan read subjects, and
//! the two bookkeeping commits in this repository's history happen to be titled
//! "record commit link(s) from post-commit hook" with the story ids left out.
//! An undocumented human convention was the only thing standing between the
//! feature and a non-terminating loop, which is why SH-61 was filed as a
//! blocker on SH-58 rather than as a comment on it.
//!
//! # Why it cannot happen now, and what is asserted
//!
//! The loop needs fuel: a link comment has to dirty a tracked file. After the
//! flip a story lives in the global store, outside every repository, and a link
//! comment writes nothing into the working tree. There is nothing to commit, so
//! there is no second commit, so there is no second link.
//!
//! SH-61's fix options all assumed the fuel would still be there — skip
//! metadata-only commits, a `Storyhook-Bookkeeping:` trailer, suppress the
//! hook's own commits. None was implemented, and none is needed: the flip
//! removed the fuel rather than adding a guard, which is the fix at the origin
//! rather than at the encounter point.
//!
//! That is an argument. This file is the evidence. It runs `commit-sync`
//! repeatedly over a repository whose commit **body** names a story, and
//! asserts three things after every pass:
//!
//! 1. the story carries exactly **one** link record, never two;
//! 2. runs two and later append **zero** events — not "no visible duplicates",
//!    zero rows;
//! 3. `git status --porcelain` is **byte-empty**, and the commit count has not
//!    moved, so the loop has nothing to feed on.

use std::path::Path;
use std::process::Command;

use storyhook::store::{ProjectId, ReadOps, SqliteStore, Store, StoryNo};
use storyhook_test_support::{ChildGuard, Project, STORY_COMMAND_DEADLINE, TestEnv};

/// How many times the loop is run. Two would prove idempotency; five is cheap
/// and makes "converges after a while" as visible a failure as "never
/// converges".
const PASSES: usize = 5;

/// Runs `git <args>` in `cwd` under the fixture's environment, asserting
/// success and returning stdout.
fn git(env: &TestEnv, cwd: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command.current_dir(cwd);
    env.apply(&mut command);
    command.env("GIT_TERMINAL_PROMPT", "0");
    let output = command.args(args).output().expect("running git");
    assert!(
        output.status.success(),
        "`git {}` in {} failed: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Everything git considers changed, added, or untracked — the exact bytes.
///
/// `--porcelain` because it is the stable, machine-readable form; empty output
/// is the whole assertion, so a format that reflows would make the test lie.
fn status(env: &TestEnv, root: &Path) -> String {
    git(env, root, &["status", "--porcelain"])
}

/// How many events the store holds for one story.
///
/// The count, not the rendered comments: two link records that happened to
/// render identically would still be two rows, and rows are what an
/// append-only log accumulates.
fn event_count(store: &SqliteStore, project: ProjectId, story: StoryNo) -> usize {
    store
        .read(|tx| Ok(tx.events_for(project, story)?.len()))
        .expect("counting events")
}

/// The story's commit links (SH-169: `referenced_by_commits`, not a
/// `[git]`-prefixed comment).
fn link_records(store: &SqliteStore, project: ProjectId, story: StoryNo) -> Vec<String> {
    store
        .read(|tx| {
            Ok(tx
                .story(project, story)?
                .expect("the story is in the read model")
                .snapshot
                .referenced_by_commits
                .into_iter()
                .map(|link| link.sha)
                .collect())
        })
        .expect("reading the story")
}

/// A git repository with a store-backed project, its pointer committed, and
/// the managed hooks installed.
fn repository() -> (&'static TestEnv, Project<'static>) {
    let env = TestEnv::shared();
    let project = env.project().git().build();
    env.story(project.path())
        .args(["hooks", "install"])
        .assert()
        .success();
    // The pointer file is untracked when the builder is done with it. Commit it
    // now, so that "the working tree is clean" is a claim about `commit-sync`
    // rather than about a fixture that was never clean to begin with.
    git(env, project.path(), &["add", "-A"]);
    git(env, project.path(), &["commit", "-qm", "track the tracker"]);
    assert_eq!(
        status(env, project.path()),
        "",
        "fixture: the working tree must start clean"
    );
    (env, project)
}

/// The wave's acceptance test.
#[test]
fn commit_sync_over_a_body_reference_converges_and_writes_nothing_to_the_repository() {
    let (env, project) = repository();
    let id = project.new_story("Referenced from a commit body");

    // The shape SH-58 fixed and SH-61 was afraid of: the reference is in the
    // BODY, so every pass of the scan sees it.
    git(
        env,
        project.path(),
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "feat: the work itself",
            "-m",
            &format!(
                "Part of {id}. Recording the link for {id} would once have \
                      needed another commit, whose message would have named {id} \
                      again."
            ),
        ],
    );

    let store = project.open_store();
    let project_id = project.project_id(&store);
    let story = project.story_no(&store, &id);

    // The post-commit hook has already run one `commit-sync`. Everything below
    // is the loop the hook would drive if a link comment ever dirtied the tree.
    let commits_before = git(env, project.path(), &["rev-list", "--count", "HEAD"]);
    let mut previous = event_count(&store, project_id, story);
    assert_eq!(
        link_records(&store, project_id, story).len(),
        1,
        "the hook's own run must have linked the commit exactly once"
    );

    for pass in 1..=PASSES {
        env.story(project.path())
            .args(["commit-sync", "--since", "1h"])
            .assert()
            .success();

        let events = event_count(&store, project_id, story);
        assert_eq!(
            events,
            previous,
            "pass {pass} appended {} event(s); a re-scan of the same window must \
             append none",
            events.saturating_sub(previous)
        );
        previous = events;

        let links = link_records(&store, project_id, story);
        assert_eq!(
            links.len(),
            1,
            "pass {pass}: exactly one link record, ever. Got {links:?}"
        );

        // The loop's fuel, checked every pass rather than once at the end: if a
        // link comment ever dirties the working tree again, this is where it
        // shows up, one pass after the change that did it.
        assert_eq!(
            status(env, project.path()),
            "",
            "pass {pass} left the working tree dirty. A link comment that writes \
             into the repository is what SH-61 is about: recording it takes a \
             commit, whose message names the same story, which the next scan \
             links, ad infinitum"
        );
    }

    assert_eq!(
        git(env, project.path(), &["rev-list", "--count", "HEAD"]),
        commits_before,
        "no bookkeeping commit may appear: {PASSES} passes of commit-sync must \
         leave the history exactly as they found it"
    );
}

/// The same loop, driven the way it would actually run: through the managed
/// `post-commit` hook, on a commit that names a story in its body.
///
/// The distinction matters. The test above calls `commit-sync` directly; this
/// one proves the hook cannot re-enter itself, which is the mechanism SH-61
/// describes — a commit fires the hook, the hook writes, the write needs a
/// commit, that commit fires the hook.
#[test]
fn a_commit_that_fires_the_hook_produces_nothing_the_hook_would_have_to_record() {
    let (env, project) = repository();
    let id = project.new_story("Named by every commit");

    for pass in 1..=PASSES {
        git(
            env,
            project.path(),
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                &format!("chore: pass {pass}"),
                "-m",
                &format!("Part of {id}"),
            ],
        );
        assert_eq!(
            status(env, project.path()),
            "",
            "pass {pass}: the post-commit hook must leave the working tree clean"
        );
    }

    let store = project.open_store();
    let project_id = project.project_id(&store);
    let story = project.story_no(&store, &id);

    // Each pass is a *different* commit, so each legitimately gets its own link
    // record — five commits, five links. What must not happen is a sixth from
    // a commit nobody wrote.
    assert_eq!(
        link_records(&store, project_id, story).len(),
        PASSES,
        "one link per commit, and no commit the user did not make"
    );
}

/// A story named in a commit's body is linked once even when several passes
/// run concurrently — the hook fires per commit, and nothing serializes two
/// commits made a second apart.
#[test]
fn concurrent_runs_over_one_commit_still_produce_one_link() {
    let (env, project) = repository();
    let id = project.new_story("Referenced once, scanned twice at once");
    git(
        env,
        project.path(),
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "feat: the work",
            "-m",
            &format!("Part of {id}"),
        ],
    );

    let mut children: Vec<ChildGuard> = (0..4)
        .map(|_| {
            ChildGuard::spawn_with_output(env.raw_story(project.path()).args([
                "commit-sync",
                "--since",
                "1h",
            ]))
            .expect("spawning commit-sync")
        })
        .collect();
    for child in &mut children {
        let output = child.wait_with_output_within(STORY_COMMAND_DEADLINE, || {
            "a concurrent commit-sync did not finish".to_string()
        });
        assert!(
            output.status.success(),
            "a concurrent commit-sync failed: {:?}",
            output.status
        );
    }

    let store = project.open_store();
    let project_id = project.project_id(&store);
    let story = project.story_no(&store, &id);
    assert_eq!(
        link_records(&store, project_id, story).len(),
        1,
        "four concurrent scans of one commit must leave one link record"
    );
    assert_eq!(status(env, project.path()), "");
}
