//! The daemon's control surface: `/api/v1/*`.
//!
//! # Loopback only, and authenticated
//!
//! This is a full-privilege API — anything the CLI can do, it can do — so it is
//! answered on the loopback listener and nowhere else, and every request must
//! carry the per-daemon token from the mode-0600 portfile.
//!
//! Both halves are necessary. Loopback is not a trust boundary: every process on
//! the machine can reach it, including a browser tab running somebody else's
//! JavaScript, which is exactly the threat the dashboard's CSRF guard exists
//! for. The token is what a page cannot obtain — it is in a file the browser
//! cannot read — and refusing the whole surface on the tailnet listener is what
//! keeps a tailnet peer from reaching it at all.
//!
//! A request that arrives on the wrong interface is answered `404`, not `403`:
//! there is nothing there to be forbidden from.

use std::path::Path;

use crate::daemon::http1::{Header, Method};

use crate::api::http::{Reply, error_reply, header_value, json_reply, text_reply, to_json};
use crate::api::wire::{WireRequest, WireResponse};
use crate::daemon::lifecycle::{self, Entry, Hello, PROTOCOL};
use crate::env::Environment;
use crate::error::AppError;
use crate::invoke::{InvokeRequest, Invoker, StoreInvoker};
use crate::store::Store;

/// The header carrying the daemon's bearer token.
pub const TOKEN_HEADER: &str = "X-Storyhook-Token";

/// What a routed control request asked for.
pub enum Answer {
    /// Answered; send this.
    Reply(Reply),
    /// Answered, and the daemon should now shut down.
    Shutdown(Reply),
}

/// Everything the control surface needs to answer a request.
///
/// Bundled rather than passed as six parameters: the daemon builds one of these
/// once and it does not change, so threading the parts through every call would
/// be noise around the two things that do vary — the request and the interface
/// it arrived on.
pub struct Surface<'a, S: Store> {
    /// The store commands run against.
    pub store: &'a S,
    /// The environment they run under.
    pub env: &'a Environment,
    /// The bearer token this daemon requires.
    pub token: &'a str,
    /// What `/api/v1/hello` answers.
    pub hello: &'a Hello,
    /// The in-flight slot the dispatcher already opened for this request.
    /// Only `invoke` names it — `hello` and `shutdown` carry no work worth
    /// publishing, exactly as before this type existed.
    pub entry: &'a Entry<'a>,
}

/// Whether a request under `/api/v1/*` is admitted, decided entirely from its
/// head — no body access, so a caller can run this *before* reading one.
///
/// `None` means "keep going": either `segments` is not under `/api/v1/` at
/// all, or it is and the caller is authorized. `Some` carries the refusal —
/// 404 off-loopback (there is nothing here to be forbidden from) or 401 for a
/// missing or wrong token.
///
/// This is the gate a worker calls before ever reading a request body
/// (SH-172): a stalled, unauthenticated `POST /api/v1/invoke` is refused
/// without the daemon waiting on a single byte of it.
pub fn admission(
    segments: &[&str],
    headers: &[Header],
    token: &str,
    loopback: bool,
) -> Option<Reply> {
    let ["api", "v1", ..] = segments else {
        return None;
    };
    if !loopback {
        return Some(text_reply(404, "Not found"));
    }
    if !token_ok(headers, token) {
        return Some(text_reply(
            401,
            "storyhook daemon: missing or invalid token",
        ));
    }
    None
}

