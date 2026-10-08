use super::super::{IntegrationPlan, assembly::AssemblyPathIdentity};
use super::*;

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
