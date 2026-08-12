//! The dashboard's scoped, revocable session capability (SH-254).
//!
//! # What this is for
//!
//! SH-251 gave `story web open` a way to hand the browser a credential without
//! anybody typing one. It was transport, done correctly — and the credential it
//! handed over was the daemon's **master token**: the whole write surface, on
//! both listeners, spendable from any tailnet peer, alive for the daemon's
//! entire run, which for a launchd `RunAtLoad` agent is weeks. A dashboard tab
//! could delete every project on the machine, and could reach
//! `POST /api/v1/invoke`, which runs every verb the CLI has.
//!
//! What redemption issues now is a *capability*: an opaque value that names a
//! subset of the surface, can be taken back without restarting anything, and is
//! worth nothing off this machine.
//!
//! # What it can do, stated plainly
//!
//! It reaches the board's own work — creating, editing, moving, labelling,
//! relating and deleting stories, and editing a project's states. It does
//! **not** reach project creation at a caller-named filesystem path, project
//! deletion, agent dispatch, or the control surface under `/api/v1/*`.
//!
//! **It can cause `sh -c` to run.** Every mutation it authorizes fires the
//! project's configured event hooks, and a hook is a shell command. It cannot
//! choose or change what runs — hook commands come from the checkout, and no
//! route it reaches can edit them or repoint a checkout — but on a project with
//! hooks configured, a holder triggers the command that project's owner wrote.
//! The boundary this draws is not "cannot execute code". It is "cannot execute
//! code the project's owner did not already configure, cannot create or destroy
//! projects, cannot spawn an agent, cannot reach the CLI, and cannot be spent
//! from another machine". `docs/spec/dashboard-authorization.md` says the same
//! in the same words, and `tests/session_capability.rs` fires a real hook on a
//! capability-authorized move rather than leaving the claim to prose.
//!
//! # Why there is no expiry
//!
//! Every other credential in this daemon that could expire, does. This one must
//! not, and the reason is the client rather than the server: the dashboard's own
//! error path clears its credential on a 401 and opens the master-token modal.
//! A capability that quietly expired under a long-open tab would therefore end
//! with a user pasting **full authority** into that tab, as a matter of routine
//! — least privilege undone by its own error handler. So the lifetime is the
//! daemon's, bounded by [`MAX_LIVE`] with the oldest idle capability evicted,
//! never written to disk, and endable on purpose with `story web revoke`.
//!
//! A pleasant consequence: there is no clock to get wrong. The whole class of
//! defects around [`crate::env::Clock`], a wall clock that runs backwards, and
//! the monotonic floor that would be needed to defend against one, simply does
//! not arise. [`Instant`] appears here only to say how long a capability has sat
//! idle, which is a diagnostic and authorizes nothing.
//!
//! # Why it only works on this machine
//!
//! [`SessionRegistry::present`] takes a [`LocalRequest`], and the only way to
//! obtain one is [`crate::api::http::local_request`]. So a capability that
//! leaked — copied out of `sessionStorage`, read from a browser profile — is
//! not spendable from a tailnet peer, which is precisely what the master token
//! *is*. That is the largest single thing this story takes away.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::api::http::{
    LocalRequest, Reply, TrustedHosts, header_value, json_reply, local_request, text_reply,
};
use crate::api::routes::{Authority, authority, classify};
use crate::api::rpc::{constant_time_eq, token_ok};
use crate::daemon::http1::{Header, Method};

/// The header a capability travels in.
///
/// **Its own header, not `X-Storyhook-Token`.** Two credential kinds sharing one
/// header would have to be told apart by their shape, and a parser standing in
/// for a type is exactly the construction this story's own requirements reject.
/// It also keeps [`crate::api::admission`]'s documented reasoning about
/// `?token=` true: the token is still always a plain hex UUID, because nothing
/// else is ever offered as one.
pub const SESSION_HEADER: &str = "X-Storyhook-Session";

/// Where a daemon lists and revokes live capabilities.
///
/// Under `/api/v1/*`, so it inherits [`crate::api::rpc::admission`]'s
/// loopback-and-master-token gate: revoking is an administrative act, and a
/// capability must never be able to revoke its siblings.
pub const SESSIONS_PATH: &str = "/api/v1/sessions";

