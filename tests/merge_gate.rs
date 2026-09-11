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
//!
//! # The landing-convergence boundary
//!
//! SH-604 keeps GitHub outside the deterministic seam for the same reason.
//! Tests supply the wire-shaped result of the authoritative refresh, then let
//! `verify-pr.sh` fetch from a real local remote, recompute through the
//! production preflight, and read receipts written by the production writer.
//! This proves that an already-merged PR still needs actual-tree certification,
//! while a changed base can request a bounded retry without granting its new
//! tree the old tree's receipt.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};

use storyhook_test_support::{ChildGuard, scratch_dir};
use tempfile::TempDir;

/// A fetch during verification must not restore newer bytes under the old
/// shared index when private Git administration is detached at gate exit.
#[test]
fn speculative_run_preserves_clean_shared_poller_when_base_ref_advances() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("candidate", "main", "candidate-file", "candidate\n");
    let next_base = repo.branch("next-base", "main", "f", "new base bytes\n");
    let later_base = repo.branch("later-base", "next-base", "f", "later base bytes\n");
    let base_ref = "refs/remotes/origin/main";
    assert_ok(
        &repo.git(&["update-ref", base_ref, &base]),
        "publishing old base",
    );
    let expected_tree = stdout(&repo.preflight(base_ref, &head));
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");
    let result = repo.speculative_run(
        &expected_tree,
        base_ref,
        &head,
        &poller,
        &["git", "update-ref", base_ref, &next_base],
    );
    assert_ok(&result, "gate updates only a shared remote-tracking ref");
    assert_eq!(repo.rev_parse(base_ref), next_base);
    let first_status = stdout(&run(&poller, "git", &["status", "--porcelain"]));
    assert_ok(
        &repo.git(&["update-ref", base_ref, &later_base]),
        "advancing base before next verification",
    );
    let next_tree = stdout(&repo.preflight(base_ref, &head));
    let retry = repo.speculative_run(&next_tree, base_ref, &head, &poller, &["true"]);
    assert_ok(
        &retry,
        "next verification must not fail restoring a shared poller dirtied by the prior run",
    );
    assert_eq!(
        first_status, "",
        "restoring the private checkout against a moving ref must not leave changed bytes beneath the original shared index",
    );
    assert_eq!(
        fs::read_to_string(poller.join("f")).unwrap(),
        "later base bytes\n"
    );
}

/// A dirty poller is infrastructure evidence; candidate tests never start.
#[test]
fn verifier_distinguishes_poller_preparation_failure_from_test_failure() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("candidate", "main", "candidate-file", "candidate\n");
    let next_base = repo.branch("next-base", "main", "f", "new base bytes\n");
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");
    fs::write(poller.join("f"), "unclassified local edits\n").unwrap();
    let expected_tree = stdout(&repo.preflight(&next_base, &head));
    let result = repo.verification_gate(
        &expected_tree,
        &next_base,
        &head,
        &poller,
        &["touch", "gate-started"],
    );
    assert_ok(&result, "verifier emits classified JSON");
    assert!(!poller.join("gate-started").exists());
    assert_eq!(
        fs::read_to_string(poller.join("f")).unwrap(),
        "unclassified local edits\n"
    );
    let payload: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(payload["result"], "infrastructure-failure", "{payload}");
    assert_eq!(payload["disposition"], "permanent");
    assert!(
        payload["detail"]
            .as_str()
            .unwrap()
            .contains("Verification infrastructure failure")
    );
    assert!(
        payload["detail"]
            .as_str()
            .unwrap()
            .contains("Candidate test status: unknown")
    );
    assert!(
        payload["detail"]
            .as_str()
            .unwrap()
            .contains(payload["log"].as_str().unwrap())
    );
    assert!(
        payload["detail"]
            .as_str()
            .unwrap()
            .contains("could not restore")
    );

    // A separate clean poller proves the same path still identifies a real
    // failing gate, and only reports success for a successful gate.
    let clean_container = repo.poller(&next_base);
    let clean = clean_container.path().join("poller");
    for (command, expected) in [("exit 7", "tests-failed"), ("exit 0", "gate-passed")] {
        let outcome = repo.verification_gate(
            &expected_tree,
            &next_base,
            &head,
            &clean,
            &["bash", "-c", command],
        );
        assert_ok(&outcome, "classifying an actual completed gate");
        let result: serde_json::Value = serde_json::from_slice(&outcome.stdout).unwrap();
        assert_eq!(result["result"], expected, "{result}");
    }
}

/// The SH-607 production incident had one decisive failure followed by 611
/// unknown outcomes. A tail cannot recover the failure from that ordering.
#[test]
fn verifier_names_a_failure_before_hundreds_of_not_rerun_entries() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("candidate", "main", "candidate-file", "candidate\n");
    let tree = stdout(&repo.preflight(&base, &head));
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");
    let command = r#"
printf 'test-delta: newly RED (1):\n'
printf '  orphan_check::postlude_fails_when_a_survivor_outlives_sigkill\n'
printf 'test-delta: not re-run since the comparison ledger -- status unknown, not assumed green (611):\n'
i=1
while [ "$i" -le 611 ]; do
    printf '  web_test::not_rerun_%03d (was PASS)\n' "$i"
    i=$((i + 1))
done
exit 101
"#;

    let outcome = repo.verification_gate(&tree, &base, &head, &poller, &["bash", "-c", command]);

    assert_ok(&outcome, "summarizing the reproduced verification failure");
    let payload: serde_json::Value = serde_json::from_slice(&outcome.stdout).unwrap();
    assert_eq!(payload["result"], "tests-failed", "{payload}");
    let detail = payload["detail"].as_str().unwrap();
    assert!(
        detail.contains("Failed tests (1)")
            && detail.contains("orphan_check::postlude_fails_when_a_survivor_outlives_sigkill"),
        "the decisive failure must survive independently of the tail: {detail}"
    );
    assert!(
        detail.contains("Not re-run (611): status unknown; not counted as pass or failure"),
        "unknown prior outcomes need their own classification: {detail}"
    );
    assert!(
        detail.contains("Ancillary context — last 40 of 615 log lines (575 earlier lines omitted)"),
        "the tail's bounded and ancillary nature must be explicit: {detail}"
    );
    assert_eq!(
        payload["log"].as_str().map(Path::new).map(Path::exists),
        Some(true),
        "the full log must remain referenced"
    );
}

#[test]
fn verifier_bounds_each_diagnostic_class_without_changing_its_meaning() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("candidate", "main", "candidate-file", "candidate\n");
    let tree = stdout(&repo.preflight(&base, &head));
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");
    let command = r#"
printf 'leg fmt: REUSED — relevant tracked inputs and command are unchanged\n'
printf 'leg clippy: REUSED — relevant tracked inputs and command are unchanged\n'
printf '     Running tests/diagnostics.rs (target/debug/deps/diagnostics-fixture)\n'
i=1
while [ "$i" -le 25 ]; do
    printf 'test failure_%02d ... FAILED\n' "$i"
    i=$((i + 1))
done
i=1
while [ "$i" -le 12 ]; do
    printf 'error[E%04d]: compiler diagnostic %02d\n' "$i" "$i"
    i=$((i + 1))
done
printf 'test-delta: not re-run since the comparison ledger -- status unknown, not assumed green (300):\n'
exit 101
"#;

    let outcome = repo.verification_gate(&tree, &base, &head, &poller, &["bash", "-c", command]);

    assert_ok(&outcome, "summarizing bounded verification diagnostics");
    let payload: serde_json::Value = serde_json::from_slice(&outcome.stdout).unwrap();
    let detail = payload["detail"].as_str().unwrap();
    let summary = detail
        .split("\nAncillary context")
        .next()
        .expect("every failure detail starts with its summary");
    assert!(
        summary.contains("Failed tests (25; showing first 20)"),
        "{detail}"
    );
    assert!(summary.contains("diagnostics::failure_20"), "{detail}");
    assert!(!summary.contains("diagnostics::failure_21"), "{detail}");
    assert!(
        summary.contains("5 additional failed tests omitted"),
        "{detail}"
    );
    assert!(
        summary.contains("Compiler/build diagnostics (12; showing first 10)"),
        "{detail}"
    );
    assert!(summary.contains("compiler diagnostic 10"), "{detail}");
    assert!(!summary.contains("compiler diagnostic 11"), "{detail}");
    assert!(
        summary.contains("2 additional compiler/build diagnostics omitted"),
        "{detail}"
    );
    assert!(detail.contains("Reused/cached legs (2)"), "{detail}");
    assert!(
        detail.contains("Not re-run (300): status unknown; not counted as pass or failure"),
        "{detail}"
    );
}

