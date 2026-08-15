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
//! # One credential, no exceptions (SH-255)
//!
//! Every `/api/**` route — reads, project/story CRUD, dispatch — now needs
//! exactly one thing: a token, master or named, offered as
//! `X-Storyhook-Token` or (named only, and only for the caller that cannot
//! set a header) the [`crate::api::tokens::cookie_name`]d cookie. Three
//! things this module used to have are gone, deliberately, not merely
//! unused:
//!
//! * **SH-250's tokenless loopback read exemption.** A read on the loopback
//!   listener used to need no credential at all, so a browser tab opened at
//!   `127.0.0.1` could render before fetching one. `story web open`'s coupon
//!   (SH-251) and, since this story, a pasted named token both authenticate
//!   before the first request now, so the exemption's remaining
//!   beneficiaries — a bookmark, a hand-typed URL, a second tab — cost a
//!   single failed round-trip and a modal instead. What it bought was
//!   permanent: any second local uid, a host-networked container, a
//!   host-permissioned browser extension, and the unclosable bare-nginx
//!   residual (`proxy_pass http://127.0.0.1:PORT` defeated the exemption's
//!   locality conjuncts by construction) could all read every story in every
//!   project with nothing to prove who they were.
//! * **`?token=` on `GET /api/events`.** `EventSource` cannot set headers,
//!   which is why the query parameter existed — but a same-origin
//!   `EventSource` carries a cookie automatically, so the one caller that
//!   needed the exception no longer does, and the last place a credential
//!   ever traveled in a URL is gone.
//! * **The scoped dashboard capability (SH-254).** `POST /handoff` used to
//!   answer with a capability limited to the board's own write surface —
//!   less than the master token, but still a second, weaker tier this module
//!   had to reason about and `crate::api::routes` had to classify every
//!   route against (that classification, `Authority`/`authority()`, is gone
//!   with it — see [`crate::api::routes`]'s module doc). There is one tier
//!   now: a named token authenticates everything the dashboard does, full
//!   stop. Redemption answers with a cookie instead (see
//!   [`crate::api::handoff`]).
//!
//! [`crate::api::tokens::TokenRegistry`] is what makes this safe to collapse:
//! unlike the master token, a named token is per-device, individually
//! revocable, and never printed to a terminal a script might log — the
//! properties the capability tier existed to approximate for a browser tab
//! specifically. See `docs/spec/dashboard-authorization.md`'s "As built"
//! section for the full record of what this replaced and why.

use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::api::http::{
    Reply, TrustedHosts, cookie_value, header_value, mutation_guard_ok, text_reply,
};
use crate::api::rest::mutating;
use crate::api::rpc::token_ok;
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
#[allow(clippy::too_many_arguments)]
pub fn admission(
    segments: &[&str],
    method: &Method,
    headers: &[Header],
    trusted_hosts: &TrustedHosts,
    token: &str,
    now: Instant,
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
    let authorized = token_ok(headers, token)
        || named_token_ok(headers, method, cookie_name, tokens, wall_now, now);
    if authorized {
        None
    } else {
        Some(text_reply(
            401,
            "storyhook daemon: missing or invalid token",
        ))
    }
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
///   **read** additionally needs [`same_origin_read`]'s proof. A
///   **mutation** needs no extra check here: it has already passed
///   [`mutation_guard_ok`] above, in [`admission`], which is what makes the
///   cookie sufficient on its own.
///
/// `pub(crate)`: [`crate::api::dispatch::intercept`] is a second, narrower
/// gate on the exact same route family (its own module doc calls it a
/// "harmless, deliberate redundancy" with this one), and it must recognize
/// every credential this gate does — a request `admission` already admitted
/// must never be refused a second time by dispatch's own copy of the check.
/// It imports this function directly rather than a copy of it, so
/// [`same_origin_read`]'s rule applies to both call sites by construction.
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
    mutating(method) || same_origin_read(headers)
}

