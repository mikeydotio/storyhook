//! The token gate every `/api/**` route answers behind — reads and writes
//! alike, on both listeners including loopback (SH-187).
//!
//! # What this closes
//!
//! Before this module existed, the dashboard's only defense against a
//! mutation was [`crate::api::http::mutation_guard_ok`]: an `X-Storyhook`
//! header plus a trusted `Host`. Both defeat a *browser* being tricked into
//! sending a request on a victim's behalf (CORS preflight; DNS rebinding).
//! Neither is a credential — anything that can set two headers directly,
//! such as `curl` from any peer the tailnet lets reach the dashboard's bound
//! IP, passed both with nothing to prove who it is. The dashboard binds the
//! machine's Tailscale IP as well as loopback, so that was not "this
//! machine's own browser" the way an unauthenticated localhost-only surface
//! would be — see `docs/spec/dashboard-authorization.md` for the full
//! review and the decision it records.
//!
//! [`crate::api::rpc`] already had the answer for `/api/v1/*`: loopback-only
//! *and* token-authenticated. [`crate::api::dispatch`] generalized the
//! token half to a second, tailnet-reachable endpoint (SH-50). This module
//! generalizes it once more, to everything else `/api/**` serves — the 25
//! mutation routes `mutation_guard_ok` alone used to gate, and the read
//! routes (including `GET /api/repos/{id}/data`, which returns every story
//! in a project) that had no gate at all.
//!
//! # Order, and why
//!
//! For a mutating method, [`mutation_guard_ok`] is checked first, then the
//! token — the same order [`crate::api::dispatch::intercept`] already
//! established for its own endpoint. The guard is a cheap, publicly
//! documented requirement (the README's Security section names both
//! headers); checking it first means a naive drive-by browser request —
//! which cannot set either header to begin with — never reaches the
//! constant-time token comparison. A caller sophisticated enough to set
//! both headers correctly still needs the actual secret. A read has no such
//! guard (none ever gated reads — the confusion attacks it defeats need a
//! mutating request to matter), so a read is admitted on the token alone.
//!
//! `GET /api/events` is the one route that also accepts the token as a
//! `?token=` query parameter: `EventSource` cannot set headers, so without
//! this the live-update feed could never authenticate at all. No other
//! route accepts it that way — a token in a URL lands in more places (proxy
//! logs, browser history) than one in a header, so the exception stays as
//! narrow as the one caller that has no alternative.
//!
//! # The loopback read exemption (SH-250)
//!
//! A **read** arriving on the loopback listener is admitted with no token, so
//! a browser tab opened at `127.0.0.1` renders without anyone fetching a
//! credential first. Six conjuncts have to hold — see [`token_exempt`] — and
//! writes are not among them: every mutation still needs the token on every
//! listener, and so does `.../dispatch`.
//!
//! **The guard did not, and does not, protect a read.** [`mutation_guard_ok`]
//! runs only for a mutating method, so before this exemption the token was the
//! *only* thing checking `Host` on `GET /api/repos/{id}/data` (every story in
//! a project) and `GET /api/events` (every live change). Removing it from a
//! read therefore had to *add* a `Host` check rather than lean on an existing
//! one: a DNS-rebound page is same-origin with `http://127.0.0.1:PORT` and can
//! read what it gets back. That is conjunct 3, and it is why this module grew
//! a `Host` requirement on reads that it never had.
//!
//! # The scoped dashboard capability (SH-254)
//!
//! A fourth thing can now admit a request, and it is asked **last**: a
//! [`crate::api::session`] capability, which `POST /handoff` issues to a
//! browser instead of the master token it used to hand over. Everything above
//! it in [`admission`] is unchanged — the guard, [`token_exempt`] and
//! [`token_ok`] each decide exactly what they decided before, and a caller
//! holding the master token returns before the capability is consulted at all.
//!
//! What a capability may reach is not decided here. It is decided by
//! [`crate::api::routes::authority`], one answer per named route, in a module
//! that imports nothing but a `Method` — so a route added to the dashboard's
//! API cannot inherit the grant, because it does not compile until somebody has
//! classified it. The alternative, which this module already documents and
//! forbids a few lines below, is a path-shape guess: matching by arity hands
//! every future route under a project whatever its neighbours had, on the day
//! it is written, with no test going red.
//!
//! Two refusals rather than one, and that difference is the whole of the
//! feature surviving contact with its own error path. A capability on a route
//! it does not reach is a **403** carrying `session_scope`; a capability the
//! daemon does not recognize is a **401** carrying `session_unknown`. The
//! dashboard clears its stored credential and opens the master-token modal on
//! an unmarked 401 and on nothing else, so answering either of these that way
//! would end with a user pasting full authority back into the tab — least
//! privilege undone by its own error handler.

