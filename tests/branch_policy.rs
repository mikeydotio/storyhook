//! Repository branch-role contracts.
//!
//! `dev` is the continuously integrated line; `main` is the stable release
//! line. These tests pin the wiring between the shared policy, observers,
//! SemVer, and the release orchestrator without executing a release.

use std::fs;
use std::path::Path;

fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    fs::read_to_string(checkout().join(relative))
        .unwrap_or_else(|error| panic!("reading {relative}: {error}"))
}

#[test]
fn repository_branch_roles_have_one_shell_source_of_truth() {
    let policy = read("scripts/branch-policy.sh");
    assert!(policy.contains("STORYHOOK_INTEGRATION_BRANCH=dev"));
    assert!(policy.contains("STORYHOOK_STABLE_BRANCH=main"));

    let semver = read(".semver/config.yaml");
    assert!(semver.contains("target_branch: \"dev\""));

    for script in [
        "scripts/browser-watch.sh",
        "scripts/browser-status.sh",
        "scripts/coverage-watch.sh",
        "scripts/coverage-status.sh",
    ] {
        let body = read(script);
        assert!(
            body.contains("source \"$script_dir/branch-policy.sh\""),
            "{script} must consume the shared branch policy"
        );
        assert!(
            body.contains("STORYHOOK_INTEGRATION_BRANCH"),
            "{script} must observe the integration branch"
        );
    }
}

#[test]
fn stable_release_uses_guarded_merges_and_stops_before_tagging_if_dev_sync_fails() {
    let release = read("scripts/release.sh");

    assert!(release.contains("source \"$SCRIPT_DIR/branch-policy.sh\""));
    assert!(release.contains("[ \"$branch\" = \"$STORYHOOK_INTEGRATION_BRANCH\" ]"));
    assert!(release.contains("--base \"$STORYHOOK_STABLE_BRANCH\""));
    assert!(release.contains("--base \"$STORYHOOK_INTEGRATION_BRANCH\""));
    // SH-691: land-pr.sh requires origin's default unless the caller states
    // its intent; the stable merge lands on `main` deliberately, and the sync
    // merge states `dev` for the same reason rather than relying on the default.
    assert!(
        release.contains("scripts/land-pr.sh --base \"$STORYHOOK_STABLE_BRANCH\" \"$stable_pr\""),
        "the stable landing must state its intended base"
    );
    assert!(
        release.contains(
            "scripts/land-pr.sh --base \"$STORYHOOK_INTEGRATION_BRANCH\" \"$integration_pr\""
        ),
        "the integration landing must state its intended base"
    );
    assert!(
        release.matches("scripts/land-pr.sh").count() >= 2,
        "both long-lived branches must use the guarded merge path"
    );
    assert!(
        !release.lines().any(|line| {
            let line = line.trim_start();
            !line.starts_with('#') && line.contains("gh pr merge")
        }),
        "release orchestration must not bypass scripts/land-pr.sh"
    );

    let stable_pr = release
        .find("--base \"$STORYHOOK_STABLE_BRANCH\"")
        .expect("stable PR");
    let stable_checkout = release
        .find("run git switch \"$STORYHOOK_STABLE_BRANCH\"")
        .expect("stable checkout");
    let integration_pr = release
        .find("--base \"$STORYHOOK_INTEGRATION_BRANCH\"")
        .expect("integration PR");
    let delete = release
        .find("run git branch -d \"$release_branch\"")
        .expect("release branch deletion");
    let build = release
        .find("step \"Building and verifying all release assets locally\"")
        .expect("stable asset build");
    let tag = release
        .find("step \"Tagging and pushing $next_version\"")
        .expect("stable tag");

    assert!(release.contains("set -euo pipefail"));
    assert!(stable_pr < stable_checkout);
    assert!(stable_checkout < integration_pr);
    assert!(integration_pr < delete);
    assert!(delete < build);
    assert!(integration_pr < tag);
}

#[test]
fn release_observer_remains_on_the_stable_branch() {
    let observer = read("scripts/release-observer.py");
    assert!(observer.contains("origin/main"));
    assert!(!observer.contains("origin/dev"));
}
