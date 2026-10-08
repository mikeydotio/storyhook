use super::super::{IntegrationPlan, assembly::AssemblyPathIdentity};
use super::*;
use crate::store::ReadOps;

fn assembly() -> AssemblyEvidence {
    let original = SubmissionObservation {
        checkout: "/registered/source".into(),
        repository: "github.example:8443/org/repo".into(),
        pull_request: "https://github.example:8443/org/repo/pull/17".into(),
        base_branch: "dev".into(),
        base: "a".repeat(40),
        head: "b".repeat(40),
    };
    AssemblyEvidence {
        version: 1,
        owner: "owner-1".into(),
        epoch: 2,
        branch: "storyhook/integration/owner-1".into(),
        workspace: AssemblyPathIdentity {
            path: "/private/objects".into(),
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
        author: "Fixture <fixture@example.test> 1 +0000".into(),
        committer: "Fixture <fixture@example.test> 1 +0000".into(),
    }
}
fn managed(a: &AssemblyEvidence) -> PullRequest {
    PullRequest {
        number: 18,
        html_url: "https://github.example:8443/org/repo/pull/18".into(),
        state: "open".into(),
        merged: false,
        body: Some("owner-marker\n\nManaged integration".into()),
        base: Side {
            sha: a.submission.base.clone(),
            branch: "dev".into(),
            repository: "org/repo".into(),
        },
        head: Side {
            sha: a.commit.clone(),
            branch: a.branch.clone(),
            repository: "org/repo".into(),
        },
    }
}

#[test]
fn sh871_publication_requires_exact_unambiguous_remote_ref() {
    let branch = "refs/heads/storyhook/integration/owner-1";
    let oid = "a".repeat(40);
    assert_eq!(parse_remote_head(b"", branch).unwrap(), None);
    assert_eq!(
        parse_remote_head(format!("{oid}\t{branch}\n").as_bytes(), branch).unwrap(),
        Some(oid.clone())
    );
    for answer in [
        format!("{oid}\trefs/heads/author\n"),
        format!("{oid}\t{branch}\n{oid}\t{branch}\n"),
        format!("abc\t{branch}\n"),
        format!("{oid} {branch}\n"),
        "\n".into(),
    ] {
        assert!(
            parse_remote_head(answer.as_bytes(), branch).is_err(),
            "{answer:?}"
        );
    }
}
#[test]
fn sh871_publication_requires_exact_tree_and_ordered_two_parents() {
    let a = assembly();
    assert!(
        validate_commit(
            format!("{}\n{} {}\n", a.tree, a.plan.base, a.plan.head).as_bytes(),
            &a
        )
        .is_ok()
    );
    for answer in [
        format!("{}\n{} {}\n", a.tree, a.plan.head, a.plan.base),
        format!("{}\n{}\n", a.tree, a.plan.base),
        format!("{}\n{} {} {}\n", a.tree, a.plan.base, a.plan.head, a.commit),
        format!("{}\n{} {}\n", a.commit, a.plan.base, a.plan.head),
    ] {
        assert!(validate_commit(answer.as_bytes(), &a).is_err());
    }
}
#[test]
fn sh871_publication_refuses_original_pr_movement_or_closure() {
    let a = assembly();
    let fresh = || {
        let mut pr = managed(&a);
        pr.number = 17;
        pr.html_url = a.submission.pull_request.clone();
        pr.head.sha = a.submission.head.clone();
        pr.head.branch = "author".into();
        pr
    };
    assert!(validate_original(&fresh(), &a.submission).is_ok());
    let changes: [fn(&mut PullRequest); 6] = [
        |p| p.state = "closed".into(),
        |p| p.merged = true,
        |p| p.base.sha = "f".repeat(40),
        |p| p.head.sha = "f".repeat(40),
        |p| p.base.branch = "main".into(),
        |p| p.html_url = "https://github.example/org/repo/pull/17".into(),
    ];
    for change in changes {
        let mut pr = fresh();
        change(&mut pr);
        assert!(validate_original(&pr, &a.submission).is_err());
    }
}
#[test]
fn sh871_publication_requires_distinct_exact_managed_pr_and_owner_marker() {
    let a = assembly();
    assert!(validate_managed(&managed(&a), &a.submission, &a, "owner-marker").is_ok());
    let changes: [fn(&mut PullRequest); 10] = [
        |p| p.number = 17,
        |p| p.html_url = "https://github.example/org/repo/pull/18".into(),
        |p| p.head.repository = "fork/repo".into(),
        |p| p.head.branch = "author".into(),
        |p| p.head.sha = "f".repeat(40),
        |p| p.base.sha = "f".repeat(40),
        |p| p.state = "closed".into(),
        |p| p.merged = true,
        |p| p.body = None,
        |p| p.body = Some("owner-marker\nowner-marker".into()),
    ];
    for change in changes {
        let mut pr = managed(&a);
        change(&mut pr);
        assert!(validate_managed(&pr, &a.submission, &a, "owner-marker").is_err());
    }
}
#[test]
fn sh871_publication_refuses_incomplete_or_ambiguous_pr_json() {
    for bytes in [
        b"{}".as_slice(),
        b"null",
        b"{\"number\":18}",
        b"[]",
        b"{}{}",
    ] {
        assert!(decode_pr(bytes).is_err());
    }
}

#[test]
fn sh871_publication_revalidates_private_objects_and_committed_policy() {
    use sha2::{Digest, Sha256};
    use std::time::Duration;
    let scratch = storyhook_test_support::scratch_dir();
    let root = scratch.path().canonicalize().unwrap();
    let deadline =
        Instant::now() + storyhook_test_support::load_grace::graced_now(Duration::from_secs(60));
    let run = |args: &[&str]| {
        let mut command = git_env::command(&root);
        command.args(args);
        String::from_utf8(capture(command, deadline, &|| false).unwrap())
            .unwrap()
            .trim()
            .to_owned()
    };
    run(&["init", "--bare", "--quiet"]);
    for (key, value) in [
        ("user.name", "Publication Fixture"),
        ("user.email", "publication@example.test"),
        ("storyhookIdentity.fixture.name", "Publication Fixture"),
        (
            "storyhookIdentity.fixture.email",
            "publication@example.test",
        ),
        ("storyhookIdentity.fixture.role", "both"),
        (
            "storyhookIdentity.fixture.reason",
            "Isolated real-Git test fixture identity",
        ),
    ] {
        run(&["config", "--local", key, value]);
    }
    let pointer = b"schema = 1\nuuid = \"fixture\"\nprefix = \"SH\"\n[integration]\nversion = 1\nenabled = true\npublication = \"managed-pr\"\nsmooth = [\"docs/\"]\n";
    let file = root.join("pointer-fixture");
    std::fs::write(&file, pointer).unwrap();
    let blob = run(&["hash-object", "-w", file.to_str().unwrap()]);
    run(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("100644,{blob},.storyhook.toml"),
    ]);
    let tree = run(&["write-tree"]);
    let base = run(&["commit-tree", "--no-gpg-sign", &tree, "-m", "base"]);
    let head = run(&[
        "commit-tree",
        "--no-gpg-sign",
        &tree,
        "-p",
        &base,
        "-m",
        "head",
    ]);
    let commit = run(&[
        "commit-tree",
        "--no-gpg-sign",
        &tree,
        "-p",
        &base,
        "-p",
        &head,
        "-m",
        "managed",
    ]);
    let mut a = assembly();
    a.workspace.path = root.clone();
    a.commit = commit;
    a.tree = tree;
    a.plan.base = base;
    a.plan.head = head;
    a.plan.policy = format!("{:x}", Sha256::digest(pointer));
    verify_objects(&a, deadline, &|| false).unwrap();
    let retained_policy = a.plan.policy.clone();
    a.plan.policy = "0".repeat(64);
    assert!(verify_objects(&a, deadline, &|| false).is_err());
    a.plan.policy = retained_policy;
    // Removing a reachable blob must not be hidden by an available source or
    // by a successful commit-header read. fsck must prove the private closure.
    std::fs::remove_file(root.join("objects").join(&blob[..2]).join(&blob[2..])).unwrap();
    assert!(verify_objects(&a, deadline, &|| false).is_err());
}

