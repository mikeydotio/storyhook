//! `STORYHOOK_WEB_TRUSTED_HOSTS` — the reverse-proxy allowlist, under test for
//! the first time.
//!
//! # Why this file exists
//!
//! The variable shipped with the mutation guard and was never asserted on by
//! anything, before SH-187 or after it; that spec recorded the gap as a
//! standing residual. SH-250 then made it load-bearing for a second reason:
//! its emptiness was the fifth conjunct of the loopback token exemption
//! (`crate::api::admission::token_exempt`), so "nobody has ever tested this"
//! stopped being merely untidy.
//!
//! SH-255 deleted that exemption, and with it the whole reason a read's
//! outcome ever depended on this variable — a read needs a token on every
//! listener now, proxy declared or not. What survives, and is still tested
//! here, is the property the variable has always actually been *for*:
//! widening the `Host` allowlist so a proxied mutation is not refused as a
//! rebinding attempt. That was never about the exemption and is unaffected
//! by its removal.
//!
//! # Why these spawn a real daemon
//!
//! The in-process seam (`try_serve_on` → `bind_and_serve`) reads the *test
//! process's* environment, and `TestEnv` deliberately never mutates that — a
//! `set_var` here would race every other test in the binary. So the variable
//! has to be set on a child, which means a real spawned daemon, the same
//! construction `tests/tailnet_advertise.rs` uses. `DaemonGuard` reaps it, and
//! `TestEnv::isolated` pins the daemon-containment variables
//! (`STORYHOOK_DAEMON_ADDR`, `STORYHOOK_PARENT_PID`) so nothing here binds
//! 3456 or outlives the run.

use storyhook_test_support::{DaemonGuard, TestEnv, scratch_dir, slug_at, wait_for_server};

/// The hostname a reverse proxy is pretended to forward under. Not resolvable
/// and not this machine — the allowlist is the only reason the daemon would
/// ever accept it.
const PROXY_HOST: &str = "dash.proxy.example";

/// A hostname deliberately *not* on the allowlist, to prove the allowlist
/// admits what it names rather than switching the check off.
const FOREIGN_HOST: &str = "evil.example";

/// A daemon serving one project, optionally behind a declared reverse proxy.
///
/// Field order is drop order, and it matters: `guard` runs `story web stop`
/// with `home`'s directory as its working directory, so the guard has to drop
/// *first*, while that directory still exists. Declaring it after would delete
/// the directory out from under the stop and turn a clean teardown into a
/// panic inside a destructor.
struct Proxied<'a> {
    guard: Option<DaemonGuard>,
    /// Where the daemon was started from, and where the guard stops it.
    home: tempfile::TempDir,
    /// Held only to keep the checkout alive for the daemon's lifetime.
    _project: storyhook_test_support::Project<'a>,
    port: u16,
    slug: String,
}

impl Drop for Proxied<'_> {
    fn drop(&mut self) {
        // Explicit rather than implicit, so the ordering above is enforced by
        // a statement somebody has to delete rather than by a field order
        // somebody could reshuffle without noticing.
        self.guard.take();
        let _ = &self.home;
    }
}

/// Starts a daemon, optionally behind a declared reverse proxy, and seeds it
/// with one project holding one story.
///
/// **The daemon is started before the project exists, and that is the whole
/// trick.** Every `story` command reaches the store through the daemon
/// (SH-114), so `story project new` auto-spawns one if none is running — and
/// a daemon spawned that way would carry the ambient environment, never this
/// fixture's `STORYHOOK_WEB_TRUSTED_HOSTS`. Starting it explicitly first is
/// what makes the variable reach the process that actually serves the
/// requests these tests make.
///
/// No `--port`: the harness pins `STORYHOOK_DAEMON_ADDR` to `127.0.0.1:0`, so
/// the port is whatever the kernel gave, read back from the portfile the
/// daemon publishes. Reserving one and asking for it would be a second way to
/// get the same answer, and one that can race.
fn daemon_with_proxy_allowlist<'a>(env: &'a TestEnv, proxy_hosts: Option<&str>) -> Proxied<'a> {
    let home = scratch_dir();
    let guard = Some(DaemonGuard::new(env, home.path()));

    let mut command = env.story(home.path());
    command.args(["web", "start"]);
    if let Some(hosts) = proxy_hosts {
        command.env("STORYHOOK_WEB_TRUSTED_HOSTS", hosts);
    }
    command.assert().success();

    let port = env
        .daemon()
        .expect("a started daemon must publish a portfile")
        .port;
    wait_for_server(port);

    let project = env.project().build();
    let slug = slug_at(env, project.path());
    project.new_story("Visible through the proxy");

    Proxied {
        guard,
        home,
        _project: project,
        port,
        slug,
    }
}

/// The token, from the portfile the daemon just published.
fn token(env: &TestEnv) -> String {
    env.daemon()
        .expect("a started daemon must publish a portfile")
        .token
}