/// Routes a request under `/api/v1/`, or `None` if it is not one.
///
/// `loopback` says whether the request arrived on the loopback listener. On any
/// other interface this surface does not exist.
pub fn route<S: Store>(
    surface: &Surface<'_, S>,
    segments: &[&str],
    method: &Method,
    headers: &[Header],
    body: &str,
    loopback: bool,
) -> Option<Answer> {
    let ["api", "v1", rest @ ..] = segments else {
        return None;
    };
    if let Some(reply) = admission(segments, headers, surface.token, loopback) {
        return Some(Answer::Reply(reply));
    }

    Some(match (rest, method) {
        (["hello"], Method::Get) => Answer::Reply(match to_json(surface.hello) {
            Ok(body) => json_reply(200, body).no_cache(),
            Err(e) => error_reply(&e),
        }),
        (["shutdown"], Method::Post) => Answer::Shutdown(json_reply(
            200,
            serde_json::json!({"result": "ok", "protocol": PROTOCOL}).to_string(),
        )),
        (["invoke"], Method::Post) => {
            Answer::Reply(invoke(surface.store, surface.env, surface.entry, body))
        }
        (["hello"] | ["shutdown"] | ["invoke"], _) => {
            Answer::Reply(text_reply(405, "Method not allowed"))
        }
        _ => Answer::Reply(text_reply(404, "Not found")),
    })
}

/// `POST /api/v1/invoke` — run one command and answer with its unrendered
/// result.
///
/// **The answer is a `Response`, never text.** Rendering is the client's job,
/// because the client is the process the user is looking at and the one that
/// knows whether `--json` was asked for. A daemon that rendered would have to
/// be told, and then two processes would share a decision that belongs to one.
///
/// The whole thing runs inside `catch_unwind`. A panic in a CLI kills one
/// command; a panic in a daemon kills everything the machine is doing, and
/// `store.transact` is panic-safe by construction (the transaction rolls back on
/// drop), so there is nothing to be gained by letting one through.
///
/// # Why this publishes what it is doing
///
/// The daemon writes no bytes until this function returns, and it serves one
/// request at a time, so a waiting client can learn nothing from its socket and
/// nothing from a second request. It therefore names `entry` here, and the
/// dispatcher that opened it closes it once this returns — `Entry`'s `Drop`,
/// so a path that returns without reaching the end of this function (there is
/// none today, but a future one need not remember) cannot leave a stale
/// record behind the way a bare `clear_current` call could — which makes the
/// record change exactly when the daemon **finishes something** — the signal
/// a client's deadline resets on (SH-144).
///
/// **Here rather than in the accept loop**, because a record is only worth
/// reading if it can *name* the command, and the command does not exist until
/// the envelope above has parsed. A record written earlier could say no more
/// than `POST /api/v1/invoke`, which is the one thing the user already knows.
fn invoke<S: Store>(store: &S, env: &Environment, entry: &Entry<'_>, body: &str) -> Reply {
    let request: WireRequest = match serde_json::from_str(body) {
        Ok(request) => request,
        Err(e) => {
            return error_reply(&AppError::Usage(format!(
                "the daemon could not read the request envelope: {e}"
            )));
        }
    };

    if let Err(mismatch) = compatible(&request) {
        // Answered inside the envelope rather than as a bare HTTP error: the
        // client is storyhook, and it should reconstruct this the same way it
        // reconstructs any other failure.
        return answer(&request.request_id, Err(mismatch));
    }

    let command = crate::invoke::invocation_name(&request.invocation);
    if let Some(conflict) = concurrency_conflict(
        env,
        command,
        request.project.as_ref().map(|p| p.slug()),
        &request.cwd,
    ) {
        return answer(&request.request_id, Err(conflict));
    }

    entry.name(lifecycle::CurrentRequest {
        request_id: request.request_id.clone(),
        command: command.to_string(),
        project: request.project.as_ref().map(|p| p.slug().to_string()),
        pid: std::process::id(),
        started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        served_deadline_secs: lifecycle::served_deadline_for(&request.invocation, &request.cwd)
            .as_secs(),
        cwd: request.cwd.clone(),
    });

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        StoreInvoker::new(store, &request.cwd, env.clone())
            .hook_depth(request.hook_depth)
            .invoke(
                InvokeRequest::new(request.invocation.clone())
                    .no_hooks(request.no_hooks)
                    .stdin(request.stdin.clone())
                    .project(request.project.clone())
                    .github_token(request.github_token.clone())
                    .actor(request.actor.clone()),
            )
    }))
    .unwrap_or_else(|_| {
        Err(AppError::Storage(
            "the daemon panicked handling this command; the store is unchanged and \
             the daemon is still running. Please report this."
                .to_string(),
        ))
    });

    answer(&request.request_id, result)
}

