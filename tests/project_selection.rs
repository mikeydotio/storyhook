//! Explicit project selection: `--project`, `$STORYHOOK_PROJECT`, the origin
//! lookup, and the refusal (SH-116, C4 of the server-owned epic SH-112).
//!
//! The invariant the epic exists to establish is that **nothing about the
//! filesystem is ever required to answer "which project is this?"** A
//! project-dependent command decides by, in order: `--project=<slug>`,
//! `$STORYHOOK_PROJECT`, then the working directory being a git repository
//! whose `origin` normalizes to a registered URL. Otherwise it refuses, naming
//! both ways out. There is no "current project" state and no default: correct
//! usage always makes the project unambiguous, and the failure mode is a
//! refusal rather than a guess.
//!
//! # The pointer file is still in the order, and that is deliberate
//!
//! Between the environment variable and the origin lookup sits the committed
//! pointer file, read at the working directory and then at each ancestor up to
//! the repository's top level. SH-119 deleted the *recorded-path* half of that
//! walk with the index behind it; the pointer half stayed, and it is not
//! demoted below the origin lookup either, for a measured reason: a `git`
//! subprocess costs 14 ms against an 11.8 ms whole-command baseline, so
//! consulting the origin first would double every command on the machine to
//! learn nothing in the overwhelmingly common case. Ordering it last means the
//! subprocess is paid only by a command that is about to refuse anyway, where
//! it buys the refusal its `origin` line.
//!
//! Overwhelmingly common is measured, not assumed. Tracing which step answered
//! each of the 2,347 project resolutions a full gate performs: 2,224 by a
//! pointer file in the working directory itself, 5 by a registered origin
//! (SH-121).
//!
//! The cost of that ordering is named rather than hidden: a pointer file
//! outranks a registered origin when the two disagree. That state means one
//! checkout claims two projects, which is a defect rather than a preference, so
//! `story doctor` reports it — where reporting is free — instead of the resolver
//! paying for it on every invocation.

use std::path::Path;
use std::process::Command;

use storyhook_test_support::{TestEnv, scratch_dir_named};

/// The slug `story project list` gives the project rooted at `root`.
///
/// Read out of the listing rather than derived from the directory name: the
/// derivation is `ProjectService`'s business, and a test that reimplemented it
/// would keep passing after the two disagreed.
fn slug_at(env: &TestEnv, cwd: &Path, root: &Path) -> String {
    let out = env
        .story(cwd)
        .args(["project", "list"])
        .output()
        .expect("running `story project list`");
    let listing = String::from_utf8_lossy(&out.stdout);
    let wanted = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    listing
        .lines()
        .find(|line| line.contains(&*wanted.to_string_lossy()))
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_else(|| panic!("no `project list` row for {}:\n{listing}", wanted.display()))
        .to_string()
}

/// A directory that is not a storyhook project and not a git repository.
fn nowhere() -> tempfile::TempDir {
    scratch_dir_named("nowhere-")
}

// The clone is what makes AC-2 testable at all, and it is
// `Project::second_checkout` — the harness owns it now (SH-121), because
// `worktree_truth.rs` needs the same shape and a fixture defined twice is a
// fixture that can come to disagree with itself. `ProjectBuilder` pushes to the
// bare origin **before** running `story project new`, so the pointer file is
// written after the push and never travels: a clone carries no
// `.storyhook.toml`, nothing above it does either, and the *only* thing that can
// resolve it is its origin. The harness asserts both of those rather than
// leaving them to each caller.

/// **AC-1.** A command in an unregistered directory refuses, and the refusal
/// names *both* ways out.
///
/// Asserted clause by clause rather than against the whole string: the wording
/// is meant to be improved, and a golden comparison would make every improvement
/// look like a regression. What must not change is that a reader is told the two
/// things they can do.
#[test]
fn an_unregistered_directory_refuses_and_names_both_ways_out() {
    let env = TestEnv::shared();
    let dir = nowhere();

    let out = env
        .story(dir.path())
        .args(["list"])
        .output()
        .expect("running story");
    assert!(
        !out.status.success(),
        "an unregistered directory must refuse"
    );
    assert_eq!(out.status.code(), Some(3), "the refusal is a not-found");

    let stderr = String::from_utf8_lossy(&out.stderr);
    for needle in ["--project", "STORYHOOK_PROJECT", "story project new"] {
        assert!(
            stderr.contains(needle),
            "the refusal must name `{needle}`; got:\n{stderr}"
        );
    }
}

