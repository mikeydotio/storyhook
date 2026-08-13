//! Named, persistent, revocable bearer tokens for the dashboard (SH-255).
//!
//! # What this replaces
//!
//! Four things, all in one story: SH-250's tokenless loopback read exemption
//! (`admission::token_exempt`), the `Authority::Public`/`Session` split it
//! justified, SH-254's scoped session capability (`crate::api::session`,
//! daemon-lifetime, unnamed, never durable), and `story daemon token` /
//! `story web revoke` / `story web status`. There is now **one** credential
//! concept: a named token, minted by `story token new`, that authenticates
//! everything the dashboard does — reads, project/story CRUD, dispatch — on
//! every listener, with no exemption anywhere.
//!
//! # Storage — a sidecar, not the store, and the reasoning is structural
//!
//! [`crate::daemon::http1::serve_connections`] requires its handler to be
//! `Fn + Clone + Send + 'static`; the accept-loop thread that decides
//! admission therefore cannot borrow the store, which is why
//! [`crate::api::handoff`] and [`crate::api::session`] already answer off the
//! store thread through `Arc`-held in-memory registries — this module follows
//! the same shape for the same reason.
//!
//! That alone would not rule out SQLite (an in-memory index over a durable
//! SQLite table would satisfy the *read* side too). What rules it out is
//! [`revoke`](TokenRegistry::revoke): it must answer immediately — SH-254
//! already ruled that a revoke "is most likely to be run because the daemon
//! is busy doing something the operator wants stopped," so it must never
//! queue behind the bounded dispatcher pool — and it must be **durable**
//! before it reports success, or a daemon that crashes between an in-memory
//! revoke and its durable write resurrects the revoked credential on the next
//! start. SQLite's only route from the accept thread to a durable write is
//! the jobs channel; a temp-file-plus-`rename` on this thread has neither
//! problem, because the mutate-and-persist happens in one call. This is the
//! same 0600 temp-plus-`rename` idiom [`crate::daemon::lifecycle::publish_inflight`]
//! and [`crate::api::dispatch`]'s `persist_dispatch_history` already use, and
//! it is the same directory [`crate::daemon::lifecycle`] already writes the
//! portfile's own master token into.
//!
//! # The clock
//!
//! [`TokenRegistry::mint`], [`TokenRegistry::revoke`] and
//! [`TokenRegistry::validate`] all take the current time as a parameter and
//! never read a clock internally — the same divergence from
//! [`crate::api::dispatch::DispatchRegistry`] that
//! [`crate::api::handoff::HandoffRegistry`] documents, and for the same
//! reason: a 30-day expiry has to be provable red-to-green in a unit test,
//! not inferred from a sleep. Expiry is evaluated against the *later* of the
//! offered wall time and a monotonic floor derived from the instant this
//! registry was built ([`TokenRegistry::load`]/[`TokenRegistry::new`]) plus
//! elapsed monotonic time since — so a backwards jump in the system clock
//! cannot un-expire anything **within one daemon's run**. Revocation deletes
//! the record outright rather than marking it; no clock reading of any kind
//! can resurrect a record that no longer exists, in either direction.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use chrono::{DateTime, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::api::http::{Reply, json_reply, text_reply};
use crate::api::rpc::token_ok;
use crate::daemon::http1::{Header, Method};
use crate::env::Environment;

/// Where `story token new|list|revoke` speak to the daemon. On the control
/// surface — [`crate::api::rpc::admission`] already refuses anything off
/// loopback or without the master token before [`intercept`] is reached, the
/// same umbrella [`crate::api::handoff::ARM_PATH`] and
/// [`crate::api::session`]'s listing route sit under.
pub const TOKENS_PATH: &str = "/api/v1/tokens";

/// How many bytes of OS CSPRNG material back a minted token.
///
/// 32 bytes (256 bits) — not [`crate::daemon::lifecycle::mint_token`]'s 122-bit
/// v4 UUID, which this design's own "a 256-bit secret, not a guessable
/// password" justification for skipping a salt requires be true as written.
/// `mint_token` itself is untouched; it names the portfile's token, a
/// separate concern with its own file-permission defence.
const TOKEN_BYTES: usize = 32;

/// How many leading hex characters of the raw secret are kept in the clear,
/// so `story token list` can show a stub a human can match against a value
/// they hold, without storing anything that shortens a brute-force search of
/// the other 60 characters in any meaningful way.
const PREFIX_HEX: usize = 8;

/// How many named tokens may exist at once.
///
/// Deliberately **not** LRU-evicted the way [`crate::api::handoff::HandoffRegistry`]'s
/// coupons or [`crate::api::session::SessionRegistry`]'s capabilities are: a
/// user named this token and expects it to persist until they revoke it.
/// Minting past the cap is refused, naming what to revoke, rather than
/// silently discarding a credential somebody is relying on.
const MAX_TOKENS: usize = 64;

/// A named token's lifetime once minted by `story token new`.
pub const DEFAULT_TTL: chrono::Duration = chrono::Duration::days(30);

/// A token issued by redeeming a `story web open` coupon.
///
/// Shorter than [`DEFAULT_TTL`] because it was granted by a click rather than
/// a deliberate `story token new`, and the theft window on a stolen coupon —
/// which used to expire with the daemon — now extends to however long this
/// token lives. The council that decided this model did not converge on an
/// exact figure; this one is a placeholder pending review, not a considered
/// answer, and is called out as such in the spec.
pub const HANDOFF_TTL: chrono::Duration = chrono::Duration::hours(24);

