//! What `story github-sync` does with a conflict it has not been told how to
//! resolve — SH-152.
//!
//! # Why this file is small, and what covers the rest
//!
//! A real conflict needs a GitHub issue that disagrees with a local story. This
//! file drives a real `story` subprocess, and `GithubClient` — even with
//! SH-158's `github::api::GithubApi` trait seam — has no wiring from that
//! subprocess to a fake, only from an in-process caller. So the conflict
//! *path* is pinned where it is reachable: `github::diff`'s unit tests hold
//! the merge base rule that the data loss actually lived in,
//! `github::outcome_tests` holds the refusal's exit code and text, and the
//! orchestration in between — including a conflict actually reaching
//! `SyncReport` — is exercised in-process against `FakeGithubApi` in
//! `tests/github_sync_engine.rs`. What is left for *this* file, an end-to-end
//! subprocess test, is the one rule that fires before any socket is opened —
//! and the structural guarantee that no prompt survives anywhere under `src/`,
//! which `tests/invoker_seam.rs` owns.
//!
//! # The defect, in one sentence
//!
//! `resolve_conflicts_interactive` drew a `dialoguer::Select` and, on the error
//! a missing terminal produces, chose `Skip` — and the daemon that runs every
//! command since SH-114 has no terminal, ever. The menu is gone.

#![cfg(feature = "github-sync")]

use storyhook_test_support::TestEnv;

/// **The footgun a resolution flag could have introduced.**
///
/// `--resolve local` over a whole sync would push every local value over every
/// conflicting remote edit the caller has never read — the same silent loss
/// this story is about, only faster and on request. Naming one story is what
/// makes it a decision about a disagreement somebody has actually seen.
///
/// The rule lives in `github::run_sync_with` rather than in the parser, so that
/// the dashboard and a hand-built `InvokeRequest` meet it too. It is asked
/// before the sync configuration is even loaded, which is what lets this test
/// run with no network and no GitHub token.
#[test]
fn a_resolution_that_names_no_story_is_refused_before_anything_is_fetched() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A story").build();

    let output = project
        .story()
        .args(["github-sync", "--resolve", "local"])
        .output()
        .expect("running story github-sync");

    assert_eq!(
        output.status.code(),
        Some(2),
        "a usage error exits 2: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("so name it"), "{stderr}");
    assert!(
        stderr.contains("have not seen"),
        "the refusal has to say why, not just that: {stderr}"
    );
    assert!(
        !stderr.contains("STORYHOOK_GITHUB_TOKEN"),
        "the rule is asked before the token is, so a caller without one still \
         gets the real reason: {stderr}"
    );
}

/// And the flag is *declared*, which is what keeps SH-62's fail-closed gate
/// from refusing it as an unknown flag. A flag the parser accepts but the table
/// does not know about is refused before the parser ever sees it.
#[test]
fn resolve_is_a_declared_flag_of_github_sync() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A story").build();

    let output = project
        .story()
        .args(["github-sync", "SH-1", "--resolve", "sideways"])
        .output()
        .expect("running story github-sync");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sideways"),
        "the value must be refused by name — an undeclared flag would be \
         refused as a flag instead, and never reach this message: {stderr}"
    );
    assert!(stderr.contains("local"), "{stderr}");
    assert!(stderr.contains("remote"), "{stderr}");
}

/// The help a user reaches for after reading a refusal has to explain the
/// refusal, or the two are separate documents saying different things.
#[test]
fn the_help_topic_explains_what_a_conflict_does() {
    let env = TestEnv::shared();
    let project = env.project().build();

    let output = project
        .story()
        .args(["help", "github-sync"])
        .output()
        .expect("running story help github-sync");
    let text = String::from_utf8_lossy(&output.stdout);

    assert!(text.contains("--resolve local|remote"), "{text}");
    assert!(
        text.contains("exits 8"),
        "the exit code is the part a script needs: {text}"
    );
    assert!(
        text.contains("merge base"),
        "and the reason re-running is safe is the part a person needs: {text}"
    );
}