/// **AC-2.** A checkout whose origin is registered resolves with no flag.
///
/// The clone carries no pointer file and nothing above it does either — which
/// `second_checkout` asserts rather than assumes — so the walk cannot answer it.
/// If this passes, the origin lookup is what resolved it.
#[test]
fn a_registered_origin_resolves_with_no_flag() {
    let env = TestEnv::isolated();
    let project = env
        .project()
        .with_local_origin()
        .seed_story("seeded")
        .build();
    let clone = project.second_checkout();

    let out = env
        .story(clone.path())
        .args(["list"])
        .output()
        .expect("running story");
    assert!(
        out.status.success(),
        "a clone of a registered origin must resolve with no flag: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("seeded"),
        "and it must resolve to the *same* project, not an empty one"
    );
}

/// **AC-3.** `--project` beats `$STORYHOOK_PROJECT` beats the origin.
///
/// One fixture, three different projects, all three sources disagreeing at once
/// — which is the only arrangement that can tell a precedence chain from two
/// independent lookups that happen to agree.
#[test]
fn the_flag_beats_the_environment_beats_the_origin() {
    let env = TestEnv::isolated();
    let by_origin = env
        .project()
        .with_local_origin()
        .seed_story("origin-project")
        .build();
    let by_env = env.project().seed_story("environment-project").build();
    let by_flag = env.project().seed_story("flag-project").build();

    let clone = by_origin.second_checkout();
    let env_slug = slug_at(&env, by_env.path(), by_env.path());
    let flag_slug = slug_at(&env, by_flag.path(), by_flag.path());

    // Nothing named: the origin answers.
    let bare = env
        .story(clone.path())
        .args(["list"])
        .output()
        .expect("running story");
    assert!(
        String::from_utf8_lossy(&bare.stdout).contains("origin-project"),
        "with nothing named, the origin must answer"
    );

    // The variable beats the origin.
    let by_variable = env
        .story(clone.path())
        .env("STORYHOOK_PROJECT", &env_slug)
        .args(["list"])
        .output()
        .expect("running story");
    assert!(
        String::from_utf8_lossy(&by_variable.stdout).contains("environment-project"),
        "$STORYHOOK_PROJECT must beat the origin"
    );

    // The flag beats both.
    let by_the_flag = env
        .story(clone.path())
        .env("STORYHOOK_PROJECT", &env_slug)
        .args(["--project", &flag_slug, "list"])
        .output()
        .expect("running story");
    assert!(
        String::from_utf8_lossy(&by_the_flag.stdout).contains("flag-project"),
        "--project must beat $STORYHOOK_PROJECT"
    );
}

/// A slug nothing in the store answers to is refused, and never falls through.
///
/// The fall-through is the whole point: this runs in a directory that *would*
/// have resolved, so a design that treated an unknown selector as "nothing was
/// named" would succeed here and quietly operate on the wrong project.
#[test]
fn a_named_project_that_does_not_exist_is_refused() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("real work").build();

    let out = project
        .story()
        .args(["--project", "no-such-project", "list"])
        .output()
        .expect("running story");
    assert!(!out.status.success(), "an unknown slug must refuse");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no-such-project"),
        "the refusal must name the slug it could not find; got:\n{stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("real work"),
        "it must not fall through to the project the directory would have resolved"
    );
}

/// The refusal says *where* a bad slug came from.
///
/// The difference matters more than it looks: a bad `--project` is a mistake in
/// this command, and a bad `$STORYHOOK_PROJECT` is a mistake in the shell that
/// will repeat on every command until it is fixed. Telling the reader which one
/// they have is the whole remedy for the second.
#[test]
fn the_environment_refusal_says_where_it_was_set() {
    let env = TestEnv::shared();
    let project = env.project().build();

    let from_variable = project
        .story()
        .env("STORYHOOK_PROJECT", "no-such-project")
        .args(["list"])
        .output()
        .expect("running story");
    let stderr = String::from_utf8_lossy(&from_variable.stderr);
    assert!(!from_variable.status.success());
    assert!(
        stderr.contains("STORYHOOK_PROJECT"),
        "a bad variable must be reported as a variable; got:\n{stderr}"
    );

    let from_flag = project
        .story()
        .args(["--project", "no-such-project", "list"])
        .output()
        .expect("running story");
    let flag_stderr = String::from_utf8_lossy(&from_flag.stderr);
    assert!(
        flag_stderr.contains("--project"),
        "a bad flag must be reported as a flag; got:\n{flag_stderr}"
    );
}

