//! `scripts/merge-preflight.sh`, **provoked** — not inspected.
//!
//! SH-396. `main` was red for 73 minutes because PR #484 merged two
//! independently green branches — SH-315's attachment CLI and an exhaustive
//! `Invocation` match on the other side —
//! into a tree that failed to compile. Zero textual conflict, so nothing
//! flagged it: a new match variant on one side, no arm added on the other.
//! `.githooks/pre-push` only ever certifies the tip tree of a *pushed* ref,
//! and `gh pr merge --merge` is a server-side merge that never pushes, so
//! that gate had no way to see this coming. Measured over the last 30 merges
//! into `main`: 14 produced a tree matching neither parent — content no
//! receipt could possibly have covered.
//!
//! `merge-preflight.sh` closes that gap by asking the exact question before
//! a merge happens: does the tree this merge WOULD produce already carry a
//! `make test` receipt? These tests drive it against **real git** the same
//! way `tests/push_gate.rs` drives the push gate — real repositories, real
//! branches, and receipts written by the **production writer**
//! (`scripts/gate-receipt.sh`), never hand-forged. A hand-written receipt
//! would prove the checker's file format, not the producer's behaviour — the
//! same anti-vacuity control SH-297's council required for the push gate.
//!
//! # The load-bearing correctness claim
//!
//! `git merge-tree --write-tree` must compute the SAME tree a real `git
//! merge` of the same two parents would produce, or a receipt written for
//! one would never satisfy a lookup for the other.
//! `the_predicted_tree_matches_a_real_merges_tree_exactly` pins this by
//! performing both and comparing tree oids, not trusting the claim.
//! Reconstructing the actual SH-396 incident (by hand, once, recorded on the
//! story rather than as a fixture here — the two commits in question are
//! `main`'s tip before PR #484 and SH-315's branch tip) reproduces the
//! broken merge's tree byte-for-byte, confirming this would have caught the
//! real defect.
//!
//! # Mutation-checked (SH-295: a pin that cannot fail is not a pin)
//!
//! Run by hand against this suite before it was committed:
//!
//! - the receipt lookup in `merge-preflight.sh` pointed at a nonexistent
//!   directory instead of the real receipt store → **3 of 7 red**,
//!   `certifying_the_predicted_tree_through_the_production_writer_clears_it`,
//!   `a_branch_that_already_contains_main_is_certified_via_its_own_receipt`
//!   and `a_new_commit_after_certification_produces_an_uncertified_tree_again`
//!   — precisely the tests whose assertions depend on a receipt actually
//!   being found (the third checks that the *first* tree stays certified
//!   after a new commit, which also needs the lookup to succeed).
//! - the exit-status check on `git merge-tree` deleted, so a conflict's
//!   "virtual" tree (conflict markers baked in) was treated as a real result
//!   → **1 of 7 red**, `a_textual_conflict_is_reported_distinctly_and_prints_no_tree`
//!   — the only test asserting on the conflict path specifically.
//!
//! # The merge-watch boundary
//!
//! `scripts/merge-watch.sh` keeps its GitHub polling and comment orchestration
//! outside this suite: mocking `gh` would validate the mock rather than the
//! integration. SH-514 extracted its private `--speculative-run` core for the
//! opposite reason: object ownership, the detached checkout, the gate-command
//! environment, poller restoration, and signal cleanup are all real local Git
//! behaviour. Those are provoked below without a GitHub imitation, following
//! the same private-core pattern as `land-pr.sh --certified-run`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use std::os::unix::process::{CommandExt, ExitStatusExt};

use storyhook_test_support::{ChildGuard, scratch_dir};
use tempfile::TempDir;

/// The checkout under test — the tracked scripts and hooks live here.
fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// A repository with `main` at one commit, ready to grow diverging branches.
struct MergeRepo {
    dir: TempDir,
}

