use super::super::{IntegrationPlan, SubmissionObservation, assembly::AssemblyPathIdentity};
use super::*;
use std::{cell::Cell, time::Duration};
fn assembly() -> AssemblyEvidence {
    let original = SubmissionObservation {
        checkout: "/registered/source".into(),
        repository: "github.example/org/repo".into(),
        pull_request: "https://github.example/org/repo/pull/17".into(),
        base_branch: "dev".into(),
        base: "a".repeat(40),
        head: "b".repeat(40),
    };
    let owner = "b0fefef1210b41adbf29a1fcd977a9c6";
    AssemblyEvidence {
        version: 1,
        owner: owner.into(),
        epoch: 2,
        branch: format!("storyhook/integration/{owner}"),
        workspace: AssemblyPathIdentity {
            path: "/historical/workspace/must-not-be-opened".into(),
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
        commit: "d".repeat(40),
        tree: "e".repeat(40),
        author: "fixture".into(),
        committer: "fixture".into(),
    }
}
fn deadline() -> Instant {
    Instant::now() + storyhook_test_support::load_grace::graced_now(Duration::from_secs(60))
}
fn observe(bytes: Vec<u8>) -> RetainedBranchObservation {
    observe_with(&assembly(), deadline(), &Cancellation::default(), |_| {
        Ok(bytes)
    })
}
fn is_unknown(observation: &RetainedBranchObservation) -> bool {
    matches!(observation.outcome, RetainedBranchOutcome::Unknown { .. })
}
#[test]
fn sh871_retained_branch_distinguishes_exact_moved_and_absent_without_effects() {
    let a = assembly();
    let reference = format!("refs/heads/{}", a.branch);
    assert_eq!(observe(Vec::new()).outcome, RetainedBranchOutcome::Absent);
    assert_eq!(
        observe(format!("{}\t{reference}\n", a.commit).into_bytes()).outcome,
        RetainedBranchOutcome::RetainedExact
    );
    let moved = "f".repeat(40);
    assert_eq!(
        observe(format!("{moved}\t{reference}\n").into_bytes()).outcome,
        RetainedBranchOutcome::MovedPreserved {
            observed_head: moved
        }
    );
}
#[test]
fn sh871_retained_branch_refuses_ambiguous_truncated_and_substituted_refs() {
    let a = assembly();
    let reference = format!("refs/heads/{}", a.branch);
    let good = format!("{}\t{reference}\n", a.commit);
    for bytes in [
        b"\n".to_vec(),
        b" \n".to_vec(),
        vec![0xff],
        format!("{good}{good}").into_bytes(),
        format!("{}\trefs/heads/dev\n", a.commit).into_bytes(),
        format!("{}\t{reference}-other\n", a.commit).into_bytes(),
        format!("{} {reference}\n", a.commit).into_bytes(),
        format!("abc\t{reference}\n").into_bytes(),
        format!("{}\t{reference}\nextra", a.commit).into_bytes(),
    ] {
        assert!(is_unknown(&observe(bytes)));
    }
}
#[test]
fn sh871_retained_branch_queries_only_exact_ref_once_and_keeps_binding() {
    let a = assembly();
    let calls = Cell::new(0);
    let result = observe_with(&a, deadline(), &Cancellation::default(), |reference| {
        calls.set(calls.get() + 1);
        assert_eq!(reference, format!("refs/heads/{}", a.branch));
        Ok(format!("{}\t{reference}\n", a.commit).into_bytes())
    });
    assert_eq!(calls.get(), 1);
    assert_eq!(result.owner, a.owner);
    assert_eq!(result.assembly_epoch, a.epoch);
    assert_eq!(result.repository, a.submission.repository);
    assert_eq!(result.expected_head, a.commit);
    // Historical workspace is intentionally nonexistent; no private path is
    // consulted by this observational binding/response path.
    assert_eq!(result.outcome, RetainedBranchOutcome::RetainedExact);
}
#[test]
fn sh871_retained_branch_transport_failure_is_unknown_and_never_retried() {
    let calls = Cell::new(0);
    let result = observe_with(&assembly(), deadline(), &Cancellation::default(), |_| {
        calls.set(calls.get() + 1);
        Err(refuse("fixture transport ambiguity"))
    });
    assert!(is_unknown(&result));
    assert_eq!(calls.get(), 1);
}
#[test]
fn sh871_retained_branch_expiry_and_cancellation_start_no_observation() {
    for expired in [false, true] {
        let cancellation = Cancellation::default();
        if !expired {
            cancellation.cancel();
        }
        let calls = Cell::new(0);
        let result = observe_with(
            &assembly(),
            if expired { Instant::now() } else { deadline() },
            &cancellation,
            |_| {
                calls.set(calls.get() + 1);
                Ok(Vec::new())
            },
        );
        assert!(is_unknown(&result));
        assert_eq!(calls.get(), 0);
    }
}
#[test]
fn sh871_retained_branch_post_read_cancellation_cannot_become_absence_or_success() {
    let a = assembly();
    for answer in [
        Vec::new(),
        format!("{}\trefs/heads/{}\n", a.commit, a.branch).into_bytes(),
    ] {
        let cancellation = Cancellation::default();
        let result = observe_with(&a, deadline(), &cancellation, |_| {
            cancellation.cancel();
            Ok(answer)
        });
        assert!(is_unknown(&result));
    }
}
#[test]
fn sh871_retained_branch_invalid_binding_never_reaches_native_reader() {
    for change in ["owner", "branch", "head", "checkout", "urn"] {
        let mut a = assembly();
        match change {
            "owner" => a.owner = "unknown-owner".into(),
            "branch" => a.branch = "author-branch".into(),
            "head" => a.commit = "short".into(),
            "checkout" => a.submission.checkout = "relative".into(),
            "urn" => {
                a.owner = format!("urn:uuid:{}", a.owner);
                a.branch = format!("storyhook/integration/{}", a.owner);
            }
            _ => unreachable!(),
        }
        let calls = Cell::new(0);
        let result = observe_with(&a, deadline(), &Cancellation::default(), |_| {
            calls.set(calls.get() + 1);
            Ok(Vec::new())
        });
        assert!(is_unknown(&result), "{change}");
        assert_eq!(calls.get(), 0);
    }
}
#[test]
fn sh871_retained_branch_unknown_detail_is_bounded_and_json_is_only_observational() {
    let result = observe_with(&assembly(), deadline(), &Cancellation::default(), |_| {
        Err(refuse(&"x".repeat(8192)))
    });
    let RetainedBranchOutcome::Unknown { detail } = &result.outcome else {
        panic!("must remain unknown")
    };
    assert!(detail.chars().count() <= 2048);
    let saved = serde_json::to_vec(&result).unwrap();
    let read: RetainedBranchObservation = serde_json::from_slice(&saved).unwrap();
    assert_eq!(read, result); // receipt replay creates no native/effect capability
}
