//! The managed git hooks, **executed**.
//!
//! `tests/hooks.rs` asserts that `story hooks install` writes three files and
//! that it declines to overwrite a user's own. It has never run one of them,
//! and neither had anything else: `POST_MERGE_HOOK` and `POST_COMMIT_HOOK` were
//! `&str` constants in `src/hooks.rs` with no coverage at all.
//!
//! That is the defect class SH-56 belongs to — *hook logic shipped as an
//! untested string literal*. The bug itself was one token (`%s` where `%B`
//! belonged, so a `Closes SH-12` trailer never closed anything), and it
//! plausibly never worked from the day it was written, because nothing
//! executed the script to find out. This file executes it.
//!
//! # What makes these tests real
//!
//! - Every `git` invocation carries [`TestEnv`]'s isolation, so the child
//!   `story` a hook spawns inherits the fixture's data directory *and* a `PATH`
//!   that finds the binary under test. Without the second half a hook in a test
//!   silently exercises the developer's installed build — which is how a
//!   pre-flip `story` came to leave a `.storyhook/lock` inside a post-flip
//!   fixture, and why `TestEnv::apply` sets `PATH` for everything now.
//! - The merges are `--no-ff`, which is what produces an `ORIG_HEAD` and a
//!   merge commit for the hook to look at.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use storyhook_test_support::{TestEnv, scratch_dir};
use tempfile::TempDir;

/// A git date `seconds` in the past, in git's **raw** format (`<epoch> <tz>`).
///
/// `GIT_COMMITTER_DATE` and `GIT_AUTHOR_DATE` parse this form; they reject the
/// approxidate spellings (`3 hours ago`) that `git commit --date` accepts.
fn epoch_ago(seconds: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock at or after the epoch")
        .as_secs();
    format!("{} +0000", now - seconds)
}

/// A git repository with a storyhook project and the managed hooks installed.
struct HookRepo {
    env: &'static TestEnv,
    dir: TempDir,
}

impl HookRepo {
    /// A repository on `main`, with one commit, a project, and the hooks in
    /// place.
    fn new() -> Self {
        let repo = Self::unhooked();
        repo.story(repo.path(), &["hooks", "install"]);
        assert!(
            repo.path().join(".git/hooks/post-merge").is_file(),
            "fixture: the managed hooks must be installed"
        );
        repo
    }