impl MergeRepo {
    fn new() -> Self {
        let repo = Self { dir: scratch_dir() };

        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "t@t"]);
        repo.git(&["config", "user.name", "t"]);

        // The tracked hooks directory, symlinked rather than copied, so
        // `gate-receipt.sh preflight`'s own executable-hook check passes the
        // same way it does in a real checkout — needed because certifying a
        // tree here goes through the real enrol-then-postlude path.
        std::os::unix::fs::symlink(checkout().join(".githooks"), repo.path().join(".githooks"))
            .expect("fixture: linking the tracked hooks directory");

        repo.write("f", "base\n");
        repo.git(&["add", "f"]);
        repo.git(&["commit", "-qm", "init"]);
        repo
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.path().join(name), body).expect("fixture: writing a tracked file");
    }

    fn git(&self, args: &[&str]) -> Output {
        run(self.path(), "git", args)
    }

    fn rev_parse(&self, rev: &str) -> String {
        let out = self.git(&["rev-parse", rev]);
        assert_ok(&out, &format!("rev-parse {rev}"));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn tree_of(&self, rev: &str) -> String {
        self.rev_parse(&format!("{rev}^{{tree}}"))
    }

    fn common_dir(&self) -> PathBuf {
        self.path().join(".git")
    }

    /// A new branch off `from`, with one commit writing `name`/`body`.
    fn branch(&self, name: &str, from: &str, file: &str, body: &str) -> String {
        assert_ok(
            &self.git(&["checkout", "-q", "-b", name, from]),
            &format!("branching {name} from {from}"),
        );
        self.write(file, body);
        self.git(&["add", file]);
        let message = format!("{name}: write {file}");
        assert_ok(
            &self.git(&["commit", "-qm", &message]),
            "committing on the new branch",
        );
        self.rev_parse("HEAD")
    }

    /// Runs the script under test.
    fn preflight(&self, base: &str, head: &str) -> Output {
        run(
            self.path(),
            "bash",
            &[
                &checkout()
                    .join("scripts/merge-preflight.sh")
                    .display()
                    .to_string(),
                base,
                head,
            ],
        )
    }

    fn preflight_with_objects(&self, objects: &Path, base: &str, head: &str) -> Output {
        run(
            self.path(),
            "bash",
            &[
                &checkout()
                    .join("scripts/merge-preflight.sh")
                    .display()
                    .to_string(),
                "--object-dir",
                &objects.display().to_string(),
                base,
                head,
            ],
        )
    }

    fn poller(&self, base: &str) -> TempDir {
        let container = scratch_dir();
        let poller = container.path().join("poller");
        let out = self.git(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            &poller.display().to_string(),
            base,
        ]);
        assert_ok(&out, "creating the speculative poller worktree");
        container
    }

    fn speculative_run(
        &self,
        expected_tree: &str,
        base: &str,
        head: &str,
        poller: &Path,
        command: &[&str],
    ) -> Output {
        let mut args = vec![
            checkout()
                .join("scripts/merge-watch.sh")
                .display()
                .to_string(),
            "--speculative-run".to_string(),
            expected_tree.to_string(),
            base.to_string(),
            head.to_string(),
            poller.display().to_string(),
            "--".to_string(),
        ];
        args.extend(command.iter().map(|arg| (*arg).to_string()));
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        Command::new("bash")
            .args(&arg_refs)
            .current_dir(self.path())
            .env("STORYHOOK_STORE_PATH", "/live/storyhook/store.db")
            .env("STORYHOOK_PROJECT", "live-project")
            .env("GH_TOKEN", "github-secret")
            .env("GITHUB_TOKEN", "github-fallback-secret")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .output()
            .expect("running speculative merge command")
    }

    fn spawn_speculative_run(
        &self,
        expected_tree: &str,
        base: &str,
        head: &str,
        poller: &Path,
        marker: &Path,
    ) -> ChildGuard {
        let mut command = Command::new("bash");
        command
            .arg(checkout().join("scripts/merge-watch.sh"))
            .args([
                "--speculative-run",
                expected_tree,
                base,
                head,
                &poller.display().to_string(),
                "--",
                "bash",
                "-c",
                "trap 'exit 129' HUP; trap 'exit 130' INT; trap 'exit 143' TERM; printf ready > \"$1\"; while :; do :; done",
                "merge-watch-signal-probe",
                &marker.display().to_string(),
            ])
            .current_dir(self.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES");
        // The Rust test harness may itself carry ignored terminal signals.
        // Reset them before exec so merge-watch starts from the ordinary
        // process contract and can install the traps this test exercises.
        unsafe {
            command.pre_exec(|| {
                libc::signal(libc::SIGHUP, libc::SIG_DFL);
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGTERM, libc::SIG_DFL);
                Ok(())
            });
        }
        ChildGuard::spawn(&mut command).expect("spawning the speculative-run signal probe")
    }

    fn spawn_blocked_speculative_run(
        &self,
        expected_tree: &str,
        base: &str,
        head: &str,
        poller: &Path,
        ready: &Path,
        release: &Path,
    ) -> ChildGuard {
        let mut command = Command::new("bash");
        command
            .arg(checkout().join("scripts/merge-watch.sh"))
            .args([
                "--speculative-run",
                expected_tree,
                base,
                head,
                &poller.display().to_string(),
                "--",
                "bash",
                "-c",
                "git rev-parse HEAD HEAD^{tree} > \"$1.tmp\" && mv \"$1.tmp\" \"$1\"; while [ ! -e \"$2\" ]; do sleep 0.01; done",
                "merge-watch-concurrency-probe",
                &ready.display().to_string(),
                &release.display().to_string(),
            ])
            .current_dir(self.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES");
        ChildGuard::spawn(&mut command).expect("spawning the blocked speculative run")
    }

    fn merge_object_artifacts(&self) -> Vec<PathBuf> {
        let root = self.common_dir().join("storyhook");
        let Ok(entries) = fs::read_dir(root) else {
            return Vec::new();
        };
        let mut paths = entries
            .map(|entry| entry.expect("reading merge state").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("merge-watch-objects."))
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    fn gate(&self, phase: &str) -> Output {
        run(
            self.path(),
            "bash",
            &[
                &checkout()
                    .join("scripts/gate-receipt.sh")
                    .display()
                    .to_string(),
                phase,
            ],
        )
    }

    /// Enrols and writes a receipt for whatever is currently checked out —
    /// the whole green path, through the production writer.
    fn enroll_and_certify(&self) {
        assert_ok(&self.gate("preflight"), "fixture: enrolling");
        assert_ok(&self.gate("postlude"), "fixture: writing the receipt");
    }

    /// `gate-receipt.sh postlude <tier> [<base>]` — the tiered form (SH-429),
    /// for tests exercising the `changed` tier specifically.
    fn gate_postlude(&self, tier: &str, base: Option<&str>) -> Output {
        let mut args = vec![
            checkout()
                .join("scripts/gate-receipt.sh")
                .display()
                .to_string(),
            "postlude".to_string(),
            tier.to_string(),
        ];
        if let Some(b) = base {
            args.push(b.to_string());
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run(self.path(), "bash", &arg_refs)
    }
}

fn run(cwd: &Path, program: &str, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        // A hook or script under test must not inherit git's own targeting
        // variables from the test runner's environment — the same scrub
        // `tests/push_gate.rs` applies.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .unwrap_or_else(|e| panic!("running {program}: {e}"))
}

fn assert_ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} should have succeeded\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn git_with_objects(cwd: &Path, primary: &Path, alternate: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_OBJECT_DIRECTORY", primary)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", alternate)
        .output()
        .expect("running git with private objects")
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(path.exists(), "timed out waiting for {}", path.display());
}

