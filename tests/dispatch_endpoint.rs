//! The dashboard's dispatch endpoint (SH-50): the token gate, the async
//! 202-then-poll shape, and how a `story.sh` outcome is relayed.
//!
//! Runs a real daemon subprocess — exactly like `tests/daemon_lifecycle.rs`
//! — rather than the in-process REST harness `tests/web_test.rs` otherwise
//! uses. This endpoint's whole reason to exist is a bearer token minted by
//! the real daemon lifecycle; the in-process harness
//! (`crates/storyhook-test-support/src/server.rs`) always serves with an
//! *empty* token, by design, for tests that never needed one before this.
//!
//! `story.sh` itself is stubbed (a tiny bash script, no `jq` dependency)
//! branching on `$DISPATCH_STUB_MODE` — no tmux, no git, no worktree. That
//! end-to-end path (a real `story.sh`, a real worktree, a real fake-tmux
//! window) is the e2e suite's job; this file is the daemon's HTTP contract
//! in front of whatever the script reports.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use storyhook::daemon::lifecycle::{self, DaemonInfo};
use storyhook_test_support::{TestEnv, scratch_dir};

/// A dispatch-stub `story.sh`. Argv is exactly what the daemon invokes:
/// `--project <slug> dispatch <story-id>`, so `$4` is the story id.
const STUB_SCRIPT: &str = r#"#!/usr/bin/env bash
set -u
case "${DISPATCH_STUB_MODE:-ok}" in
  ok)
    printf '{"ok":true,"id":"%s","display":"stub dispatched %s"}\n' "$4" "$4"
    ;;
  refuse)
    printf '{"ok":false,"display":"stub refused: not ready"}\n'
    ;;
  silent)
    exit 7
    ;;
  slow)
    sleep "${DISPATCH_STUB_SLEEP:-2}"
    printf '{"ok":true,"id":"%s","display":"stub dispatched slowly"}\n' "$4"
    ;;
  echo-args)
    printf '{"ok":true,"argv":"%s"}\n' "$*"
    ;;
esac
"#;

/// Writes the stub to a fresh temp file and returns it — kept alive by the
/// caller for as long as the daemon that was pointed at it might still run.
fn write_stub() -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().expect("a scratch file for the stub script");
    file.write_all(STUB_SCRIPT.as_bytes())
        .expect("writing the stub script");
    file
}

/// Stops whatever daemon `env` is running, even if the test panics first —
/// the same reasoning `daemon_lifecycle.rs`'s own guard exists for.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment());
    }
}

/// Starts a daemon with `STORYHOOK_DISPATCH_SCRIPT` pointed at `stub`, in
/// `mode` (read by the stub via `$DISPATCH_STUB_MODE`, inherited all the
/// way down from this process through the daemon to the dispatch child).
fn start_with_stub(env: &TestEnv, stub: &Path, mode: &str) -> DaemonInfo {
    let dir = scratch_dir();
    env.story(dir.path())
        .args(["daemon", "start"])
        .env("STORYHOOK_DISPATCH_SCRIPT", stub)
        .env("DISPATCH_STUB_MODE", mode)
        .assert()
        .success();
    env.daemon()
        .expect("a started daemon must publish a portfile")
}

fn dispatch_url(info: &DaemonInfo, project: &str, story: &str) -> String {
    format!(
        "http://127.0.0.1:{}/api/repos/{project}/story/{story}/dispatch",
        info.port
    )
}

/// A guarded, tokened POST to the dispatch endpoint.
fn post_dispatch(
    info: &DaemonInfo,
    token: &str,
    project: &str,
    story: &str,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    ureq::post(dispatch_url(info, project, story))
        .header("X-Storyhook", "1")
        .header("Host", "127.0.0.1")
        .header("X-Storyhook-Token", token)
        .send_empty()
}

/// A guarded, tokened GET against a dispatch handle.
fn get_dispatch(
    info: &DaemonInfo,
    token: &str,
    project: &str,
    story: &str,
    handle: &str,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    ureq::get(format!("{}/{handle}", dispatch_url(info, project, story)))
        .header("X-Storyhook", "1")
        .header("Host", "127.0.0.1")
        .header("X-Storyhook-Token", token)
        .call()
}

