//! The one-shot handoff coupon: how `story web open` hands the dashboard a
//! credential without anybody typing one, and without relaxing anything
//! (SH-251).
//!
//! # The shape, and the experiment that chose it
//!
//! `story web open` already resolves the running daemon and already reads the
//! mode-0600 portfile that carries its token. The obvious move was to open
//! `http://127.0.0.1:PORT/#t=<token>` and have the page strip the fragment
//! with `history.replaceState` before issuing a request — a fragment is never
//! sent to a server and never lands in a server log.
//!
//! SH-251's council ran that claim against a real Chrome instead of reasoning
//! about it, and it is false where it mattered:
//!
//! ```text
//! sqlite> select id, url from urls;
//! 1|http://127.0.0.1:8791/page.html#t=deadbeefcafe0000deadbeefcafe0001
//! 2|http://127.0.0.1:8791/page.html
//! ```
//!
//! `replaceState` does **not** scrub the persisted history row. It adds a
//! second one, and the pre-replacement URL keeps the secret on disk
//! indefinitely while `location.href` reads clean. The plain-token shape
//! therefore writes the *master* token — valid until the daemon restarts,
//! spendable for **writes** from any tailnet peer, because
//! [`crate::api::http::mutation_guard_ok`] accepts a bound tailnet `Host` —
//! into a durable, well-known, same-uid-readable file.
//!
//! Every URL-borne handoff persists. The experiment does not separate the
//! candidates on *whether* something is written, only on **what the written
//! thing is worth**. So what travels in the fragment is a coupon that is dead
//! after one redemption or [`TTL`], and its worthlessness is a property of a
//! type in this repository with an injected clock, provable red-to-green —
//! rather than a property of a browser's storage that no test here can
//! observe.
//!
//! # Why redemption sits outside `/api`
//!
//! [`crate::api::admission::admission`] matches `["api", ..]` and falls
//! through on everything else. Putting redemption at `POST /handoff`, outside
//! `/api`, means the route stands on **its own gate**
//! ([`redemption_admitted`]) rather than inheriting SH-250's tokenless read
//! exemption — and the verb becomes a free choice rather than a consequence of
//! that exemption's `Get | Head` conjunct. The first draft of this design was
//! a side-effecting `GET`, chosen only because the exemption admitted nothing
//! else; the topology deletes that compromise.
//!
//! **Do not move this route under `/api` "for consistency".**
//! `tests/handoff_endpoint.rs` fails if anyone does.
//!
//! # What a coupon buys, and what it stopped buying
//!
//! Until SH-254 a redeemed coupon bought the **master token**, and this module
//! said so plainly: *"this is transport, done correctly — it is not least
//! privilege."* It buys a [`crate::api::session`] capability now — the board's
//! own write surface, honoured only on this machine, revocable without
//! restarting the daemon.
//!
//! The gate did not move for that change. The same five conjuncts in the same
//! order decide a redemption, and the only difference visible here is that
//! [`redemption_admitted`] hands back the [`LocalRequest`] it built instead of
//! a `bool`, so the capability is issued against the locality this request
//! already proved rather than against a second derivation of it.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::api::http::{
    LocalRequest, Reply, TrustedHosts, header_value, json_reply, local_request, text_reply,
};
use crate::api::rpc::{constant_time_eq, token_ok};
use crate::api::session::SessionRegistry;
use crate::api::tokens::TokenRegistry;
use crate::daemon::http1::{Header, Method};

/// Where `story web open` arms a coupon: on the control surface, so
/// [`crate::api::rpc::admission`] has already refused an off-loopback or
/// tokenless caller before this module is reached.
pub const ARM_PATH: &str = "/api/v1/handoff";

/// Where the dashboard redeems one. **Outside `/api` on purpose** — see the
/// module doc.
pub const REDEEM_PATH: &str = "/handoff";

/// The header carrying the coupon on a redemption.
///
/// A header rather than a body, so this whole module decides from the request
/// *head* — the property [`crate::api::rpc::admission`] and
/// [`crate::api::dispatch::intercept`] already rely on, and what lets
/// [`intercept`] answer before the daemon has waited on a single body byte.
/// A header rather than a query parameter because a query string lands in
/// logs, which is the entire failure this design exists to bound.
pub const COUPON_HEADER: &str = "X-Storyhook-Handoff";

