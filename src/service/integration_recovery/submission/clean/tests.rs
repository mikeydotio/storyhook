//! Real local Git inspection; only PR metadata reads are substituted.
use super::*;
use crate::process::run_captured_query_quiescent;
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const POINTER: &str = "schema = 1\nuuid = \"clean-fixture\"\nprefix = \"SH\"\n[integration]\nversion = 1\nenabled = true\npublication = \"managed-pr\"\nsmooth = [\"docs/\"]\n";

fn deadline() -> Instant {
    Instant::now() + storyhook_test_support::load_grace::graced_now(Duration::from_secs(120))
}

fn git(root: &Path, args: &[&str]) -> String {
    let mut command = crate::env::git_env::command(root);
    command.args(args);
    let output = run_captured_query_quiescent(command, deadline(), &|| false, 8 * 1024 * 1024, &[])
        .unwrap_or_else(|error| panic!("fixture Git: {}", error.detail()));
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout_truncated);
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn commit(root: &Path, message: &str) -> String {
    git(root, &["add", "."]);
    git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", message],
    );
    git(root, &["rev-parse", "HEAD"])
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

struct Fixture {
    _root: tempfile::TempDir,
    submission: SubmissionObservation,
    expected_tree: String,
}

impl Fixture {
    fn new(pointer: &str, conflict: bool) -> Self {
        let root = storyhook_test_support::scratch_dir();
        let checkout = root.path().canonicalize().unwrap();
        git(&checkout, &["init", "-q", "--initial-branch=dev"]);
        for (key, value) in [
            ("user.name", "Clean Integration Fixture"),
            ("user.email", "clean-integration@example.test"),
            (
                "storyhookIdentity.fixture.name",
                "Clean Integration Fixture",
            ),
            (
                "storyhookIdentity.fixture.email",
                "clean-integration@example.test",
            ),
            ("storyhookIdentity.fixture.role", "both"),
            (
                "storyhookIdentity.fixture.reason",
                "Isolated real-Git clean integration fixture",
            ),
        ] {
            git(&checkout, &["config", "--local", key, value]);
        }
        fs::create_dir(checkout.join("docs")).unwrap();
        fs::write(checkout.join(".storyhook.toml"), pointer).unwrap();
        fs::write(checkout.join("docs/guide.md"), "common\n").unwrap();
        let common = commit(&checkout, "common");
        let base_path = if conflict {
            "docs/guide.md"
        } else {
            "docs/base.md"
        };
        fs::write(checkout.join(base_path), "base\n").unwrap();
        let base = commit(&checkout, "current base");
        git(&checkout, &["checkout", "-q", "--detach", &common]);
        fs::write(checkout.join("docs/guide.md"), "author\n").unwrap();
        let head = commit(&checkout, "original head");
        // Independently build the expected clean tree during fixture setup;
        // the native observer must not write it into the registered checkout.
        let expected_tree = if conflict {
            String::new()
        } else {
            fs::write(checkout.join("docs/base.md"), "base\n").unwrap();
            commit(&checkout, "expected union tree");
            let tree = git(&checkout, &["rev-parse", "HEAD^{tree}"]);
            git(&checkout, &["checkout", "-q", "--detach", &head]);
            tree
        };
        fs::write(checkout.join("untracked-sentinel"), "keep author work").unwrap();
        Self {
            _root: root,
            submission: SubmissionObservation {
                checkout,
                repository: "github.example.test/acme/widgets".into(),
                pull_request: "https://github.example.test/acme/widgets/pull/7".into(),
                base_branch: "dev".into(),
                base,
                head,
            },
            expected_tree,
        }
    }

    fn observe(&self) -> Result<NativeCleanIntegration, AppError> {
        super::super::observe_clean_for_fixture(
            &self.submission.head,
            deadline(),
            Cancellation::default(),
            || Ok(self.submission.clone()),
        )
    }
}

#[test]
fn sh871_clean_native_merge_binds_current_inputs_and_preserves_source() {
    let fixture = Fixture::new(POINTER, false);
    let before = snapshot(&fixture.submission.checkout);
    let reads = Cell::new(0);
    let proof = observe_with(
        &fixture.submission.head,
        deadline(),
        Cancellation::default(),
        || {
            reads.set(reads.get() + 1);
            Ok(fixture.submission.clone())
        },
    )
    .unwrap();
    assert_eq!(reads.get(), 2);
    assert_eq!(proof.evidence().version, 1);
    assert_eq!(proof.evidence().submission, fixture.submission);
    assert_eq!(proof.evidence().tree, fixture.expected_tree);
    assert_eq!(
        proof.evidence().policy,
        format!("{:x}", Sha256::digest(POINTER.as_bytes()))
    );
    proof.check_live().unwrap();
    assert_eq!(snapshot(&fixture.submission.checkout), before);
}

