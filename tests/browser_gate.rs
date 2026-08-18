//! The browser tier's detection layer, **provoked** — not inspected.
//!
//! SH-418. SH-394 split the gates: `make test` protects `main` and skips the
//! Playwright suite; `make test-full` adds it and protects a release. Nothing
//! then ran the release tier *between* releases, so a dashboard regression
//! could merge and sit red until somebody tried to cut a release — SH-416 is
//! the measured case. The state of this machine when SH-418 was filed makes
//! the point better than any argument: **109 receipts in the store, zero
//! carrying `tier full`.**
//!
//! The fix is a producer (`scripts/browser-watch.sh`) and a reader
//! (`scripts/browser-status.sh`). This file covers the reader exhaustively,
//! because the reader holds the entire decision: `browser-watch.sh` asks it
//! whether the browser tier needs to run and obeys the answer. That split is
//! deliberate — `scripts/merge-watch.sh`'s header explains why thin `gh`
//! orchestration carries no automated test (mocking behaviour validates the
//! mock, not the integration — SH-263, SH-345), and the lesson taken from it
//! here was to keep the *decision* out of the untestable layer rather than to
//! accept a second untested decision.
//!
//! # Real git, and receipts written by the production writer
//!
//! Every receipt here is written by `scripts/gate-receipt.sh` itself, never
//! hand-forged, exactly as `tests/merge_gate.rs` and `tests/push_gate.rs`
//! require. A hand-written receipt would prove this reader's file format
//! rather than the producer's behaviour, and the tier line is the whole
//! subject.
//!
//! # The discrimination that matters
//!
//! `.githooks/pre-push` and `scripts/merge-preflight.sh` accept **either**
//! tier — that is the entire point of SH-394's split, and nothing here
//! changes it. This reader is the first thing that ever asks the stronger
//! question, so the load-bearing case is
//! [`a_gate_tier_receipt_on_the_tip_does_not_read_as_browser_certified`]: a
//! `gate` receipt satisfying every existing gate must still read as "the
//! browser suite has never certified this".
//!
//! # Positive control (SH-364: an oracle nobody has tested is blind)
//!
//! [`a_full_tier_receipt_on_the_tip_reads_as_current`] is what stops every
//! `never`/`behind` assertion here from passing vacuously. If the fixture's
//! certification path silently stopped producing a readable `full` receipt,
//! that test fails loudly, rather than the negative cases all continuing to
//! report `never` for the wrong reason.
//!
//! # Mutation-checked (SH-295: a pin that cannot fail is not a pin)
//!
//! Run by hand against this suite before it was committed:
//!
//! - `browser-status.sh`'s tier comparison loosened from `= "full"` to
//!   "a receipt exists" → **3 of 10 red**,
//!   `a_gate_tier_receipt_on_the_tip_does_not_read_as_browser_certified`,
//!   `a_receipt_written_before_the_tier_line_existed_reads_as_gate`, and
//!   `the_pass_a_poller_would_run_is_the_full_tier_not_the_merge_gate` —
//!   the third because a poller whose reader accepts any receipt plans no
//!   work on a tree the merge gate alone certified, which is precisely the
//!   silent non-run this story exists to end.
//! - `--first-parent` dropped from the history walk → **1 of 10 red**,
//!   `a_tree_that_was_never_mains_own_does_not_count`.
//! - `browser-watch.sh`'s command array changed to `make test` → **1 of 10
//!   red**, `the_pass_a_poller_would_run_is_the_full_tier_not_the_merge_gate`.

use std::path::Path;
use std::process::{Command, Output};

use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

/// The checkout under test — the tracked scripts live here, never a copy.
fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// A repository with `main` at one commit, ready to grow history.
struct HistoryRepo {
    dir: TempDir,
}