    /// The same repository, stopping **before** `story hooks install`.
    ///
    /// SH-313's cases all turn on what the hook directory looks like at install
    /// time, so they need to arrange it first. Splitting this out is what lets
    /// them share the rest of the fixture — a real repository, a real project,
    /// and a `PATH` that finds the binary under test.
    fn unhooked() -> Self {
        let env = TestEnv::shared();
        let repo = Self {
            env,
            dir: scratch_dir(),
        };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "t@t"]);
        repo.git(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("f"), "a\n").expect("seeding a tracked file");
        repo.git(&["add", "f"]);
        repo.git(&["commit", "-qm", "init"]);

        repo.story(repo.path(), &["project", "new", "--prefix", "SH"]);
        repo
    }

    /// Points `core.hooksPath` at `dir` (relative to the repository), creating
    /// it, and seeds `body` there under **every** managed hook name when one is
    /// given.
    ///
    /// All three, not just the one a test drives. Occupying a single name
    /// leaves the other two free, storyhook installs *those* directly, and they
    /// go on doing their job — which silently confounds any test asking whether
    /// a blocked hook ran. This was not hypothetical: seeding only `post-merge`
    /// made the "a non-delegating occupant means the story does not move"
    /// control fail, because `post-commit` had been installed into the free
    /// name and moved the story through `commit-sync` instead. It is also the
    /// truthful fixture — husky and lefthook generate a directory of hooks, not
    /// one.
    fn hooks_path(&self, dir: &str, occupant: Option<&str>) -> PathBuf {
        let path = self.path().join(dir);
        std::fs::create_dir_all(&path).expect("creating the hooksPath directory");
        self.git(&["config", "core.hooksPath", dir]);
        if let Some(body) = occupant {
            for name in ["post-commit", "post-merge", "prepare-commit-msg"] {
                let hook = path.join(name);
                std::fs::write(&hook, body).expect("seeding an occupant");
                std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
                    .expect("making the occupant executable");
            }
        }
        path
    }

    /// `story <args>`, returning stdout and stderr together whatever the exit
    /// status — for the tests that are about what the command *says*.
    fn story_output(&self, args: &[&str]) -> (bool, String) {
        let out = self
            .env
            .raw_story(self.path())
            .args(args)
            .output()
            .expect("running story");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text)
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Runs `git <args>` in `cwd`, with the fixture's environment and a `PATH`
    /// that finds the binary under test.
    ///
    /// The `PATH` is the load-bearing part. A hook runs `story` by name; with
    /// the ambient `PATH` that is whatever the developer has installed, pointed
    /// at whatever store that build defaults to.
    fn git_in(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        let mut command = Command::new("git");
        command.current_dir(cwd);
        self.env.apply(&mut command);
        command.env("GIT_TERMINAL_PROMPT", "0");
        let output = command.args(args).output().expect("running git");
        assert!(
            output.status.success(),
            "`git {}` in {} failed: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn git(&self, args: &[&str]) -> std::process::Output {
        self.git_in(self.path(), args)
    }

    /// Runs `story <args>` in `cwd`, asserting success.
    fn story(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self
            .env
            .raw_story(cwd)
            .args(args)
            .output()
            .expect("running story");
        assert!(
            output.status.success(),
            "`story {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Creates a story and returns its id.
    fn new_story(&self, title: &str) -> String {
        let out = self.story(self.path(), &["new", title, "--json"]);
        let value: serde_json::Value = serde_json::from_str(&out).expect("`story new --json`");
        value["story"]["story"]["id"]
            .as_str()
            .expect("an id")
            .to_string()
    }

    /// The story's current state.
    fn state_of(&self, id: &str) -> String {
        let out = self.story(self.path(), &["show", id, "--json"]);
        let value: serde_json::Value = serde_json::from_str(&out).expect("`story show --json`");
        value["story"]["story"]["state"]
            .as_str()
            .expect("a state")
            .to_string()
    }

    /// The full SHAs of every commit linked to `id`.
    ///
    /// The link, not the state, is what SH-330's sync is observed by: its
    /// fixtures name a story with a **non-claiming** reference on purpose, so
    /// the trailer scan provably cannot produce the assertion and only the
    /// sync can.
    fn linked_shas(&self, id: &str) -> Vec<String> {
        let out = self.story(self.path(), &["show", id, "--json"]);
        let value: serde_json::Value = serde_json::from_str(&out).expect("`story show --json`");
        value["story"]["story"]["referenced_by_commits"]
            .as_array()
            .map(|commits| {
                commits
                    .iter()
                    .filter_map(|c| c["sha"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Commits on `branch` with **every git hook suppressed**, and returns the
    /// new commit's full SHA.
    ///
    /// `core.hooksPath=/dev/null` is git's own documented way to switch hooks
    /// off wholesale, and it is what makes SH-330's tests mean anything: a
    /// commit made the ordinary way in a hooked repository is linked by
    /// `post-commit` before any merge happens, so a later assertion cannot
    /// distinguish "the merge synced it" from "post-commit already had". This
    /// commit is one **no `post-commit` has ever seen**, which is exactly the
    /// commit that arrives by merge or pull from somewhere else.
    ///
    /// `date`, when given, backdates both the author and committer dates, in
    /// git's **raw** format (`<epoch> <tz>`, e.g. `1755200000 +0000`) — see
    /// [`epoch_ago`]. The environment variables take that form and reject the
    /// approxidate spellings `--date` accepts, which is worth stating because
    /// `GIT_COMMITTER_DATE="3 hours ago"` fails with `invalid date format`
    /// rather than being interpreted.
    ///
    /// Committer date is the load-bearing one — `read_log` filters the scan
    /// window with `git log --since`, which compares against it.
    fn commit_unhooked(&self, message: &str, date: Option<&str>) -> String {
        let mut command = Command::new("git");
        command.current_dir(self.path());
        self.env.apply(&mut command);
        command.env("GIT_TERMINAL_PROMPT", "0");
        if let Some(date) = date {
            command.env("GIT_AUTHOR_DATE", date);
            command.env("GIT_COMMITTER_DATE", date);
        }
        let output = command
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                message,
            ])
            .output()
            .expect("running git");
        assert!(
            output.status.success(),
            "committing with hooks suppressed failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let sha = self.git(&["rev-parse", "HEAD"]);
        String::from_utf8_lossy(&sha.stdout).trim().to_string()
    }

    /// Commits `message` on a new branch and merges it back into `main` with
    /// `--no-ff`, which is what sets `ORIG_HEAD` and fires `post-merge`.
    fn merge_branch_with_message(&self, branch: &str, message: &str) {
        self.git(&["checkout", "-q", "-b", branch]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.git(&["checkout", "-q", "main"]);
        self.git(&[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            &format!("Merge {branch}"),
            branch,
        ]);
    }
}

// ---------------------------------------------------------------------------
// SH-56: the post-merge hook and the body trailer
// ---------------------------------------------------------------------------

/// The bug, exactly as SH-56 reproduces it: a branch commit carrying a
/// `Closes SH-N` **trailer**, merged to main.
#[test]
fn a_body_trailer_closes_its_story_on_merge() {
    let repo = HookRepo::new();
    let id = repo.new_story("Closed by a trailer");
    assert_eq!(repo.state_of(&id), "todo");

    repo.merge_branch_with_message("feature", &format!("fix(x): something\n\nCloses {id}"));

    assert_eq!(
        repo.state_of(&id),
        "done",
        "a `Closes {id}` trailer in the commit BODY must close the story — the \
         hook read subjects, and this project's own commit style puts the \
         reference in the body, so following the style guide guaranteed the \
         feature did nothing"
    );
}

/// A subject-line reference still works. The fix widens the scan; it must not
/// move it.
#[test]
fn a_subject_reference_still_closes_its_story() {
    let repo = HookRepo::new();
    let id = repo.new_story("Closed from the subject");

    repo.merge_branch_with_message("feature", &format!("fix: closes {id}"));

    assert_eq!(repo.state_of(&id), "done");
}

/// One commit naming two stories closes both — SH-56's own "verify" note.
#[test]
fn a_message_naming_two_stories_closes_both() {
    let repo = HookRepo::new();
    let first = repo.new_story("First");
    let second = repo.new_story("Second");

    repo.merge_branch_with_message(
        "feature",
        &format!("feat: the work\n\nCloses {first}\nFixes {second}\n"),
    );

    assert_eq!(repo.state_of(&first), "done");
    assert_eq!(repo.state_of(&second), "done");
}

/// `Resolves` and mixed case, since the pattern claims to accept them.
#[test]
fn the_verb_may_be_resolves_and_the_case_does_not_matter() {
    let repo = HookRepo::new();
    let id = repo.new_story("Resolved");

    repo.merge_branch_with_message("feature", &format!("chore: tidy\n\nRESOLVES {id}"));

    assert_eq!(repo.state_of(&id), "done");
}

/// A bare mention is not a closing reference. Widening `%s` to `%B` exposes far
/// more text to the pattern, so what the pattern *rejects* matters more than it
/// did.
#[test]
fn a_bare_mention_in_the_body_does_not_close_anything() {
    let repo = HookRepo::new();
    let id = repo.new_story("Merely mentioned");

    repo.merge_branch_with_message(
        "feature",
        &format!("refactor: something\n\nThis is groundwork for {id}, which stays open."),
    );

    // `todo`, and since SH-124 that is true of the *transition* as well as the
    // close: the post-commit hook links any mention, but prose naming a story
    // claims nothing, so nothing moves it either.
    assert_eq!(
        repo.state_of(&id),
        "todo",
        "only closes/fixes/resolves close a story, and only a claim moves one; \
         a body full of prose must link it and leave it exactly as it was"
    );
}

/// The branch guard: merging into anything but `main` or `master` does nothing.
#[test]
fn a_merge_on_a_side_branch_closes_nothing() {
    let repo = HookRepo::new();
    let id = repo.new_story("Not on main");

    // Two branches off main, merged into each other rather than into main.
    repo.git(&["checkout", "-q", "-b", "integration"]);
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        &format!("fix: work\n\nCloses {id}"),
    ]);
    repo.git(&["checkout", "-q", "integration"]);
    repo.git(&["merge", "-q", "--no-ff", "-m", "Merge feature", "feature"]);

    // Linked by the post-commit hook, as any commit naming a story is; not
    // closed, because the merge did not land on main.
    assert_eq!(
        repo.state_of(&id),
        "in-progress",
        "the hook only auto-closes on main or master — a merge into a topic \
         branch is not a release"
    );
}

// ---------------------------------------------------------------------------
// SH-330: the post-merge hook links what arrived, as well as closing what the
// merge says it closes
// ---------------------------------------------------------------------------
//
// `post-merge` only ever auto-closed stories named by a trailer. Nothing linked
// the commits a merge *brought*, and the plugin's stand-in skipped on the
// strength of `post-commit` — a hook a merge never runs. So commits arriving by
// merge were linked by nobody.
//
// Every fixture below builds its branch commit with [`commit_unhooked`], so no
// `post-commit` can be credited for the outcome, and names its story with a
// **non-claiming** reference (`Refs`), so the trailer scan cannot be credited
// either. What remains is the sync.

/// The defect itself: a commit that arrives by merge, seen by no `post-commit`,
/// is linked to the story it names.
#[test]
fn a_merge_links_a_commit_no_post_commit_ever_saw() {
    let repo = HookRepo::new();
    let id = repo.new_story("Arrived by merge");

    repo.git(&["checkout", "-q", "-b", "feature"]);
    let sha = repo.commit_unhooked(&format!("feat: the work\n\nRefs {id}"), None);
    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["merge", "-q", "--no-ff", "-m", "Merge feature", "feature"]);

    assert!(
        repo.linked_shas(&id).contains(&sha),
        "a commit arriving by merge must be linked: no post-commit ran for it, \
         and `Refs` claims nothing, so only post-merge's own sync can do this"
    );
}

/// The branch check's placement, pinned in **both** directions at once.
///
/// Linking is a function of commits arriving, whatever branch received them.
/// Closing is a function of a merge reaching a trunk. So the sync sits outside
/// the `main`/`master` guard and the trailer scan stays inside it — and a
/// single merge onto a topic branch is the fixture that can tell.
#[test]
fn a_merge_on_a_side_branch_links_but_does_not_close() {
    let repo = HookRepo::new();
    let id = repo.new_story("Merged onto a topic branch");

    repo.git(&["checkout", "-q", "-b", "integration"]);
    repo.git(&["checkout", "-q", "-b", "feature"]);
    // `Closes`, deliberately: the trailer scan would fire on this were it not
    // for the branch guard, so the story staying open is a real assertion
    // rather than a vacuous one.
    let sha = repo.commit_unhooked(&format!("fix: work\n\nCloses {id}"), None);
    repo.git(&["checkout", "-q", "integration"]);
    repo.git(&["merge", "-q", "--no-ff", "-m", "Merge feature", "feature"]);

    assert!(
        repo.linked_shas(&id).contains(&sha),
        "the sync sits outside the branch guard — a commit that arrived is a \
         commit that arrived, whatever branch received it"
    );
    assert_ne!(
        repo.state_of(&id),
        "done",
        "and the auto-close stays inside it: a merge into a topic branch is \
         not a release"
    );
}

/// The window, and the reason it is derived rather than fixed.
///
/// **This is the mutation-check on the whole of SH-330's window arithmetic.**
/// A merged commit carries the committer date it was originally made with, and
/// `read_log` filters the scan with `git log --since`, which compares against
/// exactly that. So a literal `--since 1h` in the hook — the obvious spelling,
/// and the one the plugin's own stand-in uses — reproduces the defect it was
/// added to fix: the merge runs, the sync runs, and the commit is older than
/// the window, so nothing is linked.
///
/// Three hours is comfortably outside any fixed window anyone would plausibly
/// write, and comfortably inside a window derived from the commit's own age.
#[test]
fn a_merged_commit_older_than_an_hour_is_still_linked() {
    let repo = HookRepo::new();
    let id = repo.new_story("Made three hours ago, merged now");

    repo.git(&["checkout", "-q", "-b", "feature"]);
    let sha = repo.commit_unhooked(
        &format!("feat: older than the window\n\nRefs {id}"),
        Some(&epoch_ago(3 * 60 * 60)),
    );
    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["merge", "-q", "--no-ff", "-m", "Merge feature", "feature"]);

    assert!(
        repo.linked_shas(&id).contains(&sha),
        "the scan window is derived from the oldest commit in ORIG_HEAD..HEAD, \
         not fixed: a merged commit keeps the date it was made, so a constant \
         window misses exactly the case this hook exists for"
    );
}

/// A merge that brings nothing links nothing, and says nothing.
///
/// An empty `ORIG_HEAD..HEAD` means no commit arrived, so there is no window to
/// derive and nothing to scan. The hook must not fall back to a default window
/// — and must not compute one from an empty string either.
#[test]
fn an_empty_merge_range_links_nothing_and_exits_clean() {
    let repo = HookRepo::new();
    let id = repo.new_story("Never arrives");

    // A commit on main that names the story, made with hooks off so nothing
    // links it, and a branch that is already an ancestor of main.
    repo.git(&["checkout", "-q", "-b", "stale"]);
    repo.git(&["checkout", "-q", "main"]);
    let sha = repo.commit_unhooked(&format!("chore: already here\n\nRefs {id}"), None);
    let merge = repo.git(&["merge", "-q", "--no-ff", "-m", "Merge stale", "stale"]);

    assert!(
        merge.status.success(),
        "a merge bringing nothing must still exit clean"
    );
    assert!(
        !repo.linked_shas(&id).contains(&sha),
        "nothing arrived by the merge, so the hook derives no window and scans \
         nothing — the commit already on main is not the merge's to link"
    );
}

// ---------------------------------------------------------------------------
// The post-commit hook, from every place a commit can be made
// ---------------------------------------------------------------------------

/// The post-commit hook runs `commit-sync`, which now scans bodies. This is
/// that path end to end: `git commit` alone links the story.
#[test]
fn the_post_commit_hook_links_a_story_named_in_the_body() {
    let repo = HookRepo::new();
    let id = repo.new_story("Linked by the hook");

    repo.git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "feat: the work",
        "-m",
        &format!("Part of {id}"),
    ]);

    let shown = repo.story(repo.path(), &["show", &id]);
    assert!(
        shown.contains("[git] "),
        "the post-commit hook must have linked the commit: {shown}"
    );
    assert_eq!(
        repo.state_of(&id),
        "todo",
        "`Part of` names a story without claiming it, so the link is all that \
         happens (SH-124)"
    );
}

/// The claiming half of the same path, so the hook's transition is still
/// covered end to end and not merely assumed.
#[test]
fn the_post_commit_hook_moves_a_story_the_body_claims() {
    let repo = HookRepo::new();
    let id = repo.new_story("Claimed by the hook");

    repo.git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "feat: the work",
        "-m",
        &format!("Closes {id}"),
    ]);

    let shown = repo.story(repo.path(), &["show", &id]);
    assert!(
        shown.contains("[git] "),
        "the post-commit hook must have linked the commit: {shown}"
    );
    assert_eq!(
        repo.state_of(&id),
        "in-progress",
        "and moved the story out of the default open state"
    );
}

