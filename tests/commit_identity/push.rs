//! Outgoing commit ranges and historical inventory contracts.

use super::*;

#[test]
fn matching_and_absent_local_identity_pass_commit_and_push() {
    for local in [false, true] {
        let repo = Repo::new();
        if local {
            repo.git(&["config", "user.name", "Correct Person"]);
            repo.git(&["config", "user.email", "correct@example.test"]);
        }
        ok(&repo.commit());
        ok(&repo.push());
        ok(&repo.audit(&[]));
    }
}

#[test]
fn push_checks_stored_identity_after_config_is_repaired_and_receipts_bypassed() {
    for role in ["author", "committer"] {
        let repo = Repo::new();
        repo.git(&[
            "-c",
            &format!("{role}.email=incorrect"),
            "commit",
            "--no-verify",
            "--allow-empty",
            "-qm",
            "bad",
        ]);
        refused(&repo.push());
        refused(
            &repo
                .command("git")
                .env("SKIP_PREPUSH_TESTS", "1")
                .args(["push", "origin", "HEAD:refs/heads/feature"])
                .output()
                .unwrap(),
        );
        assert!(
            !repo
                .command("git")
                .args([
                    "--git-dir=remote",
                    "rev-parse",
                    "--verify",
                    "refs/heads/feature"
                ])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
}

#[test]
fn audit_reports_old_bad_history_but_push_checks_only_outgoing_commits() {
    let repo = Repo::new();
    repo.git(&[
        "-c",
        "user.email=incorrect",
        "commit",
        "--no-verify",
        "--allow-empty",
        "-qm",
        "old bad",
    ]);
    repo.git(&["push", "--no-verify", "origin", "HEAD:refs/heads/feature"]);
    ok(&repo.commit());
    ok(&repo.push());
    let out = repo.audit(&[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("incorrect"));
    assert!(text(&out).contains("review"));
    ok(&repo.audit(&["HEAD~1..HEAD"]));
    assert_eq!(repo.audit(&["missing-ref"]).status.code(), Some(2));
}

#[test]
fn push_does_not_truncate_at_fifty_commits() {
    let repo = Repo::new();
    repo.git(&[
        "-c",
        "user.email=incorrect",
        "commit",
        "--no-verify",
        "--allow-empty",
        "-qm",
        "old bad",
    ]);
    for _ in 0..51 {
        repo.git(&["commit", "--no-verify", "--allow-empty", "-qm", "good"]);
    }
    refused(&repo.push());
}

#[test]
fn mailmap_and_replace_refs_cannot_hide_stored_metadata() {
    let repo = Repo::new();
    repo.git(&[
        "-c",
        "user.email=incorrect",
        "commit",
        "--no-verify",
        "--allow-empty",
        "-qm",
        "bad",
    ]);
    let bad = String::from_utf8(repo.git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    repo.git(&[
        "commit",
        "--no-verify",
        "--allow-empty",
        "--amend",
        "--reset-author",
        "-qm",
        "good",
    ]);
    let good = String::from_utf8(repo.git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    repo.git(&["replace", &bad, &good]);
    repo.git(&["update-ref", "refs/heads/feature", &bad]);
    fs::write(
        repo.path().join(".mailmap"),
        "Correct Person <correct@example.test> <incorrect>\n",
    )
    .unwrap();
    refused(&repo.push());
    assert_eq!(repo.audit(&["HEAD"]).status.code(), Some(1));
}

#[test]
fn tags_deletions_and_multiple_refs_are_checked_without_losing_stdin() {
    let repo = Repo::new();
    ok(&repo.commit());
    ok(&repo.push());
    repo.git(&["tag", "-am", "good tag", "good"]);
    repo.git(&["push", "origin", "refs/tags/good"]);
    repo.git(&["push", "origin", ":refs/tags/good"]);
    let tree = String::from_utf8(repo.git(&["rev-parse", "HEAD^{tree}"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    repo.git(&["tag", "tree", &tree]);
    repo.git(&["push", "origin", "refs/tags/tree"]);
    repo.git(&[
        "-c",
        "author.email=incorrect",
        "commit",
        "--no-verify",
        "--allow-empty",
        "-qm",
        "bad",
    ]);
    repo.git(&["tag", "-am", "bad tag", "bad"]);
    refused(
        &repo
            .command("git")
            .args(["push", "origin", "HEAD:refs/heads/new", "refs/tags/bad"])
            .output()
            .unwrap(),
    );
}

#[test]
fn malformed_push_range_fails_loudly() {
    let repo = Repo::new();
    let out = repo.push_record(
        "refs/heads/x invalid refs/heads/x 0000000000000000000000000000000000000000\n",
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}

#[test]
fn imported_author_requires_explicit_approval_without_changing_authorship() {
    let repo = Repo::new();
    ok(&repo.commit());
    repo.git(&["checkout", "-qb", "contributor"]);
    fs::write(repo.path().join("contribution"), "contribution\n").unwrap();
    repo.git(&["add", "contribution"]);
    repo.git(&[
        "commit",
        "--no-verify",
        "--author=Contributor <other@example.test>",
        "-qm",
        "contribution",
    ]);
    let contribution = String::from_utf8(repo.git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    repo.git(&["checkout", "feature"]);
    // Git may bypass pre-commit during cherry-pick; the stored-object gate remains.
    repo.git(&["cherry-pick", &contribution]);
    refused(&repo.push());
    repo.approve("author", "Contributor", "other@example.test");
    ok(&repo.push());
    assert_eq!(
        String::from_utf8(repo.git(&["log", "-1", "--format=%an <%ae>"]).stdout)
            .unwrap()
            .trim(),
        "Contributor <other@example.test>"
    );
}

#[test]
fn unrelated_remote_history_does_not_hide_new_destination_commits() {
    let repo = Repo::new();
    repo.git(&[
        "-c",
        "user.email=incorrect",
        "commit",
        "--no-verify",
        "--allow-empty",
        "-qm",
        "bad",
    ]);
    repo.git(&["update-ref", "refs/remotes/elsewhere/main", "HEAD"]);
    refused(&repo.push());
    repo.git(&["push", "--no-verify", "origin", "HEAD:refs/heads/existing"]);
    // A new branch on the same destination must not re-audit published ancestors.
    ok(&repo.commit());
    ok(&repo.push());
}

#[test]
fn blob_tags_and_deletions_need_no_configured_commit_identity() {
    let repo = Repo::new();
    ok(&repo.commit());
    fs::write(repo.path().join("blob"), "blob\n").unwrap();
    let blob = String::from_utf8(repo.git(&["hash-object", "-w", "blob"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    repo.git(&["tag", "blob", &blob]);
    repo.git(&["tag", "-am", "annotated blob", "annotated-blob", &blob]);
    repo.git(&[
        "push",
        "origin",
        "refs/tags/blob",
        "refs/tags/annotated-blob",
    ]);
    fs::write(repo.path().join("home/.gitconfig"), "").unwrap();
    repo.git(&[
        "push",
        "origin",
        ":refs/tags/blob",
        ":refs/tags/annotated-blob",
    ]);
}

#[test]
fn shallow_history_cannot_report_a_complete_audit() {
    let repo = Repo::new();
    ok(&repo.commit());
    let tip = repo.git(&["rev-parse", "HEAD"]).stdout;
    fs::write(repo.path().join(".git/shallow"), tip).unwrap();
    let out = repo.audit(&[]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(text(&out).contains("unshallow"));
}

#[test]
fn unavailable_advertised_baseline_is_not_silently_ignored() {
    let repo = Repo::new();
    ok(&repo.commit());
    let tip = String::from_utf8(repo.git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    let out = repo.push_record(&format!(
        "refs/heads/feature {tip} refs/heads/feature {}\n",
        "a".repeat(tip.len())
    ));
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(text(&out).contains("advertised identity baseline"));
}

#[test]
fn authenticated_push_preserves_origin_history_and_checks_new_identities() {
    let repo = Repo::new();
    repo.git(&[
        "-c",
        "user.email=historical@example.test",
        "commit",
        "--no-verify",
        "--allow-empty",
        "-qm",
        "published history",
    ]);
    repo.git(&["push", "--no-verify", "origin", "HEAD:refs/heads/seed"]);
    ok(&repo.commit());
    repo.git(&[
        "remote",
        "set-url",
        "origin",
        "git@github.pie.apple.com:acme/widgets.git",
    ]);
    repo.git(&[
        "config",
        "remote.origin.pushurl",
        "ssh://git@github.pie.apple.com/acme/widgets.git",
    ]);
    let bin = repo.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let endpoint = serde_json::to_string(repo.path().join("remote").to_str().unwrap()).unwrap();
    // Mock only the HTTPS endpoint. Real Git, hooks, history and ref updates run.
    fs::write(
        bin.join("git"),
        format!(
            r#"#!/usr/bin/env python3
import os, sys
args = sys.argv[1:]
if 'push' in args:
    endpoint = {endpoint}
    url = 'https://github.pie.apple.com/acme/widgets.git'
    args = [endpoint if arg == url else
            arg.replace('url.' + url + '.insteadOf=', 'url.' + endpoint + '.insteadOf=', 1)
            if arg.startswith('url.' + url + '.insteadOf=')
            else arg for arg in args]
os.execv('/usr/bin/git', ['git'] + args)
"#
        ),
    )
    .unwrap();
    fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    let push = |branch: &str| {
        repo.command(storyhook_test_support::story_binary().to_str().unwrap())
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .args([
                "github",
                "git",
                "--checkout",
                repo.path().to_str().unwrap(),
                "--",
                "push",
                "origin",
                branch,
            ])
            .output()
            .unwrap()
    };
    ok(&push("HEAD:refs/heads/release"));
    assert_eq!(
        String::from_utf8(repo.git(&["config", "--get", "remote.origin.url"]).stdout)
            .unwrap()
            .trim(),
        "git@github.pie.apple.com:acme/widgets.git",
    );
    assert_eq!(
        String::from_utf8(
            repo.git(&["config", "--get", "remote.origin.pushurl"])
                .stdout
        )
        .unwrap()
        .trim(),
        "ssh://git@github.pie.apple.com/acme/widgets.git",
    );
    assert_eq!(
        repo.git(&["rev-parse", "HEAD"]).stdout,
        repo.git(&["--git-dir=remote", "rev-parse", "refs/heads/release"])
            .stdout,
    );
    repo.git(&[
        "-c",
        "user.email=new-invalid@example.test",
        "commit",
        "--no-verify",
        "--allow-empty",
        "-qm",
        "unapproved new identity",
    ]);
    let rejected = push("HEAD:refs/heads/rejected");
    assert!(!rejected.status.success(), "{}", text(&rejected));
    assert!(
        text(&rejected).contains("new-invalid@example.test"),
        "{}",
        text(&rejected)
    );
    assert!(
        !repo
            .command("git")
            .args([
                "--git-dir=remote",
                "rev-parse",
                "--verify",
                "refs/heads/rejected"
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
}
