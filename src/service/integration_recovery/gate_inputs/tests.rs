use super::super::{IntegrationPlan, SubmissionObservation, assembly::AssemblyPathIdentity};
use super::*;
use std::{cell::RefCell, time::Duration};

const OWNER: &str = "00000000-0000-4000-8000-000000000001";
const ATTEMPT: &str = "00000000-0000-4000-8000-000000000002";
struct Fixture {
    assembly: AssemblyEvidence,
    publication: PublicationEvidence,
    original: serde_json::Value,
    managed: serde_json::Value,
    remote_base: Option<String>,
    remote_head: Option<String>,
    refuse_private: bool,
    refuse_ancestor: Option<String>,
    calls: RefCell<Vec<String>>,
}
impl Fixture {
    fn new() -> Self {
        let original = SubmissionObservation {
            checkout: "/registered/source".into(),
            repository: "github.example:8443/org/repo".into(),
            pull_request: "https://github.example:8443/org/repo/pull/17".into(),
            base_branch: "dev".into(),
            base: "a".repeat(40),
            head: "b".repeat(40),
        };
        let assembly = AssemblyEvidence {
            version: 1,
            owner: OWNER.into(),
            epoch: 1,
            branch: "storyhook/integration/owner".into(),
            workspace: AssemblyPathIdentity {
                path: "/owned/private".into(),
                device: 1,
                inode: 2,
            },
            stamp_sha256: "c".repeat(64),
            submission: original.clone(),
            plan: IntegrationPlan {
                version: 1,
                base: original.base.clone(),
                head: original.head.clone(),
                conflicted_tree: "c".repeat(40),
                policy: "d".repeat(64),
                strategy: "fixture".into(),
                files: vec![],
            },
            commit: "e".repeat(40),
            tree: "f".repeat(40),
            author: "Fixture <fixture@example.test> 1 +0000".into(),
            committer: "Fixture <fixture@example.test> 1 +0000".into(),
        };
        let publication = PublicationEvidence {
            version: 1,
            owner: OWNER.into(),
            epoch: 2,
            original: original.clone(),
            branch: assembly.branch.clone(),
            commit: assembly.commit.clone(),
            tree: assembly.tree.clone(),
            parents: [original.base.clone(), original.head.clone()],
            marker: "owner-marker".into(),
            pull_request: "https://github.example:8443/org/repo/pull/18".into(),
            number: 18,
        };
        let original_json = serde_json::json!({"number":17,"html_url":original.pull_request,"state":"open","merged":false,"body":null,"base":{"sha":original.base,"ref":"dev","repository":"org/repo"},"head":{"sha":original.head,"ref":"author","repository":"fork/repo"}});
        let managed = serde_json::json!({"number":18,"html_url":publication.pull_request,"state":"open","merged":false,"body":"owner-marker","base":{"sha":original.base,"ref":"dev","repository":"org/repo"},"head":{"sha":assembly.commit,"ref":assembly.branch,"repository":"org/repo"}});
        Self {
            remote_base: Some(original.base),
            remote_head: Some(assembly.commit.clone()),
            assembly,
            publication,
            original: original_json,
            managed,
            refuse_private: false,
            refuse_ancestor: None,
            calls: RefCell::new(vec![]),
        }
    }
    fn observe(&self) -> Result<IntegrationGateInputsEvidence, AppError> {
        read_inputs(OWNER, ATTEMPT, &self.publication, &self.assembly, self)
    }
}
impl Reader for Fixture {
    fn pull_request(&self, number: u64) -> Result<Vec<u8>, AppError> {
        self.calls.borrow_mut().push(format!("pr:{number}"));
        Ok(serde_json::to_vec(if number == 17 {
            &self.original
        } else if number == 18 {
            &self.managed
        } else {
            panic!("unexpected PR")
        })
        .unwrap())
    }
    fn private_objects(&self) -> Result<(), AppError> {
        self.calls.borrow_mut().push("private".into());
        if self.refuse_private {
            Err(refuse("fixture object/policy refusal"))
        } else {
            Ok(())
        }
    }
    fn ancestor(&self, ancestor: &str, descendant: &str) -> Result<(), AppError> {
        assert_eq!(descendant, self.assembly.commit);
        self.calls.borrow_mut().push(format!("ancestor:{ancestor}"));
        if self.refuse_ancestor.as_deref() == Some(ancestor) {
            Err(refuse("fixture ancestry refusal"))
        } else {
            Ok(())
        }
    }
    fn remote_head(&self, branch: &str) -> Result<Option<String>, AppError> {
        self.calls.borrow_mut().push(format!("ref:{branch}"));
        if branch == self.publication.branch {
            Ok(self.remote_head.clone())
        } else if branch == "dev" {
            Ok(self.remote_base.clone())
        } else {
            panic!("unexpected remote branch")
        }
    }
}