/// How long a coupon stays redeemable.
///
/// Long enough for a browser to cold-start and load a page; short enough that
/// the durable history row the experiment above found is worth nothing by the
/// time anybody could read it. *"A durable record of a dead value is a durable
/// record of nothing."*
pub const TTL: Duration = Duration::from_secs(120);

/// How many coupons may be live at once.
///
/// Bounds the registry by construction rather than by a sweep somebody has to
/// remember to run. Eight is far above any real usage — a coupon is armed by a
/// human typing `story web open` and spent milliseconds later — and low enough
/// that a script arming in a loop cannot grow this daemon's memory.
const MAX_LIVE: usize = 8;

/// One armed coupon, and the instant it stops being one.
struct Armed {
    coupon: String,
    expires_at: Instant,
}

/// Every coupon this daemon has armed and not yet spent.
///
/// # The clock is a parameter
///
/// Neither [`Self::arm`] nor [`Self::redeem`] ever calls `Instant::now()`.
/// This is a deliberate divergence from the [`crate::api::dispatch::DispatchRegistry`]
/// precedent, which reads the clock inline and is not clock-testable as a
/// result. Expiry is the load-bearing half of this design's security
/// argument — the half that makes the residue in a browser's history
/// worthless — so it has to be assertable in a unit test rather than
/// inferred from a sleep.
pub struct HandoffRegistry {
    /// Oldest first, so eviction is a `pop_front`. Poisoning is recovered
    /// from rather than propagated ([`PoisonError::into_inner`]): nothing
    /// under this lock can panic, and a daemon that answered every later
    /// redemption with a panic because one unrelated thread unwound would be
    /// a worse failure than the one it reported.
    armed: Mutex<VecDeque<Armed>>,
}

impl Default for HandoffRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HandoffRegistry {
    pub fn new() -> Self {
        HandoffRegistry {
            armed: Mutex::new(VecDeque::new()),
        }
    }

    /// Mints a coupon good until `now + TTL`, evicting anything already
    /// expired and then the oldest survivors beyond [`MAX_LIVE`].
    ///
    /// 32 hex characters from the operating system's CSPRNG — the same
    /// construction and the same 122 bits as
    /// [`crate::daemon::lifecycle::mint_token`], because a coupon that is
    /// guessable inside its 120-second window is a coupon that hands out the
    /// master token.
    pub fn arm(&self, now: Instant) -> String {
        let coupon = uuid::Uuid::new_v4().simple().to_string();
        let mut armed = self.armed.lock().unwrap_or_else(PoisonError::into_inner);
        armed.retain(|entry| entry.expires_at > now);
        while armed.len() >= MAX_LIVE {
            armed.pop_front();
        }
        armed.push_back(Armed {
            coupon: coupon.clone(),
            expires_at: now + TTL,
        });
        coupon
    }

    /// Whether `offered` names a live coupon — and spends it if it does.
    ///
    /// Three properties, each of them load-bearing:
    ///
    /// * **It consumes only on a match.** A caller spraying wrong values
    ///   cannot disarm a coupon somebody is about to redeem, which a
    ///   "consume whatever was offered" spelling would allow.
    /// * **It returns `bool`, not the coupon or a reason.** No caller can
    ///   build an oracle out of it, and `#[must_use]` means no caller can
    ///   accidentally ignore it either.
    /// * **It takes a [`LocalRequest`].** The witness cannot be forged
    ///   because it cannot be constructed outside
    ///   [`crate::api::http::local_request`], so deleting the locality check
    ///   at the call site is a *compile error* rather than a test somebody
    ///   deletes alongside it.
    #[must_use]
    pub fn redeem(&self, _witness: &LocalRequest, offered: &str, now: Instant) -> bool {
        let mut armed = self.armed.lock().unwrap_or_else(PoisonError::into_inner);
        let found = armed.iter().position(|entry| {
            entry.expires_at > now && constant_time_eq(entry.coupon.as_bytes(), offered.as_bytes())
        });
        match found {
            Some(index) => {
                armed.remove(index);
                true
            }
            None => false,
        }
    }
}

/// The only refusal this module ever gives.
///
/// **Byte-identical for every reason**: absent coupon, malformed coupon,
/// expired, wrong, already spent, wrong verb, missing `X-Storyhook`, not
/// local. A route that hands out a credential must not also answer questions
/// about which part of the attempt was wrong — a 404-vs-403-vs-410 split here
/// would let a caller enumerate live coupons by their refusals rather than by
/// redeeming them.
///
/// One function rather than seven call sites spelling the same literal, so
/// `tests` can assert **equality of the replies** rather than agreement of
/// their status codes.
fn refusal() -> Reply {
    text_reply(403, "Forbidden")
}

