use super::*;
use std::{
    cell::{Cell, RefCell},
    time::Duration,
};
fn oid(c: char) -> String {
    c.to_string().repeat(40)
}
fn publication() -> PublicationEvidence {
    PublicationEvidence {
        version: 1,
        owner: "owner".into(),
        epoch: 2,
        original: super::super::SubmissionObservation {
            checkout: "/historical/checkout/not-opened-by-reader".into(),
            repository: "github.example/org/repo".into(),
            pull_request: "https://github.example/org/repo/pull/1".into(),
            base_branch: "dev".into(),
            base: oid('a'),
            head: oid('b'),
        },
        branch: "storyhook/integration/owner".into(),
        commit: oid('c'),
        tree: oid('d'),
        parents: [oid('a'), oid('b')],
        marker: "<!-- exact owner marker -->".into(),
        pull_request: "https://github.example/org/repo/pull/2".into(),
        number: 2,
    }
}
fn pr() -> serde_json::Value {
    let p = publication();
    serde_json::json!({"number":2,"html_url":p.pull_request,"state":"closed","merged":true,"merge_commit_sha":oid('e'),"body":p.marker,
        "base":{"sha":oid('f'),"ref":"dev","repository":"org/repo"},
        "head":{"sha":p.commit,"ref":p.branch,"repository":"org/repo"}})
}
struct Fixture {
    pr: serde_json::Value,
    tree: String,
    fail_ancestor: Option<String>,
    moved: bool,
    pr_reads: Cell<usize>,
    base_reads: Cell<usize>,
    calls: RefCell<Vec<Vec<String>>>,
}
impl Default for Fixture {
    fn default() -> Self {
        Self {
            pr: pr(),
            tree: oid('d'),
            fail_ancestor: None,
            moved: false,
            pr_reads: Cell::new(0),
            base_reads: Cell::new(0),
            calls: RefCell::new(Vec::new()),
        }
    }
}
impl Reader for Fixture {
    fn pr(&self, _: u64) -> Result<Vec<u8>, AppError> {
        self.pr_reads.set(self.pr_reads.get() + 1);
        Ok(serde_json::to_vec(&self.pr).unwrap())
    }
    fn base(&self, _: &str) -> Result<String, AppError> {
        self.base_reads.set(self.base_reads.get() + 1);
        Ok(if self.moved && self.base_reads.get() > 1 {
            oid('a')
        } else {
            oid('f')
        })
    }
    fn fetch(&self, oids: &[&str]) -> Result<(), AppError> {
        self.calls.borrow_mut().push(strings(oids));
        Ok(())
    }
    fn git(&self, args: &[&str]) -> Result<Vec<u8>, AppError> {
        self.calls.borrow_mut().push(strings(args));
        if args[0] == "merge-base" && self.fail_ancestor.as_deref() == Some(args[2]) {
            return Err(refuse("fixture ancestry failure"));
        }
        if args[0] == "rev-parse" {
            return Ok(if args[2].starts_with(&oid('e')) {
                self.tree.clone()
            } else {
                oid('a')
            }
            .into_bytes());
        }
        Ok(Vec::new())
    }
}
fn observe(f: &Fixture) -> Result<IntegrationLandedEvidence, AppError> {
    read_landed("owner", "intent", &publication(), &oid('d'), f)
}
#[test]
fn sh871_landed_requires_native_tree_and_all_three_ancestries() {
    let f = Fixture::default();
    let e = observe(&f).unwrap();
    assert_eq!(e.merge_tree, oid('d'));
    assert_eq!(e.observed_base_tree, oid('a')); // later commits may have another tree
    assert_eq!(e.original_pr, publication().original.pull_request);
    assert_eq!(f.pr_reads.get(), 2);
    let calls = f.calls.borrow();
    assert!(calls.iter().any(|c| c.first().is_some_and(|s| s == "fsck")));
    for (a, d) in [
        (oid('c'), oid('e')),
        (oid('b'), oid('e')),
        (oid('e'), oid('f')),
    ] {
        assert!(calls.contains(&vec!["merge-base".into(), "--is-ancestor".into(), a, d]));
    }
}
#[test]
fn sh871_landed_merged_metadata_cannot_replace_certified_tree() {
    let f = Fixture {
        tree: oid('a'),
        ..Fixture::default()
    };
    assert!(
        observe(&f)
            .unwrap_err()
            .to_string()
            .contains("certified tree")
    );
}
#[test]
fn sh871_landed_refuses_each_missing_ancestry() {
    for ancestor in ['b', 'c', 'e'] {
        let f = Fixture {
            fail_ancestor: Some(oid(ancestor)),
            ..Fixture::default()
        };
        assert!(observe(&f).is_err(), "{ancestor}");
        assert_eq!(f.pr_reads.get(), 1);
    }
}
#[test]
fn sh871_landed_refuses_wrong_managed_identity_before_fetch() {
    for (path, value) in [
        ("/number", serde_json::json!(3)),
        ("/merged", serde_json::json!(false)),
        ("/head/sha", serde_json::json!(oid('a'))),
        ("/head/repository", serde_json::json!("evil/repo")),
        ("/base/ref", serde_json::json!("main")),
        ("/body", serde_json::json!("other marker")),
        ("/merge_commit_sha", serde_json::Value::Null),
    ] {
        let mut f = Fixture::default();
        *f.pr.pointer_mut(path).unwrap() = value;
        assert!(observe(&f).is_err(), "{path}");
        assert!(f.calls.borrow().is_empty());
    }
}
#[test]
fn sh871_landed_refuses_target_movement_without_repeating_fetch() {
    let f = Fixture {
        moved: true,
        ..Fixture::default()
    };
    assert!(
        observe(&f)
            .unwrap_err()
            .to_string()
            .contains("changed during proof")
    );
    assert_eq!(
        f.calls
            .borrow()
            .iter()
            .filter(|c| c.first() == Some(&oid('c')))
            .count(),
        1
    );
}
#[test]
fn sh871_landed_final_lifetime_check_follows_slow_custody() {
    let cancellation = Cancellation::default();
    let result = finish_lifetime(
        Instant::now() + Duration::from_secs(60),
        &cancellation,
        || {
            cancellation.cancel();
            Ok(())
        },
    );
    assert!(result.is_err());
}
#[test]
fn sh871_landed_expired_observation_never_gets_renewed() {
    assert!(live(Instant::now(), &Cancellation::default()).is_err());
}