use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::api::http::{
    Reply, TrustedHosts, cookie_value, header_value, local_request, mutation_guard_ok, text_reply,
};
use crate::api::rest::mutating;
use crate::api::routes::{ProjectRoute, Route, classify};
use crate::api::rpc::{constant_time_eq, token_ok};
use crate::api::session::{
    SessionRegistry, SessionVerdict, scope_refusal, session_admission, unrecognized_refusal,
};
use crate::api::tokens::TokenRegistry;
use crate::daemon::http1::{Header, Method};

/// Whether a request under `/api/**` — other than `/api/v1/*`, which
/// [`crate::api::rpc::admission`] already owns — is admitted.
///
/// `None` means "keep going": either `segments` is not under `/api` at all
/// (the SPA shell itself, served without a token so it can bootstrap the
/// browser's own token prompt), or it is `/api/v1/*`, or the caller is
/// authorized. `Some` carries the refusal: 403 for a mutation that fails
/// the CSRF/DNS-rebinding guard, 401 for a missing or wrong token.
///
/// Decided entirely from the head — no body access, so a caller can run
/// this before reading one, the same reasoning [`crate::api::rpc::admission`]
/// documents for the same purpose (SH-172).
///
/// `tokens`/`cookie_name`/`wall_now` are SH-255's addition: a named token —
/// offered as `X-Storyhook-Token` (explicit) or as the cookie named
/// `cookie_name` (ambient, and so held to [`named_token_ok`]'s extra
/// same-origin check on a read) — is now also sufficient, checked alongside
/// the master token below. Additive only, for now: the exemption, the master
/// token and the capability all still decide exactly what they decided
/// before.
#[allow(clippy::too_many_arguments)]
pub fn admission(
    segments: &[&str],
    method: &Method,
    query: Option<&str>,
    headers: &[Header],
    trusted_hosts: &TrustedHosts,
    token: &str,
    loopback: bool,
    now: Instant,
    sessions: &SessionRegistry,
    tokens: &TokenRegistry,
    cookie_name: &str,
    wall_now: DateTime<Utc>,
) -> Option<Reply> {
    match segments {
        ["api", "v1", ..] => return None,
        ["api", ..] => {}
        _ => return None,
    }
    if mutating(method) && !mutation_guard_ok(headers, trusted_hosts) {
        return Some(text_reply(403, "Forbidden"));
    }
    if token_exempt(segments, method, headers, trusted_hosts, loopback) {
        return None;
    }
    let is_events_stream = segments == ["api", "events"];
    let authorized = token_ok(headers, token)
        || (is_events_stream && query_token_ok(query, token))
        || named_token_ok(headers, method, cookie_name, tokens, wall_now, now);
    if authorized {
        return None;
    }
    // The scoped dashboard capability (SH-254), asked **last**. Everything
    // above this line is what it was, byte for byte: the guard, the exemption
    // and the token each still decide exactly what they decided before, and a
    // caller holding the master token never reaches here — so a tab that has
    // both credentials is never scope-refused for holding the weaker one.
    match session_admission(
        segments,
        method,
        headers,
        trusted_hosts,
        loopback,
        now,
        sessions,
    ) {
        SessionVerdict::Admitted => None,
        SessionVerdict::OutOfScope => Some(scope_refusal(cli_equivalent(segments, method))),
        SessionVerdict::Unrecognized => Some(unrecognized_refusal()),
        // Byte-identical to the refusal this route has always given, because
        // nothing about a request that offers no capability has changed.
        SessionVerdict::Absent => Some(text_reply(
            401,
            "storyhook daemon: missing or invalid token",
        )),
    }
}

/// The `story` command that does what a scope-refused request was asking for.
///
/// Carried in the refusal so a user who meets the boundary is told the way
/// through it rather than left with a button that did nothing. Only the three
/// routes a capability is actually refused *in the course of using the
/// dashboard* need an answer; everything else is a path the dashboard does not
/// call, and a generic pointer is the honest reply there.
fn cli_equivalent(segments: &[&str], method: &Method) -> &'static str {
    match classify(segments, method) {
        Route::ReposCreate => "story project new",
        Route::RepoDelete { .. } => "story project delete",
        Route::Project {
            route: ProjectRoute::Dispatch { .. } | ProjectRoute::DispatchPoll,
            ..
        } => "story dispatch",
        _ => "story --help",
    }
}

