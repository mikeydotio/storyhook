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
//! whose mode is baked into its own text at write time (see [`stub_script`])
//! — no tmux, no git, no worktree. That end-to-end path (a real `story.sh`,
//! a real worktree, a real fake-tmux window) is the e2e suite's job; this
//! file is the daemon's HTTP contract in front of whatever the script
//! reports.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use storyhook::api::dispatch::REQUIRED_DISPATCH_PROTOCOL;
use storyhook::daemon::lifecycle::{self, DaemonInfo};
use storyhook_test_support::{TestEnv, scratch_dir};

/// A dispatch-stub `story.sh`, rendered with `mode` baked into its `case`
/// selector as a literal rather than read from an environment variable at
/// run time. Argv is exactly what the daemon invokes: `--project <slug>
/// dispatch <story-id>`, so `$4` is the story id.
///
/// **Baked in, not passed via env (SH-193).** This used to read
/// `$DISPATCH_STUB_MODE`, set on the daemon's own process and relied on to
/// reach the stub unfiltered all the way through the dispatch child's
/// inherited environment. `src/env/spawn_env.rs`'s allowlist now clears that
/// environment before the child ever runs, which is exactly the production
/// behavior this suite exists to exercise — so the fixture is no longer
/// allowed to lean on the hole the fix closes. Baking the mode into the
/// script's own text at write time controls the stub through its argv and
/// its file, the same two channels a real `story.sh` invocation actually
/// gets, rather than through an env var storyhook's own allowlist would
/// (correctly) now strip.
///
/// Declares the current dispatch protocol so this stub keeps resolving
/// under `resolve_dispatch_script_from`'s protocol check, which applies to
/// `STORYHOOK_DISPATCH_SCRIPT` the same as any other resolution source —
/// see `a_script_below_the_required_protocol_is_refused_before_any_handle_exists`
/// for the deliberately-unmarked negative case.
fn stub_script(mode: &str) -> String {
    format!(
        r#"#!/usr/bin/env bash
DISPATCH_PROTOCOL=3
set -u
case "{mode}" in
  ok)
    printf '{{"ok":true,"id":"%s","display":"stub dispatched %s"}}\n' "$4" "$4"
    ;;
  refuse)
    printf '{{"ok":false,"display":"stub refused: not ready"}}\n'
    ;;
  silent)
    exit 7
    ;;
  slow)
    sleep 2
    printf '{{"ok":true,"id":"%s","display":"stub dispatched slowly"}}\n' "$4"
    ;;
  echo-args)
    printf '{{"ok":true,"argv":"%s"}}\n' "$*"
    ;;
esac
"#
    )
}

/// Writes `content` to a fresh temp file and returns it — kept alive by the
/// caller for as long as the daemon that was pointed at it might still run.
fn write_script(content: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().expect("a scratch file for the stub script");
    file.write_all(content.as_bytes())
        .expect("writing the stub script");
    file
}

/// Writes [`stub_script`] rendered for `mode` to a fresh temp file.
fn write_stub(mode: &str) -> tempfile::NamedTempFile {
    write_script(&stub_script(mode))
}

/// Stops whatever daemon `env` is running, even if the test panics first —
/// the same reasoning `daemon_lifecycle.rs`'s own guard exists for.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment(), lifecycle::StopMode::Force);
    }
}

/// Starts a daemon with `STORYHOOK_DISPATCH_SCRIPT` pointed at `stub`, whose
/// own text already selects its mode — see [`stub_script`].
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

/// A guarded, tokened POST to the dispatch endpoint, with `query` appended
/// verbatim (e.g. `"auto=1"`) — SH-208's one query parameter.
fn post_dispatch_query(
    info: &DaemonInfo,
    token: &str,
    project: &str,
    story: &str,
    query: &str,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    ureq::post(format!("{}?{query}", dispatch_url(info, project, story)))
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
    let stub = write_stub("ok");
    let info = start_with_stub(&env, stub.path());

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
    let stub = write_stub("ok");
    let info = start_with_stub(&env, stub.path());

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
    let stub = write_stub("ok");
    let info = start_with_stub(&env, stub.path());

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
    let stub = write_stub("ok");
    let info = start_with_stub(&env, stub.path());

    let err = post_dispatch(&info, "not-the-token", "proj", "SH-1")
        .expect_err("the wrong token must be rejected");
    assert_eq!(status_of(&err), 401);
}

