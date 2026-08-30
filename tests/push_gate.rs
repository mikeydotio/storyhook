//! The developer push gate, **provoked** — not inspected.
//!
//! SH-306. The gate that was supposed to stop an untested push was a Claude
//! Code `PreToolUse` hook with `timeout: 900`. It fires for every invocation
//! shape, including a backgrounded one — that part of the filing was wrong —
//! but `make test` here runs nine minutes nominal and routinely longer under
//! the three-to-four concurrent worktree suites this machine actually runs. At
//! 900 seconds the harness SIGTERMs the hook (`make: *** [test] Terminated:
//! 15`) and **then lets the tool call proceed**. Six of the eight hook logs
//! left on that machine ended at exactly 900s across three days: six pushes
//! that shipped with no gate and no message saying so.
//!
//! The lesson is not "widen the matcher". It is that **a gate with a deadline
//! converts a timeout into a pass**, and no mechanism owning a deadline escapes
//! it. Git's own `pre-push` hook has no timeout and does not care how the shell
//! was invoked, so that is where the gate lives now — verifying a *receipt*
//! rather than re-running the suite, because a hook that re-ran `make test`
//! would reintroduce the very deadline pressure that caused this (and would
//! start a second nine-minute suite while the first was still running, which
//! this project has already recorded as the cause of a false red).
//!
//! # Why these tests are shaped this way
//!
//! Every test here drives **real git** against a **real bare remote** and
//! asserts on **the remote ref**, not on an exit code and never on a file
//! existing. An exit code alone does not prove nothing shipped, and "the call
//! proceeded anyway" is the precise defect. `tests/hook_execution.rs` states
//! the class these must avoid — *hook logic shipped as an untested string
//! literal* (SH-56) — so the artifact under test is the tracked
//! `.githooks/pre-push` itself, reached from `CARGO_MANIFEST_DIR`, never a copy
//! pasted in here.
//!
//! The green cases write their receipt with the **production writer**
//! (`scripts/gate-receipt.sh`), never a hand-forged file. That is the
//! anti-vacuity control SH-297's council insisted on: without it, a refusal for
//! an unrelated reason — a broken fixture, a missing remote — passes the
//! negative test for the wrong reason, and a hand-written receipt would prove
//! the checker's format rather than the producer's behaviour.
//!
//! # The gate narrowed to `main`/`master` (SH-429)
//!
//! This hook used to refuse an unreceipted push on **any** ref. Since SH-396,
//! `scripts/merge-preflight.sh` is what actually decides whether content
//! reaches `main` — a server-side `gh pr merge --merge` never triggers this
//! hook at all — so refusing an ordinary feature-branch push was gating
//! content this hook's own refusal was never the thing protecting. It now
//! refuses only a direct push to `main`/`master`, and *reports* (tier found,
//! or none, and where the real gate lives) for everything else. `GateRepo`
//! still checks out and pushes local `main` by default — most of these tests
//! are about the receipt/tier mechanism itself, which is unchanged and
//! branch-agnostic — but `push_branch` exists specifically to provoke the new
//! non-`main` report-not-refuse path, and the tests that exercise an actual
//! *refusal* now do so through `main`, which is where that behaviour survives.
//!
//! # Mutation-checked (SH-295: a pin that cannot fail is not a pin)
//!
//! All four were run by hand against this suite before it was committed, and
//! the counts below are measured, not predicted:
//!
//! - `.githooks/pre-push` made to `exit 0` unconditionally → **10 of 15 red**
//!   (was 6 of 11 pre-SH-429; the suite grew, the mutation still finds most of
//!   it), every test that asserts a refusal or a spoken line, including every
//!   test that reads the remote ref.
//! - the hook made to accept *any* receipt in the directory rather than the one
//!   named for this tree → **exactly 1 red**,
//!   `a_commit_made_after_the_receipt_is_refused`, unchanged by SH-429. That
//!   precision is the point: it is the only test whose refusal depends on
//!   *which* tree was certified, so a checker that verified mere existence
//!   would pass everything else.
//! - (SH-429) `is_protected_branch` forced to `1` unconditionally, so every ref
//!   is treated as `main`/`master` → **exactly 1 red**,
//!   `a_push_to_a_feature_branch_with_no_receipt_is_reported_but_not_refused`.
//!   The narrow blast radius is the point: only the one test that specifically
//!   provokes the non-`main` path notices a refusal that should not have fired.
//! - (SH-429) the `main | master` case arm made unreachable, so **no** ref is
//!   ever refused → **4 of 15 red**: every test whose refusal depends on `main`
//!   specifically still being protected
//!   (`a_push_to_main_with_no_receipt_is_refused_and_the_remote_does_not_move`,
//!   `a_commit_made_after_the_receipt_is_refused`,
//!   `content_that_changes_during_the_run_gets_no_receipt`,
//!   `enrollment_turns_an_ungated_clone_into_a_gated_one`).
//!
//! # What this suite deliberately does not claim
//!
//! - **Forgery is not the threat model.** Anyone who can hand-write a receipt
//!   already has `--no-verify` and `SKIP_PREPUSH_TESTS=1`, both sanctioned. The
//!   gate exists to stop a *silent, accidental* bypass from a timeout, not a
//!   determined local author.
//! - **Per-commit greenness is not enforced.** The receipt attests the tip tree
//!   of each pushed ref. Requiring one per commit would mean one nine-minute
//!   suite per commit — the exact pressure that produced SH-306 — so the hook
//!   *names* unreceipted commits in the range and lets them through.
//!   `a_pushed_commit_with_no_receipt_is_named_but_not_refused` pins that as a
//!   decision rather than an oversight.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{env, fs};