// Real local Git and Store ownership; only remote byte responses are replaced.
// The fixture never calls claim_publication_effect itself.
fn local_git(root: &Path, args: &[&str]) -> String {
    let mut command = git_env::command(root);
    command.args(args);
    let deadline = Instant::now()
        + storyhook_test_support::load_grace::graced_now(std::time::Duration::from_secs(30));
    let output = run_captured_query_quiescent(command, deadline, &|| false, 1024 * 1024, &[])
        .unwrap_or_else(|error| panic!("owned fixture Git query: {}", error.detail()));
    assert!(
        output.status.success(),
        "git {args:?}: {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout_truncated);
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn native_publication_fixture() -> (
    storyhook_test_support::ServiceFixture,
    crate::store::SqliteStore,
    crate::service::VerificationCandidate,
    BoundIntegrationProposal,
) {
    use crate::{
        service::{
            Ctx, NewStoryInput, PrLinkService, StoryService, VerificationQueue,
            attribution::{AttributionRecord, FailureCause, FailureComponent},
        },
        store::{GateAttempt, GateInputs, GateSubmission, ProjectId, ReadOps, WriteOps},
    };
    use std::io::Write;
    let fixture = storyhook_test_support::ServiceFixture::new();
    let root = fixture.github_checkout("https://github.com/acme/widgets.git");
    local_git(&root, &["config", "user.name", "Publication fixture"]);
    local_git(&root, &["config", "user.email", "publication@example.test"]);
    storyhook_test_support::approve_fixture_identity(
        &root,
        "Publication fixture",
        "publication@example.test",
    );
    let mut pointer = std::fs::OpenOptions::new()
        .append(true)
        .open(root.join(".storyhook.toml"))
        .unwrap();
    writeln!(pointer, "\n[integration]\nversion = 1\nenabled = true\npublication = \"managed-pr\"\nsmooth = [\"docs/\"]").unwrap();
    drop(pointer);
    std::fs::create_dir(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/guide.md"), "start\nend\n").unwrap();
    local_git(&root, &["add", "."]);
    local_git(&root, &["commit", "-qm", "common"]);
    let common = local_git(&root, &["rev-parse", "HEAD"]);
    std::fs::write(root.join("docs/guide.md"), "start\nbase addition\nend\n").unwrap();
    local_git(&root, &["commit", "-qam", "base"]);
    let base = local_git(&root, &["rev-parse", "HEAD"]);
    local_git(&root, &["checkout", "--detach", "-q", &common]);
    std::fs::write(root.join("docs/guide.md"), "start\nauthor addition\nend\n").unwrap();
    local_git(&root, &["commit", "-qam", "author"]);
    let head = local_git(&root, &["rev-parse", "HEAD"]);
    let store = crate::store::SqliteStore::open(fixture.store().path()).unwrap();
    let project = ProjectId::new(fixture.project().get());
    let environment = crate::env::Environment::at(fixture.cwd()).with_subprocess_patience();
    let ctx = Ctx::new(&store, project, &root, environment).no_hooks(true);
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "publication transport fixture".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&ctx)
        .link(&id, "https://github.com/acme/widgets/pull/7", true)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    let candidate = VerificationQueue::new(&store)
        .with_environment(ctx.env().clone())
        .next()
        .unwrap()
        .unwrap();
    let submission = GateSubmission {
        project,
        story_id: id,
        generation: candidate.verifying_generation,
        submitted_at: candidate.verifying_since.clone(),
    };
    const AT: &str = "2026-10-05T00:00:00Z";
    let mut attempt = GateAttempt::new(
        "publication-original-conflict".into(),
        submission.clone(),
        AT,
    );
    attempt.control_revision = Some(
        store
            .read(|tx| tx.verification_control_revision(project))
            .unwrap(),
    );
    let attribution = AttributionRecord {
        version: 1,
        id: "publication-attribution".into(),
        revision: 0,
        submission,
        attempt: attempt.id.clone(),
        inputs: GateInputs {
            head: Some(head.clone()),
            base: Some(base.clone()),
            ..Default::default()
        },
        created_at: AT.into(),
        components: vec![FailureComponent {
            id: "integration".into(),
            check: "native-merge".into(),
            signature: "insertions conflict".into(),
            requirement: "preserve both parents".into(),
            log: "real local fixture".into(),
            observed_cause: FailureCause::Integration,
        }],
        preparation: None,
        settlement: None,
        plans: vec![],
        probes: vec![],
        assessments: vec![],
        diagnosis_ms: 0,
        held: true,
        retired: None,
    };
    store
        .write(|tx| {
            tx.insert_gate_attempt(&attempt)?;
            tx.insert_attribution(&attribution)
        })
        .unwrap();
    attempt.revision = 1;
    attempt.finished_at = Some(AT.into());
    attempt.verdict = Some("conflict".into());
    assert!(
        store
            .write(|tx| tx.update_gate_attempt(&attempt, 0))
            .unwrap()
    );
    let deadline = Instant::now()
        + ctx
            .env()
            .subprocess_bound(std::time::Duration::from_secs(90));
    let cancellation = Cancellation::default();
    let super::super::Inspection::Proposed(proposal) =
        super::super::inspect(&root, &base, &head, deadline, cancellation.clone()).unwrap()
    else {
        panic!("real conflict did not produce a proposal");
    };
    let proof = BoundIntegrationProposal {
        proposal,
        deadline,
        cancellation,
        submission: SubmissionObservation {
            checkout: root,
            repository: "github.com/acme/widgets".into(),
            pull_request: "https://github.com/acme/widgets/pull/7".into(),
            base_branch: "dev".into(),
            base,
            head,
        },
    };
    (fixture, store, candidate, proof)
}

fn publication_claim(
    service: &IntegrationOwnerService<'_, crate::store::SqliteStore>,
    candidate: &crate::service::VerificationCandidate,
    proof: &BoundIntegrationProposal,
) -> PublicationClaim {
    let record = service
        .reserve(candidate, "publication-attribution", "integration", proof)
        .unwrap();
    let claim = service
        .claim_assembly(&record.id, record.revision, proof)
        .unwrap()
        .unwrap();
    let native =
        super::super::assemble_owned(service, &claim, proof, proof.deadline, &proof.cancellation)
            .unwrap();
    let assembled = service
        .accept_assembly(claim, native, proof)
        .unwrap()
        .unwrap();
    service
        .claim_publication(assembled, proof)
        .unwrap()
        .unwrap()
}

#[test]
fn sh871_publication_transport_runs_real_effect_order_and_native_acceptance() {
    let (fixture, store, candidate, proof) = native_publication_fixture();
    let ctx = crate::service::Ctx::new(
        &store,
        candidate.project,
        &candidate.checkout,
        crate::env::Environment::at(fixture.cwd()).with_subprocess_patience(),
    )
    .no_hooks(true);
    let service = IntegrationOwnerService::new(&ctx);
    let mut claim = publication_claim(&service, &candidate, &proof);
    let before = local_git(&candidate.checkout, &["show-ref", "--head"]);
    let remote =
        worker_fixture::Remote::new(proof.submission().clone(), 99, worker_fixture::Fault::None)
            .unwrap();
    let native = remote
        .publish(
            &service,
            &mut claim,
            &proof,
            proof.deadline,
            &proof.cancellation,
        )
        .unwrap();
    assert_eq!(
        claim.effects(),
        &[
            PublicationEffect::PushBranch,
            PublicationEffect::CreatePullRequest
        ]
    );
    assert_eq!(native.evidence().commit, claim.assembly().commit);
    assert_eq!(
        native.evidence().parents,
        [proof.plan().base.clone(), proof.plan().head.clone()]
    );
    let snapshot = remote.snapshot();
    assert_eq!(snapshot.push_calls, 1);
    assert_eq!(snapshot.create_calls, 1);
    assert_eq!(
        snapshot.requests,
        [
            "read-original-pr",
            "read-branch",
            "push",
            "read-original-pr",
            "read-branch",
            "create-pr",
            "read-managed-pr",
            "read-original-pr",
            "read-branch"
        ]
    );
    let id = claim.id().to_string();
    let accepted = service
        .accept_publication(claim, native, &proof)
        .unwrap()
        .unwrap();
    accepted.validate_custody().unwrap();
    let state = service.show(&id).unwrap();
    assert_eq!(state.1.phase, super::super::IntegrationPhase::Published);
    let row = store
        .read(|tx| tx.story(candidate.project, state.0.story))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "verifying", "publication claimed completion");
    let links = store
        .read(|tx| tx.open_pr_links_for_story(candidate.project, state.0.story))
        .unwrap();
    assert!(
        links
            .iter()
            .any(|link| link.url == proof.submission().pull_request && link.close_on_merge),
        "publication changed original PR custody"
    );
    assert_eq!(
        local_git(&candidate.checkout, &["show-ref", "--head"]),
        before
    );
    assert_eq!(
        local_git(&candidate.checkout, &["rev-parse", "HEAD"]),
        proof.plan().head
    );
    proof.settle().unwrap();
}

#[test]
fn sh871_publication_transport_unknown_effects_retain_real_intents_without_replay() {
    for fault in [
        worker_fixture::Fault::PushReplyLost,
        worker_fixture::Fault::CreateReplyLost,
        worker_fixture::Fault::MovedAfterPush,
        worker_fixture::Fault::MalformedManagedPr,
    ] {
        let (fixture, store, candidate, proof) = native_publication_fixture();
        let ctx = crate::service::Ctx::new(
            &store,
            candidate.project,
            &candidate.checkout,
            crate::env::Environment::at(fixture.cwd()).with_subprocess_patience(),
        )
        .no_hooks(true);
        let service = IntegrationOwnerService::new(&ctx);
        let mut claim = publication_claim(&service, &candidate, &proof);
        let remote = worker_fixture::Remote::new(proof.submission().clone(), 99, fault).unwrap();
        assert!(
            remote
                .publish(
                    &service,
                    &mut claim,
                    &proof,
                    proof.deadline,
                    &proof.cancellation
                )
                .is_err()
        );
        let before = remote.snapshot();
        assert_eq!(before.push_calls, 1);
        let created = matches!(
            fault,
            worker_fixture::Fault::CreateReplyLost | worker_fixture::Fault::MalformedManagedPr
        );
        assert_eq!(before.create_calls, usize::from(created));
        assert_eq!(claim.effects().len(), if created { 2 } else { 1 });
        let retained = service.show(claim.id()).unwrap();
        assert!(retained.0.active);
        assert_eq!(retained.1.phase, super::super::IntegrationPhase::Publishing);
        assert_eq!(retained.1.publication_effects, claim.effects());
        assert!(claim.assembly().workspace.path.exists());
        assert!(
            remote
                .publish(
                    &service,
                    &mut claim,
                    &proof,
                    proof.deadline,
                    &proof.cancellation
                )
                .is_err()
        );
        assert_eq!(
            remote.snapshot(),
            before,
            "unknown effect was repeated: {fault:?}"
        );
        assert_eq!(service.show(claim.id()).unwrap(), retained);
        proof.settle().unwrap();
    }
}

#[test]
fn sh871_publication_transport_cancelled_owner_makes_no_remote_request() {
    let (fixture, store, candidate, proof) = native_publication_fixture();
    let ctx = crate::service::Ctx::new(
        &store,
        candidate.project,
        &candidate.checkout,
        crate::env::Environment::at(fixture.cwd()).with_subprocess_patience(),
    )
    .no_hooks(true);
    let service = IntegrationOwnerService::new(&ctx);
    let mut claim = publication_claim(&service, &candidate, &proof);
    let before = service.show(claim.id()).unwrap();
    let remote =
        worker_fixture::Remote::new(proof.submission().clone(), 99, worker_fixture::Fault::None)
            .unwrap();
    proof.cancellation.cancel();
    assert!(
        remote
            .publish(
                &service,
                &mut claim,
                &proof,
                proof.deadline,
                &proof.cancellation
            )
            .is_err()
    );
    assert!(remote.snapshot().requests.is_empty());
    assert!(claim.effects().is_empty());
    assert_eq!(service.show(claim.id()).unwrap(), before);
    // Cleanup of known-quiescent private objects still occurs, but cancelled
    // native inspection cannot be returned as a successful proof receipt.
    let error = proof.settle().unwrap_err().to_string();
    assert!(
        error.contains("expired or cancelled after explicit cleanup"),
        "{error}"
    );
}