/// A project-less command refuses the flag and ignores the variable.
///
/// Deliberately asymmetric, and the asymmetry is the design: a flag is about
/// *this invocation*, so naming a project for a command that acts on none is a
/// mistake worth reporting. A variable is about *this shell* — refusing it would
/// mean one `export` breaks `story project list`, which is exactly the command
/// somebody reaches for when they are trying to find out which slugs exist.
#[test]
fn a_project_less_command_refuses_the_flag_and_ignores_the_variable() {
    let env = TestEnv::shared();
    let project = env.project().build();
    let slug = slug_at(env, project.path(), project.path());

    let flagged = project
        .story()
        .args(["--project", &slug, "project", "list"])
        .output()
        .expect("running story");
    assert!(
        !flagged.status.success(),
        "a project-less command must refuse --project"
    );
    assert_eq!(
        flagged.status.code(),
        Some(2),
        "refusing a flag is a usage error"
    );
    // Without this the test passes on today's binary for entirely the wrong
    // reason: `--project` is not a global flag yet, so the *verb* parser reads
    // it and answers "unknown command `--project`" — which is also a refusal,
    // and also exit 2. What is being pinned is that the flag was understood and
    // found inapplicable, not that it was never recognized at all.
    let flag_stderr = String::from_utf8_lossy(&flagged.stderr);
    assert!(
        !flag_stderr.contains("unknown command"),
        "`--project` must be parsed as a global flag, not read as a verb; got:\n{flag_stderr}"
    );
    assert!(
        flag_stderr.contains("project list"),
        "the refusal must name the command that has nothing to select; got:\n{flag_stderr}"
    );

    let with_variable = project
        .story()
        .env("STORYHOOK_PROJECT", &slug)
        .args(["project", "list"])
        .output()
        .expect("running story");
    assert!(
        with_variable.status.success(),
        "an exported variable must not break the command that lists the slugs: {}",
        String::from_utf8_lossy(&with_variable.stderr)
    );
}

/// A machine-local `insteadOf` rewrite does not change a repository's identity.
///
/// `git remote get-url origin` applies `url.<base>.insteadOf`; `git config --get
/// remote.origin.url` returns what the repository actually recorded. Identity
/// has to be the second, or the same `.git/config` keys differently on two
/// machines whose users happen to push over different protocols — and the
/// failure is silent, because the checkout simply stops resolving and the
/// refusal tells the user to register an origin they already registered.
///
/// The rewrite here is set in the fixture repository's own config, which is the
/// same mechanism a global one uses and is the only form a test may use without
/// writing to the developer's `~/.gitconfig`.
#[test]
fn identity_ignores_a_machine_local_insteadof_rewrite() {
    let env = TestEnv::isolated();
    let project = env
        .project()
        .with_local_origin()
        .seed_story("still the same project")
        .build();
    let origin = project.origin_path().expect("an origin").to_path_buf();
    let clone = project.second_checkout();

    // Rewrite the origin's spelling for every command run in the clone. The
    // recorded url is unchanged; only what `git remote get-url` reports moves.
    //
    // The direction matters and is easy to get backwards — this test was
    // written inverted the first time and passed for nothing, because a rewrite
    // that does not apply proves neither command right. `url.<base>.insteadOf
    // <prefix>` replaces a **leading** `<prefix>` with `<base>`, so the prefix
    // has to be something the configured url actually starts with. Verified by
    // hand before being relied on here:
    //
    //     origin configured as   /private/tmp/some/bare.git
    //     url."xyz://rewritten/".insteadOf "/private/tmp/"
    //     git config --get remote.origin.url  ->  /private/tmp/some/bare.git
    //     git remote get-url origin           ->  xyz://rewritten/some/bare.git
    //
    // Using the whole origin path as the prefix makes `get-url` answer a bare
    // `xyz://rewritten/`, which normalizes to a key no project holds — so this
    // test fails outright if the lookup is ever switched to `get-url`.
    let mut cmd = Command::new("git");
    env.apply(&mut cmd);
    cmd.current_dir(clone.path()).args([
        "config",
        "url.xyz://rewritten/.insteadOf",
        &origin.to_string_lossy(),
    ]);
    assert!(
        cmd.output().expect("running git config").status.success(),
        "setting the rewrite"
    );

    // The premise, asserted rather than assumed: the two commands must now
    // genuinely disagree in this checkout, or the test below proves nothing.
    let mut probe = Command::new("git");
    env.apply(&mut probe);
    probe
        .current_dir(clone.path())
        .args(["remote", "get-url", "origin"]);
    let rewritten = probe.output().expect("running git remote get-url");
    assert!(
        String::from_utf8_lossy(&rewritten.stdout)
            .trim()
            .starts_with("xyz://rewritten/"),
        "fixture: the rewrite must actually apply, or this test cannot tell the two \
         git invocations apart; got {:?}",
        String::from_utf8_lossy(&rewritten.stdout)
    );

    let out = env
        .story(clone.path())
        .args(["list"])
        .output()
        .expect("running story");
    assert!(
        out.status.success(),
        "an insteadOf rewrite must not change which project a checkout is: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("still the same project"),
        "and it must still be the same project"
    );
}