#[test]
fn verifier_reports_a_failed_gate_without_inventing_a_diagnosis() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("candidate", "main", "candidate-file", "candidate\n");
    let tree = stdout(&repo.preflight(&base, &head));
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");

    let outcome = repo.verification_gate(&tree, &base, &head, &poller, &["bash", "-c", "exit 9"]);

    assert_ok(&outcome, "summarizing a gate with no diagnostic output");
    let payload: serde_json::Value = serde_json::from_slice(&outcome.stdout).unwrap();
    assert_eq!(payload["result"], "tests-failed", "{payload}");
    let detail = payload["detail"].as_str().unwrap();
    assert!(
        detail.contains("The completed gate failed with exit status 9"),
        "{detail}"
    );
    assert!(
        detail.contains("No failed test or compiler/build diagnostic was recognized"),
        "{detail}"
    );
}

/// The verifier takes one machine-wide gate around the complete speculative
/// command, so its timeout contains at most one lock wait plus one gate run.
#[test]
fn verifier_holds_the_gate_across_the_complete_speculative_run() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("candidate", "main", "candidate-file", "candidate\n");
    let tree = stdout(&repo.preflight(&base, &head));
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");

    let outcome = repo.verification_gate(
        &tree,
        &base,
        &head,
        &poller,
        &[
            "bash",
            "-c",
            // `--held` asked from INSIDE the speculative checkout: the poller
            // worktree's swapped gitlink resolves the repository's own common
            // dir, so the key the inner `run-tests.sh` take derives is the one
            // the outer `verify-pr.sh` hold recorded (SH-648) — the fact the
            // reentrancy invariant now rests on.
            &format!(
                "bash '{}' --held gate || exit 99; [ -z \"${{STORYHOOK_GATE_PROGRESS_ACTIVITY_PATH:-}}\" ] || exit 98; printf gate-stdout; printf gate-stderr >&2",
                checkout().join("scripts/machine-lock.sh").display()
            ),
        ],
    );

    assert_ok(&outcome, "running the centralized verification gate");
    let payload: serde_json::Value = serde_json::from_slice(&outcome.stdout).unwrap();
    assert_eq!(payload["result"], "gate-passed", "{payload}");
    let journal = fs::read_dir(repo.path().join("activity"))
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    let rows: Vec<serde_json::Value> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for stream in ["stdout", "stderr"] {
        assert!(
            rows.iter().any(|row| row["source"] == "merge-watch.sh"
                && row["stream"] == stream
                && row["message"] == format!("gate-{stream}")),
            "gate stream missing from activity: {journal}"
        );
    }
    let progress = fs::read_to_string(repo.path().join("gate-progress.ndjson")).unwrap();
    assert!(
        progress.contains(
            r#"{"kind":"activity","path":"release gate","label":"waiting for gate lock","status":"running""#
        ),
        "the lock wait must be visible before the speculative command: {progress}"
    );
    assert!(
        progress.contains(
            r#"{"kind":"activity","path":"release gate","label":"waiting for gate lock","status":"passed""#
        ),
        "the activity must terminate after acquisition: {progress}"
    );
}

#[test]
fn same_tree_verification_attempts_keep_distinct_logs() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("candidate", "main", "candidate-file", "candidate\n");
    let tree = stdout(&repo.preflight(&base, &head));
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");

    for marker in ["first-attempt", "second-attempt"] {
        let outcome = repo.verification_gate(&tree, &base, &head, &poller, &["printf", marker]);
        assert_ok(&outcome, "running a same-tree verification attempt");
    }

    let logs = fs::read_dir(repo.common_dir().join("storyhook/verification-logs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(
        logs.len(),
        2,
        "each attempt needs its own evidence: {logs:?}"
    );
    let contents = logs
        .iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect::<Vec<_>>();
    assert!(contents.iter().any(|text| text.contains("first-attempt")));
    assert!(contents.iter().any(|text| text.contains("second-attempt")));
}