/// Whether a cookie-authenticated **read** proves it came from this
/// dashboard's own tab, tried in order of strength (SH-319):
///
/// 1. **`X-Storyhook` present.** The same marker [`mutation_guard_ok`]
///    requires for mutations, and already sent on every `fetch`/XHR read the
///    dashboard issues. Setting any custom header makes a cross-origin
///    request non-"simple," which forces a CORS preflight this daemon never
///    answers with `Access-Control-Allow-*` (`mutation_guard_ok`'s own doc);
///    the browser refuses to ever send the real request, so an offered
///    header proves same-origin as strongly as the token header case above
///    does, and for the identical reason. Stronger than either check below:
///    those let a cross-site request *reach* the daemon and rely on the
///    response being unreadable, which still leaves XS-Leak-style
///    side-channels (timing, response size) open; this one stops the
///    request from being sent at all. Deliberately no accompanying `Host`
///    check the way `mutation_guard_ok` has one: `mutation_guard_ok`'s
///    second check exists for DNS-rebinding, where the caller's page
///    becomes same-origin with the real target and so *can* set the header
///    itself — but this cookie is host-only (no `Domain`), so it is never
///    attached to a request whose target hostname is the rebound name
///    rather than this daemon's own; rebinding cannot make the browser
///    attach a cookie scoped to a hostname the request isn't addressed to.
/// 2. **`Sec-Fetch-Site: same-origin`.** A forbidden header name no page can
///    forge or suppress a false value into, sent by every browser request
///    including `EventSource` — but per the Fetch Metadata spec, only on a
///    *potentially trustworthy* URL (https, or http on `localhost`/
///    `127.0.0.1`). A plain-http tailnet or LAN origin — this daemon's usual
///    deployment off loopback — never gets it at all, which is what SH-319
///    reproduced: every such read 401s forever, forcing the token modal
///    open in a loop no amount of re-pasting the same valid token escapes.
/// 3. **`Referer`'s host matches this request's own `Host`, checked only
///    when `Sec-Fetch-Site` is absent.** The one caller left needing this:
///    `EventSource` (`GET /api/events`) cannot set custom headers, so check
///    1 is never available to it, and on a non-trustworthy origin neither is
///    check 2. `Referer` is, like `Sec-Fetch-Site`, a forbidden header name;
///    a page can suppress it (via its own `Referrer-Policy`) but never make
///    it name a false origin, so its *absence* is the only failure mode,
///    handled by refusing rather than by trusting nothing. Host **and**
///    port are compared, verbatim — reusing `crate::api::http::normalized_host`'s
///    port-stripping here (built for `host_is_trusted`'s allowlist
///    semantics) would wrongly equate two daemons on one host at different
///    ports, since this cookie's own name is derived from the store, not
///    the port, and cookies ignore port entirely (RFC 6265).
///
/// Order matters for correctness, not just clarity: a present
/// `Sec-Fetch-Site: cross-site` must refuse outright rather than ever
/// falling through to `Referer` — collapsing the three into an unordered
/// "any one passes" check would silently readmit the cross-site read
/// `Sec-Fetch-Site` exists to block.
fn same_origin_read(headers: &[Header]) -> bool {
    if header_value(headers, "X-Storyhook").is_some() {
        return true;
    }
    match header_value(headers, "Sec-Fetch-Site") {
        Some(value) => value == "same-origin",
        None => referer_matches_host(headers),
    }
}

/// The `Referer` fallback [`same_origin_read`] uses when `Sec-Fetch-Site` is
/// absent entirely. Compares the `Referer` URL's authority (host and port,
/// verbatim) against this request's own `Host` header — not
/// `crate::api::http::normalized_host`, whose port-stripping exists for a different
/// question ("is this an allowlisted host at all") than the one asked here
/// ("is this literally the same origin the request was sent to").
fn referer_matches_host(headers: &[Header]) -> bool {
    let (Some(referer), Some(host)) = (
        header_value(headers, "Referer"),
        header_value(headers, "Host"),
    ) else {
        return false;
    };
    referer_authority(referer).is_some_and(|authority| authority.eq_ignore_ascii_case(host))
}