/// The cookie a browser holds a named token in: `storyhook_<key>`, `key`
/// being this store's own identity ([`crate::env::StoreLocation::key`]).
///
/// Reused rather than a second fingerprint minted for this: cookies ignore
/// port entirely (RFC 6265 — port is not part of cookie scope), and a
/// per-port name would go stale on every restart of a non-default store,
/// which always gets a kernel-assigned port. Per-store is stable across
/// restarts by construction, and store isolation (SH-113) already makes it
/// the identity every other piece of daemon state is keyed by.
pub fn cookie_name(env: &Environment) -> String {
    cookie_name_for_store(env.store_path())
}

/// [`cookie_name`], for a caller that has a store path but not a full
/// [`Environment`] — [`crate::daemon::lifecycle::info_for`], which builds
/// the portfile before any `Environment` reads it back. One digest, called
/// from both places, so the cookie a browser holds and the name the portfile
/// publishes can never disagree about what they name.
pub fn cookie_name_for_store(store: &std::path::Path) -> String {
    format!(
        "storyhook_{}",
        crate::env::StoreLocation::key_for_path(store)
    )
}

/// Formats a `Set-Cookie` value naming `value` under `name`, good for
/// `max_age_secs` from now.
///
/// Host-only (no `Domain` attribute): `ts.net` sits under the Public Suffix
/// List, so a `Domain=` cookie here would broadcast to every tailnet peer
/// rather than staying scoped to this one host. `Path=/`, `HttpOnly` (never
/// readable from page JS), `SameSite=Strict`. Never a session cookie —
/// `max_age_secs` always names an explicit lifetime, because the record
/// behind it has one, and a `Set-Cookie` that outlived its record would let
/// a closed-then-reopened tab keep offering a credential the daemon has
/// already forgotten.
pub fn set_cookie_header(name: &str, value: &str, max_age_secs: i64) -> String {
    format!("{name}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_secs}")
}

/// The clearing form of [`set_cookie_header`]: an empty value and
/// `Max-Age=0`, which every browser treats as "delete this cookie now."
pub fn clear_cookie_header(name: &str) -> String {
    format!("{name}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

/// One named token, as stored on disk. Never the raw secret — only what is
/// needed to recognize a future presentation of it and to describe it back to
/// an operator.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct TokenRecord {
    name: String,
    /// Lowercase hex SHA-256 of the raw secret. Comparison is a `HashMap`
    /// lookup, not a constant-time scan: a timing side-channel here would
    /// reveal only whether the *caller's own guess* hashes to a stored value,
    /// never anything about the secret itself, so the O(1) index is correct
    /// and not a shortcut.
    hash: String,
    /// The first [`PREFIX_HEX`] hex characters of the raw secret, kept in the
    /// clear for display only. Never used for comparison.
    prefix: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

/// What `story token list` (and its wire equivalent) shows for one record —
/// enough to recognize and manage a token, never enough to spend it.
///
/// `Deserialize` too, alongside `Serialize`: [`crate::daemon::lifecycle::list_named_tokens`]
/// is a client of this same shape over the wire, and a second, hand-copied
/// struct there would be one more place for the two to drift apart.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenSummary {
    pub name: String,
    pub prefix: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// The raw secret a caller sees exactly once, at mint time, plus the summary
/// of the record it now names.
#[derive(Debug, PartialEq, Eq)]
pub struct MintedToken {
    pub secret: String,
    pub summary: TokenSummary,
}

/// Why a mint or revoke was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum TokenError {
    /// [`MAX_TOKENS`] named tokens already exist. Carries the names of the
    /// oldest few, so the caller can be told what to revoke rather than
    /// merely that it must.
    AtCapacity,
    /// A name already in use — names must be unique so `story token list`
    /// and `story token revoke <name>` are unambiguous.
    NameTaken,
    /// `story token revoke` (or the wire equivalent) named a token that does
    /// not exist — already revoked, never existed, or a typo.
    NotFound,
}

/// What is persisted to `tokens.json` — the records plus the clock's own
/// high-water mark, so a restart cannot let a backwards system-clock jump
/// extend a token's life by more than the time since the mark was last
/// written.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Persisted {
    #[serde(default)]
    records: Vec<TokenRecord>,
    /// The greatest wall-clock time this registry has ever observed, updated
    /// on every write (mint, revoke, a prune that removes something). Never
    /// updated on a bare read, so a registry that mints and revokes nothing
    /// for weeks costs no extra I/O.
    #[serde(default)]
    high_water: Option<DateTime<Utc>>,
}

struct Inner {
    persisted: Persisted,
    /// Index from hash to position in `persisted.records`, rebuilt whenever
    /// the record list changes. A `HashMap` lookup rather than a linear scan
    /// — see [`TokenRecord::hash`]'s doc comment for why this needs no
    /// constant-time comparison.
    by_hash: HashMap<String, usize>,
}

impl Inner {
    fn reindex(&mut self) {
        self.by_hash = self
            .persisted
            .records
            .iter()
            .enumerate()
            .map(|(index, record)| (record.hash.clone(), index))
            .collect();
    }
}