/// Intercepts the two handoff routes before a `Job` is ever built. `None`
/// means "not one of ours" — the caller's ordinary routing continues
/// unchanged.
///
/// Answered here, off the store thread, for the same reason `GET /api/events`
/// is: it needs nothing from the store, so it must never queue behind the
/// dispatcher pool. A browser redeeming a coupon on page load, behind eight
/// long-running commands, would time out for no reason at all.
///
/// `now` is passed in rather than read here so the whole module — gate
/// included, not just the registry — is clock-testable: a test can arm at one
/// instant and attempt to redeem two hundred seconds later without sleeping.
///
/// `tokens`/`cookie_name`/`wall_now` are SH-255's addition: a successful
/// redemption also mints a short-TTL named token and sets it as a cookie,
/// alongside the [`SessionRegistry`] capability this route already issued —
/// see [`handle_redeem`]'s own doc for why both are issued for now.
#[allow(clippy::too_many_arguments)]
pub fn intercept(
    segments: &[&str],
    method: &Method,
    headers: &[Header],
    trusted_hosts: &TrustedHosts,
    token: &str,
    loopback: bool,
    now: Instant,
    registry: &HandoffRegistry,
    sessions: &SessionRegistry,
    tokens: &TokenRegistry,
    cookie_name: &str,
    wall_now: DateTime<Utc>,
) -> Option<Reply> {
    match segments {
        ["api", "v1", "handoff"] => Some(handle_arm(method, headers, token, now, registry)),
        ["handoff"] => Some(handle_redeem(
            method,
            headers,
            trusted_hosts,
            loopback,
            now,
            registry,
            sessions,
            tokens,
            cookie_name,
            wall_now,
        )),
        _ => None,
    }
}

/// `POST /api/v1/handoff` — arm a coupon for whoever is about to open a
/// browser.
///
/// The [`token_ok`] check is a **labelled belt-and-braces duplicate**:
/// [`crate::api::rpc::admission`] has already refused this request if it
/// arrived off-loopback or without the token, and it runs first in
/// [`crate::daemon::serve`]'s worker. Spelled again here so that this
/// module's own gate is complete when read on its own, and so a future
/// reordering of the worker cannot silently turn arming into an unauthenticated
/// route. The composition is an AND, so the duplicate can only ever refuse
/// more, never less.
fn handle_arm(
    method: &Method,
    headers: &[Header],
    token: &str,
    now: Instant,
    registry: &HandoffRegistry,
) -> Reply {
    if !matches!(method, Method::Post) || !token_ok(headers, token) {
        return refusal();
    }
    json_reply(
        200,
        serde_json::json!({ "coupon": registry.arm(now) }).to_string(),
    )
    .no_store()
}