/// Neither pre-existing nor gate-created tracked edits may be discarded or
/// accidentally certified because checkout carries unchanged paths forward.
#[test]
fn verifier_preserves_tracked_edits_before_and_during_the_gate() {
    for during_gate in [false, true] {
        for staged in [false, true] {
            let repo = MergeRepo::new();
            let base = repo.rev_parse("main");
            let head = repo.branch("candidate", "main", "candidate-file", "candidate\n");
            let tree = stdout(&repo.preflight(&base, &head));
            let poller_container = repo.poller(&base);
            let poller = poller_container.path().join("poller");
            let original_gitlink = fs::read(poller.join(".git")).unwrap();
            if !during_gate {
                fs::write(poller.join("f"), "preserve these edits\n").unwrap();
                if staged {
                    assert_ok(&run(&poller, "git", &["add", "f"]), "stage existing edits");
                }
            }
            let command = if during_gate {
                if staged {
                    "touch gate-started; printf 'preserve these edits\\n' > f; git add f"
                } else {
                    "touch gate-started; printf 'preserve these edits\\n' > f"
                }
            } else {
                "touch gate-started"
            };
            let result =
                repo.verification_gate(&tree, &base, &head, &poller, &["bash", "-c", command]);
            assert_ok(&result, "classify tracked edits");
            let payload: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(
                payload["result"], "infrastructure-failure",
                "during_gate={during_gate}, staged={staged}: {payload}"
            );
            let evidence = if during_gate {
                let recovery = fs::read_dir(repo.common_dir().join("storyhook"))
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| {
                        path.file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with("verification-recovery-")
                    })
                    .expect("retain the complete damaged verifier unit");
                assert!(recovery.join("lease/.git/index").is_file());
                assert!(recovery.join("admin/HEAD").is_file());
                assert!(
                    !poller.exists(),
                    "retained files must not masquerade as a usable checkout"
                );
                recovery.join("worktree")
            } else {
                assert_eq!(fs::read(poller.join(".git")).unwrap(), original_gitlink);
                poller.clone()
            };
            assert_eq!(
                fs::read_to_string(evidence.join("f")).unwrap(),
                "preserve these edits\n"
            );
            assert_eq!(evidence.join("gate-started").exists(), during_gate);
        }
    }
}

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
        storyhook_test_support::approve_fixture_identity(repo.path(), "t", "t@t");

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

    fn linked_worktree(&self, name: &str, base: &str) -> TempDir {
        let container = scratch_dir();
        let worktree = container.path().join(name);
        let out = self.git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            name,
            &worktree.display().to_string(),
            base,
        ]);
        assert_ok(&out, &format!("creating the {name} linked worktree"));
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

    fn verification_gate(
        &self,
        expected_tree: &str,
        base: &str,
        head: &str,
        poller: &Path,
        command: &[&str],
    ) -> Output {
        let mut args = vec![
            checkout()
                .join("scripts/verify-pr.sh")
                .display()
                .to_string(),
            "--run-gate".to_string(),
            "668".to_string(),
            expected_tree.to_string(),
            base.to_string(),
            head.to_string(),
            poller.display().to_string(),
            "--".to_string(),
        ];
        args.extend(command.iter().map(|arg| (*arg).to_string()));
        Command::new("bash")
            .args(&args)
            .current_dir(self.path())
            .env("STORYHOOK_LOCK_DIR", self.path().join("locks"))
            .env("STORYHOOK_ACTIVITY_LOG_DIR", self.path().join("activity"))
            .env("STORYHOOK_VERIFIER_MIRROR", "0")
            .env(
                "STORYHOOK_GATE_PROGRESS",
                self.path().join("gate-progress.ndjson"),
            )
            .env_remove("STORYHOOK_MACHINE_LOCKS")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .output()
            .expect("running the centralized verification gate")
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

    /// Publishes the current local branches through a real local Git remote,
    /// including the pull-request ref shape GitHub exposes to `git fetch`.
    fn publish_origin(&self, pr: u64, head: &str) {
        assert_ok(
            &self.git(&["update-ref", &format!("refs/pull/{pr}/head"), head]),
            "publishing the pull-request ref",
        );
        assert_ok(
            &self.git(&[
                "remote",
                "add",
                "origin",
                &self.path().display().to_string(),
            ]),
            "adding the local origin",
        );
    }

    /// Installs a fake `gh` at `<repo>/bin/gh` for the public path (SH-637).
    ///
    /// It answers `pr view <pr> --json <fields>` from `$FAKE_GH_STATE/pr.json`,
    /// selecting exactly the fields the script asked for — a field the file
    /// lacks is an error, as the real `gh` errors on an unknown field — and it
    /// counts calls in `$FAKE_GH_STATE/calls`, running
    /// `$FAKE_GH_STATE/before-call-<N>.sh` first when one exists. That hook is
    /// the "head moves between fetch and verdict" instrument: with
    /// `refresh_submission_refs` passing the entry read through, call 1 is the
    /// entry read and call 2 is the verdict recheck. This is not a GitHub
    /// model: it returns the same wire shape the private seams take as an
    /// argument, one door over, because the defect under test lives in the
    /// public path above every seam and its WIRING is the property.
    fn fake_gh(&self) -> PathBuf {
        let bin = self.path().join("bin");
        fs::create_dir_all(&bin).expect("fixture: bin directory");
        let state = self.path().join("fake-gh-state");
        fs::create_dir_all(&state).expect("fixture: fake gh state");
        let script = bin.join("gh");
        fs::write(
            &script,
            r##"#!/usr/bin/env bash
set -uo pipefail
state="${FAKE_GH_STATE:?fake gh: FAKE_GH_STATE names the state directory}"
calls=$(( $(cat "$state/calls" 2>/dev/null || echo 0) + 1 ))
printf '%s\n' "$calls" > "$state/calls"
printf '%s\n' "$*" >> "$state/argv"
if [ -x "$state/before-call-$calls.sh" ]; then
    "$state/before-call-$calls.sh" || { echo "fake gh: before-call-$calls hook failed" >&2; exit 70; }
fi
if [ "${1:-}" != pr ] || [ "${2:-}" != view ] || [ "${4:-}" != --json ] || [ -z "${5:-}" ]; then
    echo "fake gh: unsupported invocation: $*" >&2
    exit 64
fi
jq -e --arg fields "$5" '
  . as $pr
  | ($fields | split(",")) as $names
  | ($names | map(select(. as $n | ($pr | has($n)) | not))) as $missing
  | if ($missing | length) > 0
    then error("fake gh: pr.json lacks " + ($missing | join(",")))
    else reduce $names[] as $n ({}; . + {($n): $pr[$n]})
    end
' "$state/pr.json"
"##,
        )
        .expect("fixture: writing the fake gh");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
            .expect("fixture: fake gh executable");
        state
    }

    /// Writes what the fake `gh` answers for the PR from now on.
    fn fake_gh_answers(&self, metadata: &str) {
        fs::write(self.path().join("fake-gh-state/pr.json"), metadata)
            .expect("fixture: fake gh answer");
    }

    /// Runs `body` (bash) before the fake `gh`'s Nth call answers.
    fn fake_gh_before_call(&self, call: u32, body: &str) {
        let hook = self
            .path()
            .join(format!("fake-gh-state/before-call-{call}.sh"));
        fs::write(
            &hook,
            format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}\n"),
        )
        .expect("fixture: fake gh hook");
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))
            .expect("fixture: fake gh hook executable");
    }

    /// How many times the public path asked the fake `gh` about the PR.
    fn fake_gh_calls(&self) -> u32 {
        fs::read_to_string(self.path().join("fake-gh-state/calls"))
            .map(|calls| calls.trim().parse().expect("a call count"))
            .unwrap_or(0)
    }

    /// Installs a fake `make` at `<repo>/bin/make` — the gate `verify_public`
    /// hands the public path is `make test`, and it resolves through `PATH`
    /// inside the poller worktree (`merge-watch.sh --speculative-run`). The
    /// body must not touch tracked files there, or the poller restore turns
    /// the run into an infrastructure failure rather than a red.
    fn fake_make(&self, body: &str) {
        self.fake_gate("make", body);
    }

    /// Installs a fake gate executable at `<repo>/bin/<name>` (SH-649): the
    /// public path runs whatever argv it is handed, resolved through `PATH`
    /// inside the poller worktree, so a configured gate is any name here.
    fn fake_gate(&self, name: &str, body: &str) {
        let bin = self.path().join("bin");
        fs::create_dir_all(&bin).expect("fixture: bin directory");
        let script = bin.join(name);
        fs::write(
            &script,
            format!("#!/usr/bin/env bash\nset -uo pipefail\n{body}\n"),
        )
        .expect("fixture: writing the fake gate");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
            .expect("fixture: fake gate executable");
    }

    /// Runs the PUBLIC path with the default gate — `verify-pr.sh <pr-url> --
    /// make test`, the argv the daemon hands it for a project whose pointer
    /// names no `[verify] gate`.
    fn verify_public(&self) -> Output {
        self.verify_public_with_gate(&["make", "test"])
    }

    /// Runs the PUBLIC path — `verify-pr.sh <pr-url> -- <gate...>` — against
    /// this fixture's local origin, with the fake `gh` and any fake gate first
    /// on `PATH` and the same containment `verification_gate` applies. An
    /// empty `gate` omits the `--` entirely, which is the usage-refusal case.
    fn verify_public_with_gate(&self, gate: &[&str]) -> Output {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let mut path = std::ffi::OsString::from(self.path().join("bin"));
        path.push(":");
        path.push(inherited);
        let mut command = Command::new("bash");
        command
            .arg(checkout().join("scripts/verify-pr.sh"))
            .arg("https://github.com/acme/widgets/pull/42");
        if !gate.is_empty() {
            command.arg("--").args(gate);
        }
        command
            .current_dir(self.path())
            .env("PATH", path)
            .env("FAKE_GH_STATE", self.path().join("fake-gh-state"))
            .env("STORYHOOK_LOCK_DIR", self.path().join("locks"))
            .env("STORYHOOK_ACTIVITY_LOG_DIR", self.path().join("activity"))
            .env("STORYHOOK_VERIFIER_MIRROR", "0")
            .env(
                "STORYHOOK_GATE_PROGRESS",
                self.path().join("gate-progress.ndjson"),
            )
            .env_remove("STORYHOOK_MACHINE_LOCKS")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .output()
            .expect("running the public verification path")
    }

    /// Runs the head-convergence seam (SH-636) with GitHub's wire shape for
    /// the submitted PR; the fetch, the branch-tip read and the comparison are
    /// all real Git against the local origin.
    fn refresh_submission(&self, metadata: &str) -> Output {
        run(
            self.path(),
            "bash",
            &[
                &checkout()
                    .join("scripts/verify-pr.sh")
                    .display()
                    .to_string(),
                "--refresh-submission",
                metadata,
            ],
        )
    }

    /// Runs the post-landing-refusal seam with authoritative metadata and real
    /// refs. GitHub itself stays outside this deterministic boundary.
    fn reconcile_landing_refusal(
        &self,
        refresh_status: i32,
        metadata: &str,
        expected_pr: u64,
        expected_base: &str,
        expected_head: &str,
        verified_tree: &str,
    ) -> Output {
        run(
            self.path(),
            "bash",
            &[
                &checkout()
                    .join("scripts/verify-pr.sh")
                    .display()
                    .to_string(),
                "--reconcile-land-refusal",
                &refresh_status.to_string(),
                metadata,
                &expected_pr.to_string(),
                expected_base,
                expected_head,
                verified_tree,
                "land-pr refused after verification",
            ],
        )
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
fn speculative_git_directory_cannot_be_inferred_as_bare() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("feature", &base, "g", "feature\n");
    let expected_tree = stdout(&repo.preflight(&base, &head));
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");

    let outcome = repo.speculative_run(
        &expected_tree,
        &base,
        &head,
        &poller,
        &[
            "bash",
            "-c",
            "private_git=$(git rev-parse --absolute-git-dir) && GIT_DIR=\"$private_git\" git init -q -b main && basename \"$private_git\"",
        ],
    );

    assert_ok(&outcome, "reinitializing the private speculative git-dir");
    assert_eq!(stdout(&outcome), ".git");
    assert_eq!(
        stdout(&repo.git(&["config", "--get", "core.bare"])),
        "false",
        "a leaked speculative GIT_DIR must not classify the shared repository as bare"
    );
}