/// Whether this request may be admitted with **no token at all** (SH-250).
///
/// # The governing rule
///
/// **An attacker-supplied header may only withhold trust, never confer it.**
///
/// The single affirmative grant is `loopback`, stamped on the listener at bind
/// time — the one input no caller can supply. Every other conjunct can only
/// turn the exemption *off*. Keep it that way: a future edit that lets a
/// request-borne value *grant* the exemption breaks the rule however
/// reasonable it looks on its own, because every such value is forgeable by
/// exactly the caller the exemption must not reach.
///
/// # The conjuncts, and what each one is for
///
/// Four of the six are locality, and they live in
/// [`crate::api::http::local_request`] rather than here (SH-251): `loopback`,
/// the loopback-literal `Host`, the absence of
/// [`crate::api::http::FORWARDING_HEADERS`], and the
/// absence of a configured reverse-proxy allowlist. That function's own doc
/// comment explains each, and holding its [`LocalRequest`](crate::api::http::LocalRequest)
/// is the only way anything in this daemon may conclude "the caller is local".
/// They were spelled inline here until a second gate needed the same answer,
/// and a governing principle living in two files is how it rots.
///
/// The two that are this module's own:
///
/// * **`Get | Head`, affirmatively.** Not `!mutating(method)`: `Put`,
///   `Options` and `Other(_)` all parse ([`Method`]) and none of them is
///   [`mutating`], so the negative spelling would hand a future `PUT` route
///   the exemption the day it was added, silently.
/// * **Not `.../dispatch`.** That route spawns processes.
///   [`crate::api::dispatch::intercept`] refuses a tokenless dispatch on its
///   own regardless, since it is never told about the exemption and both
///   gates fail closed — this conjunct stops *this* gate from admitting
///   something the next one will refuse, which would be merely confusing
///   today and load-bearing the day somebody acts on the standing note that
///   `intercept`'s duplicated checks look redundant.
///
/// `/api/v1/*` needs no conjunct: [`admission`] returns before reaching here,
/// leaving that surface entirely to [`crate::api::rpc::admission`].
///
/// # What is deliberately *not* relaxed
///
/// Every mutation, on every listener. The write surface registers projects at
/// caller-named filesystem paths, deletes projects, and reaches `sh -c`
/// through configured event hooks — so where this rule can be evaded (see the
/// residual in `docs/spec/dashboard-authorization.md`) a read exemption costs
/// disclosure where a write exemption would cost process execution.
///
/// (Spelled `sh -c` rather than naming the constructor: `tests/spawn_inventory.rs`
/// scans this file's text for process spawns, and cannot tell a doc comment
/// from a call site. Prose about spawning must not read as spawning.)
fn token_exempt(
    segments: &[&str],
    method: &Method,
    headers: &[Header],
    trusted_hosts: &TrustedHosts,
    loopback: bool,
) -> bool {
    matches!(method, Method::Get | Method::Head)
        && !crate::api::dispatch::is_dispatch_path(segments)
        && local_request(headers, trusted_hosts, loopback).is_some()
}

/// Whether `query`'s `token=` value matches `token`, constant-time.
///
/// No percent-decoding: the token is always a plain hex UUID
/// ([`crate::daemon::lifecycle::mint_token`]), so a real token never
/// contains a byte `encodeURIComponent` (`web_dashboard.html`'s
/// `connectEvents`) would have encoded, the same reasoning
/// [`crate::api::dispatch`]'s own `parse_auto` relies on for its `auto=`
/// value.
fn query_token_ok(query: Option<&str>, token: &str) -> bool {
    let Some(offered) = query.and_then(|query| {
        query
            .split('&')
            .find_map(|pair| pair.split_once('=').filter(|(key, _)| *key == "token"))
            .map(|(_, value)| value)
    }) else {
        return false;
    };
    constant_time_eq(offered.as_bytes(), token.as_bytes())
}

/// Whether a named token (SH-255) authorizes this request — the master
/// token's replacement, checked as an additional way to be admitted.
///
/// Two ways a token can arrive, held to different standards:
///
/// * **`X-Storyhook-Token`** — explicit. A page cannot forge a header on a
///   plain navigation, and a cross-origin `fetch` setting one triggers a
///   preflight this daemon never answers, so an offered header is already as
///   trustworthy as the master token check just above. No further test.
/// * **The cookie named `cookie_name`** — ambient. `SameSite=Strict` alone
///   does not distinguish this daemon's own tab from every other tailnet
///   peer's dashboard (cookies ignore port, and every peer under one
///   tailnet's registrable domain is same-site with every other), so a
///   **read** additionally requires `Sec-Fetch-Site: same-origin` — a
///   forbidden header name no page can forge or suppress, sent by every
///   browser request including `EventSource`. A **mutation** needs no extra
///   check here: it has already passed [`mutation_guard_ok`] above, in
///   [`admission`], which is what makes the cookie sufficient on its own.
///
/// `pub(crate)`: [`crate::api::dispatch::intercept`] is a second, narrower
/// gate on the exact same route family (its own module doc calls it a
/// "harmless, deliberate redundancy" with this one), and it must recognize
/// every credential this gate does — a request `admission` already admitted
/// must never be refused a second time by dispatch's own copy of the check.
pub(crate) fn named_token_ok(
    headers: &[Header],
    method: &Method,
    cookie_name: &str,
    tokens: &TokenRegistry,
    wall_now: DateTime<Utc>,
    mono_now: Instant,
) -> bool {
    if let Some(offered) = header_value(headers, crate::api::rpc::TOKEN_HEADER)
        && tokens.validate(offered, wall_now, mono_now).is_some()
    {
        return true;
    }
    let Some(offered) = cookie_value(headers, cookie_name) else {
        return false;
    };
    if tokens.validate(offered, wall_now, mono_now).is_none() {
        return false;
    }
    mutating(method) || same_origin_fetch(headers)
}

