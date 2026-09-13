//! SH-699: fixture mirror policy survives every verifier subprocess boundary.
//!
//! Each matrix row runs in its own process, so ambient settings cannot race
//! parallel tests. The children only record their environment; neither tmux
//! nor a daemon is started, even when reproducing the missing isolation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use storyhook::daemon::verification::{
    ShellVerificationActuator, VerificationActuator, VerificationOutcome,
};
use storyhook::domain::{CLEANUP_LEASE_VERSION, Priority, StoryCleanupLease, TmuxCleanupTarget};
use storyhook::env::{Environment, spawn_env::apply_verification_allowlist};
use storyhook::service::verification::{VerificationCandidate, VerificationProblem};
use storyhook::store::PrLink;
use storyhook_test_support::{
    ChildGuard, FIXTURE_NOW, STORY_COMMAND_DEADLINE, ServiceFixture, daemon_containment,
    scratch_dir,
};

const MIRROR: &str = "STORYHOOK_VERIFIER_MIRROR";
const PROBE_RESULT: &str = "STORYHOOK_MIRROR_ISOLATION_PROBE_RESULT";
const TEST_NAME: &str = "fixture_mirror_policy_survives_every_verifier_child";

fn output(command: &mut Command) -> std::process::Output {
    let result = ChildGuard::spawn_with_output(command)
        .expect("spawn bounded environment probe")
        .wait_with_output_within(STORY_COMMAND_DEADLINE, || {
            "SH-699 environment probe did not finish".into()
        });
    assert!(
        result.status.success(),
        "probe failed: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    result
}

fn mirror_value(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{MIRROR}=")))
        .map(str::to_owned)
}

fn candidate(fixture: &ServiceFixture, checkout: &Path) -> VerificationCandidate {
    VerificationCandidate {
        project: fixture.project(),
        project_slug: "fixture".into(),
        story_id: "SH-1".into(),
        title: "fixture mirror isolation".into(),
        priority: Priority::High,
        created_at: FIXTURE_NOW.into(),
        verifying_since: Some(FIXTURE_NOW.into()),
        verifying_generation: None,
        blocking_revision: None,
        checkout: checkout.to_path_buf(),
        cleanup_lease: Some(StoryCleanupLease {
            version: CLEANUP_LEASE_VERSION,
            project_slug: "fixture".into(),
            story_id: "SH-1".into(),
            repository_path: checkout.to_path_buf(),
            worktree_path: checkout.join(".codex/worktrees/SH-1"),
            branch: "worktree-SH-1".into(),
            tmux: TmuxCleanupTarget {
                socket_path: checkout.join("unused-private-tmux.sock"),
            },
        }),
        pull_request: Err(VerificationProblem::MissingPullRequest),
    }
}

fn record_actuator_children(env: Environment) -> BTreeMap<String, Option<String>> {
    let fixture = ServiceFixture::new();
    let checkout = scratch_dir();
    let scripts = scratch_dir();
    for args in [
        vec!["init", "-q"],
        vec![
            "config",
            "remote.origin.url",
            "https://github.com/acme/widgets.git",
        ],
    ] {
        output(storyhook::env::git_env::command(checkout.path()).args(args));
    }
    let candidate = candidate(&fixture, checkout.path());
    let lease = candidate.cleanup_lease.as_ref().unwrap();
    let reap_receipt = serde_json::json!({
        "ok": true,
        "receipt_version": 1,
        "story_id": "SH-1",
        "lease": lease,
        "removed": {"worktree": false, "branch": false, "tmux": false},
        "postconditions": {
            "worktree_registration_absent": true,
            "worktree_path_absent": true,
            "branch_absent": true,
            "tmux_story_windows_absent": true
        },
        "display": "environment recorded"
    });
    let submit_receipt = serde_json::json!({
        "ok": true,
        "receipt_version": 1,
        "story_id": "SH-1",
        "lease": lease,
        "pushed": true,
        "pull_request": {
            "url": "https://github.com/acme/widgets/pull/7",
            "number": 7,
            "base": "main",
            "head_oid": "0123abcd",
            "adopted": false
        },
        "display": "environment recorded"
    });
    let helper = scripts.path().join("record-helper.sh");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/bash\n/usr/bin/env > \"$3.env\"\ncase \"$3\" in\n\
             notify) printf '%s\\n' '{{\"ok\":true}}' ;;\n\
             reap) cat <<'RECEIPT'\n{reap_receipt}\nRECEIPT\n;;\n\
             submit) cat <<'RECEIPT'\n{submit_receipt}\nRECEIPT\n;;\n\
             *) exit 64 ;;\nesac\n"
        ),
    )
    .expect("write environment-recording control helper");
    let verifier = scripts.path().join("record-verifier.sh");
    std::fs::write(
        &verifier,
        "#!/bin/bash\n/usr/bin/env > verify.env\n\
         printf '%s\\n' '{\"result\":\"merged\",\"tree\":\"t\",\"detail\":\"environment recorded\"}'\n",
    )
    .expect("write environment-recording verification helper");
    let actuator =
        ShellVerificationActuator::with_paths(env, helper, PathBuf::from("/usr/bin/true"))
            .with_verifier_script(verifier);
    actuator
        .notify(&candidate, "environment probe")
        .expect("notification receipt accepted");
    actuator.reap(&candidate).expect("cleanup receipt accepted");
    actuator
        .submit(&candidate)
        .expect("submission receipt accepted");
    let pr = PrLink {
        owner: "acme".into(),
        repo: "widgets".into(),
        number: 7,
        url: "https://github.com/acme/widgets/pull/7".into(),
        close_on_merge: true,
        status: "open".into(),
        linked_at: FIXTURE_NOW.into(),
        last_checked_at: None,
    };
    let outcome = actuator.verify(&candidate, &pr);
    assert!(
        matches!(outcome, VerificationOutcome::Merged { .. }),
        "verification receipt was not accepted: {outcome:?}"
    );
    ["notify", "reap", "submit", "verify"]
        .into_iter()
        .map(|door| {
            let bytes = std::fs::read(checkout.path().join(format!("{door}.env")))
                .expect("the production actuator reached the recording child");
            (door.into(), mirror_value(&bytes))
        })
        .collect()
}