/// Production passes the fetched base ref, not its already-resolved object
/// id. Private administration must still begin with a valid detached HEAD.
#[test]
fn speculative_run_accepts_the_symbolic_base_ref_used_by_the_verifier() {
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
    let base_ref = "refs/heads/main";
    let expected_tree = stdout(&repo.preflight(base_ref, &head));
    let poller_container = repo.poller(&base);
    let poller = poller_container.path().join("poller");

    let outcome = repo.speculative_run(
        &expected_tree,
        base_ref,
        &head,
        &poller,
        &["git", "rev-parse", "HEAD^{tree}"],
    );

    assert_ok(&outcome, "speculative run from the verifier base ref");
    assert_eq!(stdout(&outcome), expected_tree);
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
    let sibling_container = repo.linked_worktree("sibling", &base);
    let sibling = sibling_container.path().join("sibling");
    let coordination = scratch_dir();
    let ready = coordination.path().join("ready");
    let release = coordination.path().join("release");

    let mut child =
        repo.spawn_blocked_speculative_run(&expected_tree, &base, &head, &poller, &ready, &release);
    wait_for(&ready);

    let private_identities = fs::read_to_string(&ready).expect("reading gate identities");
    let shared_head = repo.git(&["rev-parse", "worktrees/poller/HEAD^{commit}"]);
    let sibling_shared_head = run(
        &sibling,
        "git",
        &["rev-parse", "worktrees/poller/HEAD^{commit}"],
    );
    let ref_walk = repo.git(&["rev-list", "--all", "--objects"]);
    let fetch = repo.git(&["fetch", "-q", "."]);
    let pull_source = repo.path().display().to_string();
    let pull = run(
        &sibling,
        "git",
        &["pull", "-q", "--ff-only", &pull_source, "main"],
    );
    let gc = repo.git(&["gc"]);
    let fsck = repo.git(&[
        "fsck",
        "--connectivity-only",
        "--no-dangling",
        "--no-progress",
    ]);
    let repack = repo.git(&["repack", "-a", "-d"]);
    let worktree_prune = run(&sibling, "git", &["worktree", "prune"]);
    let good = format!("{base}~1");
    let bisect_start = run(&sibling, "git", &["bisect", "start", &base, &good]);
    let bisect_reset = run(&sibling, "git", &["bisect", "reset"]);

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
    assert_ok(
        &sibling_shared_head,
        "resolving the shared verifier HEAD from another linked worktree",
    );
    assert_eq!(stdout(&sibling_shared_head), base);
    assert_ok(&ref_walk, "walking every ordinary shared ref");
    assert_ok(&fetch, "fetching while the speculative gate is blocked");
    assert_ok(
        &pull,
        "pulling from another linked worktree while the speculative gate is blocked",
    );
    assert_ok(&gc, "running gc while the speculative gate is blocked");
    assert_ok(
        &fsck,
        "running connectivity fsck while the speculative gate is blocked",
    );
    assert_ok(&repack, "repacking while the speculative gate is blocked");
    assert_ok(
        &worktree_prune,
        "pruning worktrees while the speculative gate is blocked",
    );
    assert_ok(
        &bisect_start,
        "starting a bisect while the speculative gate is blocked",
    );
    assert_ok(
        &bisect_reset,
        "resetting a bisect while the speculative gate is blocked",
    );

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
fn verifier_rebuilds_legacy_private_object_metadata_before_fetch() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let verifier = repo
        .common_dir()
        .join("storyhook")
        .join("verification-worktree");
    fs::create_dir_all(verifier.parent().unwrap()).expect("creating the verifier state directory");
    assert_ok(
        &repo.git(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            &verifier.display().to_string(),
            &base,
        ]),
        "creating the legacy verifier worktree",
    );

    let private_objects = scratch_dir();
    let source_objects = repo.common_dir().join("objects");
    fs::write(verifier.join("private"), "private\n").expect("writing the private verifier file");
    assert_ok(
        &git_with_objects(
            &verifier,
            private_objects.path(),
            &source_objects,
            &["add", "private"],
        ),
        "staging the private verifier file",
    );
    let private_tree = git_with_objects(
        &verifier,
        private_objects.path(),
        &source_objects,
        &["write-tree"],
    );
    assert_ok(&private_tree, "writing the private verifier tree");
    let private_tree = stdout(&private_tree);
    let private_commit = git_with_objects(
        &verifier,
        private_objects.path(),
        &source_objects,
        &["commit-tree", &private_tree, "-p", &base, "-m", "private"],
    );
    assert_ok(&private_commit, "creating the private verifier commit");
    let private_commit = stdout(&private_commit);
    assert_ok(
        &git_with_objects(
            &verifier,
            private_objects.path(),
            &source_objects,
            &["checkout", "-q", "--detach", &private_commit],
        ),
        "checking out the private verifier commit",
    );
    drop(private_objects);

    let broken_fetch = repo.git(&["fetch", "-q", "."]);
    assert!(
        !broken_fetch.status.success(),
        "the fixture must reproduce fetch failure"
    );
    assert!(
        stderr(&broken_fetch).contains("bad object"),
        "the fixture must fail on the missing verifier object: {}",
        stderr(&broken_fetch)
    );

    let script = checkout().join("scripts/verify-pr.sh");
    let repaired = run(
        repo.path(),
        "bash",
        &[
            &script.display().to_string(),
            "--ensure-verifier-worktree",
            &base,
        ],
    );
    assert_ok(&repaired, "repairing the verifier worktree");
    let payload: serde_json::Value = serde_json::from_slice(&repaired.stdout)
        .expect("the verifier repair seam must return JSON");
    assert_eq!(payload["result"], "verifier-worktree-ready");

    assert_eq!(stdout(&run(&verifier, "git", &["rev-parse", "HEAD"])), base);
    assert!(!verifier.join("private").exists());
    assert_eq!(
        fs::read_to_string(
            repo.common_dir()
                .join("storyhook/verification-worktree.format")
        )
        .expect("reading the verifier format marker"),
        "private-gitdir-v1\n"
    );
    assert_ok(&repo.git(&["fetch", "-q", "."]), "fetching after repair");
    assert_ok(
        &repo.git(&[
            "fsck",
            "--no-progress",
            "--connectivity-only",
            "--no-dangling",
        ]),
        "checking connectivity after repair",
    );

    let sentinel = verifier.join("persistent-cache-sentinel");
    fs::write(&sentinel, "keep\n").expect("writing the persistent cache sentinel");
    let reused = run(
        repo.path(),
        "bash",
        &[
            &script.display().to_string(),
            "--ensure-verifier-worktree",
            &base,
        ],
    );
    assert_ok(&reused, "reusing the healthy verifier worktree");
    assert!(
        sentinel.exists(),
        "a healthy formatted verifier must preserve its caches"
    );

    fs::write(
        repo.common_dir()
            .join("storyhook/verification-worktree.format"),
        "legacy\n",
    )
    .expect("downgrading the verifier format marker");
    let marker_repair = run(
        repo.path(),
        "bash",
        &[
            &script.display().to_string(),
            "--ensure-verifier-worktree",
            &base,
        ],
    );
    assert_ok(
        &marker_repair,
        "reusing a healthy verifier with an old marker",
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&marker_repair.stdout).expect("the marker repair must return JSON");
    assert_eq!(payload["result"], "verifier-worktree-ready");
    assert!(
        sentinel.exists(),
        "a stale format marker alone must not discard a healthy verifier"
    );
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

    // SH-636: called with ref NAMES, the report still names the oids each
    // resolved to — a stale reading has to be visible in the report itself,
    // not inferred from the conflict's blob ids. Both, and on the same line.
    let by_name = repo.preflight("branch-a", "branch-b");
    assert_eq!(by_name.status.code(), Some(2));
    let conflict_line = stderr(&by_name)
        .lines()
        .find(|line| line.contains("CONFLICT —"))
        .map(str::to_string)
        .unwrap_or_default();
    assert!(
        conflict_line.contains(&format!("branch-b ({b})"))
            && conflict_line.contains(&format!("branch-a ({a})")),
        "got: {conflict_line}"
    );
    // An oid argument is not decorated with itself.
    let by_oid_line = stderr(&out)
        .lines()
        .find(|line| line.contains("CONFLICT —"))
        .map(str::to_string)
        .unwrap_or_default();
    assert!(
        !by_oid_line.contains(&format!("{b} ({b})")),
        "got: {by_oid_line}"
    );
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
    let ready = r#"{"number":42,"state":"OPEN","isDraft":false,"isCrossRepository":false,"baseRefName":"main","headRefName":"feature","headRefOid":"deadbeef","mergeCommit":null}"#;
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
    assert_eq!(draft["result"], "invalid-submission");
    assert!(draft["detail"].as_str().unwrap().contains("is a draft"));
    assert!(
        !draft["detail"]
            .as_str()
            .unwrap()
            .contains("no draft status")
    );

    let fork =
        validate(&ready.replace("\"isCrossRepository\":false", "\"isCrossRepository\":true"));
    assert_eq!(fork["result"], "invalid-submission");
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
    assert_eq!(invalid["disposition"], "permanent");
    assert!(
        invalid["detail"]
            .as_str()
            .unwrap()
            .contains("no draft status")
    );

    // SH-636: the head branch is what the pull ref mirrors, so a wire shape
    // without it cannot be checked for convergence and is refused by name.
    let branchless = validate(&ready.replace("\"headRefName\":\"feature\",", ""));
    assert_eq!(branchless["result"], "infrastructure-failure");
    assert_eq!(branchless["disposition"], "permanent");
    assert!(
        branchless["detail"]
            .as_str()
            .unwrap()
            .contains("no head branch")
    );
}