#[cfg(target_os = "macos")]
struct ImmutableObjects {
    path: PathBuf,
    armed: bool,
}

#[cfg(target_os = "macos")]
impl ImmutableObjects {
    fn freeze(path: PathBuf) -> Self {
        let out = Command::new("chflags")
            .args(["-R", "uchg"])
            .arg(&path)
            .output()
            .expect("freezing source objects");
        assert_ok(&out, "freezing source objects");
        Self { path, armed: true }
    }

    fn restore(&mut self) {
        let out = Command::new("chflags")
            .args(["-R", "nouchg"])
            .arg(&self.path)
            .output()
            .expect("restoring source objects");
        if out.status.success() {
            self.armed = false;
        }
        assert_ok(&out, "restoring source objects");
    }
}

#[cfg(target_os = "macos")]
impl Drop for ImmutableObjects {
    fn drop(&mut self) {
        if self.armed {
            let _ = Command::new("chflags")
                .args(["-R", "nouchg"])
                .arg(&self.path)
                .output();
        }
    }
}

// ---------------------------------------------------------------------------
// The provocation
// ---------------------------------------------------------------------------

/// `merge-tree --write-tree` really creates a new tree for two diverged
/// branches. The ordinary two-argument interface must keep that object out of
/// the source repository and remove every temporary artifact it owns.
#[test]
fn preflight_owns_and_cleans_speculative_objects_without_inserting_them_in_source() {
    let repo = MergeRepo::new();
    let fork = repo.rev_parse("main");
    let head = repo.branch("feature", &fork, "g", "feature\n");
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "returning to main");
    repo.write("h", "main\n");
    assert_ok(&repo.git(&["add", "h"]), "staging main's change");
    assert_ok(
        &repo.git(&["commit", "-qm", "main diverges"]),
        "advancing main",
    );
    let base = repo.rev_parse("main");
    let private_tmp = scratch_dir();

    let out = Command::new("bash")
        .arg(checkout().join("scripts/merge-preflight.sh"))
        .args([&base, &head])
        .current_dir(repo.path())
        .env("TMPDIR", private_tmp.path())
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .expect("running isolated merge preflight");

    assert_eq!(
        out.status.code(),
        Some(1),
        "the merge is clean but uncertified"
    );
    let predicted = stdout(&out);
    let source_lookup = repo.git(&["cat-file", "-e", &format!("{predicted}^{{tree}}")]);
    assert!(
        !source_lookup.status.success(),
        "the speculative tree must not be inserted into source objects"
    );
    let leftovers = fs::read_dir(private_tmp.path())
        .expect("reading isolated temporary storage")
        .map(|entry| entry.expect("reading a temporary artifact").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("storyhook-merge-"))
        })
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "preflight left private artifacts: {leftovers:?}"
    );
}

#[test]
fn caller_owned_objects_remain_resolvable_and_source_owned_paths_are_refused() {
    let repo = MergeRepo::new();
    let fork = repo.rev_parse("main");
    let head = repo.branch("feature", &fork, "g", "feature\n");
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "returning to main");
    repo.write("h", "main\n");
    assert_ok(&repo.git(&["add", "h"]), "staging main's change");
    assert_ok(
        &repo.git(&["commit", "-qm", "main diverges"]),
        "advancing main",
    );
    let base = repo.rev_parse("main");
    let lease = scratch_dir();

    let out = repo.preflight_with_objects(lease.path(), &base, &head);
    assert_eq!(out.status.code(), Some(1));
    let predicted = stdout(&out);
    assert!(
        lease
            .path()
            .read_dir()
            .expect("reading caller lease")
            .next()
            .is_some()
    );
    assert!(
        !repo
            .git(&["cat-file", "-e", &format!("{predicted}^{{tree}}")])
            .status
            .success(),
        "source alone must not resolve the generated tree"
    );
    assert_ok(
        &git_with_objects(
            repo.path(),
            lease.path(),
            &repo.common_dir().join("objects"),
            &["cat-file", "-e", &format!("{predicted}^{{tree}}")],
        ),
        "resolving the caller-owned predicted tree",
    );

    let refused = repo.preflight_with_objects(&repo.common_dir().join("objects"), &base, &head);
    assert!(!refused.status.success());
    assert_eq!(stdout(&refused), "");
    assert!(stderr(&refused).contains("outside the source object database"));
}