/// The `host[:port]` authority of a `Referer` URL, or `None` if it does not
/// look like one (no `scheme://` prefix) — never panics on a malformed or
/// hostile value, which a `Referer` header, page-suppressible but otherwise
/// browser-controlled, is not expected to be, but this function does not
/// rely on that not happening.
fn referer_authority(referer: &str) -> Option<&str> {
    let after_scheme = referer.split_once("://")?.1;
    Some(
        after_scheme
            .split(['/', '?', '#'])
            .next()
            .unwrap_or(after_scheme),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "the-real-token";

    /// The gate, asked with an empty token registry.
    ///
    /// **Shadows [`super::admission`] on purpose.** Every test below this
    /// line except the "Named tokens" section is about the two decisions
    /// that do not need a real token in the registry — the CSRF guard and
    /// the master token — so routing them through a helper that supplies an
    /// empty one keeps both byte-identical to what they always asserted.
    fn admission(
        segments: &[&str],
        method: &Method,
        headers: &[Header],
        trusted_hosts: &TrustedHosts,
        token: &str,
    ) -> Option<Reply> {
        let now = Instant::now();
        super::admission(
            segments,
            method,
            headers,
            trusted_hosts,
            token,
            now,
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
        assert!(admission(&["nonexistent"], &Method::Get, &[], &trusted_hosts(), TOKEN).is_none());
        assert!(admission(&[], &Method::Get, &[], &trusted_hosts(), TOKEN).is_none());
    }

    #[test]
    fn api_v1_is_left_to_rpc_admission() {
        assert!(
            admission(
                &["api", "v1", "hello"],
                &Method::Get,
                &[],
                &trusted_hosts(),
                TOKEN
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
            &[],
            &trusted_hosts(),
            TOKEN,
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
                &authed_headers(),
                &trusted_hosts(),
                TOKEN,
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
            &bad,
            &trusted_hosts(),
            TOKEN,
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
            &[],
            &trusted_hosts(),
            TOKEN,
        )
        .expect("an unguarded mutation must be refused");
        assert_eq!(reply.status, 403);
    }

    #[test]
    fn a_mutation_with_the_guard_but_no_token_is_401() {
        let reply = admission(
            &["api", "repos", "x", "story"],
            &Method::Post,
            &guard_headers(),
            &trusted_hosts(),
            TOKEN,
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
                &authed_headers(),
                &trusted_hosts(),
                TOKEN,
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
            &headers,
            &trusted_hosts(),
            TOKEN,
        )
        .expect("a spoofed Host must be refused regardless of the token");
        assert_eq!(reply.status, 403);
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
            &empty_offered_token,
            &trusted_hosts(),
            "",
        )
        .expect("an empty configured token must admit nothing, even to an empty offered header");
        assert_eq!(reply.status, 401);
    }

    // --- Named tokens (SH-255) ---
    //
    // These call `super::admission` directly, not the shadow above: the
    // shadow's `TokenRegistry` is always empty, on purpose, so every test
    // above this line stays byte-identical proof that the two decisions
    // that come before a token is validated -- the CSRF guard and the master
    // token -- were not weakened. What is new is checked here, on its own
    // account, against a registry that actually has a token in it.

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

    fn admits_with_tokens(
        segments: &[&str],
        method: &Method,
        headers: &[Header],
        tokens: &TokenRegistry,
    ) -> Option<Reply> {
        super::admission(
            segments,
            method,
            headers,
            &trusted_hosts(),
            TOKEN,
            Instant::now(),
            tokens,
            COOKIE_NAME,
            Utc::now(),
        )
    }

    #[test]
    fn a_header_borne_named_token_admits_a_read() {
        let (tokens, secret) = minted_tokens();
        let headers = headers(&[("Host", "127.0.0.1"), ("X-Storyhook-Token", &secret)]);
        assert!(
            admits_with_tokens(&["api", "repos"], &Method::Get, &headers, &tokens).is_none(),
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
            admits_with_tokens(&["api", "repos"], &Method::Get, &headers, &tokens).is_none(),
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
        let reply = admits_with_tokens(&["api", "repos"], &Method::Get, &headers, &tokens)
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
        let reply = admits_with_tokens(&["api", "repos"], &Method::Get, &headers, &tokens)
            .expect("a cookie under a different name must never be parsed as a credential");
        assert_eq!(reply.status, 401);
    }

    // --- SH-319: same_origin_read's X-Storyhook and Referer fallbacks ---
    //
    // `Sec-Fetch-Site` never arrives at all on a non-potentially-trustworthy
    // origin (a plain-http tailnet or LAN host, per the Fetch Metadata
    // spec) -- these pin the layered replacement, independent of the tests
    // above which stay exactly as they were for the loopback/Sec-Fetch-Site
    // case.

    #[test]
    fn x_storyhook_alone_admits_a_cookie_borne_read_with_no_sec_fetch_site_at_all() {
        let (tokens, secret) = minted_tokens();
        let headers = headers(&[
            ("X-Storyhook", "1"),
            ("Host", "psamathe.tail983f02.ts.net"),
            ("Cookie", &format!("{COOKIE_NAME}={secret}")),
        ]);
        assert!(
            admits_with_tokens(&["api", "repos"], &Method::Get, &headers, &tokens).is_none(),
            "X-Storyhook must admit a cookie-borne read on its own, the way it already does for a mutation"
        );
    }

    #[test]
    fn a_matching_referer_admits_a_read_when_sec_fetch_site_is_entirely_absent() {
        let (tokens, secret) = minted_tokens();
        let headers = headers(&[
            ("Host", "psamathe.tail983f02.ts.net:3456"),
            ("Cookie", &format!("{COOKIE_NAME}={secret}")),
            ("Referer", "http://psamathe.tail983f02.ts.net:3456/"),
        ]);
        assert!(
            admits_with_tokens(&["api", "events"], &Method::Get, &headers, &tokens).is_none(),
            "a Referer whose authority matches Host must admit a read when Sec-Fetch-Site never arrived (EventSource, non-trustworthy origin)"
        );
    }

    #[test]
    fn a_referer_naming_a_different_host_is_refused() {
        let (tokens, secret) = minted_tokens();
        let headers = headers(&[
            ("Host", "psamathe.tail983f02.ts.net:3456"),
            ("Cookie", &format!("{COOKIE_NAME}={secret}")),
            ("Referer", "http://evil.example:3456/"),
        ]);
        let reply = admits_with_tokens(&["api", "events"], &Method::Get, &headers, &tokens)
            .expect("a foreign Referer must never admit a read");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn a_referer_naming_the_same_host_on_a_different_port_is_refused() {
        // The cookie's own name is keyed by store, not port, and cookies
        // ignore port entirely (RFC 6265) -- a different port is a different
        // daemon, and this check must not equate them the way
        // `host_is_trusted`'s port-stripped comparison would.
        let (tokens, secret) = minted_tokens();
        let headers = headers(&[
            ("Host", "psamathe.tail983f02.ts.net:3456"),
            ("Cookie", &format!("{COOKIE_NAME}={secret}")),
            ("Referer", "http://psamathe.tail983f02.ts.net:9999/"),
        ]);
        let reply = admits_with_tokens(&["api", "events"], &Method::Get, &headers, &tokens)
            .expect("a Referer naming the same host but a different port must never admit a read");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn sec_fetch_site_cross_site_refuses_outright_even_with_a_matching_referer() {
        // Precedence: a present Sec-Fetch-Site is authoritative and must
        // never fall through to the Referer fallback, or a genuinely
        // cross-site read (one a real browser labels as such) would be
        // readmitted by a Referer that -- unlike Sec-Fetch-Site -- a
        // same-site-but-cross-origin attacker page can still cause to be
        // sent, since Referer only proves *page origin*, not *fetch-site
        // classification*.
        let (tokens, secret) = minted_tokens();
        let headers = headers(&[
            ("Host", "psamathe.tail983f02.ts.net:3456"),
            ("Cookie", &format!("{COOKIE_NAME}={secret}")),
            ("Sec-Fetch-Site", "cross-site"),
            ("Referer", "http://psamathe.tail983f02.ts.net:3456/"),
        ]);
        let reply = admits_with_tokens(&["api", "events"], &Method::Get, &headers, &tokens)
            .expect("Sec-Fetch-Site: cross-site must refuse even when Referer matches Host");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn a_malformed_referer_is_refused_rather_than_panicking() {
        let (tokens, secret) = minted_tokens();
        let headers = headers(&[
            ("Host", "psamathe.tail983f02.ts.net:3456"),
            ("Cookie", &format!("{COOKIE_NAME}={secret}")),
            ("Referer", "not-a-url"),
        ]);
        let reply = admits_with_tokens(&["api", "events"], &Method::Get, &headers, &tokens)
            .expect("a Referer with no scheme must be refused, not panic");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn an_invalid_named_token_is_refused() {
        let (tokens, _secret) = minted_tokens();
        let headers = headers(&[
            ("Host", "127.0.0.1"),
            ("X-Storyhook-Token", "not-a-real-token"),
        ]);
        let reply = admits_with_tokens(&["api", "repos"], &Method::Get, &headers, &tokens)
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
        let reply = admits_with_tokens(&["api", "repos"], &Method::Get, &headers, &tokens)
            .expect("a revoked token must be refused");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn a_loopback_host_read_still_needs_a_credential() {
        // SH-255 retired SH-250's tokenless loopback read exemption: a read
        // with no credential is refused whether or not `Host` names
        // loopback, which is no longer a distinction `admission` draws at
        // all -- there is no `loopback` parameter left to pass it on.
        let headers = headers(&[("Host", "127.0.0.1:3456")]);
        let reply = admission(
            &["api", "repos"],
            &Method::Get,
            &headers,
            &trusted_hosts(),
            TOKEN,
        )
        .expect("a tokenless read must be refused regardless of Host");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn the_events_stream_needs_a_credential_like_every_other_read() {
        // `?token=` used to be the one exception `EventSource` needed; the
        // named-token cookie rides a same-origin `EventSource` automatically
        // now (SH-255), so the query parameter is gone, and a tokenless
        // request for the stream is refused the same as any other read.
        let reply = admission(
            &["api", "events"],
            &Method::Get,
            &[],
            &trusted_hosts(),
            TOKEN,
        )
        .expect("a tokenless events read must be refused");
        assert_eq!(reply.status, 401);
    }
}