#[test]
fn sh871_gate_inputs_require_fresh_exact_base_and_managed_remote_head() {
    let fixture = Fixture::new();
    let observed = fixture.observe().unwrap();
    assert_eq!(observed.current_base, fixture.assembly.plan.base);
    assert_eq!(observed.tree, fixture.assembly.tree);
    assert_eq!(fixture.calls.borrow().len(), 7);
    for base in [None, Some("0".repeat(40))] {
        let mut f = Fixture::new();
        f.remote_base = base;
        assert!(f.observe().is_err());
    }
    for head in [None, Some("0".repeat(40))] {
        let mut f = Fixture::new();
        f.remote_head = head;
        assert!(f.observe().is_err());
    }
}
#[test]
fn sh871_gate_inputs_refuse_original_or_managed_pr_identity_changes() {
    for original in [true, false] {
        for (pointer, value) in [
            ("/state", serde_json::json!("closed")),
            ("/merged", serde_json::json!(true)),
            ("/head/sha", serde_json::json!("0".repeat(40))),
            ("/base/ref", serde_json::json!("main")),
            (
                "/html_url",
                serde_json::json!("https://github.example/org/repo/pull/18"),
            ),
        ] {
            let mut f = Fixture::new();
            let metadata = if original {
                &mut f.original
            } else {
                &mut f.managed
            };
            *metadata.pointer_mut(pointer).unwrap() = value;
            assert!(f.observe().is_err(), "{original} {pointer}");
            assert!(!f.calls.borrow().iter().any(|call| call == "private"));
        }
    }
    for (pointer, value) in [
        ("/body", serde_json::json!("different-owner")),
        ("/head/repository", serde_json::json!("fork/repo")),
    ] {
        let mut f = Fixture::new();
        *f.managed.pointer_mut(pointer).unwrap() = value;
        assert!(f.observe().is_err());
    }
}
#[test]
fn sh871_gate_inputs_refuse_a_different_pr_with_matching_branch_and_marker() {
    let mut f = Fixture::new();
    f.managed["number"] = serde_json::json!(19);
    f.managed["html_url"] = serde_json::json!("https://github.example:8443/org/repo/pull/19");
    assert!(f.observe().is_err());
    assert!(!f.calls.borrow().iter().any(|call| call == "private"));
}

#[test]
fn sh871_gate_inputs_require_private_closure_policy_and_both_parent_ancestries() {
    let mut f = Fixture::new();
    f.refuse_private = true;
    assert!(f.observe().is_err());
    assert_eq!(*f.calls.borrow(), vec!["pr:17", "pr:18", "private"]);
    for parent in ["a".repeat(40), "b".repeat(40)] {
        let mut f = Fixture::new();
        f.refuse_ancestor = Some(parent);
        assert!(f.observe().is_err());
        assert!(!f.calls.borrow().iter().any(|call| call.starts_with("ref:")));
    }
}
#[test]
fn sh871_gate_inputs_consumption_binds_owner_attempt_and_assembly() {
    let f = Fixture::new();
    let native = fixture_gate_inputs(
        f.observe().unwrap(),
        Instant::now() + Duration::from_secs(60),
        Cancellation::default(),
    );
    native
        .validate_observation(OWNER, ATTEMPT, &f.publication, &f.assembly)
        .unwrap();
    assert!(
        native
            .validate_observation("another-owner", ATTEMPT, &f.publication, &f.assembly)
            .is_err()
    );
    assert!(
        native
            .validate_observation(OWNER, "another-attempt", &f.publication, &f.assembly)
            .is_err()
    );
    let mut publication = f.publication.clone();
    publication.epoch += 1;
    assert!(
        native
            .validate_observation(OWNER, ATTEMPT, &publication, &f.assembly)
            .is_err()
    );
    let mut assembly = f.assembly.clone();
    assembly.tree = "0".repeat(40);
    assert!(
        native
            .validate_observation(OWNER, ATTEMPT, &f.publication, &assembly)
            .is_err()
    );
}
#[test]
fn sh871_gate_inputs_cannot_renew_original_observation_lifetime() {
    let f = Fixture::new();
    let cancelled = Cancellation::default();
    let native = fixture_gate_inputs(
        f.observe().unwrap(),
        Instant::now() + Duration::from_secs(60),
        cancelled.clone(),
    );
    cancelled.cancel();
    // New external control state cannot replace the token retained by native.
    assert!(!Cancellation::default().is_cancelled());
    assert!(
        native
            .validate_observation(OWNER, ATTEMPT, &f.publication, &f.assembly)
            .is_err()
    );
    let expired = fixture_gate_inputs(
        f.observe().unwrap(),
        Instant::now(),
        Cancellation::default(),
    );
    assert!(
        expired
            .validate_observation(OWNER, ATTEMPT, &f.publication, &f.assembly)
            .is_err()
    );
}

#[test]
fn sh871_gate_inputs_recheck_observation_cancel_after_native_custody() {
    let f = Fixture::new();
    let observation_token = Cancellation::default();
    let claim_token = Cancellation::default();
    let native = fixture_gate_inputs(
        f.observe().unwrap(),
        Instant::now() + Duration::from_secs(60),
        observation_token.clone(),
    );
    native
        .validate_observation(OWNER, ATTEMPT, &f.publication, &f.assembly)
        .unwrap();
    let result = native.finish_custody(|| {
        assert!(!claim_token.is_cancelled());
        observation_token.cancel();
        Ok(())
    });
    assert!(result.is_err());
    assert!(!claim_token.is_cancelled());
}
