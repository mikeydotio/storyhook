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

use crate::api::http::{Reply, TrustedHosts, mutation_guard_ok, text_reply};
use crate::api::rest::mutating;
use crate::api::rpc::{constant_time_eq, token_ok};
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
pub fn admission(
    segments: &[&str],
    method: &Method,
    query: Option<&str>,
    headers: &[Header],
    trusted_hosts: &TrustedHosts,
    token: &str,
) -> Option<Reply> {
    match segments {
        ["api", "v1", ..] => return None,
        ["api", ..] => {}
        _ => return None,
    }
    if mutating(method) && !mutation_guard_ok(headers, trusted_hosts) {
        return Some(text_reply(403, "Forbidden"));
    }
    let is_events_stream = segments == ["api", "events"];
    let authorized = token_ok(headers, token) || (is_events_stream && query_token_ok(query, token));
    if !authorized {
        return Some(text_reply(
            401,
            "storyhook daemon: missing or invalid token",
        ));
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "the-real-token";

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
                TOKEN
            )
            .is_none()
        );
        assert!(admission(&[], &Method::Get, None, &[], &trusted_hosts(), TOKEN).is_none());
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
        )
        .expect("an empty configured token must admit nothing, even to an empty offered header");
        assert_eq!(reply.status, 401);
    }
}