/// A commit made from a subdirectory reaches the same project.
///
/// The root-resolution family, extended to the hook path: git runs a hook from
/// the repository root, but `story` resolves the project from *its own*
/// working directory, and nothing guarantees the two agree until it is
/// asserted.
#[test]
fn a_commit_from_a_subdirectory_links_the_same_project() {
    let repo = HookRepo::new();
    let id = repo.new_story("Linked from below");
    let deep = repo.path().join("src/inner");
    std::fs::create_dir_all(&deep).expect("creating a subdirectory");

    repo.git_in(
        &deep,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "feat: from a subdirectory",
            "-m",
            &format!("Part of {id}"),
        ],
    );

    let shown = repo.story(repo.path(), &["show", &id]);
    assert!(
        shown.contains("[git] "),
        "a commit made two directories down must link the same project: {shown}"
    );
}

/// A commit made in a **linked worktree** reaches the same project.
///
/// The headline property of the rearchitecture, asked of the hook path.
/// Before the flip each checkout carried its own `.storyhook/`, so a hook
/// firing in a worktree updated a private copy of the tracker and the main
/// checkout never saw it. One store, one project, however many checkouts.
#[test]
fn a_commit_in_a_linked_worktree_links_the_same_project() {
    let repo = HookRepo::new();
    let id = repo.new_story("Linked from a worktree");

    // The pointer file is what a second checkout inherits, so it has to be
    // committed before the worktree is cut — exactly as `worktree_truth.rs`
    // does it.
    repo.git(&["add", ".storyhook.toml"]);
    repo.git(&["commit", "-qm", "track the tracker"]);
    let worktree = repo.path().join(".claude/worktrees/a");
    repo.git(&[
        "worktree",
        "add",
        "-q",
        "--no-track",
        "-b",
        "worktree-a",
        worktree.to_str().expect("a UTF-8 fixture path"),
        "HEAD",
    ]);

    // A linked worktree's `.git` is a file, so the managed hooks live in the
    // main repository's hooks directory and fire for it too.
    repo.git_in(
        &worktree,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "feat: from a worktree",
            "-m",
            &format!("Part of {id}"),
        ],
    );

    let shown = repo.story(repo.path(), &["show", &id]);
    assert!(
        shown.contains("[git] "),
        "a commit made in a linked worktree must reach the same project as one \
         made in the main checkout — two checkouts, one tracker: {shown}"
    );
}