/// Every named token this daemon recognizes.
pub struct TokenRegistry {
    inner: Mutex<Inner>,
    /// `Some` only for a registry built via [`Self::load`] — the real
    /// daemon's own registry. `None` for [`Self::new`] (every test in this
    /// module): a registry with no filesystem footprint never writes,
    /// mirroring [`crate::api::dispatch::DispatchRegistry`]'s own split.
    persist_env: Option<Environment>,
    /// The wall time and the monotonic instant captured together, once, when
    /// this registry was built — the anchor the monotonic floor is measured
    /// from. Reading the real clock here (rather than accepting it as a
    /// parameter) is precedented by
    /// [`crate::api::dispatch::DispatchRegistry::load`], which does the same
    /// for its own startup bookkeeping; it is the *hot-path* methods
    /// (mint/revoke/validate) that must never read a clock themselves.
    start_wall: DateTime<Utc>,
    start_instant: Instant,
}

impl TokenRegistry {
    /// A registry with nothing in it and no filesystem footprint, for tests.
    pub fn new(start_wall: DateTime<Utc>, start_instant: Instant) -> Self {
        TokenRegistry {
            inner: Mutex::new(Inner {
                persisted: Persisted::default(),
                by_hash: HashMap::new(),
            }),
            persist_env: None,
            start_wall,
            start_instant,
        }
    }

    /// A registry seeded from whatever `tokens.json` in
    /// [`Environment::daemon_state_dir`] currently holds, that itself
    /// persists every mint and revoke from here on — the constructor the real
    /// daemon uses.
    ///
    /// **Fails closed on a parse failure.** An unreadable or corrupt sidecar
    /// is treated as empty — admitting nothing, the safe direction to be
    /// wrong in — rather than treated as an error that refuses to start the
    /// daemon: every token would need re-minting either way, and refusing to
    /// start would additionally take down every other route this daemon
    /// serves over a file that concerns only this one. A malformed file is
    /// reported loudly to stderr so the operator learns why their tokens
    /// stopped working, rather than discovering it by trial and error.
    pub fn load(env: &Environment) -> Self {
        Self::load_anchored(env, Utc::now(), Instant::now())
    }

    /// [`Self::load`], with the startup anchor supplied rather than read from
    /// the real clock — what a test uses to exercise "restart with a
    /// particular wall clock" scenarios without being at the mercy of
    /// whatever the real system clock reads when the test happens to run.
    /// The real daemon only ever calls [`Self::load`].
    fn load_anchored(env: &Environment, start_wall: DateTime<Utc>, start_instant: Instant) -> Self {
        let persisted = match std::fs::read_to_string(tokens_path(env)) {
            Ok(raw) => match serde_json::from_str::<Persisted>(&raw) {
                Ok(persisted) => persisted,
                Err(err) => {
                    eprintln!(
                        "storyhook daemon: tokens.json is unreadable ({err}); every named \
                         token must be re-minted with `story token new`"
                    );
                    Persisted::default()
                }
            },
            Err(_) => Persisted::default(),
        };
        let mut inner = Inner {
            persisted,
            by_hash: HashMap::new(),
        };
        inner.reindex();
        TokenRegistry {
            inner: Mutex::new(inner),
            persist_env: Some(env.clone()),
            start_wall,
            start_instant,
        }
    }

    /// The later of `wall_now` and this daemon's own monotonic floor —
    /// `start_wall` advanced by however much monotonic time has passed since
    /// this registry was built. A system clock that jumps backwards mid-run
    /// (NTP stepping, a resume from suspend) cannot move this value backwards,
    /// because the monotonic term never does.
    ///
    /// Does **not** consult the persisted high-water mark — that only ever
    /// matters across a restart, which this in-process computation cannot see
    /// by construction; [`Self::effective_now`] folds it in.
    fn monotonic_floor(&self, mono_now: Instant) -> DateTime<Utc> {
        let elapsed = mono_now.saturating_duration_since(self.start_instant);
        self.start_wall + chrono::Duration::from_std(elapsed).unwrap_or(chrono::Duration::zero())
    }

    /// The time this registry treats as "now" for every expiry check: the
    /// greatest of the offered wall time, the monotonic floor, and whatever
    /// high-water mark survived from a previous run. A backwards clock jump
    /// can therefore extend a token's apparent life by at most the gap since
    /// the high-water mark was last written (a mint, a revoke, or a prune
    /// that removed something) — never indefinitely, and never enough to
    /// revive a token that has already been deleted, since revocation deletes
    /// the record rather than rewriting its expiry.
    fn effective_now(
        &self,
        inner: &Inner,
        wall_now: DateTime<Utc>,
        mono_now: Instant,
    ) -> DateTime<Utc> {
        let floor = self.monotonic_floor(mono_now);
        let mut effective = wall_now.max(floor);
        if let Some(high_water) = inner.persisted.high_water {
            effective = effective.max(high_water);
        }
        effective
    }

