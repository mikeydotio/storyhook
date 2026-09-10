//! SH-592: verification is bounded by silence, including identity-checked lock waits.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use storyhook::daemon::verification::{
    ShellVerificationActuator, VerificationActuator, VerificationOutcome,
};
use storyhook::domain::Priority;
use storyhook::service::{VerificationCandidate, VerificationProblem};
use storyhook::store::PrLink;
use storyhook_test_support::{FIXTURE_NOW, ServiceFixture, scratch_dir};

fn verify_script(script: &str, idle: Duration) -> VerificationOutcome {
    verify_with_preparation(script, idle, |_, _| {})
}

fn verify_with_preparation(
    script: &str,
    idle: Duration,
    prepare: impl FnOnce(&Path, &Path),
) -> VerificationOutcome {
    let fixture = ServiceFixture::new();
    let checkout = scratch_dir();
    for args in [
        vec!["init", "-q"],
        vec![
            "config",
            "remote.origin.url",
            "https://github.com/acme/widgets.git",
        ],
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(checkout.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    std::fs::create_dir(checkout.path().join("scripts")).unwrap();
    std::fs::write(checkout.path().join("scripts/verify-pr.sh"), script).unwrap();
    for name in ["machine-lock.sh", "gate-progress.sh"] {
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts")
                .join(name),
            checkout.path().join("scripts").join(name),
        )
        .unwrap();
    }
    let candidate = VerificationCandidate {
        project: fixture.project(),
        project_slug: "fixture".into(),
        story_id: "SH-1".into(),
        title: "progress deadline".into(),
        priority: Priority::High,
        created_at: FIXTURE_NOW.into(),
        verifying_since: Some(FIXTURE_NOW.into()),
        verifying_generation: None,
        checkout: checkout.path().to_path_buf(),
        cleanup_lease: None,
        pull_request: Err(VerificationProblem::MissingPullRequest),
    };
    let pull_request = PrLink {
        owner: "acme".into(),
        repo: "widgets".into(),
        number: 1,
        url: "https://github.com/acme/widgets/pull/1".into(),
        close_on_merge: true,
        status: "open".into(),
        linked_at: FIXTURE_NOW.into(),
        last_checked_at: None,
    };
    prepare(
        checkout.path(),
        &storyhook::daemon::verification::journal_path(fixture.env(), &candidate),
    );
    let outcome = ShellVerificationActuator::with_paths_and_timing(
        fixture.env().clone(),
        checkout.path().join("unused-helper"),
        PathBuf::from("/usr/bin/true"),
        idle,
        idle,
        idle / 4,
    )
    .verify(&candidate, &pull_request);
    assert!(!checkout.path().join("must-not-run").exists());
    outcome
}

const IDLE: Duration = Duration::from_secs(1);
const MERGED: &str =
    r#"printf '%s\n' '{"result":"merged","tree":"verified-tree","detail":"completed"}'"#;

#[test]
fn progressing_verification_can_outlive_its_idle_budget() {
    let script = format!(
        "set -eu\n. scripts/gate-progress.sh\nfor i in {{1..12}}; do\n gate_progress_emit_case 'release gate/plugin' pass\n sleep {}\ndone\n{MERGED}\n",
        IDLE.as_secs_f64() / 4.0
    );
    let outcome = verify_script(&script, IDLE);
    assert!(
        matches!(outcome, VerificationOutcome::Merged { .. }),
        "{outcome:?}"
    );
}

#[test]
fn silence_after_progress_still_times_out() {
    let script = format!(
        "set -eu\n. scripts/gate-progress.sh\ngate_progress_emit_case 'release gate/plugin' pass\nsleep {}\n{MERGED}\n",
        (IDLE * 4).as_secs()
    );
    let outcome = verify_script(&script, IDLE);
    assert!(
        matches!(outcome, VerificationOutcome::InfrastructureFailure { ref detail, .. } if detail.contains("no progress")),
        "{outcome:?}"
    );
}

#[test]
fn output_chatter_does_not_keep_a_stalled_verifier_alive() {
    let script = format!(
        "for i in {{1..12}}; do echo waiting >&2; sleep {}; done\n{MERGED}\n",
        IDLE.as_secs_f64() / 4.0
    );
    let outcome = verify_script(&script, IDLE);
    assert!(
        matches!(outcome, VerificationOutcome::InfrastructureFailure { ref detail, .. } if detail.contains("no progress")),
        "{outcome:?}"
    );
}

#[test]
fn a_live_machine_lock_wait_can_outlive_the_idle_budget() {
    // The production lock observes identity once per second. Give it three
    // observations per idle window, then hold it for two whole windows.
    let idle = IDLE * 3;
    let script = format!(
        "set -eu\nexport STORYHOOK_LOCK_DIR=\"$PWD/locks\"\nunset STORYHOOK_MACHINE_LOCKS\n\
         lock=\"$(bash scripts/machine-lock.sh --plan gate -- true | sed -n 's/^lock=//p')\"\n\
         bash scripts/machine-lock.sh gate -- sleep {} &\nholder=$!\n\
         for i in {{1..100}}; do [ ! -f \"$lock/pid\" ] || break; sleep 0.01; done\n\
         test -f \"$lock/pid\"\n\
         bash scripts/machine-lock.sh gate -- true\nwait \"$holder\"\n{MERGED}\n",
        (idle * 2).as_secs()
    );
    let outcome = verify_script(&script, idle);
    assert!(
        matches!(outcome, VerificationOutcome::Merged { .. }),
        "{outcome:?}"
    );
}

#[test]
fn loss_of_the_progress_journal_is_an_infrastructure_failure() {
    let script = format!("rm \"$STORYHOOK_GATE_PROGRESS\"\n{MERGED}\n");
    let outcome = verify_script(&script, IDLE);
    assert!(
        matches!(outcome, VerificationOutcome::InfrastructureFailure { ref detail, .. } if detail.contains("progress journal")),
        "{outcome:?}"
    );
}

#[test]
fn an_uncreatable_journal_refuses_before_starting_the_verifier() {
    let outcome = verify_with_preparation("touch must-not-run", IDLE, |_, journal| {
        std::fs::create_dir_all(journal).unwrap();
    });
    assert!(
        matches!(outcome, VerificationOutcome::InfrastructureFailure { ref detail, .. } if detail.contains("initialize progress journal")),
        "{outcome:?}"
    );
}

#[test]
fn truncating_an_observed_journal_is_an_infrastructure_failure() {
    let script = format!(
        "printf 'initial progress' >> \"$STORYHOOK_GATE_PROGRESS\"\nsleep {}\n: > \"$STORYHOOK_GATE_PROGRESS\"\n{MERGED}\n",
        IDLE.as_secs_f64() / 2.0,
    );
    let outcome = verify_script(&script, IDLE);
    assert!(
        matches!(outcome, VerificationOutcome::InfrastructureFailure { ref detail, .. } if detail.contains("shrank")),
        "{outcome:?}"
    );
}

#[test]
fn replacing_a_journal_cannot_renew_the_deadline() {
    // Publish a new inode atomically in the same directory. Moving the old
    // journal away first would race the separate missing-journal refusal.
    let script = format!(
        "printf replacement > \"$STORYHOOK_GATE_PROGRESS.replacement\"\nmv \"$STORYHOOK_GATE_PROGRESS.replacement\" \"$STORYHOOK_GATE_PROGRESS\"\n{MERGED}\n"
    );
    let outcome = verify_script(&script, IDLE);
    assert!(
        matches!(outcome, VerificationOutcome::InfrastructureFailure { ref detail, .. } if detail.contains("replaced")),
        "{outcome:?}"
    );
}