#[test]
fn speculative_run_uses_the_exact_tree_and_restores_after_success_or_failure() {
    let repo = MergeRepo::new();
    let fork = repo.rev_parse("main");
    let head = repo.branch("feature", &fork, "g", "feature\n");
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "returning to main");
    repo.write("h", "main\n");
    assert_ok(&repo.git(&["add", "h"]), "staging main's change");
    assert_ok(
        &repo.git(&["commit", "-qm", "main diverges"]),
        "advancing main",
    );
    let base = repo.rev_parse("main");
    let expected_tree = stdout(&repo.preflight(&base, &head));
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");

    let isolated = repo.speculative_run(
        &expected_tree,
        &base,
        &head,
        &poller,
        &[
            "bash",
            "-c",
            "test -z \"${STORYHOOK_STORE_PATH+x}\" && test -z \"${STORYHOOK_PROJECT+x}\" && test -z \"${GH_TOKEN+x}\" && test -z \"${GITHUB_TOKEN+x}\"",
        ],
    );
    assert_ok(
        &isolated,
        "speculative run with live StoryHook selectors removed",
    );

    let success = repo.speculative_run(
        &expected_tree,
        &base,
        &head,
        &poller,
        &["bash", "-c", "git rev-parse HEAD HEAD^{tree}"],
    );
    assert_ok(&success, "successful speculative run");
    let identities = stdout(&success)
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(
        identities.len(),
        2,
        "command must report commit and tree, got {identities:?}"
    );
    assert_eq!(
        identities[1], expected_tree,
        "speculative identities were {identities:?}"
    );
    assert!(
        !repo
            .git(&["cat-file", "-e", &format!("{}^{{commit}}", identities[0])])
            .status
            .success(),
        "the speculative commit must not enter source objects"
    );
    assert_eq!(
        stdout(&run(&poller, "git", &["rev-parse", "HEAD"])),
        base,
        "the poller must return to canonical history"
    );
    assert!(repo.merge_object_artifacts().is_empty());

    let failure = repo.speculative_run(
        &expected_tree,
        &base,
        &head,
        &poller,
        &["bash", "-c", "exit 42"],
    );
    assert_eq!(failure.status.code(), Some(42));
    assert_eq!(stdout(&run(&poller, "git", &["rev-parse", "HEAD"])), base);
    assert!(repo.merge_object_artifacts().is_empty());
}

#[test]
fn speculative_run_keeps_shared_worktree_refs_resolvable_while_gate_is_blocked() {
    let repo = MergeRepo::new();
    let fork = repo.rev_parse("main");
    let head = repo.branch("feature", &fork, "g", "feature\n");
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "returning to main");
    repo.write("h", "main\n");
    assert_ok(&repo.git(&["add", "h"]), "staging main's change");
    assert_ok(
        &repo.git(&["commit", "-qm", "main diverges"]),
        "advancing main",
    );
    let base = repo.rev_parse("main");
    let expected_tree = stdout(&repo.preflight(&base, &head));
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");
    let coordination = scratch_dir();
    let ready = coordination.path().join("ready");
    let release = coordination.path().join("release");

    let mut child =
        repo.spawn_blocked_speculative_run(&expected_tree, &base, &head, &poller, &ready, &release);
    wait_for(&ready);

    let private_identities = fs::read_to_string(&ready).expect("reading gate identities");
    let shared_head = repo.git(&["rev-parse", "worktrees/poller/HEAD^{commit}"]);
    let ref_walk = repo.git(&["rev-list", "--all", "--objects"]);
    let fetch = repo.git(&["fetch", "-q", "."]);

    fs::write(&release, "release\n").expect("releasing the speculative gate");
    let status = child.wait_within(Duration::from_secs(5), || {
        "the speculative gate did not exit after release".to_owned()
    });

    assert!(
        status.success(),
        "the released speculative gate should succeed: {status}"
    );
    assert_ok(&shared_head, "resolving the shared verifier HEAD");
    assert_eq!(stdout(&shared_head), base);
    assert_ok(&ref_walk, "walking every ordinary shared ref");
    assert_ok(&fetch, "fetching while the speculative gate is blocked");

    let identities = private_identities.lines().collect::<Vec<_>>();
    assert_eq!(identities.len(), 2, "gate identities were {identities:?}");
    assert_eq!(identities[1], expected_tree);
    assert!(
        !repo
            .git(&["cat-file", "-e", &format!("{}^{{commit}}", identities[0])])
            .status
            .success(),
        "the speculative commit must remain private"
    );
    assert_eq!(stdout(&run(&poller, "git", &["rev-parse", "HEAD"])), base);
    assert_eq!(run(&poller, "git", &["status", "--porcelain"]).stdout, b"");
    assert!(repo.merge_object_artifacts().is_empty());
}