/// GitHub's wire shape for an open same-repository PR whose head branch is
/// `feature`, as `verify-pr.sh`'s `--refresh-submission` seam consumes it.
fn open_pr_metadata(pr: u64, reported_head: &str) -> String {
    serde_json::json!({
        "number": pr,
        "state": "OPEN",
        "isDraft": false,
        "isCrossRepository": false,
        "baseRefName": "main",
        "headRefName": "feature",
        "headRefOid": reported_head,
        "mergeCommit": null,
    })
    .to_string()
}

/// The SH-630 shape: `feature` at OLD conflicts with `main`; a reconcile
/// merge NEW resolves it. Returns `(old, new)` with `feature` left at NEW and
/// `main` checked out, before any origin is published.
fn reconciled_feature(repo: &MergeRepo) -> (String, String) {
    let old = repo.branch("feature", "main", "f", "feature changes the base file\n");
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "back to main");
    repo.write("f", "main changes the base file\n");
    repo.git(&["add", "f"]);
    assert_ok(
        &repo.git(&["commit", "-qm", "main moves"]),
        "advancing main",
    );
    assert_ok(&repo.git(&["checkout", "-q", "feature"]), "onto feature");
    repo.write("f", "reconciled\n");
    repo.git(&["add", "f"]);
    assert_ok(
        &repo.git(&["commit", "-qm", "feature takes main's change"]),
        "preparing the reconcile content",
    );
    // A real merge commit, the shape a reconcile pushes: parents OLD and main.
    let out = repo.git(&["merge", "-q", "-s", "ours", "--no-edit", "main"]);
    assert_ok(&out, "recording the reconcile merge");
    let new = repo.rev_parse("HEAD");
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "back to main");
    assert_ne!(old, new);
    (old, new)
}

/// SH-636's own incident, reconstructed: the branch was pushed (`refs/heads/
/// feature` = NEW) but GitHub's two projections of it — `refs/pull/N/head`
/// and the API's `headRefOid` — still both say OLD. Comparing the projections
/// against each other passes; comparing them against the branch does not, and
/// the verdict is RETRYABLE with all three oids named, never the stale head's
/// conflict.
#[test]
fn a_pull_ref_lagging_its_branch_is_retried_not_reported_as_a_conflict() {
    let repo = MergeRepo::new();
    let (old, new) = reconciled_feature(&repo);
    repo.publish_origin(42, &old);

    let out = repo.refresh_submission(&open_pr_metadata(42, &old));
    assert_ok(&out, "refreshing a submission whose pull ref lags");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(payload["result"], "infrastructure-failure");
    assert_eq!(payload["disposition"], "retryable");
    let detail = payload["detail"].as_str().unwrap();
    assert!(
        detail.contains(&old),
        "the stale projection is named: {detail}"
    );
    assert!(detail.contains(&new), "the branch tip is named: {detail}");
    assert!(
        detail.contains("refs/heads/feature"),
        "the branch the pull ref mirrors is named: {detail}"
    );
    assert!(
        !detail.contains("CONFLICT") && !stderr(&out).contains("CONFLICT"),
        "no preflight may run against a head GitHub has not converged on"
    );
    // The retry comment has to be distinguishable from a head that moved
    // AFTER verification (SH-604's "changed head" refusal).
    assert!(detail.contains("not converged"), "{detail}");
}

/// The other half of the same lag: the API has caught up but the pull ref
/// has not (or vice versa). This used to be a PERMANENT "moved while its refs
/// were being refreshed" failure; it is the same asynchronous pipeline and
/// gets the same bounded retry.
#[test]
fn a_pull_ref_disagreeing_with_the_api_is_retryable_not_permanent() {
    let repo = MergeRepo::new();
    let (old, new) = reconciled_feature(&repo);
    repo.publish_origin(42, &old);

    let out = repo.refresh_submission(&open_pr_metadata(42, &new));
    assert_ok(
        &out,
        "refreshing a submission whose API and pull ref disagree",
    );
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(payload["result"], "infrastructure-failure");
    assert_eq!(payload["disposition"], "retryable");
    let detail = payload["detail"].as_str().unwrap();
    assert!(detail.contains(&old) && detail.contains(&new), "{detail}");
}

/// Three-way agreement is the precondition, not "no conflict": a head that
/// GitHub HAS converged on proceeds to preflight even when it genuinely
/// conflicts, so the convergence check cannot mask a real conflict — that
/// is the first SH-630 return, which was correct.
#[test]
fn an_agreed_head_proceeds_whether_or_not_it_conflicts() {
    let repo = MergeRepo::new();
    let (old, new) = reconciled_feature(&repo);

    // Converged on the reconciled head.
    repo.publish_origin(42, &new);
    let out = repo.refresh_submission(&open_pr_metadata(42, &new));
    assert_ok(&out, "refreshing a converged submission");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(payload["result"], "refs-current", "{payload}");
    assert_eq!(payload["head"], new);
    assert_eq!(repo.rev_parse("refs/remotes/origin/pr/42"), new);
    assert!(
        !repo
            .git(&[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/remotes/origin/feature"
            ])
            .status
            .success(),
        "reading the branch tip must not write a remote-tracking ref for it"
    );

    // Converged on a head that really conflicts: still current, and the
    // production preflight then reports the conflict exactly as before.
    assert_ok(
        &repo.git(&["branch", "-f", "feature", &old]),
        "rewinding the branch to the conflicting head",
    );
    assert_ok(
        &repo.git(&["update-ref", "refs/pull/42/head", &old]),
        "GitHub converging on the rewound head",
    );
    let out = repo.refresh_submission(&open_pr_metadata(42, &old));
    assert_ok(&out, "refreshing a converged, conflicting submission");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(payload["result"], "refs-current", "{payload}");
    assert_eq!(payload["head"], old);
    let conflict = repo.preflight("refs/remotes/origin/main", "refs/remotes/origin/pr/42");
    assert_eq!(conflict.status.code(), Some(2));
    assert!(stderr(&conflict).contains("CONFLICT"));
}

/// A PR whose head branch no longer exists on origin has nothing to converge
/// on: that is a submission problem for the agent, not infrastructure.
#[test]
fn a_head_branch_absent_from_origin_is_an_invalid_submission() {
    let repo = MergeRepo::new();
    let (old, _new) = reconciled_feature(&repo);
    repo.publish_origin(42, &old);
    assert_ok(
        &repo.git(&["branch", "-D", "feature"]),
        "deleting the branch on origin",
    );

    let out = repo.refresh_submission(&open_pr_metadata(42, &old));
    assert_ok(&out, "refreshing a submission whose branch is gone");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(payload["result"], "invalid-submission", "{payload}");
    let detail = payload["detail"].as_str().unwrap();
    assert!(detail.contains("refs/heads/feature"), "{detail}");
    assert!(detail.contains("push it"), "{detail}");
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
fn landing_refusal_recovers_only_the_certified_actual_merged_tree() {
    for tier in ["gate", "changed"] {
        let repo = MergeRepo::new();
        let base = repo.rev_parse("main");
        let base_tree = repo.tree_of("main");
        let head = repo.branch("feature", "main", "g", "new\n");
        assert_ok(&repo.git(&["checkout", "-q", "main"]), "returning to main");
        repo.enroll_and_certify();
        assert_ok(
            &repo.git(&["checkout", "-q", "-b", "merged", &base]),
            "branching for the landed merge",
        );
        assert_ok(
            &repo.git(&["merge", "-q", "--no-edit", &head]),
            "creating the landed merge",
        );
        let merge_oid = repo.rev_parse("HEAD");
        let merge_tree = repo.tree_of("HEAD");
        assert_ok(&repo.gate("preflight"), "enrolling the landed tree");
        assert_ok(
            &repo.gate_postlude(tier, (tier == "changed").then_some(base_tree.as_str())),
            "writing the landed-tree receipt",
        );
        assert_ok(
            &repo.git(&["branch", "-f", "main", &merge_oid]),
            "advancing the authoritative base",
        );
        repo.publish_origin(42, &head);
        let metadata = serde_json::json!({
            "number": 42,
            "state": "MERGED",
            "isDraft": false,
            "isCrossRepository": false,
            "baseRefName": "main",
            "headRefName": "feature",
            "headRefOid": head,
            "mergeCommit": {"oid": merge_oid},
        })
        .to_string();

        let out = repo.reconcile_landing_refusal(0, &metadata, 42, "main", &head, &merge_tree);
        assert_ok(&out, "classifying the refreshed merged PR");
        let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        if tier == "gate" {
            assert_eq!(payload["result"], "merged", "{payload}");
            assert_eq!(payload["tree"], merge_tree);
            assert!(
                payload["detail"]
                    .as_str()
                    .unwrap()
                    .contains("landing refusal")
            );
        } else {
            assert_eq!(payload["result"], "infrastructure-failure", "{payload}");
            assert_eq!(payload["disposition"], "permanent");
            assert!(
                payload["detail"]
                    .as_str()
                    .unwrap()
                    .contains("insufficient 'changed' receipt")
            );
        }
    }
}

#[test]
fn landing_refusal_retries_only_a_new_tree_for_the_same_submission() {
    let repo = MergeRepo::new();
    let original_base = repo.rev_parse("main");
    let head = repo.branch("feature", "main", "g", "new\n");
    let verified_tree = stdout(&repo.preflight(&original_base, &head));
    assert_ok(
        &repo.git(&["checkout", "-q", "-b", "certified", &original_base]),
        "branching for the certified merge",
    );
    assert_ok(
        &repo.git(&["merge", "-q", "--no-edit", &head]),
        "creating the certified merge",
    );
    repo.enroll_and_certify();
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "returning to main");
    repo.publish_origin(42, &head);
    let metadata = serde_json::json!({
        "number": 42,
        "state": "OPEN",
        "isDraft": false,
        "isCrossRepository": false,
        "baseRefName": "main",
        "headRefName": "feature",
        "headRefOid": head,
        "mergeCommit": null,
    })
    .to_string();

    let unchanged = repo.reconcile_landing_refusal(0, &metadata, 42, "main", &head, &verified_tree);
    let unchanged: serde_json::Value = serde_json::from_slice(&unchanged.stdout).unwrap();
    assert_eq!(unchanged["result"], "infrastructure-failure", "{unchanged}");
    assert_eq!(unchanged["disposition"], "retryable", "{unchanged}");
    assert!(
        unchanged["detail"]
            .as_str()
            .unwrap()
            .contains("remains current and certified"),
        "{unchanged}"
    );

    repo.write("h", "base advanced\n");
    assert_ok(&repo.git(&["add", "h"]), "staging the base advancement");
    assert_ok(
        &repo.git(&["commit", "-qm", "base advances after verification"]),
        "advancing the authoritative base",
    );
    let current_base = repo.rev_parse("main");
    let refreshed_tree = stdout(&repo.preflight(&current_base, &head));
    assert_ne!(refreshed_tree, verified_tree);

    let out = repo.reconcile_landing_refusal(0, &metadata, 42, "main", &head, &verified_tree);
    assert_ok(&out, "classifying the advanced base");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["result"], "infrastructure-failure", "{payload}");
    assert_eq!(payload["disposition"], "retryable", "{payload}");
    let detail = payload["detail"].as_str().unwrap();
    assert!(detail.contains(&verified_tree), "{detail}");
    assert!(detail.contains(&refreshed_tree), "{detail}");
    assert!(
        !repo
            .common_dir()
            .join("storyhook/gate-receipts")
            .join(refreshed_tree)
            .exists(),
        "reconciliation must not certify the changed tree"
    );
}