#[test]
fn put_on_a_dispatch_path_is_405() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("ok");
    let info = start_with_stub(&env, stub.path());

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
    let stub = write_stub("ok");
    let info = start_with_stub(&env, stub.path());

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
    assert_eq!(
        accepted["dispatch"]["auto"], false,
        "a plain POST (no ?auto=1) must default to attended"
    );
}

#[test]
fn a_well_formed_refusal_is_relayed_verbatim_as_refused() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("refuse");
    let info = start_with_stub(&env, stub.path());

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
    let stub = write_stub("silent");
    let info = start_with_stub(&env, stub.path());

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
    let stub = write_stub("slow");
    let info = start_with_stub(&env, stub.path());

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
    let stub = write_stub("echo-args");
    let info = start_with_stub(&env, stub.path());

    let resp =
        post_dispatch(&info, &info.token, "scad-caliper", "CAL-12").expect("dispatch accepted");
    let handle = body_json(resp)["dispatch"]["handle"]
        .as_str()
        .expect("a handle")
        .to_string();

    let record = poll_until_finished(&info, &info.token, "scad-caliper", "CAL-12", &handle);
    let argv = record["payload"]["argv"]
        .as_str()
        .expect("argv echoed back");
    assert!(argv.contains("--project scad-caliper"));
    assert!(argv.contains("dispatch CAL-12 --agent=claude"));
    assert!(
        argv.contains("--resume"),
        "every dashboard dispatch grants automatic resume permission: {argv}"
    );
    assert!(
        !argv.contains("--auto"),
        "a plain dispatch's argv must not carry --auto"
    );
}

/// SH-208: `?auto=1` is the one extra argument this endpoint ever adds to
/// story.sh's own dispatch argv, appended after the story id.
#[test]
fn auto_equals_1_appends_auto_to_the_scripts_argv_and_is_relayed_in_the_record() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("echo-args");
    let info = start_with_stub(&env, stub.path());

    let resp = post_dispatch_query(&info, &info.token, "scad-caliper", "CAL-12", "auto=1")
        .expect("dispatch accepted");
    let accepted = body_json(resp);
    assert_eq!(
        accepted["dispatch"]["auto"], true,
        "the 202 record must already report auto:true, before the script even runs"
    );
    let handle = accepted["dispatch"]["handle"]
        .as_str()
        .expect("a handle")
        .to_string();

    let record = poll_until_finished(&info, &info.token, "scad-caliper", "CAL-12", &handle);
    assert_eq!(record["auto"], true);
    let argv = record["payload"]["argv"]
        .as_str()
        .expect("argv echoed back");
    assert!(argv.contains("dispatch CAL-12 --agent=claude --resume --auto"));
}

#[test]
fn codex_agent_is_relayed_and_passed_to_the_shared_helper() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("echo-args");
    let info = start_with_stub(&env, stub.path());

    let accepted = body_json(
        post_dispatch_query(&info, &info.token, "proj", "SH-1", "agent=codex&auto=1")
            .expect("Codex dispatch accepted"),
    );
    assert_eq!(accepted["dispatch"]["agent"], "codex");
    assert_eq!(accepted["dispatch"]["auto"], true);
    let handle = accepted["dispatch"]["handle"].as_str().unwrap();
    let record = poll_until_finished(&info, &info.token, "proj", "SH-1", handle);
    assert_eq!(record["agent"], "codex");
    assert!(
        record["payload"]["argv"]
            .as_str()
            .unwrap()
            .contains("dispatch SH-1 --agent=codex --resume --auto")
    );
}