#[test]
fn speculative_run_recovers_a_poller_whose_private_head_is_unavailable() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("feature", "main", "g", "feature\n");
    let expected_tree = stdout(&repo.preflight(&base, &head));
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");

    let private_objects = scratch_dir();
    let source_objects = repo.common_dir().join("objects");
    let private_commit = git_with_objects(
        &poller,
        private_objects.path(),
        &source_objects,
        &[
            "commit-tree",
            &repo.tree_of(&base),
            "-p",
            &base,
            "-m",
            "stranded",
        ],
    );
    assert_ok(&private_commit, "creating a private stranded commit");
    let private_commit = stdout(&private_commit);
    let checkout_private = git_with_objects(
        &poller,
        private_objects.path(),
        &source_objects,
        &["checkout", "-q", "--detach", &private_commit],
    );
    assert_ok(&checkout_private, "stranding the poller on private objects");
    drop(private_objects);
    let broken = run(&poller, "git", &["status", "--short"]);
    assert!(!broken.status.success(), "fixture HEAD must be unavailable");
    assert!(stderr(&broken).contains("bad object"));

    let recovered = repo.speculative_run(
        &expected_tree,
        &base,
        &head,
        &poller,
        &["git", "cat-file", "-e", "HEAD^{commit}"],
    );

    assert_ok(&recovered, "running after a forced verifier termination");
    assert_eq!(stdout(&run(&poller, "git", &["rev-parse", "HEAD"])), base);
    assert!(repo.merge_object_artifacts().is_empty());
}

#[test]
fn speculative_run_forwards_hup_and_term_and_cleans_before_reraising() {
    let repo = MergeRepo::new();
    let fork = repo.rev_parse("main");
    let head = repo.branch("feature", &fork, "g", "feature\n");
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "returning to main");
    repo.write("h", "main\n");
    assert_ok(&repo.git(&["add", "h"]), "staging main's change");
    assert_ok(
        &repo.git(&["commit", "-qm", "main diverges"]),
        "advancing main",
    );
    let base = repo.rev_parse("main");
    let expected_tree = stdout(&repo.preflight(&base, &head));
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");

    // An asynchronously launched noninteractive Bash ignores SIGINT on the
    // supported macOS runner as a job-control property; HUP and TERM are the
    // two deterministically deliverable entries into the shared handler.
    for (name, number) in [("HUP", 1), ("TERM", 15)] {
        let marker_root = scratch_dir();
        let marker = marker_root.path().join("ready");
        let mut child = repo.spawn_speculative_run(&expected_tree, &base, &head, &poller, &marker);
        wait_for(&marker);
        let signal = Command::new("kill")
            .args(["-s", name, &child.pid().to_string()])
            .output()
            .expect("signalling speculative-run");
        assert_ok(&signal, "signalling speculative-run");
        let status = child.wait_within(Duration::from_secs(5), || {
            format!("the speculative run did not exit after {name}")
        });
        assert_eq!(status.signal(), Some(number), "{name} must be re-raised");
        assert_eq!(stdout(&run(&poller, "git", &["rev-parse", "HEAD"])), base);
        assert!(
            repo.merge_object_artifacts().is_empty(),
            "{name} left private merge objects"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn preflight_and_speculative_run_succeed_with_immutable_source_objects() {
    let repo = MergeRepo::new();
    let fork = repo.rev_parse("main");
    let head = repo.branch("feature", &fork, "g", "feature\n");
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "returning to main");
    repo.write("h", "main\n");
    assert_ok(&repo.git(&["add", "h"]), "staging main's change");
    assert_ok(
        &repo.git(&["commit", "-qm", "main diverges"]),
        "advancing main",
    );
    let base = repo.rev_parse("main");
    let expected_tree = stdout(&repo.preflight(&base, &head));
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");
    std::os::unix::fs::symlink(checkout().join(".githooks"), poller.join(".githooks"))
        .expect("linking the production hooks into the poller fixture");
    let mut immutable = ImmutableObjects::freeze(repo.common_dir().join("objects"));

    let preflight = repo.preflight(&base, &head);
    assert_eq!(preflight.status.code(), Some(1));
    assert_eq!(stdout(&preflight), expected_tree);
    let gate = checkout()
        .join("scripts/gate-receipt.sh")
        .display()
        .to_string();
    let outcome = repo.speculative_run(
        &expected_tree,
        &base,
        &head,
        &poller,
        &[
            "bash",
            "-c",
            "git cat-file -e HEAD^{commit} && bash \"$1\" preflight && bash \"$1\" postlude",
            "merge-gate-probe",
            &gate,
        ],
    );

    immutable.restore();
    assert_ok(&outcome, "speculative run with immutable source objects");
    assert_eq!(stdout(&run(&poller, "git", &["rev-parse", "HEAD"])), base);
    assert!(repo.merge_object_artifacts().is_empty());
    assert!(
        repo.common_dir()
            .join("storyhook/gate-receipts")
            .join(expected_tree)
            .is_file(),
        "the production receipt writer must certify the private speculative tree"
    );
}

/// The ordinary case: two branches that merge cleanly, neither ever tested
/// together. This is the SH-396 shape exactly — no textual conflict, so
/// nothing but a real receipt check can tell the merge is unverified.
#[test]
fn an_uncertified_merge_tree_is_reported_as_uncertified_and_names_it() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("feature", "main", "g", "new\n");

    let out = repo.preflight(&base, &head);

    assert!(
        !out.status.success(),
        "an uncertified but clean merge must not report success"
    );
    assert_eq!(out.status.code(), Some(1), "uncertified must exit 1");
    let tree = stdout(&out);
    assert_eq!(
        tree.len(),
        40,
        "stdout must be exactly the tree oid, got: {tree}"
    );
    assert!(
        stderr(&out).contains("not certified"),
        "the refusal must say so, got: {}",
        stderr(&out)
    );
}

