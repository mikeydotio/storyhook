//! `GET /api/dispatch-options` (SH-517): the per-provider model/effort/speed
//! catalog the web dispatch dialog's Model/Effort/Speed selects are built
//! from. `story.sh capabilities --agent=<agent>` is stubbed exactly the way
//! `tests/dispatch_endpoint.rs` stubs `dispatch` — a real daemon subprocess,
//! a tiny bash double, no tmux/git/worktree. That file's own module doc
//! explains why a real daemon subprocess is required here (a bearer token
//! minted by the real daemon lifecycle) rather than the in-process REST
//! harness.

use std::io::Write;
use std::path::Path;

use storyhook::daemon::lifecycle::{self, DaemonInfo};
use storyhook_test_support::{TestEnv, scratch_dir};

/// A capabilities-aware stub, unlike `dispatch_endpoint.rs`'s `stub_script`:
/// it branches on its REAL first argument (`$1`), not a mode baked into its
/// own text, because this suite needs `capabilities --agent=claude` and
/// `capabilities --agent=codex` to answer *differently* through the same
/// resolved script (`STORYHOOK_DISPATCH_SCRIPT` resolves identically for
/// both agents — see `resolve_dispatch_script_from_for_agent`). `$hits_file`
/// counts invocations, letting a cache test assert the helper was *not*
/// re-run on a repeat request rather than merely asserting matching output.
fn capabilities_stub(hits_file: &Path) -> String {
    format!(
        r#"#!/usr/bin/env bash
DISPATCH_PROTOCOL=1
set -u
echo x >> {hits_file:?}
if [ "$1" = "capabilities" ]; then
  agent="${{2#--agent=}}"
  case "$agent" in
    claude)
      printf '{{"ok":true,"agent":"claude","models":[{{"id":"opusplan","default":true}}],"efforts":[{{"id":"max"}}],"speeds":[{{"id":"fast"}}]}}\n'
      ;;
    codex)
      printf '{{"ok":true,"agent":"codex","models":[{{"id":"gpt-5.6-sol"}}],"efforts":[],"speeds":[{{"id":"fast"}}]}}\n'
      ;;
    *)
      printf '{{"ok":false,"display":"unknown agent %s"}}\n' "$agent"
      exit 1
      ;;
  esac
  exit 0
fi
printf '{{"ok":true,"id":"%s","display":"stub dispatched"}}\n' "$4"
"#
    )
}

/// A stub with no `capabilities` verb at all — story.sh's own behavior
/// before SH-517, and the shape an installed plugin predating this story
/// still has. Its top-level `case` has no arm for an unrecognized
/// subcommand's fallthrough here, so it answers a well-formed refusal
/// rather than crashing or hanging, mirroring the real script's `fail()`
/// contract for an unknown verb.
fn no_capabilities_stub() -> String {
    r#"#!/usr/bin/env bash
DISPATCH_PROTOCOL=1
set -u
if [ "$1" = "capabilities" ]; then
  printf '{"ok":false,"display":"usage: story.sh <list | view ... > -- capabilities not recognized"}\n'
  exit 1
fi
printf '{"ok":true,"id":"%s","display":"stub dispatched"}\n' "$4"
"#
    .to_string()
}

fn write_script(content: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().expect("a scratch file for the stub script");
    file.write_all(content.as_bytes())
        .expect("writing the stub script");
    file
}

struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment(), lifecycle::StopMode::Force);
    }
}

fn start_with_stub(env: &TestEnv, stub: &Path) -> DaemonInfo {
    let dir = scratch_dir();
    env.story(dir.path())
        .args(["daemon", "start"])
        .env("STORYHOOK_DISPATCH_SCRIPT", stub)
        .assert()
        .success();
    env.daemon()
        .expect("a started daemon must publish a portfile")
}

fn options_url(info: &DaemonInfo) -> String {
    format!("http://127.0.0.1:{}/api/dispatch-options", info.port)
}

fn get_options(
    info: &DaemonInfo,
    token: &str,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    ureq::get(options_url(info))
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

#[test]
fn no_token_is_401() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let hits = scratch_dir();
    let stub = write_script(&capabilities_stub(&hits.path().join("hits")));
    let info = start_with_stub(&env, stub.path());

    let err = ureq::get(options_url(&info))
        .call()
        .expect_err("no token must be rejected");
    assert_eq!(status_of(&err), 401);
}

