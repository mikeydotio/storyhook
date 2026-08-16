// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn init_git(dir: &std::path::Path) {
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();
}

#[test]
fn hooks_install_creates_hook_files() {
    let dir = tempdir().unwrap();
    init_git(dir.path());

    story(dir.path())
        .args(["hooks", "install"])
        .assert()
        .success()
        .stdout(predicate::str::contains("post-commit — installed"))
        .stdout(predicate::str::contains("post-merge — installed"))
        .stdout(predicate::str::contains("prepare-commit-msg — installed"));

    assert!(dir.path().join(".git/hooks/post-commit").exists());
    assert!(dir.path().join(".git/hooks/post-merge").exists());
    assert!(dir.path().join(".git/hooks/prepare-commit-msg").exists());
}

#[test]
fn hooks_install_skips_existing_user_hooks() {
    let dir = tempdir().unwrap();
    init_git(dir.path());

    // Create a custom user hook without the storyhook marker
    let hooks_dir = dir.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let custom_content = "#!/bin/sh\necho 'my custom hook'\n";
    std::fs::write(hooks_dir.join("post-commit"), custom_content).unwrap();

    story(dir.path())
        .args(["hooks", "install"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "post-commit — skipped (existing user hook)",
        ));

    // Verify the custom hook was NOT overwritten
    let content = std::fs::read_to_string(hooks_dir.join("post-commit")).unwrap();
    assert_eq!(content, custom_content);
}

#[test]
fn hooks_install_overwrites_storyhook_hooks() {
    let dir = tempdir().unwrap();
    init_git(dir.path());

    // Create a hook with the storyhook marker (old version)
    let hooks_dir = dir.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let old_content = "#!/bin/sh\n# storyhook managed hook -- do not edit this line\necho 'old'\n";
    std::fs::write(hooks_dir.join("post-commit"), old_content).unwrap();

    story(dir.path())
        .args(["hooks", "install"])
        .assert()
        .success()
        .stdout(predicate::str::contains("post-commit — installed"));

    // Verify the content was updated (not the old content)
    let content = std::fs::read_to_string(hooks_dir.join("post-commit")).unwrap();
    assert_ne!(content, old_content);
    assert!(content.contains("storyhook managed hook"));
}

#[test]
fn hooks_uninstall_removes_storyhook_hooks() {
    let dir = tempdir().unwrap();
    init_git(dir.path());

    // Install first
    story(dir.path())
        .args(["hooks", "install"])
        .assert()
        .success();

    assert!(dir.path().join(".git/hooks/post-commit").exists());

    // Uninstall
    story(dir.path())
        .args(["hooks", "uninstall"])
        .assert()
        .success()
        .stdout(predicate::str::contains("post-commit — removed"))
        .stdout(predicate::str::contains("post-merge — removed"))
        .stdout(predicate::str::contains("prepare-commit-msg — removed"));

    assert!(!dir.path().join(".git/hooks/post-commit").exists());
    assert!(!dir.path().join(".git/hooks/post-merge").exists());
    assert!(!dir.path().join(".git/hooks/prepare-commit-msg").exists());
}

#[test]
fn hooks_uninstall_preserves_user_hooks() {
    let dir = tempdir().unwrap();
    init_git(dir.path());

    // Install storyhook hooks
    story(dir.path())
        .args(["hooks", "install"])
        .assert()
        .success();

    // Replace post-commit with a user hook (no marker)
    let hooks_dir = dir.path().join(".git/hooks");
    let user_content = "#!/bin/sh\necho 'user hook'\n";
    std::fs::write(hooks_dir.join("post-commit"), user_content).unwrap();

    // Uninstall
    story(dir.path())
        .args(["hooks", "uninstall"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "post-commit — skipped (not a storyhook hook)",
        ));

    // Verify user hook survived
    let content = std::fs::read_to_string(hooks_dir.join("post-commit")).unwrap();
    assert_eq!(content, user_content);
}