/// The anti-vacuity control. Without a real receipt from the production
/// writer, the test above passes for the wrong reason — a broken fixture, a
/// script that always reports "uncertified".
#[test]
fn certifying_the_predicted_tree_through_the_production_writer_clears_it() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("feature", "main", "g", "new\n");

    let predicted = stdout(&repo.preflight(&base, &head));

    // Perform the real merge on a throwaway branch, in the SAME repo — same
    // `--git-common-dir`, so the receipt this writes lands in the one store
    // `merge-preflight.sh` reads.
    assert_ok(
        &repo.git(&["checkout", "-q", "-b", "merged", &base]),
        "branching for the real merge",
    );
    assert_ok(
        &repo.git(&["merge", "-q", "--no-edit", &head]),
        "the real merge must succeed cleanly",
    );
    assert_eq!(
        repo.tree_of("HEAD"),
        predicted,
        "fixture: the real merge's tree must equal the predicted one, or \
         certifying it proves nothing"
    );
    repo.enroll_and_certify();

    let out = repo.preflight(&base, &head);

    assert_ok(&out, "a merge tree with a real receipt");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        predicted,
        "the certified tree must be reported"
    );
    assert!(stderr(&out).contains("certified"), "got: {}", stderr(&out));
}

/// The load-bearing correctness claim this whole gate rests on: predicting a
/// tree and actually producing one must agree, or a receipt for one would
/// never satisfy a lookup for the other.
#[test]
fn the_predicted_tree_matches_a_real_merges_tree_exactly() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("feature", "main", "g", "new\n");

    let predicted = stdout(&repo.preflight(&base, &head));

    assert_ok(
        &repo.git(&["checkout", "-q", "-b", "merged", &base]),
        "branching for the real merge",
    );
    assert_ok(
        &repo.git(&["merge", "-q", "--no-edit", &head]),
        "the real merge",
    );

    assert_eq!(
        predicted,
        repo.tree_of("HEAD"),
        "merge-tree's prediction must be byte-identical to a real merge's tree"
    );
}

/// The exact shape of 16 of the last 30 real merges into `main`: the branch
/// already contains the base, so the merge tree IS the branch's own tip
/// tree — and if that tip already carries a receipt from its own ordinary
/// certification, nothing new needs testing.
#[test]
fn a_branch_that_already_contains_main_is_certified_via_its_own_receipt() {
    let repo = MergeRepo::new();
    let base_before = repo.rev_parse("main");
    repo.branch("feature", "main", "g", "new\n");

    // main advances independently.
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "back to main");
    repo.write("h", "on main\n");
    repo.git(&["add", "h"]);
    assert_ok(
        &repo.git(&["commit", "-qm", "main moves on"]),
        "advancing main",
    );
    let base_after = repo.rev_parse("main");
    assert_ne!(base_before, base_after, "fixture: main must have moved");

    // feature absorbs main's new tip via a real merge, then gets certified
    // as itself — the ordinary push-gate path, not this script.
    assert_ok(&repo.git(&["checkout", "-q", "feature"]), "back to feature");
    assert_ok(
        &repo.git(&["merge", "-q", "--no-edit", "main"]),
        "feature absorbs main",
    );
    let feature_tip = repo.rev_parse("HEAD");
    repo.enroll_and_certify();

    let out = repo.preflight(&base_after, &feature_tip);

    assert_ok(&out, "a branch that already contains main");
    assert_eq!(
        stdout(&out),
        repo.tree_of("HEAD"),
        "the merge tree must equal feature's own tip tree exactly"
    );
    assert!(stderr(&out).contains("certified"));
}

/// A real textual conflict must be reported distinctly from "clean but
/// untested" — and must never print the conflict's own "virtual" tree
/// (conflict markers baked in) as though it were a usable result.
#[test]
fn a_textual_conflict_is_reported_distinctly_and_prints_no_tree() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let a = repo.branch("branch-a", "main", "f", "A changes the base file\n");
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "back to main");
    let b = repo.branch("branch-b", "main", "f", "B changes the base file\n");
    let _ = base;

    let out = repo.preflight(&a, &b);

    assert!(
        !out.status.success(),
        "a real conflict must not report success"
    );
    assert_eq!(out.status.code(), Some(2), "a conflict must exit 2, not 1");
    assert_eq!(
        stdout(&out),
        "",
        "a conflict has no valid tree — stdout must be empty, not the \
         conflict's virtual tree with markers baked in"
    );
    assert!(stderr(&out).contains("CONFLICT"), "got: {}", stderr(&out));
}