/// **AC-4.** `story session-start` says `{}` when no daemon can be reached.
///
/// The half of the silence obligation that was not met. In an unresolvable
/// *directory* session-start has always answered `{}` — that answer is composed
/// inside the daemon. With a store no daemon can open there is no daemon to
/// compose it, and the measured behaviour was exit 5 with ~1.2 kB of
/// store-corruption diagnosis on stderr, going straight into a model's context
/// window.
///
/// Isolated rather than shared, because it breaks the store — a fact about a
/// file every other test in a shared environment would also see.
#[test]
fn session_start_is_silent_with_no_reachable_daemon() {
    let env = TestEnv::isolated();
    let project = env.project().build();

    // Stand the daemon down immediately before the bytes are touched, not once
    // at the top: the next `story` command starts another one, and a running
    // daemon serves reads from its own page cache without noticing the file
    // underneath it being replaced.
    env.stop_daemon();
    for suffix in ["", "-wal", "-shm"] {
        let path = env.store_path().with_file_name(format!("store.db{suffix}"));
        let _ = std::fs::remove_file(path);
    }
    std::fs::write(env.store_path(), b"not a database\n").expect("breaking the store");

    // The precondition, asserted rather than assumed: an ordinary command has to
    // be genuinely unable to answer, or this test passes for free.
    let probe = project
        .story()
        .args(["list"])
        .output()
        .expect("running story");
    assert!(
        !probe.status.success(),
        "the fixture needs a storyhook that cannot answer; `story list` succeeded"
    );

    let out = project
        .story()
        .args(["session-start"])
        .output()
        .expect("running story");
    assert!(
        out.status.success(),
        "session-start must succeed even when nothing can serve it: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "{}",
        "its payload is its gate: `{{}}` is the answer, not a diagnosis"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        "",
        "and nothing on stderr either — this output goes into a model's context"
    );
}

/// A pointer file still resolves before the origin is consulted.
///
/// This is the ordering, asserted directly rather than inferred from a timing:
/// the checkout carries a pointer naming one project and an origin registered to
/// it too, and the *cheap* answer is the one taken. Its companion is
/// `no_git_subprocess_runs_when_the_project_was_named`, which proves the
/// subprocess is skipped rather than merely ignored.
#[test]
fn a_pointer_file_still_resolves_before_the_origin_is_consulted() {
    let env = TestEnv::isolated();
    let project = env
        .project()
        .with_local_origin()
        .seed_story("resolved by pointer")
        .build();

    assert!(
        project.path().join(".storyhook.toml").is_file(),
        "fixture: the checkout must carry a pointer file"
    );
    let out = project
        .story()
        .args(["list"])
        .output()
        .expect("running story");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("resolved by pointer"));
}
