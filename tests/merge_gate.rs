//! `scripts/merge-preflight.sh`, **provoked** — not inspected.
//!
//! SH-396. `main` was red for 73 minutes because PR #484 merged two
//! independently green branches — SH-315's attachment CLI and the exhaustive
//! `Invocation` match `tests/unassessed_priority_paths.rs` runs over it —
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
//! # What this suite deliberately does not claim
//!
//! `scripts/merge-watch.sh` — the reconcile pass that calls this script per
//! open PR and posts the result to GitHub — has no automated test here. Its
//! own header explains why: it is thin orchestration over already-tested
//! primitives (this script, and `gate-receipt.sh` via a plain `make test`)
//! plus real GitHub API calls, and mocking `gh`'s behaviour would validate
//! the mock rather than the integration — this project has twice paid for
//! exactly that gap (SH-263, SH-345). Verified by hand against this repo's
//! own live PRs instead.

use std::path::Path;
use std::process::{Command, Output};

use storyhook_test_support::scratch_dir;
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

// ---------------------------------------------------------------------------
// The provocation
// ---------------------------------------------------------------------------

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