#[test]
fn hooks_install_fails_outside_git_repo() {
    let dir = tempdir().unwrap();
    // Do NOT init git

    story(dir.path())
        .args(["hooks", "install"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not a git repository"));
}

#[test]
fn hooks_uninstall_idempotent() {
    let dir = tempdir().unwrap();
    init_git(dir.path());

    // Uninstall when no hooks exist should succeed
    story(dir.path())
        .args(["hooks", "uninstall"])
        .assert()
        .success()
        .stdout(predicate::str::contains("post-commit — not present"))
        .stdout(predicate::str::contains("post-merge — not present"))
        .stdout(predicate::str::contains("prepare-commit-msg — not present"));
}

/// **Nothing that runs builds a hooks directory by hand** (SH-313, SH-314,
/// SH-320).
///
/// The defect all three stories share is one line: `<root>/.git/hooks`, assumed
/// rather than asked. That assumption is wrong twice over — `core.hooksPath`
/// replaces the directory wholesale, and a linked worktree's `.git` is a file —
/// and in both cases it fails *silently*, writing executables git will never
/// run, or declining to run a sync git will never run either, and reporting
/// success.
///
/// Derived over `git ls-files` rather than a hand-maintained list, in the style
/// of `tests/store_isolation.rs` and `tests/dead_public_surface.rs`. SH-136
/// records three separate drifts of a hand-maintained count in this repository
/// before it stopped being trusted, and SH-198 records ten dead `pub` items
/// accumulating for the same reason: a list someone has to remember to update
/// is the thing that goes stale.
///
/// **The name and the derivation are one claim, and SH-320 is what happens when
/// they disagree.** This was `no_source_file_…` over the pathspec `src/*.rs`,
/// which made it right about its own set and wrong about its advertised class:
/// `plugin/claude-code/hooks/post-git.sh` held the identical assumption for as
/// long as the scan existed, and the scan could not see it. Widening the
/// derivation without renaming would have shipped a test whose name contradicts
/// its own scan — the same defect, held one commit longer.
///
/// What it covers, stated so a reader inherits an honest boundary rather than an
/// overclaim: tracked Rust under `src/`, every tracked `*.sh`, and the
/// extensionless `.githooks/*` chainers. A `*.sh` glob alone misses those four —
/// they are hook scripts, which makes them the last place this class should be
/// allowed to hide — and deriving from the executable bit instead would be worse
/// in the other direction, since `plugin/claude-code/hooks/lib.sh` is mode
/// 100644: sourced, never executed. A fourth mechanism (a Python driver, a
/// Makefile recipe) needs its own glob arm added here; it is not covered by
/// wishing.
///
/// Test code is excluded by directory role, not by name, and the exclusion is
/// load-bearing rather than convenient: building `.git/hooks` in a fixture is
/// the *legitimate* use of the literal — `tests/hook_execution.rs`,
/// `tests/hook_silence.rs`, this file, and
/// `plugin/claude-code/tests/test-post-git-hooks-path.sh` all do it — and a test
/// that installs a hook in the wrong place fails its own assertions, which is a
/// self-policing production code does not get. The escape hatch for a script
/// that genuinely needs the literal is therefore to live under a `tests/`
/// directory, **never** to add an exception here: an exception is the seed of
/// the next stale list.
///
/// The rule is deliberately about *hooks* paths, not about `.git` generally.
/// `service::project::is_repository_top_level` joins `.git` on purpose and
/// wants a directory specifically — that is how it tells a main checkout from a
/// linked worktree — so a blanket ban would have to carve an exception for it.
///
/// Each language gets the spelling its own defect takes. Rust's is a hand-joined
/// `.git` (`join(".git")` + `join("hooks")`). Shell has no `Path::join`, so that
/// pattern has no analogue — but the defect class does, and its shell spelling is
/// `$(git rev-parse --git-dir)/hooks` or the `$GIT_DIR/hooks` git exports into
/// every hook process. Both are SH-314 spelled *correctly*: in a linked worktree
/// `--git-dir` names that worktree's **private** directory, whose `hooks` git
/// never consults, because hooks resolve from the common directory for every
/// worktree. The pattern matches the *construction* rather than the flag, because
/// `--git-dir` has legitimate uses this repository already makes — `install.sh`
/// asks it whether a directory is a repository at all, and
/// `scripts/gate-receipt.sh` wants the private directory deliberately, saying so.
#[test]
fn nothing_that_runs_builds_a_hooks_directory_by_hand() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "-z",
            "--",
            "src/*.rs",
            "*.sh",
            ".githooks/*",
            ":(exclude,glob)**/tests/**",
        ])
        .output()
        .expect("listing tracked sources");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing"
    );
    let files: Vec<&str> = std::str::from_utf8(&listed.stdout)
        .expect("utf-8 paths")
        .split('\0')
        .filter(|p| !p.is_empty())
        .collect();
    assert!(
        files.len() > 140,
        "the scan found only {} source files — it is broken, not the tree",
        files.len()
    );

    for file in files {
        let text = std::fs::read_to_string(root.join(file))
            .unwrap_or_else(|e| panic!("reading {file}: {e}"));
        // Comments only, in whichever syntax the file is written in: this is
        // about what the code does, and every mention of the old path in this
        // tree is prose explaining why it was wrong.
        let marker = if file.ends_with(".rs") { "//" } else { "#" };
        let code: String = text
            .lines()
            .filter(|line| !line.trim_start().starts_with(marker))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !code.contains(".git/hooks"),
            "{file} names `.git/hooks` in code. That is not where git looks when \
             `core.hooksPath` is set, and it does not exist in a linked worktree. \
             Ask git: `rev-parse --git-path hooks` and `--git-common-dir`."
        );
        assert!(
            !(code.contains(r#"join(".git")"#) && code.contains(r#"join("hooks")"#)),
            "{file} builds a hooks directory from a hand-joined `.git`. Ask git \
             instead — `hooks::HookDirs` already does, and two answers to one \
             question is how SH-313 and SH-314 both happened."
        );

        // Quoting and parenthesis noise dropped so one test catches every
        // spelling of the same construction — `$(… --git-dir)/hooks`,
        // `"$(… --git-dir)"/hooks`, `${GIT_DIR}/hooks`. Newlines survive, so two
        // unrelated lines cannot be spliced into a false match.
        let squeezed: String = code
            .chars()
            .filter(|c| !matches!(c, '"' | '\'' | '(' | ')' | '{' | '}'))
            .collect();
        assert!(
            !squeezed.contains("--git-dir/hooks") && !squeezed.contains("$GIT_DIR/hooks"),
            "{file} builds a hooks directory from `--git-dir`/`$GIT_DIR`. In a \
             linked worktree that is the worktree's *private* git directory, \
             whose hooks git never runs — SH-314 spelled correctly. Ask for \
             `--git-common-dir` (the directory storyhook owns) or \
             `--git-path hooks` (the one git will consult)."
        );
    }
}