#[test]
fn landing_refusal_keeps_missing_proof_and_changed_identity_distinct() {
    let repo = MergeRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("feature", "main", "g", "new\n");
    let tree = stdout(&repo.preflight(&base, &head));
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "returning to main");
    repo.publish_origin(42, &head);
    let open = serde_json::json!({
        "number": 42,
        "state": "OPEN",
        "isDraft": false,
        "isCrossRepository": false,
        "baseRefName": "main",
        "headRefName": "feature",
        "headRefOid": head,
        "mergeCommit": null,
    })
    .to_string();

    let missing = repo.reconcile_landing_refusal(0, &open, 42, "main", &head, &tree);
    let missing: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(missing["result"], "infrastructure-failure", "{missing}");
    assert_eq!(missing["disposition"], "permanent");

    for changed in [
        open.replace("\"number\":42", "\"number\":43"),
        open.replace("\"state\":\"OPEN\"", "\"state\":\"CLOSED\""),
        open.replace("\"baseRefName\":\"main\"", "\"baseRefName\":\"other\""),
        open.replace(
            &format!("\"headRefOid\":\"{head}\""),
            "\"headRefOid\":\"deadbeef\"",
        ),
    ] {
        let out = repo.reconcile_landing_refusal(0, &changed, 42, "main", &head, &tree);
        let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(payload["result"], "invalid-submission", "{payload}");
    }

    let unavailable = repo.reconcile_landing_refusal(1, "", 42, "main", &head, &tree);
    let unavailable: serde_json::Value = serde_json::from_slice(&unavailable.stdout).unwrap();
    assert_eq!(
        unavailable["result"], "infrastructure-failure",
        "{unavailable}"
    );
    assert_eq!(unavailable["disposition"], "retryable");

    assert_ok(
        &repo.git(&["remote", "set-url", "origin", "/no/such/storyhook-origin"]),
        "breaking the local origin",
    );
    let fetch_failure = repo.reconcile_landing_refusal(0, &open, 42, "main", &head, &tree);
    let fetch_failure: serde_json::Value = serde_json::from_slice(&fetch_failure.stdout).unwrap();
    assert_eq!(
        fetch_failure["result"], "infrastructure-failure",
        "{fetch_failure}"
    );
    assert_eq!(fetch_failure["disposition"], "retryable");
}

#[test]
fn landing_refusal_reports_a_conflict_in_the_refreshed_tree() {
    let repo = MergeRepo::new();
    let head = repo.branch("feature", "main", "f", "feature changes the file\n");
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "returning to main");
    repo.write("f", "base changes the file\n");
    assert_ok(&repo.git(&["add", "f"]), "staging the base conflict");
    assert_ok(
        &repo.git(&["commit", "-qm", "base conflicts after verification"]),
        "advancing the conflicting base",
    );
    repo.publish_origin(42, &head);
    let metadata = serde_json::json!({
        "number": 42,
        "state": "OPEN",
        "isDraft": false,
        "isCrossRepository": false,
        "baseRefName": "main",
        "headRefName": "feature",
        "headRefOid": head,
        "mergeCommit": null,
    })
    .to_string();

    let out = repo.reconcile_landing_refusal(
        0,
        &metadata,
        42,
        "main",
        &head,
        "previously-certified-tree",
    );
    assert_ok(&out, "classifying the refreshed conflict");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["result"], "conflict", "{payload}");
    assert!(payload["detail"].as_str().unwrap().contains("CONFLICT"));
}

#[test]
fn verifier_preserves_the_landing_scripts_terminal_classifications() {
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

    let merged = run(
        repo.path(),
        "bash",
        &[
            &script,
            "--classify-land",
            "0",
            "merge landed and was certified",
            "42",
            "deadbeef",
        ],
    );
    assert_ok(&merged, "classifying a successful landing");
    let payload: serde_json::Value = serde_json::from_slice(&merged.stdout).unwrap();
    assert_eq!(payload["result"], "merged");
    assert_eq!(payload["tree"], "deadbeef");
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

/// A head that `refresh_submission_refs` converged on (SH-636), left on
/// `feature`, `refs/pull/42/head` and the fake `gh` alike, so the public path
/// reaches preflight. Returns the metadata the fake answers with.
fn converge_public_head(repo: &MergeRepo, head: &str) {
    assert_ok(
        &repo.git(&["update-ref", "refs/heads/feature", head]),
        "the branch on origin",
    );
    assert_ok(
        &repo.git(&["update-ref", "refs/pull/42/head", head]),
        "GitHub's pull ref converged",
    );
    repo.fake_gh_answers(&open_pr_metadata(42, head));
}

/// Bash that moves the branch, the pull ref and the fake `gh`'s answer to
/// `head` — a push GitHub has fully propagated — from wherever the hook runs
/// (the fake `make` runs inside the private-gitdir poller worktree, so `-C`
/// names the fixture explicitly).
fn move_public_head(repo: &MergeRepo, head: &str) -> String {
    let fixture = repo.path().display();
    let state = repo.path().join("fake-gh-state");
    let state = state.display();
    let metadata = open_pr_metadata(42, head);
    format!(
        "git -C '{fixture}' update-ref refs/heads/feature {head}\n\
         git -C '{fixture}' update-ref refs/pull/42/head {head}\n\
         printf '%s' '{metadata}' > '{state}/pr.json'\n"
    )
}

fn public_payload(out: &Output) -> serde_json::Value {
    assert_ok(out, "the public verification path emits classified JSON");
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("public path JSON: {e}: {}", stdout(out)))
}