/// How many capabilities may be live at once.
///
/// The same number as [`crate::api::handoff`]'s coupon bound, so there is one
/// figure to reason about rather than two. Well above any real use — a
/// capability is minted by a human running `story web open` — and low enough
/// that a script redeeming in a loop cannot grow this daemon's memory.
const MAX_LIVE: usize = 8;

/// One live capability.
struct Session {
    capability: String,
    /// When it was last presented, for eviction order and for the diagnostic
    /// `story web status` prints. Authorizes nothing.
    last_used: Instant,
}

/// Every capability this daemon has issued and not lost.
///
/// Ordered least-recently-used first, so eviction is a `pop_front` and the
/// capability that goes is the one nobody is using. Poisoning is recovered from
/// rather than propagated, for [`crate::api::handoff::HandoffRegistry`]'s
/// reason: nothing under this lock can panic, and a daemon that refused every
/// later request because one unrelated thread unwound would be a worse failure
/// than the one it reported.
pub struct SessionRegistry {
    live: Mutex<VecDeque<Session>>,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRegistry {
    pub fn new() -> Self {
        SessionRegistry {
            live: Mutex::new(VecDeque::new()),
        }
    }

    /// Mints a capability for a caller already proven local.
    ///
    /// 32 hex characters from the operating system's CSPRNG — the same
    /// construction and the same 122 bits as
    /// [`crate::daemon::lifecycle::mint_token`], because a capability guessable
    /// for as long as a daemon runs is not a capability.
    ///
    /// Takes the [`LocalRequest`] rather than deriving one, so the witness the
    /// redemption gate already built is the witness this uses — there is one
    /// locality decision per request, not two.
    pub fn issue(&self, _witness: &LocalRequest, now: Instant) -> String {
        let capability = uuid::Uuid::new_v4().simple().to_string();
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        while live.len() >= MAX_LIVE {
            live.pop_front();
        }
        live.push_back(Session {
            capability: capability.clone(),
            last_used: now,
        });
        capability
    }

    /// Whether `offered` names a live capability, touching it if it does.
    ///
    /// Three properties, each load-bearing:
    ///
    /// * **It compares in constant time**, over raw bytes, never `==` on a
    ///   string — the same [`constant_time_eq`] every other credential in this
    ///   daemon is compared with.
    /// * **It takes a [`LocalRequest`].** A capability presented from anywhere
    ///   but this machine is not a capability, and deleting that check is a
    ///   compile error rather than a test somebody removes beside it.
    /// * **It returns `bool`**, so no caller can build an oracle out of it, and
    ///   `#[must_use]` means none can ignore it either.
    #[must_use]
    pub fn present(&self, _witness: &LocalRequest, offered: &str, now: Instant) -> bool {
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        let found = live
            .iter()
            .position(|s| constant_time_eq(s.capability.as_bytes(), offered.as_bytes()));
        match found {
            Some(index) => {
                // Least-recently-used order is what makes eviction pick the
                // capability nobody is using rather than the oldest tab that
                // happens to still be open.
                let mut session = live.remove(index).expect("the index was just found");
                session.last_used = now;
                live.push_back(session);
                true
            }
            None => false,
        }
    }

    /// Drops every live capability, and says how many there were.
    ///
    /// The kill switch the story required: an operator who suspects a tab, or
    /// who simply wants the machine quiet, does not have to restart a daemon
    /// that every CLI call, the TUI, every open change-feed connection and any
    /// in-flight dispatch is talking to.
    pub fn revoke_all(&self) -> usize {
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        let count = live.len();
        live.clear();
        count
    }

    /// How long each live capability has sat idle, least-recently-used first.
    ///
    /// **Never the capabilities themselves.** This is what `story web status`
    /// reports, and a status command that printed credentials would put them in
    /// exactly the scrollback this design spends its effort keeping them out of.
    pub fn idle_for(&self, now: Instant) -> Vec<Duration> {
        let live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        live.iter()
            .map(|s| now.saturating_duration_since(s.last_used))
            .collect()
    }
}

/// What a capability-bearing request is owed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionVerdict {
    /// A live capability, on a route it reaches.
    Admitted,
    /// A live capability, on a route it does not reach.
    ///
    /// **A 403, never a 401.** The dashboard clears its stored credential and
    /// opens the master-token modal on a 401 and on nothing else, so answering
    /// a scope refusal that way would destroy the capability and prompt the user
    /// to paste full authority back into the tab — least privilege undone by
    /// its own error path, which is the defect this whole story exists to avoid.
    OutOfScope,
    /// A capability the daemon does not know: revoked, evicted, or minted by a
    /// daemon that has since restarted.
    ///
    /// Distinguished from [`Self::Absent`] so the client can clear *only* the
    /// capability and say what happened, rather than escalating a tab that was
    /// working a moment ago.
    Unrecognized,
    /// No capability was offered, or the caller is not local — in which case
    /// nothing it offers could be one.
    Absent,
}