/// The receipt certifies content, not a branch name — the same doctrine
/// `tests/push_gate.rs::a_commit_made_after_the_receipt_is_refused` pins for
/// the push gate. A new commit changes the merge tree, so a certification
/// of the OLD tree must not silently cover the new one.
#[test]
fn a_new_commit_after_certification_produces_an_uncertified_tree_again() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head_v1 = repo.branch("feature", "main", "g", "v1\n");

    assert_ok(
        &repo.git(&["checkout", "-q", "-b", "merged", &base]),
        "branching for the real merge",
    );
    assert_ok(
        &repo.git(&["merge", "-q", "--no-edit", &head_v1]),
        "the real merge",
    );
    repo.enroll_and_certify();
    assert_ok(
        &repo.preflight(&base, &head_v1),
        "fixture: the first tree must already be certified",
    );

    assert_ok(&repo.git(&["checkout", "-q", "feature"]), "back to feature");
    repo.write("g", "v2\n");
    repo.git(&["add", "g"]);
    assert_ok(
        &repo.git(&["commit", "-qm", "a new commit nobody has tested"]),
        "advancing feature",
    );
    let head_v2 = repo.rev_parse("HEAD");

    let out = repo.preflight(&base, &head_v2);

    assert!(
        !out.status.success(),
        "a new commit must not inherit the old tree's receipt"
    );
    assert_eq!(out.status.code(), Some(1));
    assert_ne!(
        stdout(&out),
        stdout(&repo.preflight(&base, &head_v1)),
        "fixture: the two trees must actually differ"
    );
}

/// SH-429's council verdict, provoked directly: a `changed`-tier receipt —
/// even one that exists for the merge tree's EXACT oid — is never sufficient
/// to certify a merge. `merge-preflight.sh` is tier-blind by mere file
/// existence; this is the one place that blindness must not extend to, since
/// a selective run diffed against one branch's own history, and a merge
/// combines two branches' independently-authored diffs — the exact SH-396
/// shape (14 of 30 real merges producing a tree matching neither parent)
/// this whole gate exists to catch.
#[test]
fn a_changed_tier_receipt_does_not_certify_a_merge_even_for_the_exact_tree() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let base_tree = repo.tree_of("main");
    let head = repo.branch("feature", "main", "g", "new\n");

    let predicted = stdout(&repo.preflight(&base, &head));

    // Certify the BASE first, at `gate` tier — a `changed` receipt needs a
    // certified base of its own to name. `gate-receipt.sh` keys receipts by
    // TREE oid, not commit oid, which is why this uses `base_tree`
    // (`repo.tree_of`) rather than `base` (`repo.rev_parse`, a commit —
    // correct for `merge-preflight.sh`'s own ref-taking arguments, but not
    // for `gate-receipt.sh postlude`'s base-TREE argument). Checkout
    // precedes preflight: preflight records whichever tree is checked out
    // AT THAT MOMENT, and postlude refuses if it has drifted since.
    assert_ok(
        &repo.git(&["checkout", "-q", "main"]),
        "back to main to certify it",
    );
    assert_ok(
        &repo.gate("preflight"),
        "fixture: enrolling to certify the base",
    );
    assert_ok(
        &repo.gate_postlude("gate", None),
        "fixture: certifying the base",
    );

    // Perform the real merge on a throwaway branch, in the SAME repo, then
    // certify the resulting tree — the exact one merge-preflight.sh will
    // look up — at `changed`, the one tier that must not satisfy it.
    assert_ok(
        &repo.git(&["checkout", "-q", "-b", "merged", &base]),
        "branching for the real merge",
    );
    assert_ok(
        &repo.git(&["merge", "-q", "--no-edit", &head]),
        "the real merge must succeed cleanly",
    );
    assert_eq!(
        repo.tree_of("HEAD"),
        predicted,
        "fixture: the real merge's tree must equal the predicted one"
    );
    assert_ok(
        &repo.gate("preflight"),
        "fixture: re-enrolling on the merge branch",
    );
    assert_ok(
        &repo.gate_postlude("changed", Some(&base_tree)),
        "fixture: writing a changed-tier receipt for the merge tree itself",
    );

    let out = repo.preflight(&base, &head);

    assert!(
        !out.status.success(),
        "a changed-tier receipt must not certify a merge, got exit {:?}\nstderr: {}",
        out.status.code(),
        stderr(&out)
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("changed"),
        "the refusal must name the insufficient tier, got: {}",
        stderr(&out)
    );
}

#[test]
fn verifier_metadata_accepts_false_booleans_without_confusing_them_for_absence() {
    let repo = MergeRepo::new();
    let script = checkout().join("scripts/verify-pr.sh");
    let script = script.to_string_lossy().to_string();
    let ready = r#"{"number":42,"state":"OPEN","isDraft":false,"isCrossRepository":false,"baseRefName":"main","headRefOid":"deadbeef","mergeCommit":null}"#;
    let validate = |metadata: &str| {
        let out = run(
            repo.path(),
            "bash",
            &[&script, "--validate-metadata", metadata],
        );
        assert_ok(&out, "validating verifier pull-request metadata");
        serde_json::from_slice::<serde_json::Value>(&out.stdout)
            .expect("the verifier metadata seam must return JSON")
    };

    let accepted = validate(ready);
    assert_eq!(accepted["result"], "metadata-valid");
    assert_eq!(accepted["number"], 42);

    let draft = validate(&ready.replace("\"isDraft\":false", "\"isDraft\":true"));
    assert_eq!(draft["result"], "infrastructure-failure");
    assert!(draft["detail"].as_str().unwrap().contains("is a draft"));
    assert!(
        !draft["detail"]
            .as_str()
            .unwrap()
            .contains("no draft status")
    );

    let fork =
        validate(&ready.replace("\"isCrossRepository\":false", "\"isCrossRepository\":true"));
    assert_eq!(fork["result"], "infrastructure-failure");
    assert!(
        fork["detail"]
            .as_str()
            .unwrap()
            .contains("comes from a fork")
    );
    assert!(
        !fork["detail"]
            .as_str()
            .unwrap()
            .contains("no repository relationship")
    );

    let invalid = validate(&ready.replace("\"isDraft\":false", "\"isDraft\":\"false\""));
    assert_eq!(invalid["result"], "infrastructure-failure");
    assert!(
        invalid["detail"]
            .as_str()
            .unwrap()
            .contains("no draft status")
    );
}