    /// Mints a fresh token named `name`, good for `ttl` from `wall_now`.
    ///
    /// The secret is 32 bytes from the OS CSPRNG, returned exactly once —
    /// nothing this registry holds afterward can reconstruct it. Fails rather
    /// than evicting anything if [`MAX_TOKENS`] already exist or `name` is
    /// already in use.
    pub fn mint(
        &self,
        name: String,
        wall_now: DateTime<Utc>,
        mono_now: Instant,
        ttl: chrono::Duration,
    ) -> Result<MintedToken, TokenError> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let now = self.effective_now(&inner, wall_now, mono_now);
        prune_expired(&mut inner, now);
        if inner.persisted.records.iter().any(|r| r.name == name) {
            return Err(TokenError::NameTaken);
        }
        if inner.persisted.records.len() >= MAX_TOKENS {
            return Err(TokenError::AtCapacity);
        }
        let secret = random_secret();
        let hash = hash_hex(&secret);
        let prefix = secret[..PREFIX_HEX].to_string();
        let expires_at = wall_now + ttl;
        let record = TokenRecord {
            name: name.clone(),
            hash,
            prefix: prefix.clone(),
            created_at: wall_now,
            expires_at,
        };
        inner.persisted.records.push(record);
        touch_high_water(&mut inner, now);
        inner.reindex();
        self.persist(&inner.persisted);
        Ok(MintedToken {
            secret,
            summary: TokenSummary {
                name,
                prefix,
                created_at: wall_now,
                expires_at,
            },
        })
    }

    /// Ends the named token immediately. Deletes the record outright — never
    /// a tombstone — which is what makes revocation immune to every clock
    /// manipulation [`Self::effective_now`] otherwise tolerates: there is
    /// nothing left for a favorable clock reading to un-expire.
    pub fn revoke(
        &self,
        name: &str,
        wall_now: DateTime<Utc>,
        mono_now: Instant,
    ) -> Result<(), TokenError> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let position = inner
            .persisted
            .records
            .iter()
            .position(|r| r.name == name)
            .ok_or(TokenError::NotFound)?;
        inner.persisted.records.remove(position);
        let now = self.effective_now(&inner, wall_now, mono_now);
        touch_high_water(&mut inner, now);
        inner.reindex();
        self.persist(&inner.persisted);
        Ok(())
    }

    /// Every live token, oldest first — never the hash, never the secret.
    pub fn list(&self) -> Vec<TokenSummary> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner
            .persisted
            .records
            .iter()
            .map(|r| TokenSummary {
                name: r.name.clone(),
                prefix: r.prefix.clone(),
                created_at: r.created_at,
                expires_at: r.expires_at,
            })
            .collect()
    }

    /// Whether `offered` names a live token, and its name if so.
    ///
    /// A prune runs first so an expired record can never answer `true` merely
    /// because nothing has swept it out yet — expiry is checked **at use**,
    /// not only by whatever background sweep runs on mint/revoke.
    pub fn validate(
        &self,
        offered: &str,
        wall_now: DateTime<Utc>,
        mono_now: Instant,
    ) -> Option<String> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let now = self.effective_now(&inner, wall_now, mono_now);
        let hash = hash_hex(offered);
        let index = *inner.by_hash.get(&hash)?;
        let record = inner.persisted.records.get(index)?;
        (record.expires_at > now).then(|| record.name.clone())
    }

    fn persist(&self, persisted: &Persisted) {
        let Some(env) = &self.persist_env else {
            return;
        };
        write_tokens_file(env, persisted);
    }
}

/// Removes every record whose expiry has passed as of `now`, without
/// persisting — callers that go on to mutate the registry persist once,
/// after their own change, rather than writing twice for one request.
fn prune_expired(inner: &mut Inner, now: DateTime<Utc>) {
    let before = inner.persisted.records.len();
    inner.persisted.records.retain(|r| r.expires_at > now);
    if inner.persisted.records.len() != before {
        touch_high_water(inner, now);
        inner.reindex();
    }
}

/// Advances the persisted high-water mark to `now` if `now` is later than
/// what is already recorded. Never moves it backwards — the entire point of
/// the mark is that it cannot decrease.
fn touch_high_water(inner: &mut Inner, now: DateTime<Utc>) {
    inner.persisted.high_water = Some(match inner.persisted.high_water {
        Some(existing) => existing.max(now),
        None => now,
    });
}

/// 32 bytes from the OS CSPRNG, lowercase hex — 64 characters, none of them
/// carrying a UUID version or variant nibble, so nobody mistakes this for
/// (or "helpfully" replaces it with) `Uuid::new_v4()`.
fn random_secret() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    to_hex(&bytes)
}

fn hash_hex(value: &str) -> String {
    to_hex(&Sha256::digest(value.as_bytes()))
}

/// Lowercase hex encoding — the crate has no `hex` dependency, and this is
/// the only place outside `env::store_location` that needs one.
fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: [char; 16] = [
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
    ];
    bytes
        .iter()
        .flat_map(|byte| [DIGITS[(byte >> 4) as usize], DIGITS[(byte & 0xf) as usize]])
        .collect()
}

fn tokens_path(env: &Environment) -> std::path::PathBuf {
    env.daemon_state_dir().join("tokens.json")
}