#[test]
fn post_is_405() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let hits = scratch_dir();
    let stub = write_script(&capabilities_stub(&hits.path().join("hits")));
    let info = start_with_stub(&env, stub.path());

    // The mutation-guard headers are required here even though this route
    // never mutates anything: `admission::admission` applies that guard to
    // every `/api/**` POST BEFORE the path is even classified (SH-187), so
    // an ungated POST is refused with 403 there and never reaches
    // `handle_options`'s own 405 at all. Sending them is what actually
    // exercises this route's own "not a GET" check rather than the
    // upstream one every mutating `/api/**` request shares.
    let err = ureq::post(options_url(&info))
        .header("X-Storyhook", "1")
        .header("Host", "127.0.0.1")
        .header("X-Storyhook-Token", &info.token)
        .send_empty()
        .expect_err("POST must not be served by this GET-only route");
    assert_eq!(status_of(&err), 405);
}

/// The response carries both providers' own catalogs, each exactly what
/// their own `capabilities` call printed — this endpoint does not reshape
/// or validate the payload, only relays it (`fetch_capabilities`).
#[test]
fn reports_both_providers_own_catalogs() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let hits = scratch_dir();
    let stub = write_script(&capabilities_stub(&hits.path().join("hits")));
    let info = start_with_stub(&env, stub.path());

    let body = body_json(get_options(&info, &info.token).expect("dispatch-options accepted"));
    assert_eq!(body["claude"]["ok"], true);
    assert_eq!(body["claude"]["agent"], "claude");
    assert_eq!(body["claude"]["models"][0]["id"], "opusplan");
    assert_eq!(body["codex"]["ok"], true);
    assert_eq!(body["codex"]["agent"], "codex");
    assert_eq!(body["codex"]["models"][0]["id"], "gpt-5.6-sol");
}

/// A second request within the cache window must not re-invoke the helper
/// — the load-bearing reason `capabilities_for` caches at all (a dispatch
/// dialog reopened repeatedly must not spawn a process on every open).
#[test]
fn a_repeated_request_is_served_from_cache_without_rerunning_the_helper() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let hits_dir = scratch_dir();
    let hits_file = hits_dir.path().join("hits");
    let stub = write_script(&capabilities_stub(&hits_file));
    let info = start_with_stub(&env, stub.path());

    let first = body_json(get_options(&info, &info.token).expect("first request accepted"));
    let second = body_json(get_options(&info, &info.token).expect("second request accepted"));
    assert_eq!(first, second);

    let hits = std::fs::read_to_string(&hits_file).unwrap_or_default();
    let hit_count = hits.lines().count();
    assert_eq!(
        hit_count, 2,
        "one capabilities call per provider on the FIRST request only \
         (claude + codex); the second request must be served from cache: {hits:?}"
    );
}

/// A helper that has no `capabilities` verb at all (an installed plugin
/// predating SH-517) must degrade that one provider's slot to a well-formed
/// refusal, not break the endpoint or the other provider's slot.
#[test]
fn a_helper_with_no_capabilities_verb_degrades_gracefully() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_script(&no_capabilities_stub());
    let info = start_with_stub(&env, stub.path());

    let body = body_json(get_options(&info, &info.token).expect("dispatch-options accepted"));
    assert_eq!(
        body["claude"]["ok"], false,
        "a stale helper's slot must degrade rather than error: {body}"
    );
    assert!(body["claude"]["reason"].is_string() || body["claude"]["display"].is_string());
    assert_eq!(body["codex"]["ok"], false);
}

/// A harder failure than a well-formed refusal: the helper prints nothing
/// parseable at all (a crash, a truncated exec). `run_shell_capabilities`
/// then returns `Err`, and `fetch_capabilities` must fold that into the
/// same `{"ok": false, "reason": ...}` shape rather than the whole endpoint
/// erroring.
#[test]
fn a_helper_that_prints_nothing_parseable_also_degrades_gracefully() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_script(
        r#"#!/usr/bin/env bash
DISPATCH_PROTOCOL=1
exit 7
"#,
    );
    let info = start_with_stub(&env, stub.path());

    let body = body_json(get_options(&info, &info.token).expect("dispatch-options accepted"));
    assert_eq!(body["claude"]["ok"], false);
    assert!(body["claude"]["reason"].is_string());
    assert_eq!(body["codex"]["ok"], false);
}