fn status_of(err: &ureq::Error) -> u16 {
    match err {
        ureq::Error::StatusCode(code) => *code,
        other => panic!("expected a status-code error, got: {other}"),
    }
}

fn body_json(resp: ureq::http::Response<ureq::Body>) -> serde_json::Value {
    let text = resp
        .into_body()
        .read_to_string()
        .expect("reading the response body");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("body was not JSON: {e}: {text}"))
}

/// Polls `GET .../dispatch/{handle}` until its `state` leaves `"running"`,
/// or panics after 5s — real story.sh dispatches take much longer, but
/// every stub in this file finishes in well under a second.
fn poll_until_finished(
    info: &DaemonInfo,
    token: &str,
    project: &str,
    story: &str,
    handle: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let record = body_json(
            get_dispatch(info, token, project, story, handle)
                .unwrap_or_else(|e| panic!("polling the dispatch handle: {e}")),
        )["dispatch"]
            .clone();
        if record["state"] != "running" {
            return record;
        }
        if Instant::now() > deadline {
            panic!("dispatch {handle} did not finish within 5s: {record}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn missing_guard_header_is_403() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub();
    let info = start_with_stub(&env, stub.path(), "ok");

    let err = ureq::post(dispatch_url(&info, "proj", "SH-1"))
        .header("X-Storyhook-Token", &info.token)
        .send_empty()
        .expect_err("no X-Storyhook header must be rejected");
    assert_eq!(status_of(&err), 403);
}

#[test]
fn spoofed_host_is_403() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub();
    let info = start_with_stub(&env, stub.path(), "ok");

    let err = ureq::post(dispatch_url(&info, "proj", "SH-1"))
        .header("X-Storyhook", "1")
        .header("Host", "evil.example")
        .header("X-Storyhook-Token", &info.token)
        .send_empty()
        .expect_err("a spoofed Host must be rejected");
    assert_eq!(status_of(&err), 403);
}

#[test]
fn no_token_is_401_on_post_and_get() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub();
    let info = start_with_stub(&env, stub.path(), "ok");

    let post_err = ureq::post(dispatch_url(&info, "proj", "SH-1"))
        .header("X-Storyhook", "1")
        .header("Host", "127.0.0.1")
        .send_empty()
        .expect_err("no token must be rejected");
    assert_eq!(status_of(&post_err), 401);

    let get_err = ureq::get(format!("{}/anything", dispatch_url(&info, "proj", "SH-1")))
        .header("X-Storyhook", "1")
        .header("Host", "127.0.0.1")
        .call()
        .expect_err("no token must be rejected on GET too");
    assert_eq!(status_of(&get_err), 401);
}

#[test]
fn wrong_token_is_401() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub();
    let info = start_with_stub(&env, stub.path(), "ok");

    let err = post_dispatch(&info, "not-the-token", "proj", "SH-1")
        .expect_err("the wrong token must be rejected");
    assert_eq!(status_of(&err), 401);
}

#[test]
fn put_on_a_dispatch_path_is_405() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub();
    let info = start_with_stub(&env, stub.path(), "ok");

    let err = ureq::put(dispatch_url(&info, "proj", "SH-1"))
        .header("X-Storyhook", "1")
        .header("Host", "127.0.0.1")
        .header("X-Storyhook-Token", &info.token)
        .send_empty()
        .expect_err("PUT is not a dispatch verb");
    assert_eq!(status_of(&err), 405);
}

#[test]
fn a_successful_stub_is_relayed_as_ok_with_its_payload() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub();
    let info = start_with_stub(&env, stub.path(), "ok");

    let resp = post_dispatch(&info, &info.token, "proj", "SH-1").expect("dispatch accepted");
    assert_eq!(resp.status(), 202);
    let accepted = body_json(resp);
    let handle = accepted["dispatch"]["handle"]
        .as_str()
        .expect("a handle to poll")
        .to_string();
    assert_eq!(accepted["dispatch"]["state"], "running");
    assert_eq!(accepted["dispatch"]["project"], "proj");
    assert_eq!(accepted["dispatch"]["story"], "SH-1");

    let record = poll_until_finished(&info, &info.token, "proj", "SH-1", &handle);
    assert_eq!(record["state"], "ok");
    assert_eq!(record["payload"]["ok"], true);
    assert_eq!(record["payload"]["id"], "SH-1");
    assert!(record["finished_at"].is_string());
}