// ---------------------------------------------------------------------------
// SH-313: core.hooksPath, and what "installed" is allowed to mean
// ---------------------------------------------------------------------------
//
// `core.hooksPath` REPLACES the hook directory wholesale — `$GIT_DIR/hooks` is
// not consulted at all, not even as a fallback. `story hooks install` wrote
// there unconditionally and reported success, so for any repository using husky,
// lefthook, pre-commit, or a tracked `.githooks/` it wrote three files git would
// never execute and said it had installed them.
//
// Every test below asserts on a story MOVING, not on a file existing. That is
// the whole point: a file at a path is not evidence git will run it, and the
// old behaviour would have passed any test that only looked for the file.

/// Case 2 — `core.hooksPath` names a directory with no hook of that name. The
/// hook goes *there*, because that is where git looks.
///
/// Before the fix this failed on the last assertion: install reported three
/// hooks installed into `.git/hooks`, and the merge that should have closed the
/// story did nothing at all.
#[test]
fn a_hook_installs_where_core_hookspath_points_and_actually_runs() {
    let repo = HookRepo::unhooked();
    let hooks = repo.hooks_path("team-hooks", None);

    let (ok, output) = repo.story_output(&["hooks", "install"]);
    assert!(
        ok,
        "install must succeed when git has a directory to run: {output}"
    );
    assert!(
        hooks.join("post-merge").is_file(),
        "the hook is not in the directory core.hooksPath names, so git will \
         never run it: {output}"
    );

    let id = repo.new_story("Closed under core.hooksPath");
    repo.merge_branch_with_message("feature", &format!("fix(x): something\n\nCloses {id}"));
    assert_eq!(
        repo.state_of(&id),
        "done",
        "the managed post-merge hook did not run, so `story hooks install` \
         reported success for a file git never executes"
    );
}