/// **SH-341's shared merge-arrival shell stays shared** — one copy each of
/// the slack arithmetic and the trailer grammar, and every hook composing it
/// named in its own doc comment.
///
/// The risk this guards is exactly the one sharing the fragment was meant to
/// avoid: a future hook that needs the same shell copies the text instead of
/// calling `merge_arrival_fn!()`, and the slack arithmetic or the trailer
/// alternation drifts between copies in silence.
/// `tests/service_git.rs::every_keyword_the_merge_hook_closes_on_also_claims`
/// only checks the alternation is *present*; this is the uniqueness half.
#[test]
fn the_merge_arrival_fragment_is_shared_not_duplicated() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let hooks_rs =
        std::fs::read_to_string(root.join("src/hooks.rs")).expect("reading src/hooks.rs");

    let slack = "/ 60 + 5";
    assert_eq!(
        hooks_rs.matches(slack).count(),
        1,
        "the daemon-cold-start slack arithmetic (`{slack}`) must appear exactly \
         once in src/hooks.rs — a second, hand-copied occurrence is the drift \
         the shared fragment exists to prevent"
    );

    let alternation = "(closes?|fixes?|resolves?)";
    assert_eq!(
        hooks_rs.matches(alternation).count(),
        1,
        "the trailer alternation (`{alternation}`) must appear exactly once in \
         src/hooks.rs"
    );

    let calls = hooks_rs.matches("merge_arrival_fn!()").count();
    assert_eq!(
        calls, 2,
        "merge_arrival_fn!() is composed into {calls} hook(s); expected exactly \
         2 (post-commit and post-merge). A third caller — or one fewer — means \
         the fragment's own doc comment table needs updating alongside it"
    );
    for consumer in ["| `POST_COMMIT_HOOK`", "| `POST_MERGE_HOOK`"] {
        assert!(
            hooks_rs.contains(consumer),
            "merge_arrival_fn!()'s doc comment must name {consumer} among its \
             known consumers"
        );
    }
}