/// SH-637's own shape, reconstructed: the head was current when the attempt
/// began and GitHub had converged on it, so preflight ran and found the real
/// conflict at OLD — then the reconcile push landed before the verdict was
/// posted. The verdict is about a head nobody can act on any more, so it is
/// discarded as RETRYABLE, naming both heads; it is never posted as the
/// conflict it would have been two seconds earlier.
#[test]
fn a_conflict_verdict_is_discarded_when_the_head_moves_before_it_is_posted() {
    let repo = MergeRepo::new();
    let (old, new) = reconciled_feature(&repo);
    repo.publish_origin(42, &old);
    repo.fake_gh();
    converge_public_head(&repo, &old);
    // Call 1 is the entry read; call 2 is the recheck the verdict waits for.
    repo.fake_gh_before_call(2, &move_public_head(&repo, &new));

    let payload = public_payload(&repo.verify_public());
    assert_eq!(payload["result"], "infrastructure-failure", "{payload}");
    assert_eq!(payload["disposition"], "retryable", "{payload}");
    let detail = payload["detail"].as_str().unwrap();
    assert!(detail.contains("head moved"), "{detail}");
    assert!(
        detail.contains(&old) && detail.contains(&new),
        "both the judged and the current head are named: {detail}"
    );
    assert!(detail.contains("conflict verdict"), "{detail}");
    assert_eq!(repo.fake_gh_calls(), 2, "entry read, then the recheck");
}

/// The recheck reads the branch, not only GitHub's projections: a push that
/// has reached `refs/heads/feature` but neither projection yet is still a
/// head in flux, and SH-636's convergence rule answers for it — retryable,
/// never the stale head's conflict.
#[test]
fn a_conflict_verdict_is_discarded_when_only_the_branch_has_moved() {
    let repo = MergeRepo::new();
    let (old, new) = reconciled_feature(&repo);
    repo.publish_origin(42, &old);
    repo.fake_gh();
    converge_public_head(&repo, &old);
    repo.fake_gh_before_call(
        2,
        &format!(
            "git -C '{}' update-ref refs/heads/feature {new}\n",
            repo.path().display()
        ),
    );

    let payload = public_payload(&repo.verify_public());
    assert_eq!(payload["result"], "infrastructure-failure", "{payload}");
    assert_eq!(payload["disposition"], "retryable", "{payload}");
    let detail = payload["detail"].as_str().unwrap();
    assert!(detail.contains("not converged"), "{detail}");
    assert!(detail.contains(&new), "the branch tip is named: {detail}");
}

/// The positive control: a head that is still the head when the verdict is
/// ready gets its conflict, exactly as before, and the recheck is what asked
/// — two reads of GitHub, not one.
#[test]
fn a_conflict_on_a_head_that_stayed_put_is_still_reported_as_a_conflict() {
    let repo = MergeRepo::new();
    let (old, _new) = reconciled_feature(&repo);
    repo.publish_origin(42, &old);
    repo.fake_gh();
    converge_public_head(&repo, &old);

    let payload = public_payload(&repo.verify_public());
    assert_eq!(payload["result"], "conflict", "{payload}");
    let detail = payload["detail"].as_str().unwrap();
    assert!(detail.contains("CONFLICT"), "{detail}");
    assert!(
        detail.contains(&old),
        "SH-636's oid line names the head: {detail}"
    );
    assert_eq!(
        repo.fake_gh_calls(),
        2,
        "the verdict was confirmed against a fresh read, not the entry read"
    );
}

/// The same rule at the other verdict site, with the window that actually
/// matters: the release gate runs for minutes, and a push during it is the
/// case a verdict-time check exists for. The gate goes red on NEW, the push
/// moves the PR to NEWER before the verdict is posted, and the red for NEW is
/// discarded as retryable — carrying the log so the evidence is not lost —
/// never posted as `tests-failed`.
#[test]
fn a_red_verdict_is_discarded_when_the_head_moves_during_the_gate() {
    let repo = MergeRepo::new();
    let (_old, new) = reconciled_feature(&repo);
    assert_ok(&repo.git(&["checkout", "-q", "feature"]), "onto feature");
    repo.write("g", "a later push\n");
    repo.git(&["add", "g"]);
    assert_ok(
        &repo.git(&["commit", "-qm", "later push"]),
        "the push during the gate",
    );
    let newer = repo.rev_parse("HEAD");
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "back to main");
    repo.publish_origin(42, &new);
    repo.fake_gh();
    converge_public_head(&repo, &new);
    // The gate itself is where the push lands: the fake `make` moves the
    // head, then fails, the way a real suite would fail on the old head.
    repo.fake_make(&format!("{}exit 1\n", move_public_head(&repo, &newer)));

    let payload = public_payload(&repo.verify_public());
    assert_eq!(payload["result"], "infrastructure-failure", "{payload}");
    assert_eq!(payload["disposition"], "retryable", "{payload}");
    let detail = payload["detail"].as_str().unwrap();
    assert!(detail.contains("head moved"), "{detail}");
    assert!(detail.contains(&new) && detail.contains(&newer), "{detail}");
    assert!(detail.contains("red verdict"), "{detail}");
    assert!(
        detail.contains("Gate log of the superseded attempt"),
        "the red evidence is kept in the retry detail: {detail}"
    );
    assert_eq!(repo.fake_gh_calls(), 2, "entry read, then the recheck");
}

/// The red positive control, through the public path: a completed red on a
/// head that stayed put is `tests-failed` with the tree and the log, which is
/// also what proves `run_verification_gate`'s status split preserved the
/// verdict's shape where it is actually posted.
#[test]
fn a_red_on_a_head_that_stayed_put_is_still_reported_as_tests_failed() {
    let repo = MergeRepo::new();
    let (_old, new) = reconciled_feature(&repo);
    repo.publish_origin(42, &new);
    repo.fake_gh();
    converge_public_head(&repo, &new);
    repo.fake_make("exit 3\n");

    let payload = public_payload(&repo.verify_public());
    assert_eq!(payload["result"], "tests-failed", "{payload}");
    let expected_tree = stdout(&repo.preflight("refs/remotes/origin/main", &new));
    assert_eq!(payload["tree"], expected_tree, "{payload}");
    let log = payload["log"].as_str().unwrap();
    assert!(Path::new(log).is_file(), "the attempt log exists: {log}");
    let detail = payload["detail"].as_str().unwrap();
    assert!(
        detail.contains("failed with exit status 3"),
        "the gate's own status is reported: {detail}"
    );
    assert_eq!(repo.fake_gh_calls(), 2, "entry read, then the recheck");
}

/// What the recheck refuses to reason past: a re-read that names a different
/// PR or base is an identity change (invalid, as `reconcile_land_refusal`
/// rules), and a PR no longer OPEN belongs to the next attempt's entry path
/// (retryable). Neither posts the conflict that was computed.
#[test]
fn a_recheck_that_finds_a_different_pr_or_a_closed_one_posts_no_verdict() {
    let repo = MergeRepo::new();
    let (old, _new) = reconciled_feature(&repo);
    repo.publish_origin(42, &old);
    repo.fake_gh();
    converge_public_head(&repo, &old);
    let state = repo.path().join("fake-gh-state").display().to_string();

    let other_pr = open_pr_metadata(43, &old);
    repo.fake_gh_before_call(
        2,
        &format!("printf '%s' '{other_pr}' > '{state}/pr.json'\n"),
    );
    let payload = public_payload(&repo.verify_public());
    assert_eq!(payload["result"], "invalid-submission", "{payload}");
    assert!(
        payload["detail"].as_str().unwrap().contains("PR #43"),
        "{payload}"
    );

    // A second attempt on the same fixture: the entry read sees PR #42 again,
    // and this time the recheck finds it CLOSED.
    fs::remove_file(repo.path().join("fake-gh-state/calls")).unwrap();
    repo.fake_gh_answers(&open_pr_metadata(42, &old));
    let closed = open_pr_metadata(42, &old).replace("\"OPEN\"", "\"CLOSED\"");
    repo.fake_gh_before_call(2, &format!("printf '%s' '{closed}' > '{state}/pr.json'\n"));
    let payload = public_payload(&repo.verify_public());
    assert_eq!(payload["result"], "infrastructure-failure", "{payload}");
    assert_eq!(payload["disposition"], "retryable", "{payload}");
    assert!(
        payload["detail"].as_str().unwrap().contains("CLOSED"),
        "{payload}"
    );
}

/// The gate is the daemon's to name, never this script's to assume (SH-649):
/// `verify-pr.sh <pr-url>` with no `-- <gate...>` is refused by name before
/// GitHub is so much as asked, because a script that quietly ran `make test`
/// would be a second place the default lived — and the wrong one, since the
/// project's pointer may say otherwise.
#[test]
fn the_public_path_refuses_to_run_without_a_named_gate() {
    let repo = MergeRepo::new();
    let (_old, new) = reconciled_feature(&repo);
    repo.publish_origin(42, &new);
    repo.fake_gh();
    repo.fake_make("exit 0\n");

    let payload = public_payload(&repo.verify_public_with_gate(&[]));
    assert_eq!(payload["result"], "infrastructure-failure", "{payload}");
    assert_eq!(payload["disposition"], "permanent", "{payload}");
    let detail = payload["detail"].as_str().unwrap();
    assert!(
        detail.contains("usage: verify-pr.sh <pr-url> -- <gate-command...>"),
        "{detail}"
    );
    assert_eq!(repo.fake_gh_calls(), 0, "refused before GitHub is asked");
}