/// Case 2, continued — the report must not say a bare "installed" for a
/// directory storyhook does not own.
///
/// The council made this its one blocking condition, and both the CLI-UX and
/// the challenger seat named it independently: the file is untracked, in a
/// directory another tool regenerates and `git clean -fd` deletes, and the
/// unqualified word is the same word SH-313 was filed about.
#[test]
fn installing_outside_the_managed_directory_says_where_and_says_it_is_untracked() {
    let repo = HookRepo::unhooked();
    repo.hooks_path("team-hooks", None);

    let (ok, output) = repo.story_output(&["hooks", "install"]);
    assert!(ok, "{output}");
    assert!(
        output.contains("team-hooks"),
        "the report does not name the directory it wrote to: {output}"
    );
    assert!(
        output.contains("untracked"),
        "the report does not say the hooks it wrote are untracked: {output}"
    );
}

/// Cases 3 and 4 — someone else's hook holds the name git runs.
///
/// Storyhook cannot tell a delegating occupant (this repository's own chainers)
/// from a non-delegating one (husky's) without executing a stranger's script,
/// so it does not try. It writes the managed copy where a delegator would look,
/// leaves the occupant's bytes alone, and states the condition.
#[test]
fn a_foreign_hook_is_never_overwritten_and_the_condition_is_stated() {
    let repo = HookRepo::unhooked();
    let occupant = "#!/bin/sh\n# husky, or anyone else\nexit 0\n";
    let hooks = repo.hooks_path("team-hooks", Some(occupant));

    let (ok, output) = repo.story_output(&["hooks", "install"]);
    assert!(
        ok,
        "a foreign occupant is not a failure of this command: {output}"
    );
    assert_eq!(
        std::fs::read_to_string(hooks.join("post-merge")).expect("reading the occupant"),
        occupant,
        "storyhook overwrote a hook it did not write"
    );
    assert!(
        repo.path().join(".git/hooks/post-merge").is_file(),
        "the managed copy must go where a delegator would look for it: {output}"
    );
    assert!(
        output.contains("delegates") && output.contains("git rev-parse --git-common-dir"),
        "the report must state the condition and print the delegator verbatim, so \
         a paste carries no shell the user had to author: {output}"
    );

    // And the truth of it: a NON-delegating occupant means the story does not
    // move. Without this the two tests above would pass for the wrong reason.
    let id = repo.new_story("Behind a hook that does not delegate");
    repo.merge_branch_with_message("feature", &format!("fix(x): something\n\nCloses {id}"));
    assert_eq!(
        repo.state_of(&id),
        "todo",
        "the occupant does not delegate, so the managed hook cannot have run — \
         if this says `done`, the test is not measuring what it claims"
    );
}

