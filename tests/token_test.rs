//! `story token new|list|revoke` (SH-255), driven as the real binary would be
//! run — argument parsing, the "no daemon running" refusals, and a full
//! mint/list/revoke round trip against a real daemon subprocess.
//!
//! `src/api/tokens.rs`'s own unit tests own the registry; `tests/token_endpoint.rs`
//! owns the wire gate over a raw socket. What is here is the CLI surface: what
//! a user actually types, and what reaches their terminal.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use storyhook_test_support::{DaemonGuard, TestEnv, scratch_dir, wait_for_server};

fn story(dir: &Path) -> Command {
    TestEnv::shared().story(dir)
}

// --- CLI parsing ---

#[test]
fn token_no_subcommand_shows_usage() {
    let dir = scratch_dir();
    story(dir.path())
        .arg("token")
        .assert()
        .failure()
        .stderr(predicate::str::contains("usage: story token"));
}

#[test]
fn token_invalid_subcommand_shows_usage() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["token", "frobnicate"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("usage: story token"));
}

#[test]
fn token_new_missing_name_shows_usage() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["token", "new"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("names the token to mint"));
}

#[test]
fn token_revoke_missing_name_shows_usage() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["token", "revoke"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("names the token to end"));
}

#[test]
fn token_new_rejects_a_name_with_a_slash() {
    // A token name becomes a URL path segment (`DELETE /api/v1/tokens/{name}`)
    // and a query parameter (`POST /api/v1/tokens?name=...`) verbatim -- the
    // CLI layer is what keeps it to a shape that never needs escaping there.
    let dir = scratch_dir();
    story(dir.path())
        .args(["token", "new", "not/valid"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid token name"));
}

#[test]
fn token_new_rejects_an_empty_name() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["token", "new", ""])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid token name"));
}

#[test]
fn token_list_takes_no_arguments() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["token", "list", "extra"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("usage: story token"));
}

// --- No daemon running ---

#[test]
fn token_new_not_running_fails_with_summary() {
    let dir = scratch_dir();
    let env = TestEnv::isolated();
    env.story(dir.path())
        .args(["token", "new", "laptop"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("daemon is not running"))
        .stderr(predicate::str::contains("story daemon start"));
}

#[test]
fn token_list_not_running_fails_with_summary() {
    let dir = scratch_dir();
    let env = TestEnv::isolated();
    env.story(dir.path())
        .args(["token", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("daemon is not running"));
}

#[test]
fn token_revoke_not_running_fails_with_summary() {
    let dir = scratch_dir();
    let env = TestEnv::isolated();
    env.story(dir.path())
        .args(["token", "revoke", "laptop"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("daemon is not running"));
}

// --- Against a real daemon ---

fn started_port(env: &TestEnv) -> u16 {
    env.daemon()
        .expect("`daemon start` returned success, so its daemon has published a portfile")
        .port
}

#[test]
fn token_new_list_revoke_round_trip_when_running() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .args(["daemon", "start"])
        .assert()
        .success();
    wait_for_server(started_port(&env));

    // Standard output is the raw secret, and only the secret (SH-250's rule,
    // carried over here) -- 64 lowercase hex characters, nothing appended.
    let minted = env
        .story(dir.path())
        .args(["token", "new", "laptop"])
        .assert()
        .success();
    let secret = String::from_utf8(minted.get_output().stdout.clone())
        .expect("stdout is UTF-8")
        .trim()
        .to_string();
    assert_eq!(
        secret.len(),
        64,
        "the secret should be 64 hex chars: {secret:?}"
    );
    assert!(
        secret
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
        "the secret should be lowercase hex: {secret:?}"
    );

    env.story(dir.path())
        .args(["token", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("laptop"))
        .stdout(predicate::str::contains(&secret).not());

    env.story(dir.path())
        .args(["token", "revoke", "laptop"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Revoked"));

    env.story(dir.path())
        .args(["token", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("laptop").not());

    // A second revoke of the same name has nothing left to end -- exit code
    // 3 (`AppError::NotFound`), the same class `story show <unknown-id>`
    // uses, not 5 (`AppError::Storage`, an infrastructure failure this is
    // not).
    env.story(dir.path())
        .args(["token", "revoke", "laptop"])
        .assert()
        .failure()
        .code(3);

    env.story(dir.path())
        .args(["daemon", "stop"])
        .assert()
        .success();
}

#[test]
fn token_new_duplicate_name_is_refused_when_running() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .args(["daemon", "start"])
        .assert()
        .success();
    wait_for_server(started_port(&env));

    env.story(dir.path())
        .args(["token", "new", "laptop"])
        .assert()
        .success();
    // Exit code 2 (`AppError::Validation`) -- the caller's own input, not a
    // broken daemon (which would be exit 5, `AppError::Storage`).
    env.story(dir.path())
        .args(["token", "new", "laptop"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("already exists"));

    env.story(dir.path())
        .args(["daemon", "stop"])
        .assert()
        .success();
}

#[test]
fn token_list_with_none_minted_says_so() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .args(["daemon", "start"])
        .assert()
        .success();
    wait_for_server(started_port(&env));

    env.story(dir.path())
        .args(["token", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No named tokens exist"));

    env.story(dir.path())
        .args(["daemon", "stop"])
        .assert()
        .success();
}