fn data_url(port: u16, slug: &str) -> String {
    format!("http://127.0.0.1:{port}/api/repos/{slug}/data")
}

fn status_of(err: ureq::Error) -> u16 {
    match err {
        ureq::Error::StatusCode(code) => code,
        other => panic!("expected an HTTP status error, got: {other}"),
    }
}

/// Formerly SH-250's conjunct 5: a declared reverse proxy meant a loopback
/// connection might be a remote caller the proxy forwarded, so the exemption
/// was withdrawn and loopback required the token exactly like the tailnet
/// listener. SH-255 deleted the exemption itself, so a tokenless read is
/// refused on loopback regardless — this pins that the proxy declaration
/// still changes nothing about that outcome, positive or negative.
#[test]
fn a_declared_reverse_proxy_does_not_change_whether_a_read_needs_a_token() {
    let env = TestEnv::isolated();
    let fixture = daemon_with_proxy_allowlist(&env, Some(PROXY_HOST));
    let (port, slug) = (fixture.port, fixture.slug.as_str());

    let err = ureq::get(data_url(port, slug)).call().unwrap_err();
    assert_eq!(status_of(err), 401, "a tokenless read must be refused");

    // Positive control: the same read, credentialed, still works — so the 401
    // is the missing credential, not a daemon that cannot serve at all.
    let ok = ureq::get(data_url(port, slug))
        .header("X-Storyhook-Token", &token(&env))
        .call()
        .expect("the positive control failed: a credentialed read must succeed");
    assert_eq!(ok.status(), 200);
}

/// The paired control for the test above, differing in exactly one variable:
/// no proxy declared. Without it, that test would pass just as well against
/// a daemon that refused every tokenless read for some unrelated reason —
/// this is what proves the *variable itself* is not what is producing the
/// 401, now that there is no exemption left for it to withdraw.
#[test]
fn the_same_daemon_without_the_variable_also_needs_a_token_for_a_read() {
    let env = TestEnv::isolated();
    let fixture = daemon_with_proxy_allowlist(&env, None);

    let err = ureq::get(data_url(fixture.port, &fixture.slug))
        .call()
        .unwrap_err();
    assert_eq!(
        status_of(err),
        401,
        "a tokenless read must be refused whether or not a proxy is declared"
    );
}

/// What the variable has always been for, asserted for the first time: a
/// mutation arriving under the proxy's own hostname passes the `Host` half of
/// the mutation guard instead of being refused as a rebinding attempt.
///
/// Independent of the exemption — this request carries the token, so it would
/// behave identically if SH-250 had never happened.
#[test]
fn a_configured_proxy_host_is_trusted_for_a_mutation() {
    let env = TestEnv::isolated();
    let fixture = daemon_with_proxy_allowlist(&env, Some(PROXY_HOST));
    let (port, slug) = (fixture.port, fixture.slug.as_str());
    let token = token(&env);
    let move_url = format!("http://127.0.0.1:{port}/api/repos/{slug}/story/SH-1/move");

    let ok = ureq::post(&move_url)
        .header("X-Storyhook", "1")
        .header("Host", PROXY_HOST)
        .header("X-Storyhook-Token", &token)
        .content_type("application/json")
        .send(r#"{"state":"in-progress"}"#)
        .expect("a mutation under an allowlisted Host must be accepted");
    assert_eq!(ok.status(), 200);

    // And the allowlist admits what it names, rather than disabling the check:
    // a host that is not on it is still refused, with the same token.
    let err = ureq::post(&move_url)
        .header("X-Storyhook", "1")
        .header("Host", FOREIGN_HOST)
        .header("X-Storyhook-Token", &token)
        .content_type("application/json")
        .send(r#"{"state":"todo"}"#)
        .unwrap_err();
    assert_eq!(status_of(err), 403);
}

/// A host absent from the allowlist is refused even on a daemon that has one —
/// the allowlist is not a switch that turns the `Host` check off.
///
/// Paired with the test above so "403 for `evil.example`" cannot be explained
/// by the daemon refusing every proxied mutation.
#[test]
fn a_daemon_with_no_allowlist_refuses_the_proxys_host() {
    let env = TestEnv::isolated();
    let fixture = daemon_with_proxy_allowlist(&env, None);
    let (port, slug) = (fixture.port, fixture.slug.as_str());
    let token = token(&env);

    let err = ureq::post(format!(
        "http://127.0.0.1:{port}/api/repos/{slug}/story/SH-1/move"
    ))
    .header("X-Storyhook", "1")
    .header("Host", PROXY_HOST)
    .header("X-Storyhook-Token", &token)
    .content_type("application/json")
    .send(r#"{"state":"in-progress"}"#)
    .unwrap_err();
    assert_eq!(
        status_of(err),
        403,
        "without the variable, the proxy's hostname is just an untrusted Host"
    );
}