#[test]
fn sh871_clean_observer_refuses_changed_original_head_before_native_inspection() {
    let reads = Cell::new(0);
    let result = observe_with(&"a".repeat(40), deadline(), Cancellation::default(), || {
        reads.set(reads.get() + 1);
        Ok(SubmissionObservation {
            checkout: "/not-an-inspection-fixture".into(),
            repository: "github.example.test/acme/widgets".into(),
            pull_request: "https://github.example.test/acme/widgets/pull/7".into(),
            base_branch: "dev".into(),
            base: "c".repeat(40),
            head: "b".repeat(40),
        })
    });
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("original submitted head changed")
    );
    assert_eq!(reads.get(), 1);
}

#[test]
fn sh871_clean_observer_requires_identical_second_pr_observation() {
    let fixture = Fixture::new(POINTER, false);
    let before = snapshot(&fixture.submission.checkout);
    for field in [
        "checkout",
        "repository",
        "pull_request",
        "base_branch",
        "base",
        "head",
    ] {
        let mut moved = fixture.submission.clone();
        match field {
            "checkout" => moved.checkout = moved.checkout.join("other"),
            "repository" => moved.repository.push_str("-other"),
            "pull_request" => moved.pull_request.push('1'),
            "base_branch" => moved.base_branch.push_str("-other"),
            "base" => moved.base = "c".repeat(40),
            "head" => moved.head = "d".repeat(40),
            _ => unreachable!(),
        }
        let reads = Cell::new(0);
        let result = observe_with(
            &fixture.submission.head,
            deadline(),
            Cancellation::default(),
            || {
                reads.set(reads.get() + 1);
                Ok(if reads.get() == 1 {
                    fixture.submission.clone()
                } else {
                    moved.clone()
                })
            },
        );
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("changed after inspection"),
            "{field}"
        );
        assert_eq!(reads.get(), 2);
    }
    assert_eq!(snapshot(&fixture.submission.checkout), before);
}

#[test]
fn sh871_clean_observer_refuses_conflict_and_base_policy_without_a_second_read() {
    for (pointer, conflict, expected) in [
        (POINTER.to_owned(), true, "still conflicts"),
        (
            POINTER.replace("enabled = true", "enabled = false"),
            false,
            "disabled in the pinned base",
        ),
        (
            POINTER.replace("version = 1", "version = 99"),
            false,
            "requires version 1",
        ),
    ] {
        let fixture = Fixture::new(&pointer, conflict);
        // Candidate and working-copy policy cannot opt an explicitly disabled
        // or malformed current base into this observation.
        fs::write(fixture.submission.checkout.join(".storyhook.toml"), POINTER).unwrap();
        let before = snapshot(&fixture.submission.checkout);
        let reads = Cell::new(0);
        let result = observe_with(
            &fixture.submission.head,
            deadline(),
            Cancellation::default(),
            || {
                reads.set(reads.get() + 1);
                Ok(fixture.submission.clone())
            },
        );
        assert!(
            result.err().unwrap().to_string().contains(expected),
            "{expected}"
        );
        assert_eq!(reads.get(), 1);
        assert_eq!(snapshot(&fixture.submission.checkout), before);
    }
}

#[test]
fn sh871_clean_observer_expiry_or_cancellation_refuses_before_metadata_reads() {
    for expired in [false, true] {
        let cancellation = Cancellation::default();
        if !expired {
            cancellation.cancel();
        }
        let end = if expired { Instant::now() } else { deadline() };
        let result = observe_with(&"a".repeat(40), end, cancellation, || {
            panic!("metadata must not run")
        });
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("expired or its owner cancelled")
        );
    }
}

#[test]
fn sh871_clean_observer_checks_cancellation_after_metadata_and_before_minting() {
    let fixture = Fixture::new(POINTER, false);
    for cancel_at in [1, 2] {
        let cancellation = Cancellation::default();
        let reads = Cell::new(0);
        let result = observe_with(
            &fixture.submission.head,
            deadline(),
            cancellation.clone(),
            || {
                reads.set(reads.get() + 1);
                if reads.get() == cancel_at {
                    cancellation.cancel();
                }
                Ok(fixture.submission.clone())
            },
        );
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("expired or its owner cancelled")
        );
        assert_eq!(reads.get(), cancel_at);
    }
}