/// Refuses a second concurrent `github-sync` for the same project, or a
/// second concurrent `migrate` of the same directory — the two integrity
/// holes concurrent dispatch opens (SH-173), closed by one scan of the
/// in-flight registry rather than by a lock.
///
/// `github-sync`'s own compare-and-swap is deliberately disabled
/// (`crate::service::github`'s own docstring: it reads a story, talks to
/// GitHub for as long as that takes, and writes back), which was safe only
/// because serial dispatch made two concurrent syncs of one project
/// impossible — a duplicated *network* side effect is not rollback-able by
/// any transaction. `migrate`'s own `refuse_in_linked_worktree`
/// (`crate::service::migrate`) is a read followed by a mint; two concurrent
/// migrations of one directory both pass it and mint two projects with the
/// same prefix and overlapping story numbers.
///
/// **`github-sync`'s check is scoped to the project this request explicitly
/// named** (`--project`/`$STORYHOOK_PROJECT`) — a known gap, not an
/// oversight: a project resolved from `cwd` by git-remote inference is not
/// knowable here without a store lookup this admission check runs ahead of,
/// and a check that sometimes does not fire is still strictly safer than the
/// unconditional none it replaces.
///
/// **`migrate` has no project to scope to at all** — it is what mints one —
/// so it is scoped to `cwd` instead: two `story migrate` of *different*
/// directories running at once are unrelated and must not block each other
/// (a global lock was tried first and measurably broke exactly this — two
/// unrelated fixtures in this project's own test suite, migrating different
/// scratch directories moments apart, refused each other).
fn concurrency_conflict(
    env: &Environment,
    command: &str,
    project: Option<&str>,
    cwd: &Path,
) -> Option<AppError> {
    let inflight = lifecycle::read_inflight(env);
    let conflict = match command {
        "github-sync" => project.and_then(|project| {
            inflight
                .iter()
                .find(|r| r.command == "github-sync" && r.project.as_deref() == Some(project))
        }),
        "migrate" => inflight
            .iter()
            .find(|r| r.command == "migrate" && r.cwd == cwd),
        _ => None,
    }?;
    let side_effect = if command == "github-sync" {
        "duplicate side effects on GitHub that no local transaction can undo"
    } else {
        "mint two projects for the same directory"
    };
    Some(AppError::Usage(format!(
        "a `{}` is already running (request {}, started {}); wait for it to finish before \
         starting another. Two concurrent `{}` runs can {side_effect}.",
        conflict.command, conflict.request_id, conflict.started_at, conflict.command,
    )))
}

/// Renders a result as the wire envelope.
///
/// Always HTTP 200: the *transport* succeeded, and the command's own success or
/// failure is inside the envelope where a client can reconstruct the variant.
/// Mapping an `AppError` onto a status code as the REST surface does would make
/// a client parse two error channels to learn one thing.
fn answer(request_id: &str, result: Result<crate::output::Response, AppError>) -> Reply {
    let envelope = WireResponse::new(request_id.to_string(), result);
    match to_json(&envelope) {
        Ok(body) => json_reply(200, body).no_cache(),
        Err(e) => error_reply(&e),
    }
}

/// Whether this daemon will serve `request`.
///
/// Strict on both counts, and deliberately so. The client is expected to have
/// checked the version before sending — it reads the portfile, which says what
/// build the daemon is — so a mismatch here means the daemon was replaced
/// mid-flight. Refusing keeps `Invocation` and `Response` free of every
/// compatibility obligation they would otherwise accumulate: there is exactly
/// one version of them alive at a time.
fn compatible(request: &WireRequest) -> Result<(), AppError> {
    if request.protocol != PROTOCOL {
        return Err(AppError::Usage(format!(
            "this storyhook daemon speaks protocol {PROTOCOL}; the client sent {}. \
             Run `story daemon stop` and try again.",
            request.protocol
        )));
    }
    if request.client_version != env!("CARGO_PKG_VERSION") {
        return Err(AppError::Usage(format!(
            "this storyhook daemon is version {}; the client is {}. \
             Run `story daemon stop` and try again.",
            env!("CARGO_PKG_VERSION"),
            request.client_version
        )));
    }
    Ok(())
}

