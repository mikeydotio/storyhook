//! Who may claim a repository's origin, and who answers for a directory
//! (SH-151, blocking C7 of the server-owned epic SH-112).
//!
//! # The fact this whole file is about
//!
//! `git config --get remote.origin.url` **walks up the directory tree**. Every
//! directory inside one repository — and every worktree of it — reports the same
//! origin. An origin belongs to at most one project (the unique index on
//! `project_remotes.normalized`), so the first project created *anywhere* in a
//! repository used to claim the repository's identity and lock out every
//! sibling.
//!
//! # The rule
//!
//! **Only the directory holding the `.git` that records the remote may claim
//! it.** Concretely, and verified here against eight layouts: the working
//! directory owns its origin exactly when it is its repository's top level
//! *and* its `--git-dir` is its `--git-common-dir` — the second clause being
//! what makes a linked worktree (a `.git` **file**, sharing the main checkout's
//! config) a non-owner, and what keeps a submodule root — which has its own
//! `.git` and its own origin — an owner.
//!
//! Registration is the only thing that asks. **Resolution still walks up**,
//! because a fresh clone's subdirectory must answer for the project that owns
//! its origin; that is safe now precisely because a non-owner can no longer be
//! the project it finds.

use std::path::Path;
use std::process::Command;

use storyhook_test_support::{TestEnv, scratch_dir_named};

/// The URL the fixture repositories record. Never fetched.
const URL: &str = "git@github.com:acme/monorepo.git";

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("running git");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A git repository at its own top level, with `origin` set to `url`.
fn repo_with_origin(url: &str) -> tempfile::TempDir {
    let dir = scratch_dir_named("mono-");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@t"]);
    git(dir.path(), &["config", "user.name", "t"]);
    git(dir.path(), &["remote", "add", "origin", url]);
    git(dir.path(), &["commit", "-qm", "init", "--allow-empty"]);
    dir
}

fn subdir(root: &Path, name: &str) -> std::path::PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).expect("creating the subdirectory");
    path
}

/// `story project new --prefix P` in `cwd`, as an `Output` so a refusal can be
/// read rather than panicked on.
fn project_new(env: &TestEnv, cwd: &Path, prefix: &str) -> std::process::Output {
    env.story(cwd)
        .args(["project", "new", "--prefix", prefix, "--no-agents-md"])
        .output()
        .expect("running `story project new`")
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Every origin `story project list` reports, for the whole store.
fn registered_origins(env: &TestEnv, cwd: &Path) -> Vec<String> {
    let out = env
        .story(cwd)
        .args(["project", "list"])
        .output()
        .expect("running `story project list`");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.trim().strip_prefix("origin    "))
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// The defect SH-151 was filed about
// ---------------------------------------------------------------------------

/// **The repro from the story.** Two projects, one repository, neither of them
/// the repository. Before this rule the first one created took the origin
/// permanently and the second could never register at all.
#[test]
fn two_projects_in_one_repository_claim_no_origin_between_them() {
    let env = TestEnv::isolated();
    let repo = repo_with_origin(URL);
    let a = subdir(repo.path(), "service-a");
    let b = subdir(repo.path(), "service-b");

    project_new(&env, &a, "AA")
        .status
        .success()
        .then_some(())
        .expect("creating a");
    project_new(&env, &b, "BB")
        .status
        .success()
        .then_some(())
        .expect("creating b");

    assert!(
        registered_origins(&env, repo.path()).is_empty(),
        "neither sub-project owns the repository, so neither may claim its origin: {:?}",
        registered_origins(&env, repo.path())
    );
}

/// And the repository itself can still claim it afterwards — which is the point
/// of refusing the children. Under the old rule `service-a` held the origin, so
/// the one directory that genuinely owns it was locked out by its own child.
#[test]
fn the_repository_root_can_still_claim_the_origin_its_children_could_not() {
    let env = TestEnv::isolated();
    let repo = repo_with_origin(URL);
    let a = subdir(repo.path(), "service-a");

    project_new(&env, &a, "AA");
    let out = project_new(&env, repo.path(), "RT");
    assert!(out.status.success(), "{}", stderr(&out));

    assert_eq!(
        registered_origins(&env, repo.path()),
        vec![URL.to_string()],
        "the top level owns the origin and nothing else in the tree may hold it"
    );
}

/// **The wrong answer, not the missing one.** A directory belonging to no
/// project used to answer for whichever sibling had grabbed the enclosing
/// origin. With the claim refused there is nothing for it to resolve to, and a
/// refusal is the honest answer.
#[test]
fn a_directory_belonging_to_no_project_no_longer_answers_for_a_sibling() {
    let env = TestEnv::isolated();
    let repo = repo_with_origin(URL);
    let a = subdir(repo.path(), "service-a");
    project_new(&env, &a, "AA");
    env.story(&a).args(["new", "a story"]).assert().success();

    let elsewhere = subdir(repo.path(), "docs/notes");
    let out = env
        .story(&elsewhere)
        .args(["list"])
        .output()
        .expect("running `story list`");
    assert!(
        !out.status.success(),
        "a directory in no project must refuse, not answer for a sibling: {}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("cannot tell which project"),
        "the refusal must be the one that names both ways out: {}",
        stderr(&out)
    );
}

/// The other half of the same rule: a directory inside a repository whose
/// origin *is* registered still resolves, because resolution keeps walking up.
/// This is the fresh-clone case SH-119's acceptance criteria turn on.
#[test]
fn a_directory_answers_for_the_project_that_owns_its_repositorys_origin() {
    let env = TestEnv::isolated();
    let repo = repo_with_origin(URL);
    project_new(&env, repo.path(), "RT");
    let id = env
        .story(repo.path())
        .args(["new", "a story", "--json"])
        .output()
        .expect("creating a story");
    assert!(id.status.success(), "{}", stderr(&id));

    let deep = subdir(repo.path(), "src/deep/nested");
    let out = env
        .story(&deep)
        .args(["list"])
        .output()
        .expect("running `story list`");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("a story"),
        "a subdirectory resolves by the repository's origin: {}",
        stdout(&out)
    );
}