use std::os::unix::fs::PermissionsExt;

use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

/// The checkout under test — the tracked hooks and scripts live here.
fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// A repository with a real bare remote, ready to have pushes refused.
struct GateRepo {
    work: TempDir,
    remote: TempDir,
}

impl GateRepo {
    /// A repo on `main` with one commit and an `origin` pointing at a real bare
    /// remote. Not enrolled: `core.hooksPath` is unset, exactly as a fresh
    /// clone arrives.
    fn new() -> Self {
        let repo = Self {
            work: scratch_dir(),
            remote: scratch_dir(),
        };

        run(repo.remote.path(), "git", &["init", "-q", "--bare"]);

        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "t@t"]);
        repo.git(&["config", "user.name", "t"]);
        repo.git(&[
            "remote",
            "add",
            "origin",
            &repo.remote.path().display().to_string(),
        ]);

        // The tracked hooks directory, as a fixture repo would carry it. A
        // symlink rather than a copy so the artifact under test is the one that
        // ships, and so `core.hooksPath=.githooks` — the relative value real
        // enrollment writes — resolves here the same way it does in a checkout.
        std::os::unix::fs::symlink(checkout().join(".githooks"), repo.path().join(".githooks"))
            .expect("fixture: linking the tracked hooks directory");

        repo.write("f", "a\n");
        repo.git(&["add", "f"]);
        repo.git(&["commit", "-qm", "init"]);
        repo
    }

    fn path(&self) -> &Path {
        self.work.path()
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.path().join(name), body).expect("fixture: writing a tracked file");
    }

    fn git(&self, args: &[&str]) -> Output {
        run(self.path(), "git", args)
    }

    /// Commits a change to a tracked file, moving the tip tree.
    fn commit(&self, name: &str, body: &str, message: &str) {
        self.write(name, body);
        self.git(&["add", name]);
        self.git(&["commit", "-qm", message]);
    }

    /// Runs the production gate script in this repo.
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

    fn gate_with_path(&self, phase: &str, path: &std::ffi::OsStr) -> Output {
        Command::new("bash")
            .arg(checkout().join("scripts/gate-receipt.sh"))
            .arg(phase)
            .current_dir(self.path())
            .env("PATH", path)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("running gate-receipt.sh with a controlled PATH")
    }

    fn preflight_state(&self) -> PathBuf {
        self.path().join(".git").join("storyhook-gate-preflight")
    }

    fn preflight_lease(&self) -> PathBuf {
        let state =
            fs::read_to_string(self.preflight_state()).expect("reading the gate preflight state");
        state
            .lines()
            .find_map(|line| line.strip_prefix("objects "))
            .map(PathBuf::from)
            .expect("preflight state must name its object lease")
    }

    fn gate_artifacts(&self) -> Vec<PathBuf> {
        let mut artifacts = fs::read_dir(self.path().join(".git"))
            .expect("reading the fixture git directory")
            .map(|entry| entry.expect("reading a git-directory entry").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("storyhook-gate-"))
            })
            .collect::<Vec<_>>();
        artifacts.sort();
        artifacts
    }

    /// Enrolls the repo the way `make test` does, and writes a receipt for the
    /// current tracked content — the whole green path, through production code.
    fn enroll_and_certify(&self) {
        assert_ok(&self.gate("preflight"), "fixture: enrolling");
        assert_ok(&self.gate("postlude"), "fixture: writing the receipt");
    }

    /// `gate-receipt.sh postlude <tier>` — the tiered form (SH-394). `gate`
    /// above always takes the default tier; this names one explicitly, for
    /// tests exercising the `gate`/`full` split itself.
    fn gate_postlude(&self, tier: &str) -> Output {
        run(
            self.path(),
            "bash",
            &[
                &checkout()
                    .join("scripts/gate-receipt.sh")
                    .display()
                    .to_string(),
                "postlude",
                tier,
            ],
        )
    }

    fn push(&self, extra: &[&str]) -> Output {
        let mut args = vec!["push", "origin", "main"];
        args.extend_from_slice(extra);
        self.git(&args)
    }

    /// Pushes the current `HEAD` to `refs/heads/<remote_branch>` on the
    /// remote — a fresh branch on every call, never `main`/`master`. This is
    /// how SH-429's report-not-refuse path is provoked: `remote_ref` (the
    /// pre-push hook's third stdin field) names this branch, not the local
    /// one, so the fixture never needs a real local checkout switch.
    fn push_branch(&self, remote_branch: &str, extra: &[&str]) -> Output {
        let refspec = format!("HEAD:refs/heads/{remote_branch}");
        let mut args = vec!["push", "origin", refspec.as_str()];
        args.extend_from_slice(extra);
        self.git(&args)
    }

    /// What the remote actually holds for `refs/heads/<name>` — the only
    /// evidence that a push did or did not ship.
    fn remote_sha(&self, name: &str) -> Option<String> {
        let out = run(
            self.remote.path(),
            "git",
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{name}"),
            ],
        );
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !sha.is_empty()).then_some(sha)
    }

    fn head(&self) -> String {
        String::from_utf8_lossy(&self.git(&["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string()
    }
}

fn run(cwd: &Path, program: &str, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        // A hook must not inherit git's targeting variables from the test
        // runner's own environment; the same scrub the harness hook documents.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("SKIP_PREPUSH_TESTS")
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

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

// ---------------------------------------------------------------------------
// The provocation
// ---------------------------------------------------------------------------

/// The defect itself: content that never went green must not reach `main`
/// directly. SH-429 narrowed the hard refusal to `main`/`master` — this is
/// the case where it still applies.
///
/// The load-bearing assertion is the *remote ref*, not the exit code. SH-306's
/// whole failure mode was a refusal that never happened while everything looked
/// ordinary, so a test that only reads an exit status would have passed against
/// the broken mechanism too.
#[test]
fn a_push_to_main_with_no_receipt_is_refused_and_the_remote_does_not_move() {
    let repo = GateRepo::new();
    assert_ok(&repo.gate("preflight"), "enrolling");

    let before = repo.remote_sha("main");
    assert_eq!(before, None, "fixture: the remote starts empty");

    let out = repo.push(&[]);

    assert!(
        !out.status.success(),
        "a push to main with no receipt must be refused, got success\nstderr: {}",
        stderr(&out)
    );
    assert_eq!(
        repo.remote_sha("main"),
        None,
        "the remote moved despite the refusal — this is the SH-306 failure itself"
    );
    let err = stderr(&out);
    assert!(
        err.contains("make test"),
        "the refusal must name the remedy, got: {err}"
    );
    assert!(
        err.contains("protected branch"),
        "the refusal must say why main specifically is refused, got: {err}"
    );
}

/// SH-429's actual behavioural change: a push to an ordinary (non-`main`)
/// branch is no longer refused for lacking a receipt — it is reported. The
/// remote ref moving IS the assertion, per SH-306's own doctrine one section
/// up: a "report" that still silently blocked the push would be the same
/// defect wearing new words.
#[test]
fn a_push_to_a_feature_branch_with_no_receipt_is_reported_but_not_refused() {
    let repo = GateRepo::new();
    assert_ok(&repo.gate("preflight"), "enrolling");

    let out = repo.push_branch("feature", &[]);

    assert_ok(
        &out,
        "a feature-branch push with no receipt must still succeed",
    );
    assert_eq!(
        repo.remote_sha("feature").as_deref(),
        Some(repo.head().as_str()),
        "the push must actually have shipped -- SH-429 stops refusing this, it does not stop reporting it"
    );
    let err = stderr(&out);
    assert!(
        err.contains("reported, not refused"),
        "the report must say plainly that this is no longer a refusal, got: {err}"
    );
    assert!(
        err.contains("merge-preflight.sh"),
        "the report must name the actual enforcement point, got: {err}"
    );
}

/// The anti-vacuity control. Without it, the test above passes for any reason
/// at all — a broken fixture, a missing remote, a hook that always refuses.
#[test]
fn a_receipt_from_the_production_writer_lets_the_same_push_through() {
    let repo = GateRepo::new();
    repo.enroll_and_certify();

    let out = repo.push(&[]);

    assert_ok(&out, "a push whose tree has a receipt");
    assert_eq!(
        repo.remote_sha("main").as_deref(),
        Some(repo.head().as_str()),
        "the remote must hold exactly what was pushed"
    );
}

#[test]
fn an_empty_committed_repo_can_be_certified_and_pushed() {
    let repo = GateRepo::new();
    assert_ok(
        &repo.git(&["rm", "-q", "f"]),
        "removing the only tracked file",
    );
    assert_ok(
        &repo.git(&["commit", "-qm", "empty tree"]),
        "committing the empty tree",
    );
    let tree = String::from_utf8_lossy(&repo.git(&["rev-parse", "HEAD^{tree}"]).stdout)
        .trim()
        .to_string();

    repo.enroll_and_certify();

    assert!(
        repo.path()
            .join(".git/storyhook/gate-receipts")
            .join(&tree)
            .is_file(),
        "gate preflight/postlude must write a receipt keyed by the empty tree"
    );
    let out = repo.push(&[]);
    assert_ok(&out, "pushing a certified empty committed repo");
    assert_eq!(
        repo.remote_sha("main").as_deref(),
        Some(repo.head().as_str()),
        "the certified empty committed repo must reach the protected branch"
    );
}

/// The receipt's own failure class: it certifies one tree, not a branch.
///
/// This is the shape that makes a receipt worth having — "tested, then changed
/// something, then pushed" is the ordinary way a gate gets bypassed by accident.
#[test]
fn a_commit_made_after_the_receipt_is_refused() {
    let repo = GateRepo::new();
    repo.enroll_and_certify();
    assert_ok(&repo.push(&[]), "the certified push");
    let shipped = repo.remote_sha("main");

    repo.commit("f", "changed after the gate ran\n", "unverified change");
    let out = repo.push(&[]);

    assert!(
        !out.status.success(),
        "a tree with no receipt of its own must be refused\nstderr: {}",
        stderr(&out)
    );
    assert_eq!(
        repo.remote_sha("main"),
        shipped,
        "the remote must still hold the certified commit"
    );
}

// ---------------------------------------------------------------------------
// The tier split (SH-394) — `make test` writes a `gate` receipt, `make
// test-full` writes a `full` one, and pre-push must let either through while
// naming which it found. `tests/gate_tiers.rs` fences the Makefile wiring
// that decides which tier a run writes; these fence pre-push's own reporting
// of whatever it finds.
// ---------------------------------------------------------------------------

/// A `full` receipt must be named on the way through — without this, a push
/// that ran the browser suite looks identical at push time to one that
/// didn't, which is exactly the silent-coverage-gap shape SH-306 exists to
/// prevent one layer up.
#[test]
fn a_push_names_the_full_tier_its_receipt_carries() {
    let repo = GateRepo::new();
    assert_ok(&repo.gate("preflight"), "enrolling");
    assert_ok(
        &repo.gate_postlude("full"),
        "fixture: writing a full receipt",
    );

    let out = repo.push(&[]);

    assert_ok(&out, "a push whose tree has a full receipt");
    let err = stderr(&out);
    assert!(
        err.contains("tier full"),
        "a full receipt must be named as such, got: {err}"
    );
}

/// The ordinary case: an unqualified receipt — the default `postlude` takes,
/// and what `make test` writes — reports as the `gate` tier.
#[test]
fn an_unqualified_receipt_is_reported_as_the_gate_tier() {
    let repo = GateRepo::new();
    repo.enroll_and_certify();

    let out = repo.push(&[]);

    assert_ok(&out, "a push whose tree has a gate receipt");
    let err = stderr(&out);
    assert!(
        err.contains("tier gate"),
        "an unqualified receipt must report as the gate tier, got: {err}"
    );
}

/// A `full` receipt is a strictly stronger claim than a `gate` one for the
/// same tree. A later, cheaper `make test` run over unchanged content must
/// not quietly erase the evidence that the expensive tier already passed.
#[test]
fn a_gate_tier_postlude_does_not_downgrade_an_existing_full_receipt() {
    let repo = GateRepo::new();
    assert_ok(&repo.gate("preflight"), "enrolling");
    assert_ok(&repo.gate_postlude("full"), "writing the full receipt");

    assert_ok(&repo.gate("preflight"), "re-enrolling");
    assert_ok(
        &repo.gate_postlude("gate"),
        "a gate-tier postlude over the same tree",
    );

    let out = repo.push(&[]);

    assert_ok(&out, "a push whose tree still has a full receipt");
    let err = stderr(&out);
    assert!(
        err.contains("tier full"),
        "the full receipt must survive a later gate-tier run, got: {err}"
    );
}

/// Mid-run drift. `make test` is nine minutes and an agent editing tracked
/// files during it is routine here — so a receipt written at the end would
/// otherwise certify a mixture that no single run ever tested.
#[test]
fn content_that_changes_during_the_run_gets_no_receipt() {
    let repo = GateRepo::new();
    repo.write("f", "already dirty when the suite began\n");
    assert_ok(&repo.gate("preflight"), "enrolling");

    repo.write("f", "edited while the suite was running\n");
    let out = repo.gate("postlude");

    assert!(
        !out.status.success(),
        "the postlude must refuse to certify a tree that drifted mid-run"
    );
    let err = stderr(&out);
    assert!(
        err.contains('f'),
        "the refusal must name what changed, got: {err}"
    );
    assert!(
        repo.gate_artifacts().is_empty(),
        "a drift refusal must clean its preflight state and private objects"
    );

    // And the refusal is not cosmetic: nothing may ship on the strength of it.
    repo.git(&["add", "f"]);
    repo.git(&["commit", "-qm", "the drifted content"]);
    assert!(
        !repo.push(&[]).status.success(),
        "a run that refused to certify must leave the push refused"
    );
    assert_eq!(repo.remote_sha("main"), None);
}

#[test]
fn unchanged_dirty_content_is_certified_and_cleans_its_private_objects() {
    let repo = GateRepo::new();
    repo.write("f", "dirty content held constant across the run\n");

    assert_ok(&repo.gate("preflight"), "recording a dirty preflight tree");
    assert!(
        repo.preflight_lease().is_dir(),
        "the dirty preflight must retain caller-owned objects for postlude"
    );
    assert_ok(&repo.gate("postlude"), "certifying unchanged dirty content");
    assert!(
        repo.gate_artifacts().is_empty(),
        "successful postlude must remove state and private objects"
    );

    assert_ok(&repo.git(&["add", "f"]), "staging the certified content");
    assert_ok(
        &repo.git(&["commit", "-qm", "commit exactly what was certified"]),
        "committing the certified content",
    );
    assert_ok(
        &repo.push(&[]),
        "the committed form of the certified dirty tree must push",
    );
}

#[test]
fn a_new_preflight_reaps_a_valid_abandoned_lease() {
    let repo = GateRepo::new();
    repo.write("f", "dirty content needing private objects\n");
    assert_ok(&repo.gate("preflight"), "recording the first preflight");
    let abandoned = repo.preflight_lease();
    assert!(abandoned.is_dir(), "fixture: the abandoned lease exists");

    let replacement = repo.gate("preflight");

    assert_ok(&replacement, "replacing an abandoned preflight");
    assert!(
        stderr(&replacement).contains("abandoned preflight"),
        "stale cleanup must be announced, got: {}",
        stderr(&replacement)
    );
    assert!(
        !abandoned.exists(),
        "the validated abandoned lease must be reaped"
    );
    let current = repo.preflight_lease();
    assert_ne!(current, abandoned, "the new run must own a fresh lease");
    assert_ok(&repo.gate("postlude"), "finishing the replacement run");
    assert!(repo.gate_artifacts().is_empty());
}

#[test]
fn corrupt_preflight_state_never_deletes_an_unscoped_directory() {
    let repo = GateRepo::new();
    assert_ok(&repo.gate("preflight"), "recording a valid preflight");
    let original_lease = repo.preflight_lease();
    let tree = String::from_utf8_lossy(&repo.git(&["rev-parse", "HEAD^{tree}"]).stdout)
        .trim()
        .to_string();
    let must_survive = repo.path().join("must-survive-corrupt-state");
    fs::create_dir(&must_survive).expect("creating the unscoped directory");
    fs::write(
        repo.preflight_state(),
        format!("tree {tree}\nobjects {}\n", must_survive.display()),
    )
    .expect("corrupting the preflight lease path");

    let out = repo.gate("preflight");

    assert!(!out.status.success(), "corrupt state must fail closed");
    assert!(
        stderr(&out).contains("invalid preflight state"),
        "the corruption must be named, got: {}",
        stderr(&out)
    );
    assert!(
        must_survive.is_dir(),
        "an unscoped path from corrupt state must never be deleted"
    );
    assert!(
        !repo.preflight_state().exists(),
        "invalid state must be removed so the next run can recover"
    );
    fs::remove_dir_all(original_lease).expect("cleaning the fixture's orphaned valid lease");
}

#[test]
fn a_symlink_substituted_for_a_persisted_lease_is_rejected_before_git_writes() {
    let repo = GateRepo::new();
    repo.write("f", "dirty content whose objects must stay private\n");
    assert_ok(&repo.gate("preflight"), "recording the dirty preflight");
    let lease = repo.preflight_lease();
    fs::remove_dir_all(&lease).expect("removing the caller-owned lease");

    let external = repo.path().join("must-survive-lease-substitution");
    fs::create_dir(&external).expect("creating the external object target");
    fs::write(external.join("sentinel"), "untouched\n")
        .expect("seeding the external object target");
    std::os::unix::fs::symlink(&external, &lease)
        .expect("substituting a symlink for the persisted lease");

    let out = repo.gate("postlude");

    assert!(
        !out.status.success(),
        "a substituted lease must fail closed"
    );
    assert!(
        stderr(&out).contains("preflight state is invalid"),
        "the ownership failure must be named, got: {}",
        stderr(&out)
    );
    assert_eq!(
        fs::read_dir(&external)
            .expect("reading the external target")
            .map(|entry| entry.expect("reading an external entry").file_name())
            .collect::<Vec<_>>(),
        vec!["sentinel"],
        "Git must not write any object beneath the substituted target"
    );
    assert_eq!(
        fs::read_to_string(external.join("sentinel")).expect("reading the sentinel"),
        "untouched\n",
        "the external target must remain byte-for-byte unchanged"
    );
    assert!(
        !repo.preflight_state().exists(),
        "invalid state must be removed so the next run can recover"
    );
    assert!(
        !repo.path().join(".git/storyhook/gate-receipts").exists(),
        "a substituted lease must never produce a receipt"
    );
    assert!(
        lease.is_symlink(),
        "cleanup must leave the unproven filesystem entry untouched"
    );
}

#[test]
fn a_same_path_directory_replacement_is_not_treated_as_the_owned_lease() {
    let repo = GateRepo::new();
    repo.write("f", "dirty content whose objects must stay private\n");
    assert_ok(&repo.gate("preflight"), "recording the dirty preflight");
    let lease = repo.preflight_lease();
    fs::remove_dir_all(&lease).expect("removing the caller-owned lease");
    fs::create_dir(&lease).expect("replacing the lease at the same path");
    fs::write(lease.join("sentinel"), "untouched\n").expect("seeding the replacement directory");

    let out = repo.gate("postlude");

    assert!(
        !out.status.success(),
        "a same-path directory replacement must fail closed"
    );
    assert!(
        stderr(&out).contains("preflight state is invalid"),
        "the ownership failure must be named, got: {}",
        stderr(&out)
    );
    assert_eq!(
        fs::read_to_string(lease.join("sentinel")).expect("reading the replacement sentinel"),
        "untouched\n",
        "cleanup must not recursively delete an identity-mismatched directory"
    );
    assert!(
        !repo.path().join(".git/storyhook/gate-receipts").exists(),
        "an identity-mismatched lease must never produce a receipt"
    );
}

#[test]
fn a_failed_drift_diff_fails_closed_and_cleans_gate_state() {
    let repo = GateRepo::new();
    repo.write("f", "dirty before preflight\n");
    assert_ok(&repo.gate("preflight"), "recording the dirty preflight");
    repo.write("f", "different dirty content after preflight\n");

    let shim_dir = scratch_dir();
    let real_git = env::split_paths(&env::var_os("PATH").expect("PATH is set"))
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .expect("finding the real git binary");
    let shim = shim_dir.path().join("git");
    let real_git = real_git
        .to_str()
        .expect("the real git path must be UTF-8")
        .replace('\'', "'\"'\"'");
    fs::write(
        &shim,
        format!(
            "#!/usr/bin/env bash\nif [ \"${{1:-}}\" = diff ] && [ \"${{2:-}}\" = --name-only ]; then exit 42; fi\nexec '{real_git}' \"$@\"\n"
        ),
    )
    .expect("writing the git diff failure shim");
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755))
        .expect("making the git shim executable");
    let controlled_path = env::join_paths(
        std::iter::once(shim_dir.path().to_path_buf())
            .chain(env::split_paths(&env::var_os("PATH").expect("PATH is set"))),
    )
    .expect("building the controlled PATH");

    let out = repo.gate_with_path("postlude", &controlled_path);

    assert!(
        !out.status.success(),
        "a failed diagnostic diff must fail closed"
    );
    assert!(
        stderr(&out).contains("could not compare the preflight tree"),
        "the diff failure must be distinguished from an empty diagnostic, got: {}",
        stderr(&out)
    );
    assert!(
        repo.gate_artifacts().is_empty(),
        "the diff failure must clean preflight state and private objects"
    );
}

/// Both documented bypasses keep working — and the one the hook can see says so
/// out loud, because a silent bypass is the same defect class in a friendlier
/// costume.
#[test]
fn the_documented_bypasses_still_work_and_the_visible_one_is_loud() {
    let repo = GateRepo::new();
    assert_ok(&repo.gate("preflight"), "enrolling");

    let out = Command::new("git")
        .args(["push", "origin", "main"])
        .current_dir(repo.path())
        .env("SKIP_PREPUSH_TESTS", "1")
        .output()
        .expect("pushing with the documented bypass");

    assert_ok(&out, "SKIP_PREPUSH_TESTS=1 must still push");
    assert!(
        stderr(&out).to_lowercase().contains("bypass"),
        "a bypass must announce itself, got: {}",
        stderr(&out)
    );
    assert!(repo.remote_sha("main").is_some());

    // `--no-verify` is git's own bypass: the hook is never invoked at all, so
    // there is nothing for it to announce. Pinned because the carve-out is
    // named in CLAUDE.md and must not regress.
    let repo = GateRepo::new();
    assert_ok(&repo.gate("preflight"), "enrolling");
    assert_ok(&repo.push(&["--no-verify"]), "--no-verify must still push");
    assert!(repo.remote_sha("main").is_some());
}

/// The hole the tracked-hooks choice creates, pinned as behaviour rather than
/// left as folklore: an unenrolled clone is ungated, and enrollment is what
/// closes it. This is why enrollment is a step of `make test` and not a
/// documented ritual. Uses `main` (via the default `push`) rather than a
/// feature branch specifically because SH-429 only leaves a refusal on the
/// table for `main`/`master` — everywhere else, enrollment changes what is
/// *reported*, not whether the push succeeds.
#[test]
fn enrollment_turns_an_ungated_clone_into_a_gated_one() {
    let repo = GateRepo::new();

    // Before enrollment there is no gate at all — stated, not wished away.
    assert_ok(&repo.push(&[]), "an unenrolled clone pushes ungated");
    assert!(repo.remote_sha("main").is_some());

    repo.commit("f", "b\n", "second");
    assert_ok(&repo.gate("preflight"), "enrolling");

    let out = repo.push(&[]);
    assert!(
        !out.status.success(),
        "after enrollment the same push must be refused"
    );
}

/// A ref deletion ships no content, so it needs no receipt. Git signals it with
/// an all-zero local sha, and a hook that resolved `^{tree}` on that would fail
/// for the wrong reason.
#[test]
fn deleting_a_ref_needs_no_receipt() {
    let repo = GateRepo::new();
    repo.enroll_and_certify();
    assert_ok(&repo.push(&[]), "seeding the remote");
    repo.git(&["push", "origin", "main:doomed"]);
    assert!(
        repo.remote_sha("doomed").is_some(),
        "fixture: the ref to delete"
    );

    let out = repo.git(&["push", "origin", "--delete", "doomed"]);

    assert_ok(&out, "deleting a remote ref must not need a receipt");
    assert_eq!(repo.remote_sha("doomed"), None, "the ref should be gone");
}

/// Enrollment never clobbers a setting another tool owns — that would be the
/// same silent-breakage class this whole story is about, pointed at whoever
/// set it first.
#[test]
fn enrollment_refuses_to_overwrite_a_foreign_hooks_path() {
    let repo = GateRepo::new();
    repo.git(&["config", "core.hooksPath", "somebody-elses-hooks"]);

    let out = repo.gate("preflight");

    assert!(
        !out.status.success(),
        "preflight must refuse rather than overwrite a foreign core.hooksPath"
    );
    assert!(
        stderr(&out).contains("somebody-elses-hooks"),
        "the refusal must name the value it declined to overwrite, got: {}",
        stderr(&out)
    );
    let still = repo.git(&["config", "--get", "core.hooksPath"]);
    assert_eq!(
        String::from_utf8_lossy(&still.stdout).trim(),
        "somebody-elses-hooks",
        "the foreign value must survive"
    );
}

/// Per-commit greenness is a rule this repo states ("every commit passes `make
/// test`; history stays bisectable") and one the gate deliberately does not
/// enforce, because the blocking form costs one nine-minute suite per commit.
/// The compromise is that the hook *names* what it is letting through.
#[test]
fn a_pushed_commit_with_no_receipt_is_named_but_not_refused() {
    let repo = GateRepo::new();
    assert_ok(&repo.gate("preflight"), "enrolling");

    repo.commit("f", "b\n", "an intermediate commit nobody tested");
    repo.commit("f", "c\n", "the tip, which will be certified");
    // Both phases *after* the commits — the ordinary shape of "commit, then run
    // the gate on what you are about to push". Running the suite across the two
    // commits instead is what the drift refusal exists to reject, and it does.
    repo.enroll_and_certify();

    let out = repo.push(&[]);

    assert_ok(
        &out,
        "an unreceipted commit in the range must not block the push",
    );
    assert!(
        repo.remote_sha("main").is_some(),
        "the push must actually have shipped"
    );
    assert!(
        stderr(&out).contains("intermediate commit nobody tested")
            || stderr(&out).to_lowercase().contains("no receipt"),
        "the hook must name what it let through, got: {}",
        stderr(&out)
    );
}

// ---------------------------------------------------------------------------
// Derived fences — the wiring that makes the gate an invariant, not a habit
// ---------------------------------------------------------------------------

/// `make test` must enroll at the start, wrap its whole fallible body, and
/// certify at the very end. The receipt postlude being **last** is what makes
/// "no receipt unless every leg and orphan cleanup passed" true by make's own
/// fail-fast semantics — so a leg inserted after it would silently start
/// certifying failed runs.
#[test]
fn the_makefile_enrolls_first_and_certifies_last() {
    let makefile = std::fs::read_to_string(checkout().join("Makefile")).expect("reading Makefile");

    let body: Vec<&str> = makefile
        .lines()
        .skip_while(|l| !l.starts_with("test:"))
        .skip(1)
        .take_while(|l| l.starts_with('\t'))
        .collect();
    assert!(
        body.len() == 3,
        "the test target's recipe was not found — this fence is broken, not the Makefile"
    );

    assert!(
        body[0].contains("gate-receipt.sh preflight"),
        "the first line of `test` must enroll, got: {}",
        body[0]
    );
    assert!(
        body[1].contains("with-orphan-postlude.sh") && body[1].contains("_test-body"),
        "the middle line of `test` must wrap its fallible body, got: {}",
        body[1]
    );
    assert!(
        body[body.len() - 1].contains("gate-receipt.sh postlude"),
        "the LAST line of `test` must write the receipt, got: {}",
        body[body.len() - 1]
    );
}

/// `core.hooksPath` disables `$GIT_DIR/hooks` **wholesale**, and that is where
/// `story hooks install` puts the storyhook-managed hooks this repo dogfoods.
/// Enrolling without chaining to them would switch the tracker's own hooks off
/// silently — the fix causing the class it fixes. Derived from `src/hooks.rs`
/// so a fourth managed hook fails this test instead of going dark.
#[test]
fn every_managed_hook_has_a_chainer_in_the_tracked_hooks_directory() {
    let src =
        std::fs::read_to_string(checkout().join("src/hooks.rs")).expect("reading src/hooks.rs");

    let managed: Vec<String> = src
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("(\"")?;
            let name = rest.split('"').next()?;
            l.contains("_HOOK)").then(|| name.to_string())
        })
        .collect();

    assert!(
        managed.len() >= 3,
        "the HOOKS table scan found {} entries — the matcher is broken, not the table",
        managed.len()
    );

    for name in managed {
        let chainer: PathBuf = checkout().join(".githooks").join(&name);
        assert!(
            chainer.is_file(),
            ".githooks/{name} is missing: enrolling core.hooksPath would silently \
             disable the managed {name} hook this repo dogfoods"
        );
        let body = std::fs::read_to_string(&chainer).expect("reading the chainer");
        assert!(
            body.contains("--git-common-dir"),
            ".githooks/{name} must chain to the managed hook in the common dir"
        );
    }
}