/// Whether the request carries the daemon's token.
///
/// Compared in constant time. The attack it defends against — measuring how long
/// a mismatch takes, over loopback, to recover 128 bits a byte at a time — is
/// remote, but the defence costs one loop and the alternative is having to
/// argue that it is remote.
///
/// An empty `expected` always fails, even against an equally-empty offered
/// header: every real caller of this function passes a token
/// [`lifecycle::mint_token`] minted, which is never empty, so an empty
/// `expected` only ever means "unconfigured" — fail closed rather than let
/// that state authenticate anything.
pub(crate) fn token_ok(headers: &[Header], expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let Some(offered) = header_value(headers, TOKEN_HEADER) else {
        return false;
    };
    constant_time_eq(offered.as_bytes(), expected.as_bytes())
}

/// Byte equality that takes the same time whatever the inputs are.
///
/// `pub(crate)` for one other caller — `GithubToken`'s `PartialEq`
/// (`domain::secret`), which compares a credential and should not grow a second
/// copy of this loop to do it.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for (x, y) in a.iter().zip(b) {
        difference |= x ^ y;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Hello {
        Hello {
            version: "0.0.0".to_string(),
            protocol: PROTOCOL,
            pid: 42,
            started_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn header(name: &str, value: &str) -> Header {
        Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
    }

    /// A store to route against.
    ///
    /// These tests are about the guard in front of the surface rather than
    /// about the commands behind it, so an empty store is enough — every case
    /// here is answered before anything is read.
    fn store() -> (tempfile::TempDir, crate::store::SqliteStore) {
        let dir = tempfile::Builder::new()
            .prefix("storyhook-rpc-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory");
        let store =
            crate::store::SqliteStore::open(dir.path().join("store.db")).expect("opening a store");
        (dir, store)
    }

    /// Routes one request, with the token `"t"` expected.
    fn routed(
        segments: &[&str],
        method: &Method,
        headers: &[Header],
        loopback: bool,
    ) -> Option<Answer> {
        let (dir, store) = store();
        let env = Environment::at(dir.path());
        let hello = hello();
        let inflight = lifecycle::InFlight::new(env.clone());
        let entry = inflight.enter();
        let surface = Surface {
            store: &store,
            env: &env,
            token: "t",
            hello: &hello,
            entry: &entry,
        };
        route(&surface, segments, method, headers, "", loopback)
    }

    fn status(answer: Option<Answer>) -> u16 {
        match answer.expect("a control route") {
            Answer::Reply(reply) | Answer::Shutdown(reply) => reply.status,
        }
    }

    fn with_token() -> Vec<Header> {
        vec![header(TOKEN_HEADER, "t")]
    }

    #[test]
    fn a_path_outside_the_control_surface_is_not_routed_here() {
        assert!(routed(&["api", "repos"], &Method::Get, &with_token(), true).is_none());
        assert!(routed(&[], &Method::Get, &with_token(), true).is_none());
    }

    /// `admission` is what a worker calls before reading a body at all, so it
    /// must let a non-control path through without an opinion — refusing here
    /// would mean the dashboard's own routes gained a token requirement they
    /// were never meant to have.
    #[test]
    fn admission_has_no_opinion_on_a_path_outside_the_control_surface() {
        assert!(admission(&["api", "repos"], &[], "t", true).is_none());
        assert!(admission(&[], &[], "t", true).is_none());
    }

    #[test]
    fn admission_passes_an_authorized_control_request() {
        assert!(admission(&["api", "v1", "hello"], &with_token(), "t", true).is_none());
    }

    #[test]
    fn hello_answers_the_daemons_identity() {
        let control = routed(&["api", "v1", "hello"], &Method::Get, &with_token(), true);
        assert_eq!(status(control), 200);
    }

    /// The surface does not exist off loopback — not even to be forbidden from,
    /// which is why this is a 404 and not a 403. A tailnet peer must not be able
    /// to learn that there is a control API here at all.
    #[test]
    fn the_control_surface_does_not_exist_off_loopback() {
        for path in [
            ["api", "v1", "hello"].as_slice(),
            ["api", "v1", "shutdown"].as_slice(),
            ["api", "v1", "invoke"].as_slice(),
        ] {
            assert_eq!(
                status(routed(path, &Method::Post, &with_token(), false)),
                404,
                "{path:?}"
            );
        }
    }

    #[test]
    fn a_request_without_the_token_is_refused() {
        assert_eq!(
            status(routed(&["api", "v1", "hello"], &Method::Get, &[], true)),
            401
        );
    }

    #[test]
    fn a_request_with_the_wrong_token_is_refused() {
        let wrong = vec![header(TOKEN_HEADER, "not-the-token")];
        assert_eq!(
            status(routed(&["api", "v1", "hello"], &Method::Get, &wrong, true)),
            401
        );
    }

    /// Loopback is not a trust boundary: a page in a browser on this machine can
    /// reach it. The token is the thing that page cannot obtain, so the check
    /// must come before anything is served — including the identity endpoint,
    /// which would otherwise confirm to an attacker that storyhook is here.
    #[test]
    fn the_token_is_checked_before_anything_is_served() {
        assert_eq!(
            status(routed(
                &["api", "v1", "definitely-not-a-route"],
                &Method::Get,
                &[],
                true
            )),
            401,
            "an unauthenticated request must not be able to tell a real route \
             from a missing one"
        );
    }

    #[test]
    fn shutdown_is_a_post() {
        assert!(matches!(
            routed(
                &["api", "v1", "shutdown"],
                &Method::Post,
                &with_token(),
                true
            ),
            Some(Answer::Shutdown(_))
        ));
        assert_eq!(
            status(routed(
                &["api", "v1", "shutdown"],
                &Method::Get,
                &with_token(),
                true
            )),
            405
        );
    }

    #[test]
    fn invoke_is_a_post() {
        assert_eq!(
            status(routed(
                &["api", "v1", "invoke"],
                &Method::Get,
                &with_token(),
                true
            )),
            405
        );
    }

    /// A malformed envelope is a client bug, and the daemon says so rather than
    /// answering something a client would then try to reconstruct.
    #[test]
    fn an_unreadable_envelope_is_refused_with_a_reason() {
        let (dir, store) = store();
        let env = Environment::at(dir.path());
        let inflight = lifecycle::InFlight::new(env.clone());
        let entry = inflight.enter();
        let reply = invoke(&store, &env, &entry, "{ not json");
        assert_eq!(reply.status, 400);
    }

    /// A version mismatch is answered *inside* the envelope: the client is
    /// storyhook, and it reconstructs this the way it reconstructs any other
    /// failure.
    #[test]
    fn a_client_from_another_build_is_refused_inside_the_envelope() {
        let (dir, store) = store();
        let env = Environment::at(dir.path());
        let mut request =
            crate::api::wire::WireRequest::new(crate::cli::Invocation::Version, dir.path());
        request.client_version = "0.0.0-not-this-one".to_string();
        let inflight = lifecycle::InFlight::new(env.clone());
        let entry = inflight.enter();
        let reply = invoke(
            &store,
            &env,
            &entry,
            &serde_json::to_string(&request).unwrap(),
        );
        assert_eq!(
            reply.status, 200,
            "the transport succeeded; the command did not"
        );
    }

    #[test]
    fn a_protocol_mismatch_is_named_rather_than_interpreted() {
        let request = crate::api::wire::WireRequest {
            protocol: PROTOCOL + 1,
            ..crate::api::wire::WireRequest::new(crate::cli::Invocation::Version, "/tmp")
        };
        assert!(compatible(&request).is_err());
    }

    #[test]
    fn constant_time_equality_still_answers_correctly() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn token_ok_rejects_an_empty_expected_token_even_against_an_empty_offered_one() {
        // constant_time_eq("", "") alone would call this a match; token_ok
        // must not, because an empty `expected` only ever means the daemon
        // was never configured with a real one.
        let empty_offered = vec![header(TOKEN_HEADER, "")];
        assert!(!token_ok(&empty_offered, ""));
        assert!(!token_ok(&[], ""));
    }

    fn scratch_env() -> (tempfile::TempDir, Environment) {
        let dir = tempfile::Builder::new()
            .prefix("storyhook-rpc-conflict-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory");
        let env = Environment::at(dir.path());
        std::fs::create_dir_all(env.daemon_state_dir()).expect("the daemon state dir");
        (dir, env)
    }

    /// A record naming `command`, running for `project` at `cwd` — otherwise
    /// uninteresting.
    fn a_running(command: &str, project: Option<&str>, cwd: &str) -> lifecycle::CurrentRequest {
        lifecycle::CurrentRequest {
            request_id: "already-running".to_string(),
            command: command.to_string(),
            project: project.map(str::to_string),
            pid: 4711,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            served_deadline_secs: 120,
            cwd: std::path::PathBuf::from(cwd),
        }
    }

    #[test]
    fn a_second_github_sync_for_the_same_project_is_refused() {
        let (_dir, env) = scratch_env();
        lifecycle::publish_inflight(&env, &[a_running("github-sync", Some("PB"), "/a")]);

        assert!(
            concurrency_conflict(&env, "github-sync", Some("PB"), Path::new("/b")).is_some(),
            "a second sync for the same project must be refused, whatever cwd it runs from"
        );
    }

    #[test]
    fn a_github_sync_for_a_different_project_is_not_refused() {
        let (_dir, env) = scratch_env();
        lifecycle::publish_inflight(&env, &[a_running("github-sync", Some("PB"), "/a")]);

        assert!(
            concurrency_conflict(&env, "github-sync", Some("OTHER"), Path::new("/a")).is_none(),
            "a different project's sync must not be blocked by this one"
        );
    }

    /// The documented gap: a project this request did not explicitly name
    /// (relying on `cwd` inference instead) is not knowable here, so the
    /// check does not fire for it — still strictly safer than the
    /// unconditional none it replaces, never worse.
    #[test]
    fn a_github_sync_with_no_explicitly_named_project_is_never_checked() {
        let (_dir, env) = scratch_env();
        lifecycle::publish_inflight(&env, &[a_running("github-sync", Some("PB"), "/a")]);

        assert!(concurrency_conflict(&env, "github-sync", None, Path::new("/a")).is_none());
    }

    #[test]
    fn a_second_migrate_of_the_same_directory_is_refused() {
        let (_dir, env) = scratch_env();
        lifecycle::publish_inflight(&env, &[a_running("migrate", None, "/checkout")]);

        assert!(concurrency_conflict(&env, "migrate", None, Path::new("/checkout")).is_some());
    }

    /// The regression this check exists to prevent: a global migrate lock
    /// was tried first and broke two *unrelated* concurrent migrations —
    /// measured directly in this project's own test suite, where two
    /// fixtures migrating different scratch directories moments apart
    /// refused each other. `migrate` has no project to scope to, but it does
    /// have a directory, and that is the thing two concurrent instances can
    /// actually collide over.
    #[test]
    fn a_migrate_of_a_different_directory_is_not_refused() {
        let (_dir, env) = scratch_env();
        lifecycle::publish_inflight(&env, &[a_running("migrate", None, "/checkout-one")]);

        assert!(
            concurrency_conflict(&env, "migrate", None, Path::new("/checkout-two")).is_none(),
            "migrating an unrelated directory must never be blocked by this one"
        );
    }

    #[test]
    fn an_ordinary_command_is_never_refused_by_this_check() {
        let (_dir, env) = scratch_env();
        lifecycle::publish_inflight(&env, &[a_running("comment", Some("PB"), "/a")]);

        assert!(concurrency_conflict(&env, "comment", Some("PB"), Path::new("/a")).is_none());
    }

    #[test]
    fn nothing_in_flight_never_conflicts() {
        let (_dir, env) = scratch_env();
        assert!(concurrency_conflict(&env, "github-sync", Some("PB"), Path::new("/a")).is_none());
        assert!(concurrency_conflict(&env, "migrate", None, Path::new("/a")).is_none());
    }
}