fn probe(result_path: &Path) {
    let mut allowlisted = Command::new("/usr/bin/env");
    apply_verification_allowlist(&mut allowlisted);
    let allowlist = mirror_value(&output(&mut allowlisted).stdout);
    let root = scratch_dir();
    let fixture_env = Environment::at(root.path());
    let process_env = Environment::from_process(Some(&root.path().join("process.db")))
        .expect("explicit scratch process environment");
    let child_policy = |env: &Environment| {
        let mut child = Command::new("/usr/bin/env");
        child.env_clear().envs(env.child_vars());
        mirror_value(&output(&mut child).stdout)
    };
    let observed = serde_json::json!({
        "allowlist": allowlist,
        "fixture_vars": child_policy(&fixture_env),
        "process_vars": child_policy(&process_env),
        "fixture_doors": record_actuator_children(fixture_env),
        "process_doors": record_actuator_children(process_env),
    });
    std::fs::write(result_path, observed.to_string()).expect("record isolated observations");
}

#[test]
fn fixture_mirror_policy_survives_every_verifier_child() {
    if let Some(result_path) = std::env::var_os(PROBE_RESULT) {
        probe(Path::new(&result_path));
        return;
    }
    let mut failures = Vec::new();
    for ambient in [None, Some("0"), Some("1"), Some(""), Some("false")] {
        let root = scratch_dir();
        let result_path = root.path().join("result.json");
        let mut command = Command::new(std::env::current_exe().expect("this test binary"));
        command
            .env_clear()
            .envs(daemon_containment())
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", root.path())
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env(PROBE_RESULT, &result_path)
            .args(["--exact", TEST_NAME, "--nocapture"]);
        match ambient {
            Some(value) => {
                command.env(MIRROR, value);
            }
            None => {
                command.env_remove(MIRROR);
            }
        }
        output(&mut command);
        let observed: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&result_path).expect("probe recorded its observations"),
        )
        .expect("observations are JSON");
        let process_policy = if ambient == Some("0") { "0" } else { "1" };
        let mut expect = |boundary: &str, actual: &serde_json::Value, expected: Option<&str>| {
            if actual.as_str() != expected {
                failures.push(format!(
                    "ambient={ambient:?} {boundary}: expected {expected:?}, got {actual}"
                ));
            }
        };
        expect("verification allowlist", &observed["allowlist"], ambient);
        expect(
            "Environment::at child_vars",
            &observed["fixture_vars"],
            Some("0"),
        );
        expect(
            "Environment::from_process child_vars",
            &observed["process_vars"],
            Some(process_policy),
        );
        for door in ["notify", "reap", "submit", "verify"] {
            expect(
                &format!("fixture {door} child"),
                &observed["fixture_doors"][door],
                Some("0"),
            );
            expect(
                &format!("process {door} child"),
                &observed["process_doors"][door],
                Some(process_policy),
            );
        }
    }
    assert!(
        failures.is_empty(),
        "SH-699 mirror policy was lost at subprocess boundaries:\n{}",
        failures.join("\n")
    );
}