/// Whether a capability admits this request, and what to say if it does not.
///
/// Consulted only after the master token has been checked and has failed, so a
/// caller holding both credentials is never scope-refused: the token wins, and
/// the capability is never in its way.
///
/// # The order is the contract
///
/// Locality is asked **first**, before the offered value is compared against
/// anything. A caller that is not on this machine learns nothing about which
/// capabilities exist, not even by timing — it takes the same path as a caller
/// that offered nothing at all.
pub fn session_admission(
    segments: &[&str],
    method: &Method,
    headers: &[Header],
    trusted_hosts: &TrustedHosts,
    loopback: bool,
    now: Instant,
    sessions: &SessionRegistry,
) -> SessionVerdict {
    let Some(offered) = header_value(headers, SESSION_HEADER) else {
        return SessionVerdict::Absent;
    };
    let Some(witness) = local_request(headers, trusted_hosts, loopback) else {
        return SessionVerdict::Absent;
    };
    if !sessions.present(&witness, offered, now) {
        return SessionVerdict::Unrecognized;
    }
    match authority(&classify(segments, method)) {
        Authority::Public | Authority::Session => SessionVerdict::Admitted,
        Authority::MasterToken => SessionVerdict::OutOfScope,
    }
}

/// The reply a scope refusal gets.
///
/// Carries a machine-readable `code` because the dashboard has to tell this
/// apart from every other 403 it can meet — `mutation_guard_ok`'s, for one —
/// and because the message names the CLI command that *can* do what was asked,
/// so the answer to "the button did nothing" is in the answer itself.
pub fn scope_refusal(cli_hint: &str) -> Reply {
    json_reply(
        403,
        serde_json::json!({
            "result": "error",
            "code": "session_scope",
            "error": format!(
                "This dashboard session is not allowed to do that. It can edit stories and \
                 states; creating or deleting a project and dispatching an agent need the \
                 daemon's own token. Run `{cli_hint}` instead, or paste the token from \
                 `story daemon token`."
            ),
        })
        .to_string(),
    )
    .no_store()
}

/// The reply a capability the daemon does not recognize gets.
///
/// A 401, because the credential really is invalid — but with a `code` the
/// dashboard reads, so it clears the capability alone and says what to do,
/// instead of clearing everything and prompting for the master token.
pub fn unrecognized_refusal() -> Reply {
    json_reply(
        401,
        serde_json::json!({
            "result": "error",
            "code": "session_unknown",
            "error": "This dashboard session is no longer valid — it was revoked, or the \
                      daemon restarted. Run `story web open` to start a new one.",
        })
        .to_string(),
    )
    .no_store()
}

