//! [`ProjectBuilder`] — storyhook project fixtures, from a bare directory with
//! `.storyhook/` in it up to a git repo with a local origin and a dispatched
//! worktree.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use crate::env::TestEnv;
use crate::scratch::scratch_dir_named;

/// Describes a storyhook project fixture; [`ProjectBuilder::build`] makes it
/// real.
///
/// Each step is opt-in because they are not free: `git()` costs a real `git
/// init` and commit, `with_local_origin()` adds a bare repo and two pushes,
/// and most tests need neither.
pub struct ProjectBuilder<'a> {
    env: &'a TestEnv,
    prefix: Option<String>,
    git: bool,
    local_origin: bool,
    legacy: bool,
    local: bool,
    worktrees: Vec<String>,
    stories: Vec<String>,
}

impl<'a> ProjectBuilder<'a> {
    /// A project in `env`: a directory with `story init` run in it, nothing
    /// more.
    pub fn new(env: &'a TestEnv) -> Self {
        ProjectBuilder {
            env,
            prefix: None,
            git: false,
            local_origin: false,
            legacy: false,
            local: false,
            worktrees: Vec::new(),
            stories: Vec::new(),
        }
    }

    /// Passes `--prefix` to `story init`, so ids read `<prefix>-1` rather than
    /// the default `SH-1`.
    pub fn prefix(mut self, prefix: &str) -> Self {
        self.prefix = Some(prefix.to_string());
        self
    }

    /// Makes the project a real git repository (`main`, one commit, a local
    /// `user.name`/`user.email`) rather than a bare directory.
    pub fn git(mut self) -> Self {
        self.git = true;
        self
    }

    /// Adds a *local bare* `origin` and pushes `main` to it.
    ///
    /// Ported from the bash suite's `mk_story_repo`, whose hard-won details all
    /// matter and none of which are obvious:
    ///
    /// - The origin is a bare repo on disk, so the `git fetch` that dispatch
    ///   performs resolves fully offline and deterministically — no network,
    ///   and no credential prompt to hang on.
    /// - `push -u` sets the upstream, so `@{u}` resolves in the fixture.
    /// - `git remote set-head origin main` creates `refs/remotes/origin/HEAD`,
    ///   which `git` does not do for you on push and which anything asking for
    ///   the default branch needs.
    /// - `GIT_TERMINAL_PROMPT=0` is set on every `git` this builder runs, so a
    ///   future fixture regression that points at a *real* origin fails fast
    ///   instead of blocking on a credential prompt.
    ///
    /// Implies [`ProjectBuilder::git`].
    pub fn with_local_origin(mut self) -> Self {
        self.git = true;
        self.local_origin = true;
        self
    }

    /// Adds a linked worktree at `.claude/worktrees/<name>` on branch
    /// `worktree-<name>` — the exact shape `story.sh dispatch` creates, and the
    /// shape under which two concurrent sessions can mint colliding story ids.
    ///
    /// Implies [`ProjectBuilder::git`].
    pub fn worktree(mut self, name: &str) -> Self {
        self.git = true;
        self.worktrees.push(name.to_string());
        self
    }

    /// Creates a story titled `title` once the project exists. Order is
    /// preserved, so the ids are predictable.
    pub fn seed_story(mut self, title: &str) -> Self {
        self.stories.push(title.to_string());
        self
    }

    /// Runs every `story` command this fixture makes **in its own process**,
    /// whatever invoker the run is using.
    ///
    /// For a test about *bytes on disk*, a daemon is not a neutral bystander. It
    /// keeps the database open with its own page cache and its own
    /// write-ahead-log handle, so it answers reads from memory, holds a log that
    /// would otherwise be discarded, and does not notice that the file
    /// underneath it has been replaced. A backup drill, a corruption matrix or a
    /// crash matrix asked through a daemon is asking a different question — and
    /// under `make test-daemon` that is exactly what `build()` would otherwise
    /// arrange, because `story init` runs on the ambient invoker and leaves a
    /// daemon behind for the rest of the fixture's life.
    ///
    /// The second reason is cheaper and just as real: each such fixture is
    /// another live daemon in a test binary, and the daemon leg is bounded at
    /// four threads precisely because that number matters.
    pub fn local(mut self) -> Self {
        self.local = true;
        self
    }