/// The configured argv reaches the gate word for word, resolved through
/// `PATH` inside the poller worktree exactly as `make test` is. The red form
/// is used so the run stops at the gate and the recorded argv is the whole
/// evidence.
#[test]
fn a_configured_gate_runs_with_the_argv_it_was_named_with() {
    let repo = MergeRepo::new();
    let (_old, new) = reconciled_feature(&repo);
    repo.publish_origin(42, &new);
    repo.fake_gh();
    converge_public_head(&repo, &new);
    let record = repo.path().join("gate-argv");
    repo.fake_gate(
        "gate-bin",
        &format!("printf '%s\\n' \"$@\" > '{}'\nexit 3\n", record.display()),
    );

    let payload = public_payload(&repo.verify_public_with_gate(&["gate-bin", "--ci", "unit"]));
    assert_eq!(payload["result"], "tests-failed", "{payload}");
    assert_eq!(
        fs::read_to_string(&record).expect("the gate ran and recorded its argv"),
        "--ci\nunit\n"
    );
    let detail = payload["detail"].as_str().unwrap();
    assert!(
        detail.contains("failed with exit status 3"),
        "the gate's own status is reported: {detail}"
    );
}

/// A gate that exits 0 but mints no `gate`/`full` receipt has certified
/// nothing, and landing would refuse it downstream with a diagnosis about the
/// wrong layer ("no longer has a qualifying release-gate receipt", from
/// `reconcile_land_refusal`, after `gh` had been asked again). The public
/// path re-asks `merge-preflight.sh` — the reader `land-pr.sh` consults —
/// immediately after the gate and refuses by name, before any landing read.
/// The configured argv is recorded on the way, which is also the proof that
/// the gate this refusal names is the one that ran.
#[test]
fn a_configured_gate_that_exits_green_but_certifies_nothing_is_refused_before_landing() {
    let repo = MergeRepo::new();
    let (_old, new) = reconciled_feature(&repo);
    repo.publish_origin(42, &new);
    repo.fake_gh();
    converge_public_head(&repo, &new);
    let record = repo.path().join("gate-argv");
    repo.fake_gate(
        "gate-bin",
        &format!("printf '%s\\n' \"$@\" > '{}'\nexit 0\n", record.display()),
    );

    let payload = public_payload(&repo.verify_public_with_gate(&["gate-bin", "--ci"]));
    assert_eq!(payload["result"], "infrastructure-failure", "{payload}");
    assert_eq!(payload["disposition"], "permanent", "{payload}");
    let detail = payload["detail"].as_str().unwrap();
    assert!(detail.contains("certified nothing"), "{detail}");
    assert!(
        detail.contains("`gate-bin --ci`"),
        "names the gate: {detail}"
    );
    assert!(
        detail.contains("gate-receipt.sh postlude"),
        "names the remedy: {detail}"
    );
    assert!(
        detail.contains("\"$STORYHOOK_GATE_RECEIPT\" preflight"),
        "{detail}"
    );
    assert!(
        detail.contains("\"$STORYHOOK_GATE_RECEIPT\" postlude gate"),
        "{detail}"
    );
    let tree = stdout(&repo.preflight("refs/remotes/origin/main", &new));
    assert!(detail.contains(&tree), "names the tree: {detail}");
    assert_eq!(fs::read_to_string(&record).expect("the gate ran"), "--ci\n");
    assert!(
        !repo
            .common_dir()
            .join("storyhook/gate-receipts")
            .join(&tree)
            .exists(),
        "nothing certified the tree"
    );
    assert_eq!(
        repo.fake_gh_calls(),
        1,
        "refused after the entry read and before any landing read"
    );
}

/// SH-666's second incident, reconstructed: the gate ran green on the tree
/// preflight computed and certified it through the production writer — and
/// while it ran, a fetch in the shared repository (a `/story do` creating a
/// worktree, a poller, anything) moved `refs/remotes/origin/main` to a tip
/// that conflicts with the PR. The post-gate certification check then
/// re-resolved that REF instead of the commit the gate ran on, computed a
/// different merge, met the conflict, and reported "certified nothing" as a
/// PERMANENT halt of the whole queue. A base that moved is the story's own
/// business: landing refreshes it under the merge lock and answers CONFLICT,
/// which the daemon holds the queue on while the implementer reconciles — so
/// the check must ask exactly "did the gate certify the tree it ran on",
/// against the pinned commits, and let landing find the moved base.
#[test]
fn a_base_that_moves_during_the_gate_is_a_conflict_for_the_story_never_a_halt() {
    let repo = MergeRepo::new();
    let (_old, new) = reconciled_feature(&repo);
    // Main will move here during the gate: `f` diverges from NEW's reconcile.
    let later = repo.branch("later", "main", "f", "main moves again during the gate\n");
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "back to main");
    repo.publish_origin(42, &new);
    repo.fake_gh();
    converge_public_head(&repo, &new);
    let hooks = checkout().join(".githooks");
    let writer = checkout().join("scripts/gate-receipt.sh");
    let fixture = repo.path().display().to_string();
    repo.fake_gate(
        "gate-bin",
        &format!(
            "ln -sfn '{}' .githooks && bash '{writer}' preflight && bash '{writer}' postlude || exit $?\n\
             git -C '{fixture}' update-ref refs/heads/main {later}\n\
             git -C '{fixture}' update-ref refs/remotes/origin/main {later}\n",
            hooks.display(),
            writer = writer.display()
        ),
    );

    let payload = public_payload(&repo.verify_public_with_gate(&["gate-bin"]));
    // The gate certified the tree it ran on: preflight of the pinned parents.
    let gate_tree = {
        let base_before = repo.rev_parse("later~1");
        stdout(&repo.preflight(&base_before, &new))
    };
    let receipt = repo
        .common_dir()
        .join("storyhook/gate-receipts")
        .join(gate_tree.trim());
    assert!(
        fs::read_to_string(&receipt)
            .unwrap_or_else(|e| panic!("the gate certified the tree it ran on: {e}"))
            .contains("tier gate"),
        "the production writer minted a gate-tier receipt for the gate's own tree"
    );
    assert_eq!(
        payload["result"], "conflict",
        "a base that moved into conflict is the story's to reconcile: {payload}"
    );
    let detail = payload["detail"].as_str().unwrap();
    assert!(detail.contains("CONFLICT"), "{detail}");
    assert!(
        !detail.contains("certified nothing"),
        "a certified tree is never reported as uncertified because the base moved: {detail}"
    );
    assert!(
        repo.fake_gh_calls() >= 2,
        "the conflict was found by landing's own refresh, after the gate: {payload}"
    );
}

/// The positive control, without which a check that always refused would
/// pass the test above: a gate that certifies through the production writer
/// (`gate-receipt.sh preflight` then `postlude`, inside the speculative
/// checkout) gets past the check and on to landing — where this fixture's
/// fake `gh` is asked again, which is what proves the refusal did not fire.
#[test]
fn a_configured_gate_that_certifies_through_the_production_writer_proceeds_to_landing() {
    let repo = MergeRepo::new();
    let (_old, new) = reconciled_feature(&repo);
    repo.publish_origin(42, &new);
    repo.fake_gh();
    converge_public_head(&repo, &new);
    let hooks = checkout().join(".githooks");
    let writer = checkout().join("scripts/gate-receipt.sh");
    // The hooks link is untracked, so the poller restore tolerates it; the
    // production writer needs an executable pre-push beside it to enrol.
    repo.fake_gate(
        "gate-bin",
        &format!(
            "ln -sfn '{}' .githooks && bash '{writer}' preflight && bash '{writer}' postlude\n",
            hooks.display(),
            writer = writer.display()
        ),
    );

    let payload = public_payload(&repo.verify_public_with_gate(&["gate-bin"]));
    let tree = stdout(&repo.preflight("refs/remotes/origin/main", &new));
    let receipt = repo
        .common_dir()
        .join("storyhook/gate-receipts")
        .join(&tree);
    assert!(
        fs::read_to_string(&receipt)
            .unwrap_or_else(|e| panic!("the gate certified the tree: {e}"))
            .contains("tier gate"),
        "the production writer minted a gate-tier receipt"
    );
    let detail = payload["detail"].as_str().unwrap_or("");
    assert!(
        !detail.contains("certified nothing"),
        "a certified tree is not refused: {payload}"
    );
    assert!(
        repo.fake_gh_calls() >= 2,
        "landing was attempted after the gate: {payload}"
    );
}