impl HistoryRepo {
    fn new() -> Self {
        let repo = Self { dir: scratch_dir() };

        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "t@t"]);
        repo.git(&["config", "user.name", "t"]);

        // The tracked hooks directory, symlinked rather than copied, so
        // `gate-receipt.sh preflight`'s executable-hook check passes the same
        // way it does in a real checkout — certifying a tree here goes through
        // the real enrol-then-postlude path.
        std::os::unix::fs::symlink(checkout().join(".githooks"), repo.path().join(".githooks"))
            .expect("fixture: linking the tracked hooks directory");

        repo.commit("f", "base\n", "init");
        repo
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn git(&self, args: &[&str]) -> Output {
        run(self.path(), "git", args)
    }

    /// Writes `body` to `name`, commits it, and returns the new commit's sha.
    fn commit(&self, name: &str, body: &str, message: &str) -> String {
        std::fs::write(self.path().join(name), body).expect("fixture: writing a tracked file");
        assert_ok(&self.git(&["add", name]), "fixture: staging");
        assert_ok(
            &self.git(&["commit", "-qm", message]),
            "fixture: committing",
        );
        self.rev_parse("HEAD")
    }

    fn rev_parse(&self, rev: &str) -> String {
        let out = self.git(&["rev-parse", rev]);
        assert_ok(&out, &format!("fixture: rev-parse {rev}"));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Certifies whatever is checked out right now, at `tier`, through the
    /// production writer — never a hand-forged receipt.
    fn certify(&self, tier: &str) {
        let script = checkout()
            .join("scripts/gate-receipt.sh")
            .display()
            .to_string();
        assert_ok(
            &run(self.path(), "bash", &[&script, "preflight"]),
            "fixture: enrolling",
        );
        assert_ok(
            &run(self.path(), "bash", &[&script, "postlude", tier]),
            "fixture: writing the receipt",
        );
    }

    /// Strips the `tier` line from a receipt, reproducing one written before
    /// SH-394 added it. The receipt is still the production writer's — only
    /// the line that did not exist then is removed.
    fn strip_tier_line(&self, tree: &str) {
        let receipt = self.path().join(".git/storyhook/gate-receipts").join(tree);
        let body = std::fs::read_to_string(&receipt).expect("fixture: reading the receipt");
        let kept: String = body
            .lines()
            .filter(|l| !l.starts_with("tier "))
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(
            kept.len() < body.len(),
            "fixture: the receipt had no tier line to strip"
        );
        std::fs::write(&receipt, kept).expect("fixture: rewriting the receipt");
    }

    fn status(&self, git_ref: &str) -> Output {
        run(
            self.path(),
            "bash",
            &[
                &checkout()
                    .join("scripts/browser-status.sh")
                    .display()
                    .to_string(),
                git_ref,
            ],
        )
    }

    fn watch_plan(&self, git_ref: &str) -> Output {
        run(
            self.path(),
            "bash",
            &[
                &checkout()
                    .join("scripts/browser-watch.sh")
                    .display()
                    .to_string(),
                "--plan",
                "--ref",
                git_ref,
            ],
        )
    }
}