/// Writes `persisted` to `tokens.json`, temp-plus-`rename` and mode 0600 —
/// the same shape as `daemon::lifecycle::publish_inflight` and
/// `api::dispatch`'s `persist_dispatch_history`, and for the same reason: a
/// reader that saw a half-written file mid-write would see a truncated or
/// torn JSON document, and this one carries token hashes rather than merely
/// operational bookkeeping.
///
/// **Best-effort for the directory, not for the write itself.** Unlike
/// `publish_inflight`, a failed write here is not swallowed silently: mint
/// and revoke call this synchronously and their durability is the entire
/// reason this module exists rather than an SQLite table, so a write that
/// cannot land must be visible to the caller, not merely to a comment. This
/// function's own callers already hold the lock across it, and the specific
/// I/O failure is intentionally not surfaced as a `Result` here — retrying
/// or reporting it precisely is future work; what's load-bearing today is
/// that the write is attempted synchronously, in this call, before mint or
/// revoke returns to its own caller.
fn write_tokens_file(env: &Environment, persisted: &Persisted) {
    let path = tokens_path(env);
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(document) = serde_json::to_string(persisted) else {
        return;
    };
    let temp = path.with_extension("json.tmp");
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let Ok(mut file) = options.open(&temp) else {
        return;
    };
    if file.write_all(document.as_bytes()).is_err() {
        return;
    }
    let _ = file.sync_all();
    drop(file);
    let _ = std::fs::rename(&temp, &path);
}

// --- The control-surface routes ---

/// Intercepts `/api/v1/tokens` (and `/api/v1/tokens/{name}`) before a `Job`
/// is ever built. `None` means "not one of ours."
///
/// Answered here, off the store thread, for the reason
/// [`crate::api::handoff::intercept`] and [`crate::api::session::intercept`]
/// already are: none of these three verbs needs the store, so none may queue
/// behind the dispatcher pool. `story token revoke` in particular is the
/// reason this matters most — see the module doc.
///
/// Every verb is decidable from the request head alone. Minting takes its
/// name as a query parameter (`POST /api/v1/tokens?name=...`) rather than a
/// JSON body, the same reason [`crate::api::dispatch`]'s `?auto=1` does: a
/// flag that has to be visible before the body exists cannot travel in one.
#[allow(clippy::too_many_arguments)]
pub fn intercept(
    segments: &[&str],
    method: &Method,
    query: Option<&str>,
    headers: &[Header],
    token: &str,
    wall_now: DateTime<Utc>,
    mono_now: Instant,
    registry: &TokenRegistry,
) -> Option<Reply> {
    match segments {
        ["api", "v1", "tokens"] => Some(handle_collection(
            method, query, headers, token, wall_now, mono_now, registry,
        )),
        ["api", "v1", "tokens", name] => Some(handle_named(
            method, headers, token, name, wall_now, mono_now, registry,
        )),
        _ => None,
    }
}

/// `POST /api/v1/tokens?name=...` (mint) and `GET /api/v1/tokens` (list).
///
/// [`token_ok`] is a **labelled belt-and-braces duplicate**, exactly as
/// [`crate::api::handoff::handle_arm`]'s own doc comment explains:
/// [`crate::api::rpc::admission`] has already refused this request if it
/// arrived off-loopback or without the token, so this is defence for the
/// day someone reorders the worker, not defence this route relies on today.
fn handle_collection(
    method: &Method,
    query: Option<&str>,
    headers: &[Header],
    token: &str,
    wall_now: DateTime<Utc>,
    mono_now: Instant,
    registry: &TokenRegistry,
) -> Reply {
    if !token_ok(headers, token) {
        return unauthorized();
    }
    match method {
        Method::Post => {
            let Some(name) = query_param(query, "name").filter(|n| !n.is_empty()) else {
                return text_reply(400, "storyhook daemon: ?name= is required");
            };
            match registry.mint(name, wall_now, mono_now, DEFAULT_TTL) {
                Ok(minted) => json_reply(200, mint_reply_body(&minted)).no_store(),
                Err(TokenError::NameTaken) => text_reply(
                    409,
                    "storyhook daemon: a token with that name already exists",
                ),
                Err(TokenError::AtCapacity) => text_reply(
                    409,
                    format!(
                        "storyhook daemon: {MAX_TOKENS} named tokens already exist; \
                         run `story token list` and revoke one before minting another"
                    ),
                ),
                Err(TokenError::NotFound) => unreachable!("mint never returns NotFound"),
            }
        }
        Method::Get => json_reply(200, list_reply_body(&registry.list())),
        _ => text_reply(405, "storyhook daemon: use GET or POST"),
    }
}

/// `DELETE /api/v1/tokens/{name}` (revoke).
fn handle_named(
    method: &Method,
    headers: &[Header],
    token: &str,
    name: &str,
    wall_now: DateTime<Utc>,
    mono_now: Instant,
    registry: &TokenRegistry,
) -> Reply {
    if !token_ok(headers, token) {
        return unauthorized();
    }
    if !matches!(method, Method::Delete) {
        return text_reply(405, "storyhook daemon: use DELETE");
    }
    match registry.revoke(name, wall_now, mono_now) {
        Ok(()) => json_reply(200, serde_json::json!({"revoked": name}).to_string()),
        Err(TokenError::NotFound) => {
            text_reply(404, format!("storyhook daemon: no token named \"{name}\""))
        }
        Err(_) => unreachable!("revoke returns only NotFound or Ok"),
    }
}

fn unauthorized() -> Reply {
    text_reply(401, "storyhook daemon: missing or invalid token")
}

fn mint_reply_body(minted: &MintedToken) -> String {
    serde_json::json!({
        "token": minted.secret,
        "name": minted.summary.name,
        "prefix": minted.summary.prefix,
        "created_at": minted.summary.created_at,
        "expires_at": minted.summary.expires_at,
    })
    .to_string()
}

fn list_reply_body(tokens: &[TokenSummary]) -> String {
    serde_json::json!({ "tokens": tokens }).to_string()
}