#[test]
fn verifier_restart_recovers_only_a_certified_merge_on_the_current_base() {
    let repo = MergeRepo::new();
    let original_base = repo.rev_parse("main");
    let head = repo.branch("feature", "main", "g", "new\n");
    assert_ok(
        &repo.git(&["checkout", "-q", "-b", "merged", &original_base]),
        "branching for the landed merge",
    );
    assert_ok(
        &repo.git(&["merge", "-q", "--no-edit", &head]),
        "creating the landed merge",
    );
    let merge_oid = repo.rev_parse("HEAD");
    let merge_tree = repo.tree_of("HEAD");
    let script = checkout().join("scripts/verify-pr.sh");
    let script = script.to_string_lossy().to_string();
    let before_uncertified = repo.git(&["status", "--porcelain"]).stdout;

    let uncertified = run(
        repo.path(),
        "bash",
        &[&script, "--recover-merged", "HEAD", &merge_oid, "42"],
    );
    assert_ok(
        &uncertified,
        "the verifier protocol reports refusals as JSON",
    );
    let payload: serde_json::Value = serde_json::from_slice(&uncertified.stdout).unwrap();
    assert_eq!(payload["result"], "infrastructure-failure");
    assert!(
        payload["detail"]
            .as_str()
            .unwrap()
            .contains("without a release-gate receipt")
    );
    assert_eq!(
        repo.git(&["status", "--porcelain"]).stdout,
        before_uncertified
    );

    repo.enroll_and_certify();
    repo.write("later", "base advanced\n");
    assert_ok(&repo.git(&["add", "later"]), "staging base advancement");
    assert_ok(
        &repo.git(&["commit", "-qm", "base advanced"]),
        "advancing base",
    );
    let before_recovery = repo.git(&["status", "--porcelain"]).stdout;

    let recovered = run(
        repo.path(),
        "bash",
        &[&script, "--recover-merged", "HEAD", &merge_oid, "42"],
    );
    assert_ok(&recovered, "recovering a certified landed merge");
    let payload: serde_json::Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(payload["result"], "merged");
    assert_eq!(payload["tree"], merge_tree);
    assert!(
        payload["detail"]
            .as_str()
            .unwrap()
            .contains("after verifier restart")
    );

    let wrong_base = run(
        repo.path(),
        "bash",
        &[
            &script,
            "--recover-merged",
            &original_base,
            &merge_oid,
            "42",
        ],
    );
    let payload: serde_json::Value = serde_json::from_slice(&wrong_base.stdout).unwrap();
    assert_eq!(payload["result"], "infrastructure-failure");
    assert!(
        payload["detail"]
            .as_str()
            .unwrap()
            .contains("not on the refreshed base")
    );
    assert_eq!(repo.git(&["status", "--porcelain"]).stdout, before_recovery);
}

#[test]
fn verifier_preserves_the_landing_scripts_conflict_classification() {
    let repo = MergeRepo::new();
    let script = checkout().join("scripts/verify-pr.sh");
    let script = script.to_string_lossy().to_string();

    let conflicted = run(
        repo.path(),
        "bash",
        &[
            &script,
            "--classify-land",
            "2",
            "merge-preflight found a textual conflict",
            "42",
            "deadbeef",
        ],
    );
    assert_ok(&conflicted, "classifying a landing conflict");
    let payload: serde_json::Value = serde_json::from_slice(&conflicted.stdout).unwrap();
    assert_eq!(payload["result"], "conflict");
    assert!(
        payload["detail"]
            .as_str()
            .unwrap()
            .contains("textual conflict")
    );

    let infrastructure = run(
        repo.path(),
        "bash",
        &[
            &script,
            "--classify-land",
            "1",
            "GitHub unavailable",
            "42",
            "deadbeef",
        ],
    );
    assert_ok(&infrastructure, "classifying a landing refusal");
    let payload: serde_json::Value = serde_json::from_slice(&infrastructure.stdout).unwrap();
    assert_eq!(payload["result"], "infrastructure-failure");
}

/// Missing arguments are refused with a message naming correct usage, in
/// `gate-receipt.sh`'s own idiom — not a bash positional-parameter panic.
#[test]
fn missing_arguments_are_refused_with_a_usage_message() {
    let repo = MergeRepo::new();

    let out = run(
        repo.path(),
        "bash",
        &[&checkout()
            .join("scripts/merge-preflight.sh")
            .display()
            .to_string()],
    );

    assert!(!out.status.success());
    assert!(stderr(&out).contains("usage"), "got: {}", stderr(&out));
}