/// **R3's pin, recorded on SH-119.** A second project in one repository is
/// reachable by its committed pointer file, *while* the repository's own origin
/// belongs to the root project. Deleting the pointer step — which SH-119's
/// acceptance criteria currently ask for — silently turns this into the root
/// project answering for `service-b`.
#[test]
fn a_second_project_in_one_repository_resolves_by_its_pointer() {
    let env = TestEnv::isolated();
    let repo = repo_with_origin(URL);
    project_new(&env, repo.path(), "RT");
    let b = subdir(repo.path(), "service-b");
    project_new(&env, &b, "BB");

    env.story(&b).args(["new", "b story"]).assert().success();
    env.story(repo.path())
        .args(["new", "root story"])
        .assert()
        .success();

    let out = env
        .story(&b)
        .args(["list"])
        .output()
        .expect("running `story list`");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("b story") && !stdout(&out).contains("root story"),
        "the sub-project answers for itself, not for the repository it sits in: {}",
        stdout(&out)
    );
}

// ---------------------------------------------------------------------------
// The layout table — the eight shapes the predicate must get right
// ---------------------------------------------------------------------------

/// A linked worktree shares its main checkout's config, so the origin recorded
/// there is not its to claim — wherever the worktree is placed. This one is
/// outside the repository, which is the placement a "recurse until the parent
/// disagrees" rule gets wrong: its parent reports nothing, so it would look like
/// a second owner of one origin.
#[test]
fn a_linked_worktree_outside_the_tree_claims_nothing() {
    let env = TestEnv::isolated();
    let repo = repo_with_origin(URL);
    let elsewhere = scratch_dir_named("wt-");
    let wt = elsewhere.path().join("checkout");
    git(
        repo.path(),
        &["worktree", "add", "-q", &wt.to_string_lossy(), "-b", "side"],
    );

    project_new(&env, &wt, "WT");
    assert!(
        registered_origins(&env, repo.path()).is_empty(),
        "a linked worktree may not claim the origin its main checkout owns"
    );
}

/// The same rule for the placement this repository itself uses — worktrees
/// under `.claude/worktrees/`, inside the main checkout.
#[test]
fn a_linked_worktree_inside_the_tree_claims_nothing() {
    let env = TestEnv::isolated();
    let repo = repo_with_origin(URL);
    let wt = repo.path().join(".claude/worktrees/side");
    std::fs::create_dir_all(wt.parent().unwrap()).expect("creating the worktree parent");
    git(
        repo.path(),
        &["worktree", "add", "-q", &wt.to_string_lossy(), "-b", "side"],
    );

    project_new(&env, &wt, "WT");
    assert!(
        registered_origins(&env, repo.path()).is_empty(),
        "a worktree inside the checkout is still a worktree"
    );
}

/// **A submodule root owns its own origin**, and this is the case that kills
/// the tempting shortcut: `git worktree list --porcelain` reports the *gitdir*
/// inside a submodule, so a rule written against it would call this directory a
/// non-owner and leave a real repository unable to register the identity it
/// genuinely holds.
#[test]
fn a_submodule_root_claims_its_own_origin() {
    let env = TestEnv::isolated();
    let inner = repo_with_origin("git@github.com:acme/inner.git");
    let outer = repo_with_origin(URL);
    git(
        outer.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &inner.path().to_string_lossy(),
            "vendor",
        ],
    );

    let vendor = outer.path().join("vendor");
    let out = project_new(&env, &vendor, "IN");
    assert!(out.status.success(), "{}", stderr(&out));

    let origins = registered_origins(&env, outer.path());
    assert_eq!(
        origins.len(),
        1,
        "the submodule root owns exactly one origin: {origins:?}"
    );
    assert!(
        origins[0].contains(&*inner.path().to_string_lossy()) || origins[0].contains("inner"),
        "and it is the submodule's own, not the superproject's: {origins:?}"
    );
}