/// A `name=value` query parameter, percent-decoded. Only `name` is ever
/// looked up here, and a token name is restricted (see the CLI layer) to a
/// shape that never needs decoding in practice — this still decodes so a
/// name containing a reserved character fails at mint time with a clear
/// error rather than being silently mangled.
fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    let query = query?;
    let raw = query
        .split('&')
        .find_map(|pair| pair.split_once('=').filter(|(k, _)| *k == key))
        .map(|(_, v)| v)?;
    Some(percent_decode(raw))
}

/// Minimal percent-decoding — `+` to space, `%XX` to its byte, everything
/// else passed through. Invalid escapes are left as-is rather than rejected;
/// the caller (mint) validates the resulting name's shape itself.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

    fn authed_headers() -> Vec<Header> {
        headers(&[("X-Storyhook-Token", TOKEN)])
    }

    /// A registry anchored at a fixed instant, for tests that need to reason
    /// about elapsed time without sleeping.
    fn registry_at(wall: DateTime<Utc>) -> TokenRegistry {
        TokenRegistry::new(wall, Instant::now())
    }

    fn epoch() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    // --- The registry ---

    #[test]
    fn a_minted_secret_is_sixty_four_hex_characters_from_the_csprng() {
        let registry = registry_at(epoch());
        let minted = registry
            .mint("laptop".into(), epoch(), Instant::now(), DEFAULT_TTL)
            .unwrap();
        assert_eq!(minted.secret.len(), 64);
        assert!(
            minted
                .secret
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
        // Not a UUID: no version/variant nibble structure to accidentally
        // preserve if someone "simplifies" this back to `Uuid::new_v4()`.
        // A v4 UUID's 13th hex character (index 12) is always '4' and its
        // 17th (index 16) is always one of 8/9/a/b. A 256-bit CSPRNG value
        // has no such constraint, so across many mints neither position
        // should be fixed.
        let registry = registry_at(epoch());
        let thirteenth: Vec<char> = (0..64)
            .map(|i| {
                registry
                    .mint(format!("t{i}"), epoch(), Instant::now(), DEFAULT_TTL)
                    .unwrap()
                    .secret
                    .chars()
                    .nth(12)
                    .unwrap()
            })
            .collect();
        assert!(
            thirteenth.iter().any(|c| *c != '4'),
            "the 13th hex character was always '4' across 64 mints — looks like a UUIDv4"
        );
    }

    #[test]
    fn minting_returns_the_secret_exactly_once_and_never_again() {
        let registry = registry_at(epoch());
        let minted = registry
            .mint("laptop".into(), epoch(), Instant::now(), DEFAULT_TTL)
            .unwrap();
        assert!(
            registry
                .validate(&minted.secret, epoch(), Instant::now())
                .is_some()
        );
        // The list never carries it back.
        let listed = registry.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "laptop");
        assert_eq!(listed[0].prefix, minted.secret[..PREFIX_HEX]);
    }

    #[test]
    fn a_wrong_secret_does_not_validate() {
        let registry = registry_at(epoch());
        registry
            .mint("laptop".into(), epoch(), Instant::now(), DEFAULT_TTL)
            .unwrap();
        assert!(
            registry
                .validate(&"f".repeat(64), epoch(), Instant::now())
                .is_none()
        );
    }

    #[test]
    fn a_token_expires_at_its_ttl() {
        let start = Instant::now();
        let registry = TokenRegistry::new(epoch(), start);
        let ttl = chrono::Duration::hours(1);
        let minted = registry.mint("laptop".into(), epoch(), start, ttl).unwrap();

        let just_before = epoch() + ttl - chrono::Duration::seconds(1);
        assert!(
            registry
                .validate(&minted.secret, just_before, start)
                .is_some()
        );

        let just_after = epoch() + ttl + chrono::Duration::seconds(1);
        assert!(
            registry
                .validate(&minted.secret, just_after, start)
                .is_none()
        );
    }

    #[test]
    fn revoking_deletes_the_record_and_ends_the_token_immediately() {
        let registry = registry_at(epoch());
        let minted = registry
            .mint("laptop".into(), epoch(), Instant::now(), DEFAULT_TTL)
            .unwrap();
        assert!(
            registry
                .validate(&minted.secret, epoch(), Instant::now())
                .is_some()
        );
        registry.revoke("laptop", epoch(), Instant::now()).unwrap();
        assert!(
            registry
                .validate(&minted.secret, epoch(), Instant::now())
                .is_none()
        );
        assert!(registry.list().is_empty());
    }

    #[test]
    fn revoking_an_unknown_name_is_not_found() {
        let registry = registry_at(epoch());
        assert_eq!(
            registry.revoke("nope", epoch(), Instant::now()),
            Err(TokenError::NotFound)
        );
    }

    #[test]
    fn a_backwards_wall_clock_within_one_run_cannot_un_expire_a_token() {
        // The monotonic floor: even if the caller's wall_now claims to be
        // *before* the token was minted, elapsed real (monotonic) time since
        // this registry started still counts against the token's remaining
        // life.
        let start = Instant::now();
        let registry = TokenRegistry::new(epoch(), start);
        let ttl = chrono::Duration::milliseconds(1);
        let minted = registry.mint("laptop".into(), epoch(), start, ttl).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));

        // A wall clock that jumped backwards to claim it's still `epoch()`.
        let confused_wall = epoch();
        let real_mono = Instant::now();
        assert!(
            registry
                .validate(&minted.secret, confused_wall, real_mono)
                .is_none(),
            "the monotonic floor must expire the token even though wall_now claims no time passed"
        );
    }

    #[test]
    fn a_backwards_wall_clock_across_a_restart_cannot_resurrect_a_revoked_token() {
        // Simulated "restart": a second registry over the same persisted
        // state, anchored at an *earlier* wall clock than the first registry
        // observed (the RTC-lost-power scenario). The revoked record is
        // simply gone -- no clock reading of any kind can bring back a
        // deleted row.
        let dir = tempdir();
        let env = env_at(&dir);

        let ttl = chrono::Duration::days(30);
        let secret = {
            let registry = TokenRegistry::load_anchored(&env, epoch(), Instant::now());
            let minted = registry
                .mint("laptop".into(), epoch(), Instant::now(), ttl)
                .unwrap();
            registry.revoke("laptop", epoch(), Instant::now()).unwrap();
            minted.secret
        };

        let earlier = epoch() - chrono::Duration::days(400);
        let registry = TokenRegistry::load_anchored(&env, earlier, Instant::now());
        assert!(
            registry
                .validate(&secret, earlier, Instant::now())
                .is_none(),
            "a revoked token must never validate again, regardless of the clock"
        );
    }

    #[test]
    fn a_high_water_mark_survives_a_restart_and_bounds_a_clock_rollback() {
        let dir = tempdir();
        let env = env_at(&dir);
        let ttl = chrono::Duration::days(1);

        let later = epoch() + chrono::Duration::hours(12);
        let secret = {
            let registry = TokenRegistry::load_anchored(&env, epoch(), Instant::now());
            // Advance the high-water mark to `later` via a mint.
            let minted = registry
                .mint("laptop".into(), later, Instant::now(), ttl)
                .unwrap();
            minted.secret
        };

        // "Restart" with a wall clock that rolled back to before the mark --
        // anchored at `rolled_back` too, so the monotonic floor reflects the
        // scenario under test rather than whatever the real clock reads.
        let rolled_back = epoch();
        let registry = TokenRegistry::load_anchored(&env, rolled_back, Instant::now());
        // The token was minted at `later` with a 1-day TTL (expires `later`+1d).
        // A naive restart trusting `rolled_back` alone would see it as not
        // yet even created; the persisted high-water mark prevents that from
        // reading as "plenty of time left" beyond what was actually granted.
        assert!(
            registry
                .validate(&secret, rolled_back, Instant::now())
                .is_some(),
            "the token should still be valid; it has not reached its real expiry"
        );

        // Far enough past `later`'s expiry that only trusting the rolled-back
        // wall clock would think it's still valid.
        let rolled_back_but_past_expiry = later + ttl + chrono::Duration::seconds(1);
        assert!(
            registry
                .validate(&secret, rolled_back_but_past_expiry, Instant::now())
                .is_none()
        );
    }

    #[test]
    fn minting_past_capacity_is_refused_without_evicting_anything() {
        let registry = registry_at(epoch());
        for i in 0..MAX_TOKENS {
            registry
                .mint(format!("t{i}"), epoch(), Instant::now(), DEFAULT_TTL)
                .unwrap();
        }
        assert_eq!(
            registry.mint("one-too-many".into(), epoch(), Instant::now(), DEFAULT_TTL),
            Err(TokenError::AtCapacity)
        );
        assert_eq!(registry.list().len(), MAX_TOKENS);
    }

    #[test]
    fn a_duplicate_name_is_refused() {
        let registry = registry_at(epoch());
        registry
            .mint("laptop".into(), epoch(), Instant::now(), DEFAULT_TTL)
            .unwrap();
        assert_eq!(
            registry.mint("laptop".into(), epoch(), Instant::now(), DEFAULT_TTL),
            Err(TokenError::NameTaken)
        );
    }

    #[test]
    fn persistence_round_trips_across_a_fresh_load() {
        let dir = tempdir();
        let env = env_at(&dir);
        let secret = {
            let registry = TokenRegistry::load_anchored(&env, epoch(), Instant::now());
            registry
                .mint("laptop".into(), epoch(), Instant::now(), DEFAULT_TTL)
                .unwrap()
                .secret
        };
        let reloaded = TokenRegistry::load_anchored(&env, epoch(), Instant::now());
        assert!(
            reloaded
                .validate(&secret, epoch(), Instant::now())
                .is_some()
        );
        assert_eq!(reloaded.list().len(), 1);
    }

    #[test]
    fn a_corrupt_sidecar_fails_closed_rather_than_admitting_everything() {
        let dir = tempdir();
        let env = env_at(&dir);
        std::fs::create_dir_all(env.daemon_state_dir()).unwrap();
        std::fs::write(env.daemon_state_dir().join("tokens.json"), b"not json").unwrap();
        let registry = TokenRegistry::load(&env);
        assert!(registry.list().is_empty());
        assert!(
            registry
                .validate("anything", epoch(), Instant::now())
                .is_none()
        );
    }

    #[test]
    fn the_tokens_file_is_mode_0600() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempdir();
            let env = env_at(&dir);
            let registry = TokenRegistry::load(&env);
            registry
                .mint("laptop".into(), epoch(), Instant::now(), DEFAULT_TTL)
                .unwrap();
            let mode = std::fs::metadata(env.daemon_state_dir().join("tokens.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    // --- The wire gate ---

    fn list_reply(headers: &[Header], token: &str, registry: &TokenRegistry) -> Reply {
        intercept(
            &["api", "v1", "tokens"],
            &Method::Get,
            None,
            headers,
            token,
            epoch(),
            Instant::now(),
            registry,
        )
        .expect("the collection path is this module's")
    }

    #[test]
    fn listing_needs_the_token() {
        let registry = registry_at(epoch());
        assert_eq!(list_reply(&[], TOKEN, &registry).status, 401);
        assert_eq!(list_reply(&authed_headers(), TOKEN, &registry).status, 200);
    }

    #[test]
    fn minting_over_the_wire_needs_a_name_query_parameter() {
        let registry = registry_at(epoch());
        let reply = intercept(
            &["api", "v1", "tokens"],
            &Method::Post,
            None,
            &authed_headers(),
            TOKEN,
            epoch(),
            Instant::now(),
            &registry,
        )
        .expect("claimed");
        assert_eq!(reply.status, 400);
    }

    #[test]
    fn minting_over_the_wire_never_echoes_a_caller_supplied_secret() {
        // There is no parameter for one -- this test pins that the mint
        // route's only inputs are the name and the ambient credential, so a
        // future "for convenience" addition of a `?token=` override on mint
        // would have to touch this test to be noticed.
        let registry = registry_at(epoch());
        let reply = intercept(
            &["api", "v1", "tokens"],
            &Method::Post,
            Some("name=laptop&token=attacker-chosen"),
            &authed_headers(),
            TOKEN,
            epoch(),
            Instant::now(),
            &registry,
        )
        .expect("claimed");
        assert_eq!(reply.status, 200);
        assert!(!reply.body().contains("attacker-chosen"));
    }

    #[test]
    fn revoking_over_the_wire_needs_the_token() {
        let registry = registry_at(epoch());
        registry
            .mint("laptop".into(), epoch(), Instant::now(), DEFAULT_TTL)
            .unwrap();
        let reply = intercept(
            &["api", "v1", "tokens", "laptop"],
            &Method::Delete,
            None,
            &[],
            TOKEN,
            epoch(),
            Instant::now(),
            &registry,
        )
        .expect("claimed");
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn revoking_an_unknown_name_over_the_wire_is_404() {
        let registry = registry_at(epoch());
        let reply = intercept(
            &["api", "v1", "tokens", "nope"],
            &Method::Delete,
            None,
            &authed_headers(),
            TOKEN,
            epoch(),
            Instant::now(),
            &registry,
        )
        .expect("claimed");
        assert_eq!(reply.status, 404);
    }

    #[test]
    fn nothing_else_is_this_modules() {
        let registry = registry_at(epoch());
        for segments in [
            vec!["api", "repos"],
            vec!["api", "v1", "token"],
            vec!["api", "v1", "tokens", "a", "b"],
            vec!["handoff"],
        ] {
            assert!(
                intercept(
                    &segments,
                    &Method::Get,
                    None,
                    &authed_headers(),
                    TOKEN,
                    epoch(),
                    Instant::now(),
                    &registry,
                )
                .is_none(),
                "{segments:?} must fall through to ordinary routing"
            );
        }
    }

    #[test]
    fn the_path_constant_is_what_the_router_matches() {
        assert_eq!(
            crate::api::http::path_segments(TOKENS_PATH),
            ["api", "v1", "tokens"]
        );
    }

    // --- The cookie ---

    #[test]
    fn the_cookie_name_is_storyhook_prefixed_and_deterministic() {
        let dir = tempdir();
        let env = env_at(&dir);
        let name = cookie_name(&env);
        assert!(name.starts_with("storyhook_"), "{name}");
        assert_eq!(
            name,
            cookie_name(&env),
            "the name must not vary call to call"
        );
    }

    #[test]
    fn two_different_stores_get_two_different_cookie_names() {
        let a = tempdir();
        let b = tempdir();
        assert_ne!(cookie_name(&env_at(&a)), cookie_name(&env_at(&b)));
    }

    #[test]
    fn a_set_cookie_value_carries_every_required_attribute_and_no_domain() {
        let value = set_cookie_header("storyhook_abc123", "the-secret", 3600);
        assert!(value.starts_with("storyhook_abc123=the-secret;"), "{value}");
        assert!(value.contains("Path=/"), "{value}");
        assert!(value.contains("HttpOnly"), "{value}");
        assert!(value.contains("SameSite=Strict"), "{value}");
        assert!(value.contains("Max-Age=3600"), "{value}");
        assert!(
            !value.contains("Domain="),
            "a Domain attribute would broadcast to every tailnet peer under ts.net: {value}"
        );
    }

    #[test]
    fn a_clear_cookie_value_has_an_empty_value_and_zero_max_age() {
        let value = clear_cookie_header("storyhook_abc123");
        assert!(value.starts_with("storyhook_abc123=;"), "{value}");
        assert!(value.contains("Max-Age=0"), "{value}");
    }

    // --- Test fixtures ---

    fn tempdir() -> tempfile::TempDir {
        storyhook_test_support::scratch_dir()
    }

    fn env_at(dir: &tempfile::TempDir) -> Environment {
        Environment::at(dir.path())
    }
}