/// **The class fence for SH-355.** `prepare-commit-msg` used to end on
/// `[ -n "$STORY_ID" ] && { ... }` with nothing after it — an empty backlog
/// left that conditional's own exit status (1) as the script's last command,
/// and `prepare-commit-msg` is the one managed hook whose exit status git
/// actually obeys (`githooks(5)`), so the commit was silently aborted.
/// `post-commit`/`post-merge` were never reachable by that exact mechanism —
/// git ignores their exit codes — but the invariant is stated and checked for
/// all three, so a fourth managed hook, or an edit appended after this line
/// in any of them, cannot reintroduce the trap silently.
///
/// Reads the **installed artifact**, not the Rust source: what git will
/// actually execute is what must be checked. The hook set comes from
/// `story hooks install`'s own output rather than a hand-maintained list, so
/// a fourth managed hook is covered by construction.
#[test]
fn no_managed_hook_lets_its_own_last_statement_decide_gits_verdict() {
    let dir = tempdir().unwrap();
    init_git(dir.path());

    story(dir.path())
        .args(["hooks", "install"])
        .assert()
        .success();

    for name in ["post-commit", "post-merge", "prepare-commit-msg"] {
        let path = dir.path().join(".git/hooks").join(name);
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading the installed {name} hook: {e}"));
        let last = body
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or_else(|| panic!("the installed {name} hook is empty"));
        assert_eq!(
            last, "exit 0",
            "{name}'s last non-blank line must be an unconditional `exit 0`, \
             so nothing appended above it can leak its own status into git's \
             verdict — got {last:?}"
        );
    }
}

/// **The plugin's copy of the hook marker still matches the one storyhook
/// writes** (SH-320).
///
/// `post-git.sh` decides whether git will run the managed `post-commit` hook by
/// grepping it for a marker line, and that literal is the one thing the shell
/// guard still duplicates from `src/hooks.rs` — the location question it used to
/// duplicate is now git's to answer. Edit `HOOK_MARKER` and nothing fails: the
/// plugin simply never recognises a managed hook again and syncs on top of every
/// one of them, forever, in silence.
///
/// That is the *mild* direction — a permanent double sync, idempotent since W6,
/// not the silent miss SH-320 was filed about — which is exactly why it needs a
/// test rather than a reader's vigilance.
///
/// Read as text in one direction rather than through the crate: `HOOK_MARKER` is
/// private, and making a const `pub` so a test can reach it would trade a small
/// duplication for the defect class `tests/dead_public_surface.rs` exists to hunt
/// (SH-198). Asserting `src/hooks.rs` merely *contains* the grepped literal also
/// keeps the plugin free to grep a prefix, which it does — the full marker ends
/// `-- do not edit this line`.
#[test]
fn the_plugin_greps_for_a_marker_storyhook_still_writes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let guard = std::fs::read_to_string(root.join("plugin/claude-code/hooks/post-git.sh"))
        .expect("reading the plugin's post-git hook");

    let grepped = guard
        .split("grep -q \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("post-git.sh no longer greps a quoted literal — has the guard changed shape?");
    assert!(
        grepped.contains("storyhook"),
        "the literal extracted from post-git.sh is `{grepped}`, which does not \
         look like the hook marker — this test is reading the wrong thing"
    );

    let hooks_rs =
        std::fs::read_to_string(root.join("src/hooks.rs")).expect("reading src/hooks.rs");
    assert!(
        hooks_rs.contains(grepped),
        "post-git.sh greps for `{grepped}`, which no longer appears in \
         src/hooks.rs. The plugin would stop recognising every managed hook and \
         sync on top of all of them, silently and forever."
    );
}
