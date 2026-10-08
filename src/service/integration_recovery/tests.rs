use super::*;
use std::{fs, time::Duration};

const IDENTITY: &str = "schema = 1\nuuid = \"u\"\nprefix = \"SH\"\n";
const SINGLE: &str = "[integration]\nversion = 1\nenabled = true\npublication = \"managed-pr\"\nsmooth = [\"docs/\"]\n";

fn policy(tables: &str) -> Result<IntegrationPolicy, String> {
    policy_from_pointer(Some(format!("{IDENTITY}{tables}").as_bytes()))
}

#[test]
fn single_integration_requires_independent_explicit_policy_and_publication() {
    for tables in [
        "",
        "[batch]\nsmooth = [\"docs/\"]\n",
        "[integration]\nversion = 1\nsmooth = [\"docs/\"]\n",
    ] {
        assert!(!policy(tables).unwrap().enabled);
    }
    assert!(!policy_from_pointer(None).unwrap().enabled);
    assert!(policy(SINGLE).unwrap().enabled);
    for table in [
        SINGLE.replace("version = 1", "version = 2"),
        SINGLE.replace("publication = \"managed-pr\"\n", ""),
        SINGLE.replace("managed-pr", "rewrite-author"),
        SINGLE.replace("smooth", "smoth"),
        SINGLE.replace("[\"docs/\"]", "[]"),
    ] {
        assert!(policy(&table).is_err(), "accepted {table}");
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = crate::env::git_env::command(root)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().into()
}

struct Fixture {
    root: tempfile::TempDir,
    base: String,
    head: String,
}

impl Fixture {
    fn new(policy: &str, path: &str, ours: &[u8], theirs: &[u8]) -> Self {
        let root = storyhook_test_support::scratch_dir();
        git(root.path(), &["init", "-q"]);
        git(root.path(), &["config", "user.name", "fixture"]);
        git(
            root.path(),
            &["config", "user.email", "fixture@example.invalid"],
        );
        fs::create_dir_all(root.path().join(path).parent().unwrap()).unwrap();
        fs::write(root.path().join(path), "start\nend\n").unwrap();
        fs::write(
            root.path().join(".storyhook.toml"),
            format!("{IDENTITY}{policy}"),
        )
        .unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "common"]);
        let common = git(root.path(), &["rev-parse", "HEAD"]);
        fs::write(root.path().join(path), ours).unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "current base"]);
        let base = git(root.path(), &["rev-parse", "HEAD"]);
        git(root.path(), &["checkout", "-q", "--detach", &common]);
        fs::write(root.path().join(path), theirs).unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "submitted head"]);
        let head = git(root.path(), &["rev-parse", "HEAD"]);
        Self { root, base, head }
    }

    fn inspect(&self) -> Inspection {
        inspect(
            self.root.path(),
            &self.base,
            &self.head,
            Instant::now() + Duration::from_secs(30),
            Cancellation::default(),
        )
        .unwrap()
    }

    fn snapshot(&self) -> Vec<String> {
        vec![
            git(self.root.path(), &["rev-parse", "HEAD"]),
            git(self.root.path(), &["show-ref", "--head"]),
            git(self.root.path(), &["status", "--porcelain=v1"]),
            git(self.root.path(), &["count-objects", "-v"]),
        ]
    }
}

#[test]
fn native_single_proposal_preserves_both_insertions_and_all_source_authority() {
    let f = Fixture::new(
        SINGLE,
        "docs/guide.md",
        b"start\nbase addition\nend\n",
        b"start\nauthor addition\nend\n",
    );
    let before = f.snapshot();
    let Inspection::Proposed(proposal) = f.inspect() else {
        panic!("native insertion conflict was not proposed")
    };
    assert_eq!(proposal.files().len(), 1);
    assert_eq!(
        proposal.files()[0].resolved,
        "start\nbase addition\nauthor addition\nend\n"
    );
    assert_eq!(proposal.plan().base, f.base);
    assert_eq!(proposal.plan().head, f.head);
    let plan = proposal.settle().unwrap();
    assert_eq!(
        f.snapshot(),
        before,
        "private inspection changed author work, refs, or repository objects"
    );
    let Inspection::Proposed(replayed) = f.inspect() else {
        panic!("restarted inspection changed classification")
    };
    assert_eq!(replayed.settle().unwrap(), plan);
    assert_eq!(f.snapshot(), before);
}

#[test]
fn native_single_proposal_rejects_code_binary_semantic_and_candidate_policy_changes() {
    for (path, ours, theirs) in [
        (
            "src/lib.rs",
            &b"start\nbase\nend\n"[..],
            &b"start\nauthor\nend\n"[..],
        ),
        (
            "docs/guide.md",
            &b"start\nbase\0\nend\n"[..],
            &b"start\nauthor\0\nend\n"[..],
        ),
        (
            "docs/guide.md",
            &b"changed by base\nend\n"[..],
            &b"changed by author\nend\n"[..],
        ),
    ] {
        let f = Fixture::new(
            &SINGLE.replace("[\"docs/\"]", "[\"docs/\", \"src/\"]"),
            path,
            ours,
            theirs,
        );
        let before = f.snapshot();
        assert!(matches!(f.inspect(), Inspection::Held { .. }));
        assert_eq!(f.snapshot(), before);
    }
    let mut f = Fixture::new(
        "[batch]\nsmooth = [\"docs/\"]\n",
        "docs/guide.md",
        b"start\nbase\nend\n",
        b"start\nauthor\nend\n",
    );
    // The submitted branch cannot opt its own conflict into smoothing.
    fs::write(
        f.root.path().join(".storyhook.toml"),
        format!("{IDENTITY}{SINGLE}"),
    )
    .unwrap();
    git(f.root.path(), &["add", ".storyhook.toml"]);
    git(
        f.root.path(),
        &["commit", "-qm", "candidate tries enabling integration"],
    );
    f.head = git(f.root.path(), &["rev-parse", "HEAD"]);
    let before = f.snapshot();
    assert!(matches!(f.inspect(), Inspection::Held { .. }));
    assert_eq!(f.snapshot(), before);
}

#[test]
fn cancelled_single_inspection_creates_no_repository_effects() {
    let f = Fixture::new(
        SINGLE,
        "docs/guide.md",
        b"start\nbase\nend\n",
        b"start\nauthor\nend\n",
    );
    let before = f.snapshot();
    let cancelled = Cancellation::default();
    cancelled.cancel();
    assert!(
        inspect(
            f.root.path(),
            &f.base,
            &f.head,
            Instant::now() + Duration::from_secs(30),
            cancelled
        )
        .is_err()
    );
    assert_eq!(f.snapshot(), before);
}

mod owner;

#[test]
fn integration_native_origin_inspection_honors_existing_deadline_and_cancellation() {
    let f = Fixture::new(
        SINGLE,
        "docs/guide.md",
        b"start\nbase\nend\n",
        b"start\nauthor\nend\n",
    );
    git(
        f.root.path(),
        &[
            "config",
            "remote.origin.url",
            "https://github.com/acme/widgets.git",
        ],
    );
    let before = f.snapshot();
    let expired = Instant::now() - Duration::from_secs(1);
    assert!(
        crate::github_access::Repository::resolve_controlled(f.root.path(), expired, &|| false)
            .is_err(),
        "origin subquery renewed an expired operation deadline"
    );
    assert!(
        crate::github_access::Repository::resolve_controlled(
            f.root.path(),
            Instant::now() + Duration::from_secs(30),
            &|| true
        )
        .is_err(),
        "origin inspection ignored its owner cancellation"
    );
    assert_eq!(f.snapshot(), before);
}