/// `POST /handoff` — spend a coupon for a scoped dashboard capability.
///
/// **Not the master token** (SH-254). What the browser gets is a
/// [`crate::api::session`] capability: the board's own write surface, honoured
/// only on this machine, revocable without restarting anything. Until SH-254
/// this handed over the daemon's token itself, which made a dashboard tab able
/// to delete every project on the machine and to reach `POST /api/v1/invoke` —
/// every verb the CLI has. That was transport done correctly, and it was not
/// least privilege.
///
/// The redemption *gate* did not move an inch for this: the same five conjuncts
/// in the same order decide it. All that changed is the value on the other side
/// — and that [`redemption_admitted`] now hands back the [`LocalRequest`] it
/// built rather than a `bool`, so the capability is issued against the witness
/// this request already earned instead of deriving locality a second time.
///
/// `no_store` rather than `no_cache`: `no-cache` permits a browser to *store*
/// a response and revalidate it, which is the wrong instruction for the one
/// reply in this daemon that carries a credential.
///
/// # Also sets a cookie now (SH-255), transitionally alongside the capability
///
/// A successful redemption additionally mints a short-TTL
/// [`crate::api::tokens::HANDOFF_TTL`] named token and sets it as a cookie —
/// the mechanism the whole named-token model is replacing the capability
/// above with. Both are issued for now: `admission.rs` does not yet accept
/// the cookie as a credential for anything (that lands next), so a redemption
/// that stopped returning the capability today would leave a freshly opened
/// tab unable to do a single write until the remaining steps land. The
/// capability, `token_exempt`, and this dual-issuance all go away together
/// once the model is fully switched over.
///
/// Minting is non-fatal, the same resilience idiom [`crate::web::armed_handoff`]'s
/// own doc describes for arming: if the registry is at capacity or the coupon
/// header is unreadable, the capability redemption still succeeds and no
/// cookie is set — a degraded outcome identical to what redemption already
/// was before this, never a failure this route did not have before.
#[allow(clippy::too_many_arguments)]
fn handle_redeem(
    method: &Method,
    headers: &[Header],
    trusted_hosts: &TrustedHosts,
    loopback: bool,
    now: Instant,
    registry: &HandoffRegistry,
    sessions: &SessionRegistry,
    tokens: &TokenRegistry,
    cookie_name: &str,
    wall_now: DateTime<Utc>,
) -> Reply {
    let Some(witness) =
        redemption_admitted(method, headers, trusted_hosts, loopback, now, registry)
    else {
        return refusal();
    };
    let reply = json_reply(
        200,
        serde_json::json!({ "session": sessions.issue(&witness, now) }).to_string(),
    )
    .no_store();
    match mint_handoff_token(headers, tokens, wall_now, now) {
        Some(minted) => reply.with_cookie(crate::api::tokens::set_cookie_header(
            cookie_name,
            &minted.secret,
            crate::api::tokens::HANDOFF_TTL.num_seconds(),
        )),
        None => reply,
    }
}

/// Mints a token for a just-redeemed coupon, named from a slice of the
/// coupon's own high-entropy value (`handoff-<12 hex chars>`) so it needs no
/// separate randomness source and is vanishingly unlikely to collide with
/// another live name — [`TokenRegistry::mint`] refuses a genuine collision
/// rather than silently overwriting, so this degrades to `None` rather than
/// clobbering an existing token in the astronomically unlikely case it does.
///
/// `None` on any failure — an unreadable coupon header (should not happen;
/// [`redemption_admitted`] already required one), the registry at capacity,
/// or a name collision. See [`handle_redeem`]'s doc for why a failure here
/// must not fail the redemption itself.
fn mint_handoff_token(
    headers: &[Header],
    tokens: &TokenRegistry,
    wall_now: DateTime<Utc>,
    mono_now: Instant,
) -> Option<crate::api::tokens::MintedToken> {
    let coupon = header_value(headers, COUPON_HEADER)?;
    let name = format!("handoff-{}", &coupon[..12.min(coupon.len())]);
    tokens
        .mint(name, wall_now, mono_now, crate::api::tokens::HANDOFF_TTL)
        .ok()
}