#[test]
fn absent_agent_defaults_to_claude_and_invalid_or_duplicate_values_are_400() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("ok");
    let info = start_with_stub(&env, stub.path());

    let accepted = body_json(
        post_dispatch(&info, &info.token, "proj", "SH-1").expect("default dispatch accepted"),
    );
    assert_eq!(accepted["dispatch"]["agent"], "claude");

    for query in [
        "agent=claude-code",
        "agent=unknown",
        "agent=",
        "agent=claude&agent=codex",
    ] {
        let err = post_dispatch_query(&info, &info.token, "proj", "SH-2", query)
            .expect_err("invalid agent must be rejected synchronously");
        assert_eq!(status_of(&err), 400, "query={query}");
    }
}

/// `auto=true` is the other recognized spelling (the dashboard sends `1`;
/// this is the human-typable form a `curl` caller would reach for).
#[test]
fn auto_equals_true_is_also_accepted() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("ok");
    let info = start_with_stub(&env, stub.path());

    let resp = post_dispatch_query(&info, &info.token, "proj", "SH-1", "auto=true")
        .expect("dispatch accepted");
    assert_eq!(body_json(resp)["dispatch"]["auto"], true);
}

/// A caller who typed `?auto=0` expecting attended must be told they got it
/// wrong rather than silently downgraded — see `parse_auto`'s own doc
/// comment for why this is a 400 and not a quiet `false`.
#[test]
fn an_unrecognized_auto_value_is_400() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("ok");
    let info = start_with_stub(&env, stub.path());

    let err = post_dispatch_query(&info, &info.token, "proj", "SH-1", "auto=0")
        .expect_err("auto=0 must be rejected, not silently treated as attended");
    assert_eq!(status_of(&err), 400);
}

/// The idempotency wrinkle: a story already dispatching (attended) is not
/// restarted in autonomous mode by a `?auto=1` POST that loses the race —
/// it reuses the running attempt's handle, and that handle's record still
/// reports the mode that is *actually running*.
#[test]
fn a_repeated_post_with_a_different_auto_reuses_the_first_attempts_mode() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("slow");
    let info = start_with_stub(&env, stub.path());

    let first = body_json(
        post_dispatch(&info, &info.token, "proj", "SH-1").expect("first dispatch accepted"),
    );
    assert_eq!(first["dispatch"]["auto"], false);
    let first_handle = first["dispatch"]["handle"]
        .as_str()
        .expect("a handle")
        .to_string();

    let second = body_json(
        post_dispatch_query(&info, &info.token, "proj", "SH-1", "auto=1")
            .expect("second dispatch accepted"),
    );
    assert_eq!(
        second["dispatch"]["handle"].as_str(),
        Some(first_handle.as_str()),
        "a story already dispatching must not spawn a second, autonomous script"
    );
    assert_eq!(
        second["dispatch"]["auto"], false,
        "the reused record must report the FIRST attempt's mode, not the second request's"
    );
}

#[test]
fn a_repeated_post_with_a_different_agent_reuses_the_first_attempts_agent() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("slow");
    let info = start_with_stub(&env, stub.path());

    let first = body_json(
        post_dispatch_query(&info, &info.token, "proj", "SH-1", "agent=codex")
            .expect("first dispatch accepted"),
    );
    let second = body_json(
        post_dispatch_query(&info, &info.token, "proj", "SH-1", "agent=claude")
            .expect("deduped dispatch accepted"),
    );
    assert_eq!(second["dispatch"]["handle"], first["dispatch"]["handle"]);
    assert_eq!(second["dispatch"]["agent"], "codex");
}

#[test]
fn an_unknown_handle_is_404() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("ok");
    let info = start_with_stub(&env, stub.path());

    let err = get_dispatch(&info, &info.token, "proj", "SH-1", "no-such-handle")
        .expect_err("a handle that was never minted must 404");
    assert_eq!(status_of(&err), 404);
}