/// Whether `headers` carries the browser-supplied, unforgeable
/// `Sec-Fetch-Site: same-origin`. See [`named_token_ok`] for why this is
/// what a cookie-authenticated read is held to.
fn same_origin_fetch(headers: &[Header]) -> bool {
    header_value(headers, "Sec-Fetch-Site") == Some("same-origin")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Named here rather than in the module's own imports: since SH-251 the
    // constant lives in `http.rs` beside the locality predicate that reads
    // it, and this module's only remaining use of it is conjunct 4's test —
    // which stays byte-identical across that extraction, which is what makes
    // the extraction's behaviour-preservation checkable by reading the diff.
    use crate::api::http::FORWARDING_HEADERS;

    const TOKEN: &str = "the-real-token";

    /// The gate, asked with no dashboard capability anywhere in play.
    ///
    /// **Shadows [`super::admission`] on purpose.** SH-254 gave the real gate
    /// two more parameters, and every test below this line is about the three
    /// decisions that come *before* a capability is consulted — the CSRF guard,
    /// SH-250's tokenless read exemption, and the token. Routing them through a
    /// helper that supplies an empty registry keeps all of them byte-identical
    /// across that change, and an unmodified diff is the only cheap proof that
    /// a story which promised not to weaken those three did not weaken them.
    ///
    /// The capability's own truth table lives in `crate::api::session`, where
    /// it can be read beside the thing it describes.
    #[allow(clippy::too_many_arguments)]
    fn admission(
        segments: &[&str],
        method: &Method,
        query: Option<&str>,
        headers: &[Header],
        trusted_hosts: &TrustedHosts,
        token: &str,
        loopback: bool,
    ) -> Option<Reply> {
        let now = Instant::now();
        super::admission(
            segments,
            method,
            query,
            headers,
            trusted_hosts,
            token,
            loopback,
            now,
            &SessionRegistry::new(),
            &TokenRegistry::new(Utc::now(), now),
            "storyhook_test",
            Utc::now(),
        )
    }

    fn headers(pairs: &[(&str, &str)]) -> Vec<Header> {
        pairs
            .iter()
            .map(|(name, value)| Header::from_bytes(*name, *value).unwrap())
            .collect()
    }

    fn guard_headers() -> Vec<Header> {
        headers(&[("X-Storyhook", "1"), ("Host", "127.0.0.1")])
    }

    fn authed_headers() -> Vec<Header> {
        headers(&[
            ("X-Storyhook", "1"),
            ("Host", "127.0.0.1"),
            ("X-Storyhook-Token", TOKEN),
        ])
    }

    fn trusted_hosts() -> TrustedHosts {
        // Loopback is always trusted, and no reverse proxy is configured —
        // nothing extra is needed for these cases.
        TrustedHosts::default()
    }

    #[test]
    fn a_path_outside_api_is_not_admission_controlled() {
        assert!(
            admission(
                &["nonexistent"],
                &Method::Get,
                None,
                &[],
                &trusted_hosts(),
                TOKEN,
                false,
            )
            .is_none()
        );
        assert!(admission(&[], &Method::Get, None, &[], &trusted_hosts(), TOKEN, false).is_none());
    }

    #[test]
    fn api_v1_is_left_to_rpc_admission() {
        assert!(
            admission(
                &["api", "v1", "hello"],
                &Method::Get,
                None,
                &[],
                &trusted_hosts(),
                TOKEN,
                false,
            )
            .is_none(),
            "an unauthenticated /api/v1 request must pass through untouched, \
             for rpc::admission to refuse on its own terms"
        );
    }

    #[test]
    fn a_read_without_a_token_is_401() {
        let reply = admission(
            &["api", "repos"],
            &Method::Get,
            None,
            &[],
            &trusted_hosts(),
            TOKEN,
            false,
        )
        .expect("a tokenless read must be refused");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn a_read_with_the_token_is_admitted() {
        assert!(
            admission(
                &["api", "repos"],
                &Method::Get,
                None,
                &authed_headers(),
                &trusted_hosts(),
                TOKEN,
                false,
            )
            .is_none()
        );
    }

    #[test]
    fn a_wrong_token_is_401() {
        let mut bad = authed_headers();
        bad.retain(|h| !h.field.equiv("X-Storyhook-Token"));
        bad.push(Header::from_bytes("X-Storyhook-Token", "not-the-token").unwrap());
        let reply = admission(
            &["api", "repos"],
            &Method::Get,
            None,
            &bad,
            &trusted_hosts(),
            TOKEN,
            false,
        )
        .expect("a wrong token must be refused");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn a_mutation_without_the_csrf_guard_is_403_before_the_token_is_consulted() {
        // No X-Storyhook/Host at all, and also no token -- if the token were
        // checked first this would be 401, proving the guard ran first.
        let reply = admission(
            &["api", "repos", "x", "story"],
            &Method::Post,
            None,
            &[],
            &trusted_hosts(),
            TOKEN,
            false,
        )
        .expect("an unguarded mutation must be refused");
        assert_eq!(reply.status, 403);
    }

    #[test]
    fn a_mutation_with_the_guard_but_no_token_is_401() {
        let reply = admission(
            &["api", "repos", "x", "story"],
            &Method::Post,
            None,
            &guard_headers(),
            &trusted_hosts(),
            TOKEN,
            false,
        )
        .expect("a guarded-but-tokenless mutation must be refused");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn a_mutation_with_the_guard_and_the_token_is_admitted() {
        assert!(
            admission(
                &["api", "repos", "x", "story"],
                &Method::Post,
                None,
                &authed_headers(),
                &trusted_hosts(),
                TOKEN,
                false,
            )
            .is_none()
        );
    }

    #[test]
    fn a_spoofed_host_on_a_mutation_is_403_even_with_a_valid_token() {
        let headers = headers(&[
            ("X-Storyhook", "1"),
            ("Host", "evil.example"),
            ("X-Storyhook-Token", TOKEN),
        ]);
        let reply = admission(
            &["api", "repos", "x", "story"],
            &Method::Post,
            None,
            &headers,
            &trusted_hosts(),
            TOKEN,
            false,
        )
        .expect("a spoofed Host must be refused regardless of the token");
        assert_eq!(reply.status, 403);
    }

    #[test]
    fn the_events_stream_accepts_the_token_as_a_query_parameter() {
        assert!(
            admission(
                &["api", "events"],
                &Method::Get,
                Some("token=the-real-token"),
                &[],
                &trusted_hosts(),
                TOKEN,
                false,
            )
            .is_none()
        );
    }

    #[test]
    fn a_wrong_query_token_on_the_events_stream_is_401() {
        let reply = admission(
            &["api", "events"],
            &Method::Get,
            Some("token=nope"),
            &[],
            &trusted_hosts(),
            TOKEN,
            false,
        )
        .expect("a wrong query token must be refused");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn a_query_token_is_not_accepted_on_any_other_route() {
        let reply = admission(
            &["api", "repos"],
            &Method::Get,
            Some("token=the-real-token"),
            &[],
            &trusted_hosts(),
            TOKEN,
            false,
        )
        .expect("a query token must not authenticate a route other than /api/events");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn an_empty_configured_token_admits_nothing() {
        // Fail closed: an empty expected token (the test seam's old default,
        // pre-SH-187 -- `bind_and_serve` now mints a real one instead, see
        // its own doc comment) must never be satisfiable, not even by a
        // client that offers an equally-empty header. `constant_time_eq`
        // alone would call that a match; it never runs here because there
        // is no scenario an empty configured token should ever admit.
        let empty_offered_token = headers(&[("X-Storyhook-Token", "")]);
        let reply = admission(
            &["api", "repos"],
            &Method::Get,
            None,
            &empty_offered_token,
            &trusted_hosts(),
            "",
            false,
        )
        .expect("an empty configured token must admit nothing, even to an empty offered header");
        assert_eq!(reply.status, 401);
    }

    // --- The loopback read exemption (SH-250) ---
    //
    // A truth table over `token_exempt`'s six conjuncts: one admitted case,
    // then one test per conjunct falsifying that conjunct alone and asserting
    // 401. Every conjunct is load-bearing, so every one of these fails if its
    // conjunct is deleted as "redundant".

    /// The headers a browser on this machine actually sends for a read: no
    /// token, no `X-Storyhook` (a read never needed one), just a `Host`.
    fn loopback_read_headers() -> Vec<Header> {
        headers(&[("Host", "127.0.0.1:3456")])
    }

    /// A read on loopback, with no token anywhere.
    fn loopback_read(
        segments: &[&str],
        method: &Method,
        headers: &[Header],
        trusted: &TrustedHosts,
    ) -> Option<Reply> {
        admission(segments, method, None, headers, trusted, TOKEN, true)
    }

    #[test]
    fn a_loopback_read_needs_no_token() {
        assert!(
            loopback_read(
                &["api", "repos"],
                &Method::Get,
                &loopback_read_headers(),
                &trusted_hosts(),
            )
            .is_none(),
            "the whole point of SH-250: a tab opened at 127.0.0.1 renders \
             without anyone fetching a credential first"
        );
    }

    #[test]
    fn a_loopback_head_needs_no_token() {
        assert!(
            loopback_read(
                &["api", "repos"],
                &Method::Head,
                &loopback_read_headers(),
                &trusted_hosts(),
            )
            .is_none()
        );
    }

    #[test]
    fn the_loopback_events_stream_needs_no_token() {
        // The route that cannot send a custom header at all (`EventSource`),
        // which is why conjunct 3 -- the `Host` check -- has to carry it.
        assert!(
            loopback_read(
                &["api", "events"],
                &Method::Get,
                &loopback_read_headers(),
                &trusted_hosts(),
            )
            .is_none()
        );
    }

    #[test]
    fn conjunct_1_the_same_read_on_the_tailnet_listener_is_401() {
        let reply = admission(
            &["api", "repos"],
            &Method::Get,
            None,
            &loopback_read_headers(),
            &trusted_hosts(),
            TOKEN,
            false,
        )
        .expect("only the loopback listener is exempt");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn conjunct_2_a_loopback_put_is_not_exempt() {
        // `Put` parses, is not `mutating`, and routes nowhere today. The
        // affirmative `Get | Head` spelling is what stops a future PUT route
        // from inheriting the exemption on the day it is added; `!mutating`
        // would have handed it over silently. Deleting this test's conjunct
        // turns that into a live hole rather than a hypothetical one.
        let reply = loopback_read(
            &["api", "repos"],
            &Method::Put,
            &loopback_read_headers(),
            &trusted_hosts(),
        )
        .expect("only GET and HEAD are exempt");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn conjunct_3_a_loopback_read_from_a_rebound_host_is_401() {
        // DNS rebinding: the page is same-origin with http://127.0.0.1:PORT
        // and can read what comes back, and `mutation_guard_ok` never runs on
        // a read -- so this conjunct is the *only* thing standing between a
        // rebound origin and every story in the project.
        let rebound = headers(&[("Host", "evil.example:3456")]);
        let reply = loopback_read(&["api", "repos"], &Method::Get, &rebound, &trusted_hosts())
            .expect("a rebound Host must not be exempt");
        assert_eq!(reply.status, 401);

        // Positive control: the same rebound request *with* the token is
        // admitted, exactly as it was before SH-250 -- so the 401 above is the
        // exemption being withheld, not a new refusal of a credentialed read.
        let mut credentialed = rebound;
        credentialed.push(Header::from_bytes("X-Storyhook-Token", TOKEN).unwrap());
        assert!(
            loopback_read(
                &["api", "repos"],
                &Method::Get,
                &credentialed,
                &trusted_hosts()
            )
            .is_none()
        );
    }

    #[test]
    fn conjunct_3_a_loopback_read_with_no_host_header_is_401() {
        // An absent input can never grant. HTTP/1.0 clients may omit `Host`.
        let reply = loopback_read(&["api", "repos"], &Method::Get, &[], &trusted_hosts())
            .expect("a missing Host must not be exempt");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn conjunct_3_a_trusted_host_is_not_thereby_a_loopback_host() {
        // The exemption asks `host_is_loopback`, never `host_is_trusted`.
        // Here the allowlist would happily *trust* `proxy.example` for a
        // mutation -- and it still must not read as "this caller is local".
        // Note the allowlist's `bound` half is used, so `behind_a_proxy` is
        // false and conjunct 5 is not what produces this 401.
        let trusted = TrustedHosts::from_parts(vec!["proxy.example".to_string()], Vec::new());
        let reply = loopback_read(
            &["api", "repos"],
            &Method::Get,
            &headers(&[("Host", "proxy.example")]),
            &trusted,
        )
        .expect("a merely-trusted Host must not be exempt");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn conjunct_4_any_forwarding_header_withdraws_the_exemption() {
        for name in FORWARDING_HEADERS {
            let mut forwarded = loopback_read_headers();
            forwarded.push(Header::from_bytes(name, "203.0.113.1").unwrap());
            let reply = loopback_read(
                &["api", "repos"],
                &Method::Get,
                &forwarded,
                &trusted_hosts(),
            )
            .unwrap_or_else(|| panic!("a request carrying {name} must not be exempt"));
            assert_eq!(reply.status, 401, "{name}");
        }
    }

    #[test]
    fn conjunct_5_a_configured_proxy_allowlist_re_arms_the_token() {
        // The first assertion anywhere that STORYHOOK_WEB_TRUSTED_HOSTS being
        // set changes any outcome at all -- the residual SH-187 recorded and
        // could not close.
        let behind_proxy = TrustedHosts::from_parts(Vec::new(), vec!["proxy.example".to_string()]);
        let reply = loopback_read(
            &["api", "repos"],
            &Method::Get,
            &loopback_read_headers(),
            &behind_proxy,
        )
        .expect("a declared reverse proxy must re-arm the token on loopback");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn conjunct_5_a_bound_host_allowlist_does_not_withdraw_the_exemption() {
        // The test that fails against a naive `trusted_hosts.is_empty()`
        // implementation of conjunct 5. A bound tailnet interface is an
        // interface this daemon serves directly, not an indirection it was
        // told about -- so on every Tailscale machine, where the allowlist is
        // non-empty with zero proxies configured, loopback stays exempt.
        let tailnet_bound = TrustedHosts::from_parts(
            vec![
                "100.71.206.33".to_string(),
                "psamathe.tail983f02.ts.net".to_string(),
            ],
            Vec::new(),
        );
        assert!(
            loopback_read(
                &["api", "repos"],
                &Method::Get,
                &loopback_read_headers(),
                &tailnet_bound,
            )
            .is_none()
        );
    }

    #[test]
    fn conjunct_6_a_loopback_dispatch_poll_is_not_exempt() {
        // `GET .../dispatch/{handle}` is a read, so every other conjunct
        // holds -- this one is what refuses it. `dispatch::intercept` would
        // refuse it too; this keeps the two gates from disagreeing.
        let reply = loopback_read(
            &[
                "api", "repos", "proj", "story", "SH-1", "dispatch", "h4nd1e",
            ],
            &Method::Get,
            &loopback_read_headers(),
            &trusted_hosts(),
        )
        .expect("the process-spawning route is never exempt");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn a_loopback_mutation_still_needs_the_token() {
        // The exemption is for reads. The write surface registers projects at
        // caller-named paths, deletes projects, and reaches `sh -c` through
        // event hooks, so it keeps its credential on every listener.
        let reply = admission(
            &["api", "repos", "x", "story"],
            &Method::Post,
            None,
            &guard_headers(),
            &trusted_hosts(),
            TOKEN,
            true,
        )
        .expect("a loopback mutation must still be refused without the token");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn the_exemption_relaxes_the_token_and_never_the_guard() {
        // If this ever returns 200, `mutation_guard_ok` has been dissolved
        // along with the token and the CSRF / DNS-rebinding defence is gone.
        // The story asked for exactly this test.
        let reply = admission(
            &["api", "repos", "x", "story"],
            &Method::Post,
            None,
            &[],
            &trusted_hosts(),
            TOKEN,
            true,
        )
        .expect("an unguarded loopback mutation must be refused");
        assert_eq!(reply.status, 403);
    }

    // --- Named tokens (SH-255) ---
    //
    // These call `super::admission` directly, not the shadow above: the
    // shadow's `TokenRegistry` is always empty, on purpose, so every test
    // above this line stays byte-identical proof that the three decisions
    // that come before a capability -- the CSRF guard, the exemption, the
    // master token -- were not weakened. What is new is checked here, on its
    // own account, against a registry that actually has a token in it.

    const COOKIE_NAME: &str = "storyhook_test";

    fn minted_tokens() -> (TokenRegistry, String) {
        let registry = TokenRegistry::new(Utc::now(), Instant::now());
        let minted = registry
            .mint(
                "laptop".to_string(),
                Utc::now(),
                Instant::now(),
                crate::api::tokens::DEFAULT_TTL,
            )
            .expect("minting into a fresh registry");
        (registry, minted.secret)
    }

    #[allow(clippy::too_many_arguments)]
    fn admits_with_tokens(
        segments: &[&str],
        method: &Method,
        headers: &[Header],
        loopback: bool,
        tokens: &TokenRegistry,
    ) -> Option<Reply> {
        super::admission(
            segments,
            method,
            None,
            headers,
            &trusted_hosts(),
            TOKEN,
            loopback,
            Instant::now(),
            &SessionRegistry::new(),
            tokens,
            COOKIE_NAME,
            Utc::now(),
        )
    }

    // Every read below runs on the tailnet listener (`loopback: false`), not
    // the loopback one: SH-250's exemption already admits an unauthenticated
    // loopback `GET` regardless of any token, which would make these tests
    // pass whether or not `named_token_ok` did anything at all. The tailnet
    // listener never gets that exemption, so admission here can only come
    // from the path under test. Mutations are unaffected either way — the
    // exemption never covers them — and are exercised on loopback to match
    // how the dashboard actually reaches this daemon most of the time.

    #[test]
    fn a_header_borne_named_token_admits_a_read() {
        let (tokens, secret) = minted_tokens();
        let headers = headers(&[("Host", "127.0.0.1"), ("X-Storyhook-Token", &secret)]);
        assert!(
            admits_with_tokens(&["api", "repos"], &Method::Get, &headers, false, &tokens).is_none(),
            "a valid named token in the header must admit a read"
        );
    }

    #[test]
    fn a_header_borne_named_token_admits_a_mutation() {
        let (tokens, secret) = minted_tokens();
        let headers = headers(&[
            ("X-Storyhook", "1"),
            ("Host", "127.0.0.1"),
            ("X-Storyhook-Token", &secret),
        ]);
        assert!(
            admits_with_tokens(
                &["api", "repos", "x", "story"],
                &Method::Post,
                &headers,
                true,
                &tokens
            )
            .is_none(),
            "a valid named token in the header must admit a mutation"
        );
    }

    #[test]
    fn a_cookie_borne_named_token_admits_a_read_with_same_origin_fetch() {
        let (tokens, secret) = minted_tokens();
        let headers = headers(&[
            ("Host", "127.0.0.1"),
            ("Cookie", &format!("{COOKIE_NAME}={secret}")),
            ("Sec-Fetch-Site", "same-origin"),
        ]);
        assert!(
            admits_with_tokens(&["api", "repos"], &Method::Get, &headers, false, &tokens).is_none(),
            "a valid cookie plus Sec-Fetch-Site: same-origin must admit a read"
        );
    }

    #[test]
    fn a_cookie_borne_named_token_without_same_origin_fetch_is_refused_on_a_read() {
        // `SameSite=Strict` alone does not distinguish this daemon's own tab
        // from every other tailnet peer's dashboard -- this is the check
        // that closes that gap.
        let (tokens, secret) = minted_tokens();
        let headers = headers(&[
            ("Host", "127.0.0.1"),
            ("Cookie", &format!("{COOKIE_NAME}={secret}")),
        ]);
        let reply = admits_with_tokens(&["api", "repos"], &Method::Get, &headers, false, &tokens)
            .expect("a cookie with no Sec-Fetch-Site must be refused on a read");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn a_cookie_borne_named_token_admits_a_mutation_without_same_origin_fetch() {
        // A mutation has already passed `mutation_guard_ok` (X-Storyhook +
        // trusted Host) before this check runs, which is what makes the
        // cookie sufficient here with no extra header.
        let (tokens, secret) = minted_tokens();
        let headers = headers(&[
            ("X-Storyhook", "1"),
            ("Host", "127.0.0.1"),
            ("Cookie", &format!("{COOKIE_NAME}={secret}")),
        ]);
        assert!(
            admits_with_tokens(
                &["api", "repos", "x", "story"],
                &Method::Post,
                &headers,
                true,
                &tokens
            )
            .is_none(),
            "a valid cookie must admit a mutation once the CSRF guard already passed"
        );
    }

    #[test]
    fn a_cookie_under_the_wrong_name_is_never_a_credential() {
        let (tokens, secret) = minted_tokens();
        let headers = headers(&[
            ("Host", "127.0.0.1"),
            ("Cookie", &format!("some_other_cookie={secret}")),
            ("Sec-Fetch-Site", "same-origin"),
        ]);
        let reply = admits_with_tokens(&["api", "repos"], &Method::Get, &headers, false, &tokens)
            .expect("a cookie under a different name must never be parsed as a credential");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn an_invalid_named_token_is_refused() {
        let (tokens, _secret) = minted_tokens();
        let headers = headers(&[
            ("Host", "127.0.0.1"),
            ("X-Storyhook-Token", "not-a-real-token"),
        ]);
        let reply = admits_with_tokens(&["api", "repos"], &Method::Get, &headers, false, &tokens)
            .expect("an unrecognized token must be refused");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn a_revoked_named_token_no_longer_admits() {
        let (tokens, secret) = minted_tokens();
        tokens
            .revoke("laptop", Utc::now(), Instant::now())
            .expect("revoking the freshly minted token");
        let headers = headers(&[("Host", "127.0.0.1"), ("X-Storyhook-Token", &secret)]);
        let reply = admits_with_tokens(&["api", "repos"], &Method::Get, &headers, false, &tokens)
            .expect("a revoked token must be refused");
        assert_eq!(reply.status, 401);
    }
}