#[test]
fn sh871_clean_capability_remains_bound_to_its_original_lifetime() {
    let fixture = Fixture::new(POINTER, false);
    let mut proof = fixture.observe().unwrap();
    proof.deadline = Instant::now();
    assert!(proof.check_live().is_err());
    let proof = fixture.observe().unwrap();
    proof.cancellation.cancel();
    assert!(proof.check_live().is_err());
}

#[test]
fn sh871_clean_capability_requires_the_same_live_cancellation_owner() {
    let fixture = Fixture::new(POINTER, false);
    let cancellation = Cancellation::default();
    let proof = observe_with(
        &fixture.submission.head,
        deadline(),
        cancellation.clone(),
        || Ok(fixture.submission.clone()),
    )
    .unwrap();
    proof.check_owner(&cancellation).unwrap();
    assert!(proof.check_owner(&Cancellation::default()).is_err());
    cancellation.cancel();
    assert!(proof.check_owner(&cancellation).is_err());
}

#[test]
fn sh871_clean_cleanup_refusal_never_becomes_success() {
    // Pure result-combination detector: actual native fixtures above use the
    // real close(). This does not claim an injected filesystem cleanup failure.
    assert!(settled(Ok("native clean answer"), Err(refuse("cleanup refusal"))).is_err());
    let both: Result<(), AppError> =
        settled(Err(refuse("merge refusal")), Err(refuse("cleanup refusal")));
    let detail = both.unwrap_err().to_string();
    assert!(detail.contains("merge refusal"));
    assert!(detail.contains("cleanup refusal"));
    assert!(settled::<()>(Err(refuse("merge refusal")), Ok(())).is_err());
}

#[test]
fn sh871_clean_evidence_rejects_unknown_wire_authority() {
    let fixture = Fixture::new(POINTER, false);
    let proof = fixture.observe().unwrap();
    let mut wire = serde_json::to_value(proof.evidence()).unwrap();
    wire["certified"] = serde_json::json!(true);
    assert!(serde_json::from_value::<CleanIntegrationEvidence>(wire).is_err());
}

#[test]
fn sh871_clean_and_first_inspection_ignore_source_policy_blob_replacements() {
    let disabled = POINTER.replace("enabled = true", "enabled = false");
    let fixture = Fixture::new(&disabled, false);
    let checkout = &fixture.submission.checkout;
    let original = git(
        checkout,
        &[
            "rev-parse",
            &format!("{}:.storyhook.toml", fixture.submission.base),
        ],
    );
    fs::write(checkout.join("enabled-policy-fixture"), POINTER).unwrap();
    let replacement = git(
        checkout,
        &["hash-object", "-w", "--", "enabled-policy-fixture"],
    );
    git(checkout, &["replace", &original, &replacement]);
    // Positive control proves the source namespace really substitutes enabled
    // bytes. Both SH-871 entry points must still read the disabled pinned blob.
    assert_eq!(
        git(checkout, &["cat-file", "blob", &original]),
        POINTER.trim()
    );
    let before = snapshot(checkout);
    assert!(
        fixture
            .observe()
            .err()
            .unwrap()
            .to_string()
            .contains("disabled in the pinned base")
    );
    let first = crate::service::integration_recovery::inspect(
        checkout,
        &fixture.submission.base,
        &fixture.submission.head,
        deadline(),
        Cancellation::default(),
    )
    .unwrap();
    assert!(
        matches!(first, crate::service::integration_recovery::Inspection::Held { reason }
        if reason.contains("disabled in the pinned base"))
    );
    assert_eq!(snapshot(checkout), before);
}

#[test]
fn sh871_clean_private_administration_does_not_inherit_source_grafts() {
    let fixture = Fixture::new(POINTER, false);
    fs::write(
        fixture.submission.checkout.join(".git/info/grafts"),
        format!("{}\n", fixture.submission.head),
    )
    .unwrap();
    let before = snapshot(&fixture.submission.checkout);
    let proof = fixture.observe().unwrap();
    assert_eq!(proof.evidence().tree, fixture.expected_tree);
    assert_eq!(snapshot(&fixture.submission.checkout), before);
}