/// A script that predates `DISPATCH_PROTOCOL` entirely -- the exact shape of
/// the machine that produced SH-196: an installed plugin cached before the
/// daemon's own argv contract existed.
const UNMARKED_SCRIPT: &str = r#"#!/usr/bin/env bash
set -u
printf '{"ok":true,"id":"%s","display":"stub dispatched %s"}\n' "$4" "$4"
"#;

#[test]
fn a_script_below_the_required_protocol_is_refused_before_any_handle_exists() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_script(UNMARKED_SCRIPT);
    let info = start_with_stub(&env, stub.path());

    // http_status_as_error(false): the default "non-2xx is an Err" behavior
    // discards the body (ureq::Error::StatusCode carries only the code, as
    // every other error-body assertion in this tree already works around --
    // see web_test.rs's web_serve_error_reply_uses_standard_envelope), and
    // the whole point of this test is the body's diagnosis.
    let resp = ureq::post(dispatch_url(&info, "proj", "SH-1"))
        .header("X-Storyhook", "1")
        .header("Host", "127.0.0.1")
        .header("X-Storyhook-Token", &info.token)
        .config()
        .http_status_as_error(false)
        .build()
        .send_empty()
        .expect("the request itself must succeed even though the daemon refuses it");
    assert_eq!(
        resp.status(),
        500,
        "a version-skewed script must be refused, not run blind (SH-196)"
    );
    let body = resp
        .into_body()
        .read_to_string()
        .expect("reading the response body");
    assert!(
        body.contains("out of date"),
        "the 500 must name the real diagnosis, not a generic failure: {body}"
    );
    assert!(
        body.contains("story plugin install claude"),
        "the 500 must name the remedy: {body}"
    );
    assert!(
        body.contains("story plugin install codex"),
        "the 500 must list the Codex remedy too: {body}"
    );

    // No handle was ever minted -- confirmed the same way an_unknown_handle_is_404
    // confirms it: any id polls 404, because try_start() is never reached
    // when resolve_dispatch_script() fails first.
    let poll_err = get_dispatch(&info, &info.token, "proj", "SH-1", "no-such-handle")
        .expect_err("no handle exists to have been minted");
    assert_eq!(status_of(&poll_err), 404);
}

/// SH-517: an unselected dispatch's argv must be byte-identical to what
/// `the_project_and_story_reach_the_script_verbatim` already pinned before
/// this story touched anything -- no `--model`, `--effort`, or `--speed`
/// flag appears merely because the feature now exists.
#[test]
fn an_unselected_dispatch_carries_no_model_effort_or_speed_flag() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("echo-args");
    let info = start_with_stub(&env, stub.path());

    let resp = post_dispatch(&info, &info.token, "proj", "SH-1").expect("dispatch accepted");
    let handle = body_json(resp)["dispatch"]["handle"]
        .as_str()
        .expect("a handle")
        .to_string();
    let record = poll_until_finished(&info, &info.token, "proj", "SH-1", &handle);
    let argv = record["payload"]["argv"]
        .as_str()
        .expect("argv echoed back");
    assert_eq!(
        argv.trim(),
        "--project proj dispatch SH-1 --agent=claude --resume",
        "an unselected dashboard dispatch carries only automatic resume permission"
    );
    assert!(!record.as_object().unwrap().contains_key("model"));
    assert!(!record.as_object().unwrap().contains_key("effort"));
    assert_eq!(record["fast"], false);
}

#[test]
fn a_helper_below_the_required_protocol_is_refused_before_dispatch() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub =
        write_script("#!/usr/bin/env bash\nDISPATCH_PROTOCOL=1\nprintf '{\"ok\":true}\\n'\n");
    let info = start_with_stub(&env, stub.path());

    let response = ureq::post(dispatch_url(&info, "proj", "SH-1"))
        .header("X-Storyhook", "1")
        .header("Host", "127.0.0.1")
        .header("X-Storyhook-Token", &info.token)
        .config()
        .http_status_as_error(false)
        .build()
        .send_empty()
        .expect("the HTTP exchange itself succeeds");
    assert_eq!(response.status(), 500);
    let body = response.into_body().read_to_string().unwrap();
    assert!(body.contains("protocol 1"), "{body}");
    assert!(
        body.contains(&format!("needs at least {REQUIRED_DISPATCH_PROTOCOL}")),
        "{body}"
    );
}