#[test]
fn a_well_formed_refusal_is_relayed_verbatim_as_refused() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub();
    let info = start_with_stub(&env, stub.path(), "refuse");

    let resp = post_dispatch(&info, &info.token, "proj", "SH-1").expect("dispatch accepted");
    let handle = body_json(resp)["dispatch"]["handle"]
        .as_str()
        .expect("a handle")
        .to_string();

    let record = poll_until_finished(&info, &info.token, "proj", "SH-1", &handle);
    assert_eq!(record["state"], "refused");
    assert_eq!(record["payload"]["ok"], false);
    assert_eq!(record["payload"]["display"], "stub refused: not ready");
    assert!(
        record.get("error").is_none(),
        "a business refusal is not a script failure"
    );
}

#[test]
fn a_script_that_prints_nothing_is_reported_as_failed() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub();
    let info = start_with_stub(&env, stub.path(), "silent");

    let resp = post_dispatch(&info, &info.token, "proj", "SH-1").expect("dispatch accepted");
    let handle = body_json(resp)["dispatch"]["handle"]
        .as_str()
        .expect("a handle")
        .to_string();

    let record = poll_until_finished(&info, &info.token, "proj", "SH-1", &handle);
    assert_eq!(record["state"], "failed");
    assert!(
        record.get("payload").is_none(),
        "a failure carries no story.sh payload -- there wasn't one"
    );
    assert!(
        record["error"]
            .as_str()
            .unwrap()
            .contains("without printing a result")
    );
}

#[test]
fn a_repeated_post_for_a_running_story_reuses_the_handle_rather_than_spawning_twice() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub();
    let info = start_with_stub(&env, stub.path(), "slow");

    let first = body_json(
        post_dispatch(&info, &info.token, "proj", "SH-1").expect("first dispatch accepted"),
    )["dispatch"]["handle"]
        .as_str()
        .expect("a handle")
        .to_string();
    let second = body_json(
        post_dispatch(&info, &info.token, "proj", "SH-1").expect("second dispatch accepted"),
    )["dispatch"]["handle"]
        .as_str()
        .expect("a handle")
        .to_string();

    assert_eq!(
        first, second,
        "a story already dispatching must not spawn a second script"
    );

    // Once it finishes, a fresh POST for the same story is a genuinely new
    // attempt and gets its own handle -- the registry does not remember a
    // finished dispatch as "still claiming" the story.
    let _ = poll_until_finished(&info, &info.token, "proj", "SH-1", &first);
    let third = body_json(
        post_dispatch(&info, &info.token, "proj", "SH-1").expect("third dispatch accepted"),
    )["dispatch"]["handle"]
        .as_str()
        .expect("a handle")
        .to_string();
    assert_ne!(
        third, first,
        "a finished dispatch must not block a fresh attempt for the same story"
    );
}

#[test]
fn the_project_and_story_reach_the_script_verbatim() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub();
    let info = start_with_stub(&env, stub.path(), "echo-args");

    let resp =
        post_dispatch(&info, &info.token, "scad-caliper", "CAL-12").expect("dispatch accepted");
    let handle = body_json(resp)["dispatch"]["handle"]
        .as_str()
        .expect("a handle")
        .to_string();

    let record = poll_until_finished(&info, &info.token, "scad-caliper", "CAL-12", &handle);
    let argv = record["payload"]["argv"].as_str().expect("argv echoed back");
    assert!(argv.contains("--project scad-caliper"));
    assert!(argv.contains("dispatch CAL-12"));
}

#[test]
fn an_unknown_handle_is_404() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub();
    let info = start_with_stub(&env, stub.path(), "ok");

    let err = get_dispatch(&info, &info.token, "proj", "SH-1", "no-such-handle")
        .expect_err("a handle that was never minted must 404");
    assert_eq!(status_of(&err), 404);
}