/// Whether a redemption is admitted — and, if it is, the coupon is now spent
/// and the caller's proof of locality is handed back.
///
/// Returns the [`LocalRequest`] rather than a `bool` (SH-254) so the capability
/// issued on the other side is issued against the witness *this* request
/// earned. Deriving locality twice would be two chances to derive it
/// differently, and the second derivation would be the one nobody reads.
///
/// One named predicate with one truth table, the `token_exempt` idiom: a
/// tokenless credential-issuing route may not be defended more weakly than
/// the tokenless *read* exemption is.
///
/// # The conjuncts, and what each one is for
///
/// 1. **`Post`, affirmatively.** Spelled `matches!` rather than
///    `!matches!(Get)` for the reason SH-250 spells its own verb conjunct
///    affirmatively: every other method this daemon parses would otherwise
///    inherit the route the day anything routed one.
/// 2. **`X-Storyhook` is present.** A custom header forces a CORS preflight
///    on any cross-origin `fetch`, which this daemon never answers with
///    `Access-Control-Allow-*`, so the browser blocks the real request — and
///    an HTML `<form>` cannot set custom headers at all. This is the same
///    defence [`crate::api::http::mutation_guard_ok`] rests half its weight
///    on, and it is why `tests/handoff_endpoint.rs` pins the *absence* of
///    every CORS header on this route rather than assuming it.
/// 3. **A coupon was offered**, in [`COUPON_HEADER`].
/// 4. **The caller is local** — [`local_request`], the same four conjuncts
///    SH-250's read exemption derives locality from, and the only way to
///    obtain the [`LocalRequest`] the next line needs.
/// 5. **The coupon is live** — and is spent by asking.
///
/// **The order is the contract.** Redemption consumes, so it must be the last
/// question asked: any conjunct evaluated after it would be a way to burn a
/// coupon on a request that was going to be refused anyway.
fn redemption_admitted(
    method: &Method,
    headers: &[Header],
    trusted_hosts: &TrustedHosts,
    loopback: bool,
    now: Instant,
    registry: &HandoffRegistry,
) -> Option<LocalRequest> {
    if !matches!(method, Method::Post) {
        return None;
    }
    header_value(headers, "X-Storyhook")?;
    let offered = header_value(headers, COUPON_HEADER)?;
    let witness = local_request(headers, trusted_hosts, loopback)?;
    registry.redeem(&witness, offered, now).then_some(witness)
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

    /// This module's routes, with a fresh capability registry behind them.
    ///
    /// **Shadows [`super::intercept`] on purpose.** SH-254 gave it one more
    /// parameter — where a redeemed coupon's capability is issued — and every
    /// test below is about the *gate*, which that change did not touch. Routing
    /// them through a helper keeps SH-251's whole truth table byte-identical
    /// across the change, which is what makes "the gate did not move" checkable
    /// by reading the diff rather than by taking somebody's word for it.
    #[allow(clippy::too_many_arguments)]
    fn intercept(
        segments: &[&str],
        method: &Method,
        headers: &[Header],
        trusted_hosts: &TrustedHosts,
        token: &str,
        loopback: bool,
        now: Instant,
        registry: &HandoffRegistry,
    ) -> Option<Reply> {
        super::intercept(
            segments,
            method,
            headers,
            trusted_hosts,
            token,
            loopback,
            now,
            registry,
            &SessionRegistry::new(),
            &TokenRegistry::new(Utc::now(), now),
            "storyhook_test",
            Utc::now(),
        )
    }

    /// A witness, for the registry tests that need one and are not about the
    /// gate that produces it.
    fn witness() -> LocalRequest {
        local_request(&headers(&[("Host", "127.0.0.1")]), &trusted_hosts(), true)
            .expect("a loopback request with a loopback Host is local")
    }

    // --- The registry: worthlessness, asserted rather than assumed ---

    #[test]
    fn a_coupon_is_thirty_two_hex_characters() {
        let coupon = HandoffRegistry::new().arm(Instant::now());
        assert_eq!(coupon.len(), 32);
        assert!(
            coupon
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
    }

    #[test]
    fn a_coupon_redeems_once() {
        // The load-bearing residue assertion. What Chrome writes to disk is a
        // value that stops working the first time it is used, so the durable
        // row is a record of nothing.
        let registry = HandoffRegistry::new();
        let now = Instant::now();
        let coupon = registry.arm(now);
        assert!(registry.redeem(&witness(), &coupon, now));
        assert!(
            !registry.redeem(&witness(), &coupon, now),
            "a spent coupon must never redeem again"
        );
    }

    #[test]
    fn a_coupon_expires() {
        // The other half of worthlessness: a coupon nobody ever spends is
        // dead on its own schedule, so a browser-history row survives the
        // value it records by at most TTL.
        let registry = HandoffRegistry::new();
        let now = Instant::now();
        let coupon = registry.arm(now);
        assert!(!registry.redeem(&witness(), &coupon, now + TTL));
        assert!(!registry.redeem(&witness(), &coupon, now + TTL + Duration::from_secs(1)));
        // And it really was live a moment before, so the assertions above are
        // expiry rather than a coupon that never worked at all.
        assert!(registry.redeem(&witness(), &coupon, now + TTL - Duration::from_secs(1)));
    }

    #[test]
    fn a_spray_of_wrong_values_does_not_disarm_a_live_coupon() {
        // `redeem` consumes only on a match. The alternative spelling — take
        // the front entry and compare it — would let anything that can reach
        // loopback burn a coupon a browser is milliseconds from spending.
        let registry = HandoffRegistry::new();
        let now = Instant::now();
        let coupon = registry.arm(now);
        for attempt in 0..64 {
            assert!(!registry.redeem(&witness(), &format!("{attempt:032x}"), now));
        }
        assert!(
            registry.redeem(&witness(), &coupon, now),
            "the real coupon must still be live after a spray of wrong ones"
        );
    }

    #[test]
    fn a_ninth_arm_evicts_the_oldest_and_only_the_oldest() {
        let registry = HandoffRegistry::new();
        let now = Instant::now();
        let coupons: Vec<String> = (0..MAX_LIVE).map(|_| registry.arm(now)).collect();
        registry.arm(now);

        assert!(
            !registry.redeem(&witness(), &coupons[0], now),
            "the oldest coupon must be evicted by the ninth arm"
        );
        for (index, coupon) in coupons.iter().enumerate().skip(1) {
            assert!(
                registry.redeem(&witness(), coupon, now),
                "coupon {index} must survive the ninth arm"
            );
        }
    }

    #[test]
    fn arming_sweeps_what_has_already_expired() {
        // Expiry alone would leave a dead entry occupying one of MAX_LIVE
        // slots until eight more arms pushed it out, which is how a bound
        // becomes a leak.
        let registry = HandoffRegistry::new();
        let now = Instant::now();
        let stale: Vec<String> = (0..MAX_LIVE).map(|_| registry.arm(now)).collect();
        let later = now + TTL + Duration::from_secs(1);
        let fresh = registry.arm(later);

        for coupon in &stale {
            assert!(!registry.redeem(&witness(), coupon, later));
        }
        assert!(
            registry.redeem(&witness(), &fresh, later),
            "the coupon armed after the sweep must be live"
        );
    }

    // --- The redemption gate: one admitted case, one test per conjunct ---

    /// What the dashboard actually sends to redeem: the CSRF header, the
    /// coupon, and the loopback `Host` a browser at 127.0.0.1 supplies.
    fn redeem_headers(coupon: &str) -> Vec<Header> {
        headers(&[
            ("X-Storyhook", "1"),
            (COUPON_HEADER, coupon),
            ("Host", "127.0.0.1:3456"),
        ])
    }

    /// A redemption attempt on the loopback listener.
    fn redeem(headers: &[Header], registry: &HandoffRegistry, now: Instant) -> Reply {
        intercept(
            &path_segments(REDEEM_PATH),
            &Method::Post,
            headers,
            &trusted_hosts(),
            TOKEN,
            true,
            now,
            registry,
        )
        .expect("the redemption path is this module's")
    }

    /// An armed registry, and the coupon it armed.
    fn armed(now: Instant) -> (HandoffRegistry, String) {
        let registry = HandoffRegistry::new();
        let coupon = registry.arm(now);
        (registry, coupon)
    }

    #[test]
    fn a_local_post_with_a_live_coupon_gets_a_capability_and_never_the_token() {
        // SH-254's headline: this used to answer with the daemon's master
        // token. What comes back now is a scoped capability, and the assertion
        // is written in both directions on purpose — that the reply *is* a
        // capability, and that the token appears nowhere in it. The second half
        // is the one that would notice a well-meaning "include the token too,
        // for compatibility".
        let now = Instant::now();
        let (registry, coupon) = armed(now);
        let sessions = SessionRegistry::new();
        let tokens = TokenRegistry::new(Utc::now(), now);
        let reply = super::intercept(
            &path_segments(REDEEM_PATH),
            &Method::Post,
            &redeem_headers(&coupon),
            &trusted_hosts(),
            TOKEN,
            true,
            now,
            &registry,
            &sessions,
            &tokens,
            "storyhook_test",
            Utc::now(),
        )
        .expect("the redemption path is this module's");

        assert_eq!(reply.status, 200);
        assert!(
            !reply.body().contains(TOKEN),
            "the redemption reply carried the master token: {}",
            reply.body()
        );

        // And what it did carry is a capability this daemon will honour.
        let issued: String = serde_json::from_str::<serde_json::Value>(reply.body())
            .expect("the reply is JSON")
            .get("session")
            .and_then(|v| v.as_str())
            .expect("the reply names a session")
            .to_string();
        assert!(
            sessions.present(&witness(), &issued, now),
            "the capability handed to the browser must be one this daemon knows"
        );

        // SH-255: a named token is also minted and set as a cookie, alongside
        // the capability above -- the mechanism replacing it.
        let cookie = reply.cookie().expect("a redemption must set a cookie");
        assert!(cookie.starts_with("storyhook_test="), "{cookie}");
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("SameSite=Strict"), "{cookie}");
        assert!(cookie.contains("Path=/"), "{cookie}");
        assert!(!cookie.contains("Domain="), "{cookie}");
        let secret = cookie
            .split(';')
            .next()
            .and_then(|kv| kv.split_once('='))
            .map(|(_, v)| v)
            .expect("a name=value pair");
        assert!(
            tokens.validate(secret, Utc::now(), now).is_some(),
            "the cookie's value must be a token this daemon's registry recognizes"
        );
        assert!(
            !reply.body().contains(secret),
            "the minted secret must never also appear in the JSON body"
        );
    }

    #[test]
    fn conjunct_1_a_get_is_refused() {
        let now = Instant::now();
        let (registry, coupon) = armed(now);
        let reply = intercept(
            &path_segments(REDEEM_PATH),
            &Method::Get,
            &redeem_headers(&coupon),
            &trusted_hosts(),
            TOKEN,
            true,
            now,
            &registry,
        )
        .expect("the redemption path is this module's");
        assert_eq!(reply, refusal());
        // And the coupon was not spent by the attempt.
        assert!(registry.redeem(&witness(), &coupon, now));
    }

    #[test]
    fn conjunct_2_a_missing_csrf_header_is_refused() {
        let now = Instant::now();
        let (registry, coupon) = armed(now);
        let mut without = redeem_headers(&coupon);
        without.retain(|header| !header.field.equiv("X-Storyhook"));
        assert_eq!(redeem(&without, &registry, now), refusal());
        assert!(registry.redeem(&witness(), &coupon, now));
    }

    #[test]
    fn conjunct_3_a_missing_coupon_is_refused() {
        let now = Instant::now();
        let (registry, coupon) = armed(now);
        let mut without = redeem_headers(&coupon);
        without.retain(|header| !header.field.equiv(COUPON_HEADER));
        assert_eq!(redeem(&without, &registry, now), refusal());
        assert!(registry.redeem(&witness(), &coupon, now));
    }

    #[test]
    fn conjunct_4_the_same_redemption_on_the_tailnet_listener_is_refused() {
        // The conjunct that makes this route safe to serve without a token:
        // only a caller on this machine may spend a coupon, so a tailnet peer
        // that somehow learned one still cannot exchange it.
        let now = Instant::now();
        let (registry, coupon) = armed(now);
        let reply = intercept(
            &path_segments(REDEEM_PATH),
            &Method::Post,
            &redeem_headers(&coupon),
            &trusted_hosts(),
            TOKEN,
            false,
            now,
            &registry,
        )
        .expect("the redemption path is this module's");
        assert_eq!(reply, refusal());
        assert!(registry.redeem(&witness(), &coupon, now));
    }

    #[test]
    fn conjunct_4_a_rebound_host_is_refused() {
        // DNS rebinding: the page is same-origin with http://127.0.0.1:PORT,
        // so it can read whatever comes back — including, without this
        // conjunct, the master token.
        let now = Instant::now();
        let (registry, coupon) = armed(now);
        let mut rebound = redeem_headers(&coupon);
        rebound.retain(|header| !header.field.equiv("Host"));
        rebound.push(Header::from_bytes("Host", "evil.example:3456").unwrap());
        assert_eq!(redeem(&rebound, &registry, now), refusal());
        assert!(registry.redeem(&witness(), &coupon, now));
    }

    #[test]
    fn conjunct_5_an_unknown_coupon_is_refused() {
        let now = Instant::now();
        let (registry, coupon) = armed(now);
        assert_eq!(
            redeem(&redeem_headers(&"f".repeat(32)), &registry, now),
            refusal()
        );
        assert!(registry.redeem(&witness(), &coupon, now));
    }

    #[test]
    fn conjunct_5_an_expired_coupon_is_refused() {
        let now = Instant::now();
        let (registry, coupon) = armed(now);
        assert_eq!(
            redeem(&redeem_headers(&coupon), &registry, now + TTL),
            refusal()
        );
    }

    #[test]
    fn conjunct_5_a_spent_coupon_is_refused() {
        let now = Instant::now();
        let (registry, coupon) = armed(now);
        assert_eq!(redeem(&redeem_headers(&coupon), &registry, now).status, 200);
        assert_eq!(redeem(&redeem_headers(&coupon), &registry, now), refusal());
    }

    #[test]
    fn every_refusal_is_byte_identical() {
        // Asserted as equality of whole replies rather than as seven status
        // checks: a route that hands out a credential must not answer
        // questions about *which* part of an attempt was wrong.
        let now = Instant::now();
        let (registry, coupon) = armed(now);

        let mut no_csrf = redeem_headers(&coupon);
        no_csrf.retain(|header| !header.field.equiv("X-Storyhook"));
        let mut no_coupon = redeem_headers(&coupon);
        no_coupon.retain(|header| !header.field.equiv(COUPON_HEADER));
        let mut rebound = redeem_headers(&coupon);
        rebound.retain(|header| !header.field.equiv("Host"));
        rebound.push(Header::from_bytes("Host", "evil.example").unwrap());
        let mut forwarded = redeem_headers(&coupon);
        forwarded.push(Header::from_bytes("X-Forwarded-For", "203.0.113.1").unwrap());

        let refusals = [
            // wrong verb
            intercept(
                &path_segments(REDEEM_PATH),
                &Method::Get,
                &redeem_headers(&coupon),
                &trusted_hosts(),
                TOKEN,
                true,
                now,
                &registry,
            )
            .expect("claimed"),
            // no witness: the wrong listener
            intercept(
                &path_segments(REDEEM_PATH),
                &Method::Post,
                &redeem_headers(&coupon),
                &trusted_hosts(),
                TOKEN,
                false,
                now,
                &registry,
            )
            .expect("claimed"),
            redeem(&no_csrf, &registry, now),
            redeem(&no_coupon, &registry, now),
            redeem(&rebound, &registry, now),
            redeem(&forwarded, &registry, now),
            // malformed
            redeem(&redeem_headers("not-a-coupon"), &registry, now),
            // wrong
            redeem(&redeem_headers(&"a".repeat(32)), &registry, now),
            // expired
            redeem(&redeem_headers(&coupon), &registry, now + TTL),
        ];
        for (index, reply) in refusals.iter().enumerate() {
            assert_eq!(*reply, refusal(), "refusal {index} differs");
        }

        // Already-spent, which needs the coupon actually spent first — and
        // proves every refusal above left it unspent.
        assert_eq!(redeem(&redeem_headers(&coupon), &registry, now).status, 200);
        assert_eq!(redeem(&redeem_headers(&coupon), &registry, now), refusal());
    }

    // --- Arming ---

    #[test]
    fn arming_needs_the_token() {
        let registry = HandoffRegistry::new();
        let now = Instant::now();
        let armed = |headers: &[Header]| {
            intercept(
                &path_segments(ARM_PATH),
                &Method::Post,
                headers,
                &trusted_hosts(),
                TOKEN,
                true,
                now,
                &registry,
            )
            .expect("the arm path is this module's")
        };
        assert_eq!(armed(&[]), refusal());
        assert_eq!(
            armed(&headers(&[("X-Storyhook-Token", "not-the-token")])),
            refusal()
        );

        let reply = armed(&headers(&[("X-Storyhook-Token", TOKEN)]));
        assert_eq!(reply.status, 200);
    }

    #[test]
    fn arming_is_a_post() {
        let registry = HandoffRegistry::new();
        let reply = intercept(
            &path_segments(ARM_PATH),
            &Method::Get,
            &headers(&[("X-Storyhook-Token", TOKEN)]),
            &trusted_hosts(),
            TOKEN,
            true,
            Instant::now(),
            &registry,
        )
        .expect("the arm path is this module's");
        assert_eq!(reply, refusal());
    }

    #[test]
    fn nothing_else_is_this_modules() {
        let registry = HandoffRegistry::new();
        for path in [
            "/",
            "/api/repos",
            "/api/handoff",
            "/api/v1/hello",
            "/handoff/redeem",
            "/handoffs",
        ] {
            assert!(
                intercept(
                    &path_segments(path),
                    &Method::Post,
                    &[],
                    &trusted_hosts(),
                    TOKEN,
                    true,
                    Instant::now(),
                    &registry,
                )
                .is_none(),
                "{path} must fall through to ordinary routing"
            );
        }
    }

    #[test]
    fn the_path_constants_are_what_the_router_matches() {
        // The constants are the contract `daemon::lifecycle` and the
        // dashboard's own JavaScript are written against; the `match` above
        // is what actually decides. This is the only thing tying them
        // together, since a `match` cannot pattern on a `const`.
        assert_eq!(path_segments(ARM_PATH), ["api", "v1", "handoff"]);
        assert_eq!(path_segments(REDEEM_PATH), ["handoff"]);
    }
}