/// The ownership probe must not believe an inherited git environment. With
/// `GIT_DIR` naming another repository, `git config --get remote.origin.url`
/// answers for *that* repository while `git rev-parse --show-toplevel` answers
/// for the working directory — so an unscrubbed check agrees with itself and
/// registers the wrong identity.
#[test]
fn the_ownership_probe_ignores_an_inherited_git_dir() {
    let env = TestEnv::isolated();
    let repo = repo_with_origin(URL);
    let other = repo_with_origin("git@github.com:acme/elsewhere.git");

    let out = env
        .story(repo.path())
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .env("GIT_DIR", other.path().join(".git"))
        .output()
        .expect("running `story project new`");
    assert!(out.status.success(), "{}", stderr(&out));

    assert_eq!(
        registered_origins(&env, repo.path()),
        vec![URL.to_string()],
        "the claim is the directory's own origin, not the one $GIT_DIR points at"
    );
}

// ---------------------------------------------------------------------------
// The refusals
// ---------------------------------------------------------------------------

/// **D4.** A URL the user typed is still subject to the rule, and this is the
/// case the pre-existing "already registered to project X" refusal cannot
/// cover: nobody holds the monorepo's origin yet, so there is no collision —
/// only a permanent, silent theft of the enclosing repository's identity.
#[test]
fn link_origin_refuses_the_enclosing_repositorys_url_from_a_subdirectory() {
    let env = TestEnv::isolated();
    let repo = repo_with_origin(URL);
    let b = subdir(repo.path(), "service-b");
    project_new(&env, &b, "BB");

    let out = env
        .story(&b)
        .args(["project", "link", "origin", URL])
        .output()
        .expect("running `story project link origin`");
    assert!(
        !out.status.success(),
        "claiming the enclosing repository's origin must refuse: {}",
        stdout(&out)
    );
    let message = stderr(&out);
    assert!(
        message.contains(&*repo.path().canonicalize().unwrap().to_string_lossy()),
        "the refusal must name the directory that does own it: {message}"
    );
    assert!(
        registered_origins(&env, repo.path()).is_empty(),
        "and it must write nothing"
    );
}

/// **D6(i), the SH-116 deviation withdrawn.** `project new` used to *skip* a
/// collision silently, because refusing it would have made a monorepo's second
/// project impossible. Ownership removes that reason: a collision now means a
/// genuinely new project trying to take an origin another project already
/// holds, which is a duplicate identity and is worth refusing.
#[test]
fn project_new_refuses_an_origin_another_project_already_holds() {
    let env = TestEnv::isolated();
    let first = repo_with_origin(URL);
    project_new(&env, first.path(), "FI");

    // A second clone of the same repository — no pointer file, because it was
    // never committed, so nothing short-circuits on identity.
    let second = repo_with_origin(URL);
    let out = project_new(&env, second.path(), "SE");

    assert!(
        !out.status.success(),
        "a second project may not take an origin the first holds: {}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("already registered"),
        "the refusal must name the holder: {}",
        stderr(&out)
    );
    assert_eq!(
        registered_origins(&env, first.path()),
        vec![URL.to_string()],
        "and the holder keeps it"
    );
}

/// **D5.** A checkout carrying a pointer to a project this store does not have,
/// sitting inside a repository whose origin belongs to *another* project, must
/// say so rather than quietly answering as the enclosing project. This is the
/// fresh-clone-of-a-monorepo shape: `service-b`'s identity travelled in the
/// commit, the store has never seen it, and the root project's origin is right
/// there to be resolved by mistake.
#[test]
fn a_pointer_naming_an_unknown_project_refuses_rather_than_resolving_by_origin() {
    let env = TestEnv::isolated();
    let repo = repo_with_origin(URL);
    project_new(&env, repo.path(), "RT");
    let b = subdir(repo.path(), "service-b");
    std::fs::write(
        b.join(".storyhook.toml"),
        "schema = 1\nuuid = \"11111111-2222-3333-4444-555555555555\"\nprefix = \"BB\"\n",
    )
    .expect("writing the stale pointer");

    let out = env
        .story(&b)
        .args(["list"])
        .output()
        .expect("running `story list`");
    assert!(
        !out.status.success(),
        "a checkout claiming a project this store lacks must not answer as its neighbour: {}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("11111111-2222-3333-4444-555555555555"),
        "the refusal must name the project the pointer claims: {}",
        stderr(&out)
    );
}