/// Case 4 proper — an occupant that *does* delegate, which is exactly how this
/// repository runs its own managed hooks under SH-306's push gate.
///
/// This is the regression both directions the story suggested would have caused:
/// installing into the effective directory alone would have skipped the name and
/// left `.git/hooks` empty, and refusing outright would have made storyhook's own
/// repository permanently un-installable.
#[test]
fn a_delegating_occupant_still_reaches_the_managed_hook() {
    let repo = HookRepo::unhooked();
    let chainer = "#!/bin/sh\n\
         managed=\"$(git rev-parse --git-common-dir)/hooks/$(basename \"$0\")\"\n\
         [ -x \"$managed\" ] || exit 0\n\
         exec \"$managed\" \"$@\"\n";
    repo.hooks_path("team-hooks", Some(chainer));

    let (ok, output) = repo.story_output(&["hooks", "install"]);
    assert!(ok, "{output}");

    let id = repo.new_story("Closed through a chainer");
    repo.merge_branch_with_message("feature", &format!("fix(x): something\n\nCloses {id}"));
    assert_eq!(
        repo.state_of(&id),
        "done",
        "a chainer delegating to the common directory must still reach the \
         managed hook — this is how storyhook's own repository works: {output}"
    );
}

/// `core.hooksPath` pointing at something that is not a directory means git runs
/// no hooks at all. There is nothing to install and nowhere to install it, so
/// the command fails rather than reporting success.
///
/// Generalised rather than special-cased: `/dev/null` is the deliberate
/// off-switch, but the dangerous member of this class is a typo naming a regular
/// file, which would otherwise get the cheerful "installed" this story is about.
#[test]
fn a_hooks_path_that_is_not_a_directory_is_refused() {
    let repo = HookRepo::unhooked();
    std::fs::write(repo.path().join("not-a-dir"), "").expect("seeding a regular file");
    repo.git(&["config", "core.hooksPath", "not-a-dir"]);

    let (ok, output) = repo.story_output(&["hooks", "install"]);
    assert!(
        !ok,
        "git runs no hooks here, so reporting success is the SH-313 lie: {output}"
    );
    assert!(
        output.contains("not a directory") && output.contains("core.hooksPath"),
        "the refusal must name the cause and the setting: {output}"
    );
}