    /// Builds a **legacy** `.storyhook/` tree instead of a project in the
    /// store.
    ///
    /// Two things still read that tree, and `story init` creates neither: the
    /// unmigrated-repository guard, which asks whether one is there, and
    /// `story migrate`, whose entire subject is a tree it is importing. The
    /// tree is therefore written directly, against the layout
    /// [`storyhook::legacy`] reads — see [`crate::legacy_tree`] for why that is
    /// a writer rather than a call into the old entry point.
    ///
    /// The CLI helpers on the resulting [`Project`] — `run`, `json`, `story` —
    /// address the *store*, so on a legacy fixture they answer about a project
    /// that is not there, or refuse with the migrate diagnosis. Drive a legacy
    /// project through `story migrate`, or through whichever component is under
    /// test.
    pub fn legacy(mut self) -> Self {
        self.legacy = true;
        self
    }

    /// Builds the project. Panics — loudly, naming the failing step — rather
    /// than returning a `Result`: a fixture that half-exists produces failures
    /// that read as bugs in the code under test.
    pub fn build(self) -> Project<'a> {
        let dir = scratch_dir_named("project-");
        let root = dir.path().to_path_buf();

        let origin = self.local_origin.then(|| {
            let origin_dir = scratch_dir_named("origin-");
            // The bare repo sits one level down so the fixture exercises an
            // origin path with a parent, exactly as `mk_story_repo` does.
            let bare = origin_dir.path().join("fake/repo.git");
            std::fs::create_dir_all(bare.parent().unwrap()).expect("creating the origin's parent");
            self.run_git(
                &root,
                &["init", "-q", "--bare", "-b", "main", &path_arg(&bare)],
            );
            (origin_dir, bare)
        });

        if self.git {
            self.run_git(&root, &["init", "-q", "-b", "main"]);
            self.run_git(&root, &["config", "user.email", "t@t"]);
            self.run_git(&root, &["config", "user.name", "t"]);
            if let Some((_, bare)) = &origin {
                self.run_git(&root, &["remote", "add", "origin", &path_arg(bare)]);
            }
            std::fs::write(root.join("f"), "a\n").expect("seeding a tracked file");
            self.run_git(&root, &["add", "f"]);
            self.run_git(&root, &["commit", "-qm", "init"]);
            if origin.is_some() {
                self.run_git(&root, &["push", "-qu", "origin", "main"]);
                self.run_git(&root, &["remote", "set-head", "origin", "main"]);
            }
        }

        if self.legacy {
            crate::legacy_tree::init(&root, self.prefix.as_deref());
        } else {
            let mut init = vec!["project", "init"];
            if let Some(prefix) = &self.prefix {
                init.push("--prefix");
                init.push(prefix);
            }
            let mut cmd = self.env.story(&root);
            if self.local {
                cmd.env("STORYHOOK_INVOKER", "local");
            }
            cmd.args(&init)
                .assert()
                .try_success()
                .unwrap_or_else(|e| panic!("`story {}` in the fixture: {e}", init.join(" ")));
        }