/// `?model=`/`?effort=`/`?speed=fast` each append their own flag to the
/// script's argv, in that order, and are relayed on the record (SH-517).
#[test]
fn model_effort_and_speed_append_their_own_flags_and_are_relayed_in_the_record() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("echo-args");
    let info = start_with_stub(&env, stub.path());

    let accepted = body_json(
        post_dispatch_query(
            &info,
            &info.token,
            "proj",
            "SH-1",
            "model=haiku&effort=max&speed=fast",
        )
        .expect("dispatch accepted"),
    );
    assert_eq!(accepted["dispatch"]["model"], "haiku");
    assert_eq!(accepted["dispatch"]["effort"], "max");
    assert_eq!(accepted["dispatch"]["fast"], true);
    let handle = accepted["dispatch"]["handle"].as_str().unwrap();

    let record = poll_until_finished(&info, &info.token, "proj", "SH-1", handle);
    assert_eq!(record["model"], "haiku");
    assert_eq!(record["effort"], "max");
    assert_eq!(record["fast"], true);
    let argv = record["payload"]["argv"]
        .as_str()
        .expect("argv echoed back");
    assert!(
        argv.contains(
            "dispatch SH-1 --agent=claude --resume --model=haiku --effort=max --speed=fast"
        ),
        "argv: {argv}"
    );
}

/// `speed=standard` is the explicit spelling of "no selection" — it must
/// behave exactly like an absent `speed` (no `--speed` flag at all), never
/// pass `--speed=standard` through to the helper.
#[test]
fn speed_equals_standard_appends_no_flag() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("echo-args");
    let info = start_with_stub(&env, stub.path());

    let accepted = body_json(
        post_dispatch_query(&info, &info.token, "proj", "SH-1", "speed=standard")
            .expect("dispatch accepted"),
    );
    assert_eq!(accepted["dispatch"]["fast"], false);
    let handle = accepted["dispatch"]["handle"].as_str().unwrap();
    let record = poll_until_finished(&info, &info.token, "proj", "SH-1", handle);
    let argv = record["payload"]["argv"]
        .as_str()
        .expect("argv echoed back");
    assert!(!argv.contains("--speed"), "argv: {argv}");
}

/// Every rejection [`parse_option_token`]/`parse_speed` can produce: a
/// duplicate key, an empty value, a value over 64 characters, and every
/// hazard class `OptionToken`'s charset gate exists to keep out of a shell
/// command line a real tmux pane later execs verbatim.
#[test]
fn invalid_model_effort_or_speed_values_are_400() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let stub = write_stub("ok");
    let info = start_with_stub(&env, stub.path());

    let sixty_five = "a".repeat(65);
    for query in [
        "model=",
        "model=a;rm-rf",
        "model=$(id)",
        "model=a%20b",
        "model=a%0ab",
        &format!("model={sixty_five}"),
        "model=haiku&model=opus",
        "effort=",
        "effort=max&effort=low",
        "speed=warp",
        "speed=",
        "speed=fast&speed=standard",
    ] {
        // A raw shell metacharacter can make the value invalid URI syntax
        // before the request is even sent (ureq's client-side URI parser
        // refuses it) rather than reaching the server to be answered with a
        // 400 — an even stronger outcome for a value the charset gate exists
        // to keep out of a shell command line, since it never left this
        // process as bytes at all. Both count as "rejected".
        match post_dispatch_query(&info, &info.token, "proj", "SH-1", query) {
            Err(ureq::Error::StatusCode(code)) => assert_eq!(code, 400, "query={query}"),
            Err(ureq::Error::Http(_)) => {} // rejected before it could be sent
            other => panic!("query={query}: expected a rejection, got {other:?}"),
        }
    }
}