fn run(cwd: &Path, program: &str, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        // A script under test must not inherit git's own targeting variables
        // from the test runner's environment — the same scrub
        // `tests/merge_gate.rs` and `tests/push_gate.rs` apply.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
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
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// The value of `key=` on stdout, or `None` if the script did not report it.
fn field(out: &Output, key: &str) -> Option<String> {
    stdout(out)
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")).map(str::to_string))
}

fn field_or_panic(out: &Output, key: &str) -> String {
    field(out, key).unwrap_or_else(|| {
        panic!(
            "expected a `{key}=` line\nstdout: {}\nstderr: {}",
            stdout(out),
            stderr(out)
        )
    })
}

// ---------------------------------------------------------------------------
// The reader
// ---------------------------------------------------------------------------

/// The state SH-418 was filed in, and the honest reading of it: a history
/// nothing has ever browser-certified is `never`, never a quiet pass.
#[test]
fn an_empty_receipt_store_reads_as_never_and_names_the_distance() {
    let repo = HistoryRepo::new();
    repo.commit("g", "one\n", "second");

    let out = repo.status("main");

    assert_eq!(out.status.code(), Some(2), "never must exit 2");
    assert_eq!(field_or_panic(&out, "state"), "never");
    assert_eq!(
        field_or_panic(&out, "behind"),
        "2",
        "both first-parent commits are behind the last green"
    );
    assert!(
        stderr(&out).contains("never"),
        "the human line must say so: {}",
        stderr(&out)
    );
}

/// The positive control, and the whole point of the `tier` line: a real
/// `full` receipt on the tip must read as current. Without this passing,
/// every `never` assertion in this file could be passing for the wrong
/// reason (SH-364).
#[test]
fn a_full_tier_receipt_on_the_tip_reads_as_current() {
    let repo = HistoryRepo::new();
    repo.commit("g", "one\n", "second");
    repo.certify("full");

    let out = repo.status("main");

    assert_eq!(out.status.code(), Some(0), "current must exit 0");
    assert_eq!(field_or_panic(&out, "state"), "current");
    assert_eq!(field_or_panic(&out, "behind"), "0");
    assert_eq!(
        field_or_panic(&out, "certified_tree"),
        field_or_panic(&out, "tip_tree"),
        "the certified tree is the tip's own"
    );
}

/// The load-bearing discrimination. A `gate` receipt satisfies
/// `.githooks/pre-push` and `scripts/merge-preflight.sh` — deliberately, that
/// is SH-394's whole design — and must still read here as "the browser suite
/// has certified nothing".
#[test]
fn a_gate_tier_receipt_on_the_tip_does_not_read_as_browser_certified() {
    let repo = HistoryRepo::new();
    repo.commit("g", "one\n", "second");
    repo.certify("gate");

    let out = repo.status("main");

    assert_eq!(
        out.status.code(),
        Some(2),
        "a gate receipt is not a browser certification\nstdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
    assert_eq!(field_or_panic(&out, "state"), "never");
    assert_eq!(
        field(&out, "certified"),
        None,
        "nothing may be reported as certified"
    );
}

/// A receipt written before SH-394 added the `tier` line carried no claim
/// about the browser suite either way. It is read as `gate` on purpose:
/// guessing generously would make the oldest, least recoverable receipts the
/// only ones that ever read as `full`.
#[test]
fn a_receipt_written_before_the_tier_line_existed_reads_as_gate() {
    let repo = HistoryRepo::new();
    let tip = repo.commit("g", "one\n", "second");
    repo.certify("full");
    let tree = repo.rev_parse(&format!("{tip}^{{tree}}"));
    repo.strip_tier_line(&tree);

    let out = repo.status("main");

    assert_eq!(out.status.code(), Some(2), "a tierless receipt is not full");
    assert_eq!(field_or_panic(&out, "state"), "never");
}

/// Distance, exactly counted — the reading that grows while a poller is dead,
/// a `main` is red, or nobody has run the suite since Tuesday.
#[test]
fn a_certified_ancestor_reports_the_exact_first_parent_distance() {
    let repo = HistoryRepo::new();
    let green = repo.commit("g", "one\n", "second");
    repo.certify("full");
    repo.commit("h", "two\n", "third");
    repo.commit("i", "three\n", "fourth");

    let out = repo.status("main");

    assert_eq!(out.status.code(), Some(1), "behind must exit 1");
    assert_eq!(field_or_panic(&out, "state"), "behind");
    assert_eq!(field_or_panic(&out, "behind"), "2");
    assert_eq!(field_or_panic(&out, "certified"), green);
    assert_ne!(
        field_or_panic(&out, "certified_at"),
        "unknown",
        "the age of the last green run must be reportable"
    );
}

/// `--first-parent` is not a detail. A commit a merge brought in was never
/// `main`'s own content, and certifying it says nothing about any tree `main`
/// ever had — which is SH-396's finding (a merge tree matches neither parent)
/// read from the other end.
#[test]
fn a_tree_that_was_never_mains_own_does_not_count() {
    let repo = HistoryRepo::new();
    let base = repo.rev_parse("main");

    assert_ok(
        &repo.git(&["checkout", "-q", "-b", "side", &base]),
        "fixture: branching",
    );
    let side = repo.commit("s", "side\n", "on the side branch");
    repo.certify("full");
    assert_ok(
        &repo.git(&["checkout", "-q", "main"]),
        "fixture: back to main",
    );
    // `main` has to move too, or the merge would produce the side branch's
    // own tree byte-for-byte and the receipt would legitimately cover it —
    // which is a true fact about content-addressed receipts, and not the
    // thing this test is about.
    repo.commit("m", "mainline\n", "on main");
    assert_ok(
        &repo.git(&["merge", "-q", "--no-ff", "-m", "merge side", &side]),
        "fixture: merging the side branch",
    );

    let out = repo.status("main");

    assert_eq!(
        out.status.code(),
        Some(2),
        "a certified commit off the first-parent line must not count\nstdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
    assert_eq!(field_or_panic(&out, "state"), "never");
    assert_eq!(
        field_or_panic(&out, "scanned"),
        "3",
        "the walk sees the merge, main's own commit and the base — never the side commit"
    );
}

/// A ref that does not resolve is refused, loudly, rather than silently
/// reading as "never" — a reader that answers confidently about a ref it
/// could not find is the SH-306 shape in miniature.
#[test]
fn a_ref_that_does_not_resolve_is_refused_rather_than_read_as_never() {
    let repo = HistoryRepo::new();

    let out = repo.status("origin/nonexistent");

    assert_eq!(out.status.code(), Some(3), "an unresolvable ref exits 3");
    assert!(
        stderr(&out).contains("does not resolve"),
        "the refusal must name the problem: {}",
        stderr(&out)
    );
}

// ---------------------------------------------------------------------------
// The producer's decision
// ---------------------------------------------------------------------------

/// The one fact about `browser-watch.sh` that a test can pin without mocking
/// `make`: the command it would run is the FULL tier. `--plan` prints the
/// same array the run path executes, so this cannot drift from what runs.
#[test]
fn the_pass_a_poller_would_run_is_the_full_tier_not_the_merge_gate() {
    let repo = HistoryRepo::new();
    repo.commit("g", "one\n", "second");
    repo.certify("gate");

    let out = repo.watch_plan("main");

    assert_ok(&out, "planning a pass");
    assert_eq!(field_or_panic(&out, "decision"), "run");
    assert_eq!(
        field_or_panic(&out, "command"),
        "make test-full",
        "a poller that ran the merge gate would certify the wrong tier forever"
    );
    assert_eq!(field_or_panic(&out, "state"), "never");
}

/// The idempotent half: an already-certified tip is not re-run. This is what
/// makes the trigger free to evaluate on any cadence, and what coalesces a
/// burst of merges into one run against the newest tip.
#[test]
fn an_already_certified_tip_plans_no_work() {
    let repo = HistoryRepo::new();
    repo.commit("g", "one\n", "second");
    repo.certify("full");

    let out = repo.watch_plan("main");

    assert_ok(&out, "planning a pass");
    assert_eq!(field_or_panic(&out, "decision"), "none");
    assert_eq!(
        field(&out, "command"),
        None,
        "nothing may be proposed for a tree that already passed"
    );
}

/// SH-357's doctrine, which this project applies one guard ahead of every
/// verb: an argument that lands nowhere is refused, never dropped.
#[test]
fn an_unknown_argument_is_refused_rather_than_ignored() {
    let repo = HistoryRepo::new();

    let out = run(
        repo.path(),
        "bash",
        &[
            &checkout()
                .join("scripts/browser-watch.sh")
                .display()
                .to_string(),
            "--plan",
            "--ref",
            "main",
            "junk",
        ],
    );

    assert_eq!(out.status.code(), Some(2), "an unknown argument is refused");
    assert!(
        stderr(&out).contains("junk"),
        "the refusal must name the offending word: {}",
        stderr(&out)
    );
}