        let mut worktrees = BTreeMap::new();
        for name in &self.worktrees {
            let path = root.join(".claude/worktrees").join(name);
            self.run_git(
                &root,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "--no-track",
                    "-b",
                    &format!("worktree-{name}"),
                    &path_arg(&path),
                    "HEAD",
                ],
            );
            worktrees.insert(name.clone(), path);
        }

        let project = Project {
            env: self.env,
            dir,
            origin,
            worktrees,
            legacy: self.legacy,
            local: self.local,
        };
        for title in &self.stories {
            project.new_story(title);
        }
        project
    }

    /// Runs `git <args>` in `cwd` under this environment, asserting success.
    fn run_git(&self, cwd: &Path, args: &[&str]) {
        let mut cmd = Command::new("git");
        cmd.current_dir(cwd);
        self.env.apply(&mut cmd);
        // Defensive: a fixture regression that ever points at a real origin
        // must fail fast rather than block on a credential prompt.
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd.args(args);
        let out = cmd.output().expect("running git");
        assert!(
            out.status.success(),
            "`git {}` in {} failed: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Fixture paths are always UTF-8 (they are minted under `/private/tmp` from
/// an ASCII prefix), so this never fires — but it fires *loudly* rather than
/// silently mangling a path if that ever stops being true.
fn path_arg(path: &Path) -> String {
    path.to_str()
        .unwrap_or_else(|| panic!("fixture path is not UTF-8: {}", path.display()))
        .to_string()
}

/// A built storyhook project fixture. Dropping it removes the project (and its
/// origin, if it has one) from disk.
pub struct Project<'a> {
    env: &'a TestEnv,
    dir: TempDir,
    origin: Option<(TempDir, PathBuf)>,
    worktrees: BTreeMap<String, PathBuf>,
    legacy: bool,
    local: bool,
}

impl<'a> Project<'a> {
    /// The project's root directory.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The environment this project lives in.
    pub fn env(&self) -> &'a TestEnv {
        self.env
    }

    /// The bare origin repository, if [`ProjectBuilder::with_local_origin`] was
    /// used.
    pub fn origin_path(&self) -> Option<&Path> {
        self.origin.as_ref().map(|(_, bare)| bare.as_path())
    }

    /// A linked worktree added by [`ProjectBuilder::worktree`].
    pub fn worktree_path(&self, name: &str) -> &Path {
        self.worktrees
            .get(name)
            .unwrap_or_else(|| panic!("no worktree named {name} in this fixture"))
    }

    /// A `story` command in this project, ready for arguments.
    ///
    /// Forced into this process when the fixture was built with
    /// [`ProjectBuilder::local`] — otherwise a command would go to a daemon the
    /// fixture deliberately does not have.
    pub fn story(&self) -> assert_cmd::Command {
        let mut cmd = self.env.story(self.path());
        if self.local {
            cmd.env("STORYHOOK_INVOKER", "local");
        }
        cmd
    }

    /// Runs `story <args>` in this project and returns the assertion handle —
    /// the caller decides whether success, failure, or particular output is
    /// what the test is about.
    pub fn run(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        self.story().args(args).assert()
    }

    /// Runs `story <args> --json` in this project, asserts it succeeded, and
    /// parses stdout.
    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let out = self
            .story()
            .args(args)
            .arg("--json")
            .output()
            .unwrap_or_else(|e| {
                panic!("running `story {} --json`: {e}", args.join(" "));
            });
        assert!(
            out.status.success(),
            "`story {} --json` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "`story {} --json` did not print JSON ({e}): {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }

    /// Whether this fixture is a legacy `.storyhook/` tree.
    pub fn is_legacy(&self) -> bool {
        self.legacy
    }

    /// Creates a story and returns the id storyhook minted for it.
    pub fn new_story(&self, title: &str) -> String {
        if self.legacy {
            return crate::legacy_tree::new_story(self.path(), title);
        }
        self.json(&["new", title])["story"]["story"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("`story new {title} --json` did not report an id"))
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_project_is_initialized_and_usable() {
        let env = TestEnv::shared();
        let project = env.project().build();

        // The postcondition is that a *project* exists, asked the way anything
        // else would ask: the checkout carries the pointer file naming it, and
        // the store resolves that pointer to a project that can mint an id.
        // It used to be `.storyhook/project.toml exists`, which was a claim
        // about a directory rather than about a tracker.
        assert!(
            project.pointer().is_some(),
            "build() must leave a checkout that names the project it belongs to"
        );
        let store = project.open_store();
        assert!(
            project.try_project_id(&store).is_some(),
            "build() must leave a real, initialized storyhook project behind"
        );
        assert_eq!(project.new_story("first"), "SH-1");
    }

    #[test]
    fn the_prefix_reaches_the_minted_ids() {
        let env = TestEnv::shared();
        let project = env.project().prefix("TST").build();
        assert_eq!(project.new_story("first"), "TST-1");
    }

    #[test]
    fn seeded_stories_exist_in_order() {
        let env = TestEnv::shared();
        let project = env.project().seed_story("one").seed_story("two").build();

        let listed = project.json(&["list"]).to_string();
        assert!(
            listed.contains("one"),
            "seeded stories must exist: {listed}"
        );
        assert!(
            listed.contains("two"),
            "seeded stories must exist: {listed}"
        );
        assert_eq!(
            project.new_story("three"),
            "SH-3",
            "seeding must advance the id counter, not sidestep it"
        );
    }

    #[test]
    fn a_git_project_is_a_real_repository_on_main() {
        let env = TestEnv::shared();
        let project = env.project().git().build();

        let head = git_stdout(project.path(), &["rev-parse", "--abbrev-ref", "HEAD"]);
        assert_eq!(head, "main");
        let log = git_stdout(project.path(), &["log", "--oneline"]);
        assert!(log.contains("init"), "expected the seed commit, got {log}");
    }

    #[test]
    fn a_local_origin_resolves_offline_with_an_upstream_and_a_head() {
        let env = TestEnv::shared();
        let project = env.project().with_local_origin().build();

        // The whole point: `git fetch` completes with no network and no
        // credential prompt, which is what dispatch does on every run.
        git_stdout(project.path(), &["fetch", "origin"]);

        assert_eq!(
            git_stdout(project.path(), &["rev-parse", "--abbrev-ref", "main@{u}"]),
            "origin/main",
            "push -u must have set the upstream"
        );
        assert_eq!(
            git_stdout(
                project.path(),
                &["symbolic-ref", "refs/remotes/origin/HEAD"]
            ),
            "refs/remotes/origin/main",
            "set-head must have created origin/HEAD — git does not do it on push"
        );
        assert!(
            project
                .origin_path()
                .is_some_and(|p| p.join("HEAD").exists()),
            "the origin must be a real bare repository on disk"
        );
    }

    #[test]
    fn a_worktree_is_a_real_linked_worktree_in_the_dispatch_shape() {
        let env = TestEnv::shared();
        let project = env.project().worktree("abc-SH-1").build();
        let worktree = project.worktree_path("abc-SH-1");

        assert_eq!(
            worktree,
            project.path().join(".claude/worktrees/abc-SH-1"),
            "the path must match what `story.sh dispatch` creates"
        );
        assert!(
            worktree.join(".git").is_file(),
            "a linked worktree's .git is a file pointing at the main repo, not a directory"
        );
        assert_eq!(
            git_stdout(worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "worktree-abc-SH-1"
        );
        let listed = git_stdout(project.path(), &["worktree", "list"]);
        assert!(
            listed.contains(".claude/worktrees/abc-SH-1"),
            "git must know about the worktree: {listed}"
        );
    }

    #[test]
    fn projects_are_isolated_from_each_other() {
        let env = TestEnv::shared();
        let a = env.project().build();
        let b = env.project().build();

        assert_ne!(a.path(), b.path());
        assert_eq!(a.new_story("only in a"), "SH-1");
        assert_eq!(
            b.new_story("only in b"),
            "SH-1",
            "two fixtures in one environment must not share a story counter"
        );
    }

    #[test]
    fn a_project_is_removed_when_it_drops() {
        let env = TestEnv::shared();
        let path = {
            let project = env.project().build();
            project.path().to_path_buf()
        };
        assert!(!path.exists(), "a fixture must not outlive its test");
    }

    fn git_stdout(cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(args)
            .output()
            .expect("running git");
        assert!(
            out.status.success(),
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}