/// Intercepts `GET`/`DELETE /api/v1/sessions` before a `Job` is ever built.
///
/// Answered here, off the store thread, for [`crate::api::handoff::intercept`]'s
/// reason: it needs nothing from the store, so it must never queue behind the
/// dispatcher pool — least of all `story web revoke`, which somebody may well
/// be running *because* the daemon is busy doing something they want stopped.
///
/// The [`token_ok`] check is a **labelled belt-and-braces duplicate**, exactly
/// as [`crate::api::handoff`]'s arming route spells one:
/// [`crate::api::rpc::admission`] has already refused an off-loopback or
/// tokenless caller, and it runs first. Spelled again so this module's gate is
/// complete when read on its own, and so a future reordering of the worker
/// cannot silently turn revocation into an unauthenticated route. The
/// composition is an AND — the duplicate can only ever refuse more.
pub fn intercept(
    segments: &[&str],
    method: &Method,
    headers: &[Header],
    token: &str,
    now: Instant,
    sessions: &SessionRegistry,
) -> Option<Reply> {
    if segments != ["api", "v1", "sessions"] {
        return None;
    }
    if !token_ok(headers, token) {
        return Some(text_reply(403, "Forbidden"));
    }
    match method {
        Method::Get => Some(
            json_reply(
                200,
                serde_json::json!({
                    "sessions": sessions
                        .idle_for(now)
                        .iter()
                        .map(|idle| serde_json::json!({ "idle_seconds": idle.as_secs() }))
                        .collect::<Vec<_>>(),
                })
                .to_string(),
            )
            .no_store(),
        ),
        Method::Delete => Some(
            json_reply(
                200,
                serde_json::json!({ "revoked": sessions.revoke_all() }).to_string(),
            )
            .no_store(),
        ),
        _ => Some(text_reply(405, "Method not allowed")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::http::path_segments;

    const TOKEN: &str = "the-real-token";

    fn headers(pairs: &[(&str, &str)]) -> Vec<Header> {
        pairs
            .iter()
            .map(|(name, value)| Header::from_bytes(*name, *value).unwrap())
            .collect()
    }

    fn trusted_hosts() -> TrustedHosts {
        TrustedHosts::default()
    }

    /// A witness, for the registry tests that are not about the gate producing
    /// one.
    fn witness() -> LocalRequest {
        local_request(&headers(&[("Host", "127.0.0.1")]), &trusted_hosts(), true)
            .expect("a loopback request with a loopback Host is local")
    }

    // --- The registry ---

    #[test]
    fn a_capability_is_thirty_two_hex_characters() {
        let capability = SessionRegistry::new().issue(&witness(), Instant::now());
        assert_eq!(capability.len(), 32);
        assert!(
            capability
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
    }

    #[test]
    fn a_capability_keeps_working() {
        // The deliberate difference from a handoff coupon, which dies on first
        // use: a dashboard tab presents this on every write it makes for as
        // long as it is open.
        let registry = SessionRegistry::new();
        let now = Instant::now();
        let capability = registry.issue(&witness(), now);
        for minute in 0..60 {
            assert!(
                registry.present(
                    &witness(),
                    &capability,
                    now + Duration::from_secs(minute * 60)
                ),
                "a capability must not expire on its own"
            );
        }
    }

    #[test]
    fn an_unknown_capability_is_refused() {
        let registry = SessionRegistry::new();
        let now = Instant::now();
        let capability = registry.issue(&witness(), now);
        for attempt in 0..64 {
            assert!(!registry.present(&witness(), &format!("{attempt:032x}"), now));
        }
        assert!(
            registry.present(&witness(), &capability, now),
            "a spray of wrong values must not disturb a live capability"
        );
    }

    #[test]
    fn revoking_ends_every_capability_at_once() {
        let registry = SessionRegistry::new();
        let now = Instant::now();
        let capabilities: Vec<String> = (0..3).map(|_| registry.issue(&witness(), now)).collect();
        assert_eq!(registry.revoke_all(), 3);
        for capability in &capabilities {
            assert!(
                !registry.present(&witness(), capability, now),
                "a revoked capability must never work again"
            );
        }
        assert_eq!(
            registry.revoke_all(),
            0,
            "revoking twice must report nothing left rather than repeat itself"
        );
    }

    #[test]
    fn a_ninth_capability_evicts_the_least_recently_used() {
        // Not the oldest: the oldest may be the tab somebody has had open all
        // week and is using right now.
        let registry = SessionRegistry::new();
        let now = Instant::now();
        let capabilities: Vec<String> = (0..MAX_LIVE)
            .map(|_| registry.issue(&witness(), now))
            .collect();

        // Touch the first one, making the *second* the least recently used.
        assert!(registry.present(&witness(), &capabilities[0], now + Duration::from_secs(1)));
        registry.issue(&witness(), now + Duration::from_secs(2));

        assert!(
            registry.present(&witness(), &capabilities[0], now + Duration::from_secs(3)),
            "the capability in active use must survive"
        );
        assert!(
            !registry.present(&witness(), &capabilities[1], now + Duration::from_secs(3)),
            "the least recently used capability is the one evicted"
        );
    }

    #[test]
    fn idle_time_is_reported_and_capabilities_are_not() {
        let registry = SessionRegistry::new();
        let now = Instant::now();
        let capability = registry.issue(&witness(), now);
        let idle = registry.idle_for(now + Duration::from_secs(90));
        assert_eq!(idle.len(), 1);
        assert_eq!(idle[0].as_secs(), 90);
        assert!(
            registry.present(&witness(), &capability, now + Duration::from_secs(90)),
            "reporting idle time must not consume anything"
        );
    }

    // --- The gate ---

    fn issued(now: Instant) -> (SessionRegistry, String) {
        let registry = SessionRegistry::new();
        let capability = registry.issue(&witness(), now);
        (registry, capability)
    }

    fn session_headers(capability: &str) -> Vec<Header> {
        headers(&[
            ("X-Storyhook", "1"),
            (SESSION_HEADER, capability),
            ("Host", "127.0.0.1:3456"),
        ])
    }

    fn verdict(
        path: &str,
        method: &Method,
        headers: &[Header],
        loopback: bool,
        registry: &SessionRegistry,
        now: Instant,
    ) -> SessionVerdict {
        session_admission(
            &path_segments(path),
            method,
            headers,
            &trusted_hosts(),
            loopback,
            now,
            registry,
        )
    }

    #[test]
    fn a_capability_admits_the_board_s_own_work() {
        let now = Instant::now();
        let (registry, capability) = issued(now);
        for (path, method) in [
            ("/api/repos/p/story", Method::Post),
            ("/api/repos/p/story/SH-1", Method::Patch),
            ("/api/repos/p/story/SH-1/move", Method::Post),
            ("/api/repos/p/states/todo", Method::Patch),
            ("/api/repos/p/relate", Method::Post),
            ("/api/repos/p/data", Method::Get),
        ] {
            assert_eq!(
                verdict(
                    path,
                    &method,
                    &session_headers(&capability),
                    true,
                    &registry,
                    now
                ),
                SessionVerdict::Admitted,
                "{method} {path}"
            );
        }
    }

    #[test]
    fn a_capability_is_refused_the_three_routes_it_exists_to_take_away() {
        let now = Instant::now();
        let (registry, capability) = issued(now);
        for (path, method) in [
            ("/api/repos", Method::Post),
            ("/api/repos/p", Method::Delete),
            ("/api/repos/p/story/SH-1/dispatch", Method::Post),
            ("/api/repos/p/story/SH-1/dispatch/h4nd1e", Method::Get),
        ] {
            assert_eq!(
                verdict(
                    path,
                    &method,
                    &session_headers(&capability),
                    true,
                    &registry,
                    now
                ),
                SessionVerdict::OutOfScope,
                "{method} {path}"
            );
        }
    }

    #[test]
    fn a_capability_is_worth_nothing_off_this_machine() {
        // The largest single thing this story takes away: the master token is
        // spendable from any tailnet peer, and this is not.
        let now = Instant::now();
        let (registry, capability) = issued(now);
        assert_eq!(
            verdict(
                "/api/repos/p/story",
                &Method::Post,
                &session_headers(&capability),
                false,
                &registry,
                now
            ),
            SessionVerdict::Absent,
            "a capability presented on the tailnet listener must buy nothing, and must not \
             even report itself as unrecognized"
        );

        let mut rebound = session_headers(&capability);
        rebound.retain(|h| !h.field.equiv("Host"));
        rebound.push(Header::from_bytes("Host", "evil.example:3456").unwrap());
        assert_eq!(
            verdict(
                "/api/repos/p/story",
                &Method::Post,
                &rebound,
                true,
                &registry,
                now
            ),
            SessionVerdict::Absent,
            "a DNS-rebound page is same-origin with 127.0.0.1 and must still be refused"
        );

        let mut forwarded = session_headers(&capability);
        forwarded.push(Header::from_bytes("X-Forwarded-For", "203.0.113.1").unwrap());
        assert_eq!(
            verdict(
                "/api/repos/p/story",
                &Method::Post,
                &forwarded,
                true,
                &registry,
                now
            ),
            SessionVerdict::Absent,
            "a request that came through a proxy is not from this machine"
        );
    }

    #[test]
    fn an_unrecognized_capability_is_told_apart_from_no_capability() {
        // The distinction the client needs to avoid escalating a tab that was
        // working a moment ago into the master-token modal.
        let now = Instant::now();
        let (registry, capability) = issued(now);
        registry.revoke_all();
        assert_eq!(
            verdict(
                "/api/repos/p/story",
                &Method::Post,
                &session_headers(&capability),
                true,
                &registry,
                now
            ),
            SessionVerdict::Unrecognized
        );
        assert_eq!(
            verdict(
                "/api/repos/p/story",
                &Method::Post,
                &headers(&[("X-Storyhook", "1"), ("Host", "127.0.0.1:3456")]),
                true,
                &registry,
                now
            ),
            SessionVerdict::Absent
        );
    }

    #[test]
    fn a_capability_cannot_reach_a_route_this_daemon_does_not_have() {
        let now = Instant::now();
        let (registry, capability) = issued(now);
        for (path, method) in [
            ("/api/nope", Method::Get),
            ("/api/repos/p/nope", Method::Post),
            ("/api/repos/p/story/SH-1/no-such-action", Method::Post),
            ("/api/v1/invoke", Method::Post),
        ] {
            assert_eq!(
                verdict(
                    path,
                    &method,
                    &session_headers(&capability),
                    true,
                    &registry,
                    now
                ),
                SessionVerdict::OutOfScope,
                "{method} {path}"
            );
        }
    }

    // --- The control route ---

    #[test]
    fn listing_and_revoking_need_the_token() {
        let registry = SessionRegistry::new();
        let now = Instant::now();
        let capability = registry.issue(&witness(), now);
        for method in [Method::Get, Method::Delete] {
            let reply = intercept(
                &path_segments(SESSIONS_PATH),
                &method,
                &[],
                TOKEN,
                now,
                &registry,
            )
            .expect("the sessions path is this module's");
            assert_eq!(reply, text_reply(403, "Forbidden"), "{method}");
        }
        assert!(
            registry.present(&witness(), &capability, now),
            "a refused revocation must not have revoked anything"
        );
    }

    #[test]
    fn listing_reports_a_count_and_never_a_capability() {
        let registry = SessionRegistry::new();
        let now = Instant::now();
        let capability = registry.issue(&witness(), now);
        let reply = intercept(
            &path_segments(SESSIONS_PATH),
            &Method::Get,
            &headers(&[("X-Storyhook-Token", TOKEN)]),
            TOKEN,
            now,
            &registry,
        )
        .expect("the sessions path is this module's");
        assert_eq!(reply.status, 200);
        assert!(
            !format!("{reply:?}").contains(&capability),
            "the listing route served a live capability"
        );
    }

    #[test]
    fn revoking_over_the_route_ends_every_capability() {
        let registry = SessionRegistry::new();
        let now = Instant::now();
        let capability = registry.issue(&witness(), now);
        let reply = intercept(
            &path_segments(SESSIONS_PATH),
            &Method::Delete,
            &headers(&[("X-Storyhook-Token", TOKEN)]),
            TOKEN,
            now,
            &registry,
        )
        .expect("the sessions path is this module's");
        assert_eq!(reply.status, 200);
        assert!(!registry.present(&witness(), &capability, now));
    }

    #[test]
    fn nothing_else_is_this_modules() {
        let registry = SessionRegistry::new();
        for path in [
            "/",
            "/api/v1/handoff",
            "/api/sessions",
            "/api/v1/sessions/1",
        ] {
            assert!(
                intercept(
                    &path_segments(path),
                    &Method::Delete,
                    &headers(&[("X-Storyhook-Token", TOKEN)]),
                    TOKEN,
                    Instant::now(),
                    &registry,
                )
                .is_none(),
                "{path} must fall through to ordinary routing"
            );
        }
    }

    #[test]
    fn the_path_constant_is_what_the_router_matches() {
        assert_eq!(path_segments(SESSIONS_PATH), ["api", "v1", "sessions"]);
    }
}
