//! Effective identity and persistent policy contracts.

use super::*;

#[test]
fn local_and_command_scope_overrides_are_refused_before_commit() {
    for key in [
        "user.name",
        "user.email",
        "author.name",
        "author.email",
        "committer.name",
        "committer.email",
    ] {
        for command_scope in [false, true] {
            let repo = Repo::new();
            let out = if command_scope {
                repo.command("git")
                    .args([
                        "-c",
                        &format!("{key}=incorrect"),
                        "commit",
                        "--allow-empty",
                        "-qm",
                        "bad",
                    ])
                    .output()
                    .unwrap()
            } else {
                repo.git(&["config", key, "incorrect"]);
                repo.commit()
            };
            refused(&out);
            assert!(
                !repo
                    .command("git")
                    .args(["rev-parse", "--verify", "HEAD"])
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
    }
}

#[test]
fn inherited_author_and_committer_overrides_are_refused() {
    for key in [
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
    ] {
        let repo = Repo::new();
        refused(
            &repo
                .command("git")
                .env(key, "incorrect")
                .args(["commit", "--allow-empty", "-qm", "bad"])
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn explicit_alternatives_require_a_reason_and_are_role_specific() {
    let repo = Repo::new();
    repo.approve("author", "Contributor", "other@example.test");
    repo.git(&["config", "author.name", "Contributor"]);
    repo.git(&["config", "author.email", "other@example.test"]);
    let out = repo.commit();
    ok(&out);
    assert!(text(&out).contains("Preserve reviewed contributor identity"));
    ok(&repo.push());
    repo.git(&["config", "committer.name", "Contributor"]);
    repo.git(&["config", "committer.email", "other@example.test"]);
    refused(&repo.commit());
    repo.git(&["config", "storyhookIdentity.reviewed.role", "both"]);
    ok(&repo.commit());
    repo.git(&["config", "--unset", "storyhookIdentity.reviewed.reason"]);
    refused(&repo.commit());
}

#[test]
fn command_scope_policy_cannot_approve_itself() {
    let repo = Repo::new();
    let out = repo
        .command("git")
        .args([
            "-c",
            "user.email=incorrect",
            "-c",
            "storyhookIdentity.injected.name=Correct Person",
            "-c",
            "storyhookIdentity.injected.email=incorrect",
            "-c",
            "storyhookIdentity.injected.role=both",
            "-c",
            "storyhookIdentity.injected.reason=injected",
            "commit",
            "--allow-empty",
            "-qm",
            "bad",
        ])
        .output()
        .unwrap();
    refused(&out);
}

#[test]
fn missing_baseline_requires_an_explicit_complete_alternative() {
    let repo = Repo::new();
    fs::write(repo.path().join("home/.gitconfig"), "").unwrap();
    repo.git(&["config", "user.name", "Correct Person"]);
    repo.git(&["config", "user.email", "correct@example.test"]);
    refused(&repo.commit());
    repo.approve("both", "Correct Person", "correct@example.test");
    ok(&repo.commit());
}

#[test]
fn conditional_global_and_role_specific_identities_are_honored() {
    let repo = Repo::new();
    fs::write(
        repo.path().join("home/work"),
        "[author]\nname = Other Author\nemail = author@example.test\n",
    )
    .unwrap();
    let config = format!(
        "[user]\nname = Correct Person\nemail = correct@example.test\n[includeIf \"gitdir:{}/\"]\npath = work\n",
        repo.path().display()
    );
    fs::write(repo.path().join("home/.gitconfig"), config).unwrap();
    ok(&repo.commit());
    assert!(
        text(&repo.git(&["log", "-1", "--format=%an <%ae>"]))
            .contains("Other Author <author@example.test>")
    );
    ok(&repo.push());
}

#[test]
fn linked_worktrees_preserve_managed_precommit_failure() {
    let repo = Repo::new();
    ok(&repo.commit());
    repo.git(&["worktree", "add", "-qb", "linked", "linked"]);
    symlink(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".githooks"),
        repo.path().join("linked/.githooks"),
    )
    .unwrap();
    let hook = repo.path().join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\necho managed-refusal >&2\nexit 17\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let out = repo
        .command("git")
        .current_dir(repo.path().join("linked"))
        .args(["commit", "--allow-empty", "-qm", "linked"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(text(&out).contains("managed-refusal"));
    fs::remove_file(hook).unwrap();
    ok(&repo
        .command("git")
        .current_dir(repo.path().join("linked"))
        .args(["commit", "--allow-empty", "-qm", "linked"])
        .output()
        .unwrap());
}

#[test]
fn configuration_errors_are_distinct_from_identity_mismatches() {
    for field in ["role", "reason", "unexpected"] {
        let repo = Repo::new();
        repo.approve("both", "Correct Person", "correct@example.test");
        repo.git(&[
            "config",
            &format!("storyhookIdentity.reviewed.{field}"),
            " ",
        ]);
        let out = repo
            .command("bash")
            .arg(helper())
            .arg("current")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    }
    let repo = Repo::new();
    ok(&repo.commit());
    fs::write(repo.path().join("home/.gitconfig"), "[invalid\n").unwrap();
    assert_eq!(repo.audit(&[]).status.code(), Some(2));
}

#[test]
fn injected_global_file_and_indexed_command_configuration_are_not_policy() {
    let repo = Repo::new();
    fs::write(
        repo.path().join("injected-config"),
        "[user]\nname = Incorrect\nemail = incorrect\n",
    )
    .unwrap();
    refused(
        &repo
            .command("git")
            .env("GIT_CONFIG_GLOBAL", repo.path().join("injected-config"))
            .args(["commit", "--allow-empty", "-qm", "bad"])
            .output()
            .unwrap(),
    );
    refused(
        &repo
            .command("git")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "user.email")
            .env("GIT_CONFIG_VALUE_0", "incorrect")
            .args(["commit", "--allow-empty", "-qm", "bad"])
            .output()
            .unwrap(),
    );
}

#[test]
fn local_policy_fields_override_global_fields_with_literal_unicode_values() {
    let repo = Repo::new();
    for (key, value) in [
        ("name", "Zoë O'Neil $(touch injected)"),
        ("email", "other@example.test"),
        ("role", "author"),
        ("reason", "global reason"),
    ] {
        repo.git(&[
            "config",
            "--global",
            &format!("storyhookIdentity.reviewed.{key}"),
            value,
        ]);
    }
    repo.git(&[
        "config",
        "storyhookIdentity.reviewed.reason",
        "local reason",
    ]);
    let out = repo
        .command("git")
        .args([
            "commit",
            "--allow-empty",
            "-qm",
            "explicit author",
            "--author=Zoë O'Neil $(touch injected) <other@example.test>",
        ])
        .output()
        .unwrap();
    ok(&out);
    assert!(text(&out).contains("local reason"));
    assert!(!repo.path().join("injected").exists());
    ok(&repo.push());
}
