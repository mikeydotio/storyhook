//! One answer for "what is origin's default branch?" across the plugin
//! (`plugins/story/lib/session.sh`'s `default_branch`) and the verifier
//! bundle (`scripts/origin-default-branch.sh`) — SH-691.
//!
//! The two ship separately (the bundle inside the daemon binary, the plugin
//! with a provider) and neither can source the other, so the derivation
//! exists twice. This pins the copies to each other behaviourally, the way
//! `tests/plugin_contract.rs` pins cross-bundle constants: the same fixture
//! remotes, the same stdout, the same success or failure. The binary's own
//! copy (`src/service/cleanup.rs`) is covered by its unit tests.
//!
//! The cases are the ones the retired helper got wrong: a stale local
//! `refs/remotes/origin/HEAD` cache (the answer must be origin's), an origin
//! that advertises no default (unborn or detached HEAD — no `ref:` line at
//! exit 0, which must not become `main`), and no origin at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

struct Fixture {
    _dir: TempDir,
    repo: PathBuf,
    origin: PathBuf,
}

impl Fixture {
    /// A checkout with a real bare origin: `main` pushed, and the local
    /// origin/HEAD cache set to `main` the way `git clone` would.
    fn new() -> Self {
        let dir = scratch_dir();
        let repo = dir.path().join("repo");
        let origin = dir.path().join("origin.git");
        std::fs::create_dir_all(&repo).expect("fixture: creating the checkout");
        git(
            dir.path(),
            &["init", "-q", "--bare", "-b", "main", &path_arg(&origin)],
        );
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("f"), "a\n").expect("fixture: seeding a file");
        git(&repo, &["add", "f"]);
        git(&repo, &["commit", "-qm", "init"]);
        git(&repo, &["remote", "add", "origin", &path_arg(&origin)]);
        git(&repo, &["push", "-q", "-u", "origin", "main"]);
        git(&repo, &["remote", "set-head", "origin", "main"]);
        Self {
            _dir: dir,
            repo,
            origin,
        }
    }

    /// Points origin's advertised HEAD at `branch`, pushing it at `main`'s
    /// commit first when it does not exist yet; the local cache is left alone.
    fn advertise(&self, branch: &str) {
        if branch != "main" {
            git(
                &self.repo,
                &["push", "-q", "origin", &format!("main:refs/heads/{branch}")],
            );
        }
        self.origin_git(&["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")]);
    }

    fn origin_git(&self, args: &[&str]) {
        let origin = path_arg(&self.origin);
        let mut all = vec!["--git-dir", origin.as_str()];
        all.extend_from_slice(args);
        git(&self.repo, &all);
    }

    fn plugin(&self) -> Output {
        let lib = checkout().join("plugins/story/lib/session.sh");
        run(
            &self.repo,
            "bash",
            &[
                "-c",
                &format!("source '{}' && default_branch", lib.display()),
            ],
        )
    }

    fn bundle(&self) -> Output {
        let script = checkout().join("scripts/origin-default-branch.sh");
        run(&self.repo, "bash", &[&script.display().to_string()])
    }

    /// Runs both copies and requires them to agree; returns the shared
    /// (trimmed) stdout and whether they succeeded.
    fn ask_both(&self, case: &str) -> (String, bool) {
        let plugin = self.plugin();
        let bundle = self.bundle();
        assert_eq!(
            plugin.status.success(),
            bundle.status.success(),
            "{case}: the plugin and the bundle must agree on success\nplugin: {}\nbundle: {}",
            describe(&plugin),
            describe(&bundle)
        );
        assert_eq!(
            text(&plugin.stdout),
            text(&bundle.stdout),
            "{case}: the plugin and the bundle must print the same name"
        );
        (text(&plugin.stdout), plugin.status.success())
    }
}

fn path_arg(path: &Path) -> String {
    path.display().to_string()
}

fn run(cwd: &Path, program: &str, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap_or_else(|e| panic!("running {program}: {e}"))
}

fn git(cwd: &Path, args: &[&str]) -> Output {
    let out = run(cwd, "git", args);
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        describe(&out)
    );
    out
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

fn describe(out: &Output) -> String {
    format!(
        "status {:?}, stdout {:?}, stderr {:?}",
        out.status.code(),
        text(&out.stdout),
        text(&out.stderr)
    )
}

#[test]
fn a_stale_local_cache_does_not_outvote_origin() {
    let fx = Fixture::new();
    fx.advertise("dev");
    assert_eq!(
        text(&git(&fx.repo, &["symbolic-ref", "refs/remotes/origin/HEAD"]).stdout),
        "refs/remotes/origin/main",
        "fixture: the local cache still says main"
    );

    let (name, ok) = fx.ask_both("stale cache");
    assert!(ok);
    assert_eq!(name, "dev");
}

#[test]
fn a_slashed_default_branch_name_survives_whole() {
    let fx = Fixture::new();
    fx.advertise("release/1.0");

    let (name, ok) = fx.ask_both("slashed default");
    assert!(ok);
    assert_eq!(name, "release/1.0");
}

#[test]
fn a_detached_origin_head_is_unknown_never_main() {
    let fx = Fixture::new();
    let tip = text(&git(&fx.repo, &["rev-parse", "main"]).stdout);
    fx.origin_git(&["update-ref", "--no-deref", "HEAD", &tip]);

    let (name, ok) = fx.ask_both("detached origin HEAD");
    assert!(!ok, "no symbolic HEAD is not an answer");
    assert_eq!(name, "", "nothing is printed on failure");
    for (which, out) in [("plugin", fx.plugin()), ("bundle", fx.bundle())] {
        let err = text(&out.stderr);
        assert!(err.contains("no symbolic HEAD"), "{which}: {err}");
    }
}

#[test]
fn an_unborn_origin_head_is_unknown_never_main() {
    let fx = Fixture::new();
    fx.origin_git(&["symbolic-ref", "HEAD", "refs/heads/ghost"]);

    let (name, ok) = fx.ask_both("unborn origin HEAD");
    assert!(!ok);
    assert_eq!(name, "");
}

#[test]
fn a_missing_origin_is_unknown_never_main() {
    let fx = Fixture::new();
    git(&fx.repo, &["remote", "remove", "origin"]);

    let (name, ok) = fx.ask_both("no origin");
    assert!(!ok);
    assert_eq!(name, "");
    for (which, out) in [("plugin", fx.plugin()), ("bundle", fx.bundle())] {
        let err = text(&out.stderr);
        assert!(err.contains("did not answer"), "{which}: {err}");
    }
}

#[test]
fn an_unreachable_origin_is_unknown_never_main() {
    let fx = Fixture::new();
    git(
        &fx.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "/nonexistent/storyhook-origin.git",
        ],
    );

    let (name, ok) = fx.ask_both("unreachable origin");
    assert!(!ok);
    assert_eq!(name, "");
}
