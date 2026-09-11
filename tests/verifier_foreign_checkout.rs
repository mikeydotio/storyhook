//! The daemon verifies a checkout that is not storyhook's (SH-654).
//!
//! `ShellVerificationActuator::verify` used to spawn `scripts/verify-pr.sh`
//! with the registered checkout as its working directory, so the script
//! was found only when that checkout was a storyhook clone. Against any
//! other project `bash` found nothing, printed nothing on stdout, and the
//! daemon reported `scripts/verify-pr.sh returned invalid JSON` — a
//! permanent infrastructure failure that halted the queue.
//!
//! This is the regression test for that exact symptom: the **production**
//! actuator (`new`, not a `with_paths` seam) against a git repository whose
//! tree holds no `scripts/` directory at all, with a fake `gh` on `PATH`
//! answering a closed pull request. Before the fix the outcome was the
//! invalid-JSON infrastructure failure; after it, the verifier ran from the
//! bundle this binary projected and returned the PR's own verdict —
//! `InvalidSubmission`, naming the closed PR. The test also pins *where*
//! the script ran from: the leaf under the daemon's own state directory,
//! never the checkout.
//!
//! One `#[test]` in this binary on purpose. The actuator's child inherits
//! `PATH` through the verification allowlist from the test process, so the
//! fake `gh` has to be on the process's `PATH`; `std::env::set_var` is
//! unsound with other threads reading the environment, and a one-test
//! binary is how this file guarantees there are none.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use storyhook::daemon::verification::{
    ShellVerificationActuator, VerificationActuator, VerificationOutcome,
};
use storyhook::daemon::verifier_bundle;
use storyhook::domain::Priority;
use storyhook::env::Environment;
use storyhook::service::{VerificationCandidate, VerificationProblem};
use storyhook::store::PrLink;
use storyhook_test_support::{FIXTURE_NOW, ServiceFixture, scratch_dir};

const PR_URL: &str = "https://github.com/acme/widgets/pull/7";

fn git(dir: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

#[test]
fn a_checkout_with_no_scripts_tree_is_verified_from_the_daemons_own_bundle() {
    let fixture = ServiceFixture::new();
    let env_root = scratch_dir();
    let daemon_env = Environment::at(env_root.path());

    // A registered project that is not storyhook: a real repository with one
    // commit and the PR's origin, and deliberately no `scripts/` directory.
    let checkout = scratch_dir();
    git(checkout.path(), &["init", "-q", "-b", "main"]);
    git(checkout.path(), &["config", "user.email", "t@t"]);
    git(checkout.path(), &["config", "user.name", "t"]);
    git(
        checkout.path(),
        &[
            "config",
            "remote.origin.url",
            "https://github.com/acme/widgets.git",
        ],
    );
    fs::write(checkout.path().join("README"), "not storyhook\n").unwrap();
    git(checkout.path(), &["add", "README"]);
    git(checkout.path(), &["commit", "-qm", "init"]);
    assert!(!checkout.path().join("scripts").exists());

    // A fake `gh` whose only answer is a closed PR with every field the
    // verifier reads — the wire shape, one door over, never a GitHub model.
    let bin = scratch_dir();
    let gh = bin.path().join("gh");
    fs::write(
        &gh,
        r#"#!/usr/bin/env bash
set -euo pipefail
[ "${1:-}" = pr ] && [ "${2:-}" = view ] || { echo "fake gh: unsupported: $*" >&2; exit 64; }
printf '%s\n' '{"number":7,"state":"CLOSED","isDraft":false,"isCrossRepository":false,"baseRefName":"main","headRefName":"feature","headRefOid":"0000000000000000000000000000000000000007","mergeCommit":null}'
"#,
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut path = std::ffi::OsString::from(bin.path());
    path.push(":");
    path.push(inherited);
    // SAFETY: this binary holds exactly one test, so no other thread reads
    // or writes the process environment while it is changed (see the module
    // doc). The actuator's child receives `PATH` through the verification
    // allowlist, which is why the fake has to be on the process's own.
    unsafe { std::env::set_var("PATH", &path) };

    let candidate = VerificationCandidate {
        blocked_by: Vec::new(),
        landing_pending: false,
        project: fixture.project(),
        project_slug: "widgets".into(),
        story_id: "WG-1".into(),
        title: "a foreign project".into(),
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
        number: 7,
        url: PR_URL.into(),
        close_on_merge: true,
        status: "open".into(),
        linked_at: FIXTURE_NOW.into(),
        last_checked_at: None,
    };

    let outcome =
        ShellVerificationActuator::new(daemon_env.clone()).verify(&candidate, &pull_request);

    match &outcome {
        VerificationOutcome::InvalidSubmission { detail } => assert!(
            detail.contains("PR #7 is CLOSED"),
            "the verifier ran and judged the PR, but said: {detail}"
        ),
        VerificationOutcome::InfrastructureFailure { detail, .. } => panic!(
            "the verifier never ran from the daemon's own bundle against a non-storyhook checkout: {detail}"
        ),
        other => panic!("unexpected outcome {other:?}"),
    }

    // Where it ran from: the leaf this binary projects, holding the tracked
    // verify-pr.sh byte for byte — and nothing was ever looked for in the
    // checkout.
    let bundled = verifier_bundle::bundle_dir(&daemon_env).join(verifier_bundle::VERIFY_SCRIPT);
    assert!(
        bundled.is_file(),
        "{} was not materialized",
        bundled.display()
    );
    let tracked = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/verify-pr.sh");
    assert_eq!(fs::read(&bundled).unwrap(), fs::read(&tracked).unwrap());
    assert!(!checkout.path().join("scripts").exists());
}
