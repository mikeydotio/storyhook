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
    exercise(script, idle, prepare, |actuator, candidate, pr, _| {
        actuator.verify(candidate, pr)
    })
}

fn exercise<T>(
    script: &str,
    idle: Duration,
    prepare: impl FnOnce(&Path, &Path),
    action: impl FnOnce(
        ShellVerificationActuator,
        &VerificationCandidate,
        &PrLink,
        &ServiceFixture,
    ) -> T,
) -> T {
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
    // The fake verifier lives beside the real siblings it sources, in a
    // directory of its own — never inside the checkout, which since SH-654
    // contributes nothing the actuator runs. A checkout with no scripts tree
    // is the ordinary shape of a registered project now.
    let tools = scratch_dir();
    std::fs::write(tools.path().join("verify-pr.sh"), script).unwrap();
    for name in ["machine-lock.sh", "gate-progress.sh"] {
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts")
                .join(name),
            tools.path().join(name),
        )
        .unwrap();
    }
    assert!(!checkout.path().join("scripts").exists());
    let candidate = VerificationCandidate {
        blocked_by: Vec::new(),
        landing_pending: false,
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
    let actuator = ShellVerificationActuator::with_paths_and_timing(
        fixture.env().clone(),
        checkout.path().join("unused-helper"),
        PathBuf::from("/usr/bin/true"),
        idle,
        idle,
        idle / 4,
    )
    .with_verifier_script(tools.path().join("verify-pr.sh"));
    let outcome = action(actuator, &candidate, &pull_request, &fixture);
    assert!(!checkout.path().join("must-not-run").exists());
    outcome
}

const IDLE: Duration = Duration::from_secs(1);
/// How a fake reaches the real sibling beside it — the shape the shipped
/// family uses, since the fake's directory is the bundle's stand-in.
const SIBLING: &str = r#""$(dirname "${BASH_SOURCE[0]}")""#;
const MERGED: &str = r#"printf '%s\n' '{"result":"certified","head":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","tree":"verified-tree","detail":"completed"}'"#;

#[test]
fn progressing_verification_can_outlive_its_idle_budget() {
    let script = format!(
        "set -eu\n. {SIBLING}/gate-progress.sh\nfor i in {{1..12}}; do\n gate_progress_emit_case 'release gate/plugin' pass\n sleep {}\ndone\n{MERGED}\n",
        IDLE.as_secs_f64() / 4.0
    );
    let outcome = verify_script(&script, IDLE);
    assert!(
        matches!(outcome, VerificationOutcome::Certified { .. }),
        "{outcome:?}"
    );
}

#[test]
fn silence_after_progress_still_times_out() {
    let script = format!(
        "set -eu\n. {SIBLING}/gate-progress.sh\ngate_progress_emit_case 'release gate/plugin' pass\nsleep {}\n{MERGED}\n",
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
         lock=\"$(bash {SIBLING}/machine-lock.sh --plan gate -- true | sed -n 's/^lock=//p')\"\n\
         bash {SIBLING}/machine-lock.sh gate -- sleep {} &\nholder=$!\n\
         for i in {{1..100}}; do [ ! -f \"$lock/pid\" ] || break; sleep 0.01; done\n\
         test -f \"$lock/pid\"\n\
         bash {SIBLING}/machine-lock.sh gate -- true\nwait \"$holder\"\n{MERGED}\n",
        (idle * 2).as_secs()
    );
    let outcome = verify_script(&script, idle);
    assert!(
        matches!(outcome, VerificationOutcome::Certified { .. }),
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

#[test]
fn progressing_landing_outlives_the_control_budget_but_silence_keeps_authority_uncertain() {
    use storyhook::daemon::verification::{LandingOutcome, journal_path};
    use storyhook::domain::landing::VerifiedSubmission;
    use storyhook::store::{GlobalSeq, LandingIntent, StoryNo};
    for progressing in [true, false] {
        let work = if progressing {
            "for i in {1..12}; do gate_progress_emit_case 'landing/lock' pass; sleep 0.25; done"
        } else {
            "sleep 4"
        };
        let script = format!(
            "set -eu\n. {SIBLING}/gate-progress.sh\n{work}\nprintf '%s\\n' '{{\"result\":\"merged\",\"detail\":\"confirmed\"}}'\n"
        );
        let outcome = exercise(
            &script,
            IDLE,
            |_, journal| {
                std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
                std::fs::write(journal, "").unwrap();
            },
            |actuator, candidate, pr, f| {
                let mut candidate = candidate.clone();
                candidate.pull_request = Ok(pr.clone());
                assert!(journal_path(f.env(), &candidate).exists());
                let intent = LandingIntent {
                    id: "test-attempt".into(),
                    project: candidate.project,
                    story: StoryNo::parse_id("SH", "SH-1").unwrap(),
                    story_id: candidate.story_id.clone(),
                    project_slug: candidate.project_slug.clone(),
                    generation: GlobalSeq::new(1),
                    pull_request: pr.url.clone(),
                    checkout: candidate.checkout.clone(),
                    certification: VerifiedSubmission {
                        head: "a".repeat(40),
                        tree: "b".repeat(40),
                        gate: "test gate".into(),
                    },
                    created_at: FIXTURE_NOW.into(),
                };
                actuator.land(&candidate, &intent)
            },
        );
        if progressing {
            assert!(
                matches!(outcome, LandingOutcome::Merged { .. }),
                "{outcome:?}"
            );
        } else {
            assert!(
                matches!(outcome, LandingOutcome::Uncertain { .. }),
                "{outcome:?}"
            );
        }
    }
}
