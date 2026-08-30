//! The dashboard's dispatch endpoint (SH-50).
//!
//! `POST /api/repos/{project}/story/{id}/dispatch` runs the same
//! `plugins/story/bin/story.sh dispatch` the CLI's `/story do` uses:
//! a git worktree, a tmux window, an agent started in plan mode on the
//! story. `GET .../dispatch/{handle}` polls the outcome. This is
//! deliberately not a reimplementation of dispatch inside the daemon — the
//! daemon only invokes the script, exactly as the story that specified this
//! endpoint requires.
//!
//! # Off the store thread, on purpose
//!
//! [`crate::daemon::serve::dispatch`] runs on a fixed pool of
//! `crate::daemon::serve::DISPATCHERS` threads that own the store between
//! them — every request beyond that many queues behind whichever
//! [`crate::api::rest::route`] or [`crate::api::rpc::route`] call is in
//! flight on the rest. A dispatch takes 15-35 seconds even on the happy path
//! (worktree creation, then waiting for claude's TUI to accept a pasted
//! prompt), and it gets there by making several of its own `story` CLI
//! calls — each of which, since store isolation landed, reaches this *same*
//! daemon over its own `/api/v1/invoke` connection, at `hook_depth` 0 (a
//! dispatch child sets nothing that would mark it otherwise). Answering a
//! dispatch request on a pool thread would therefore risk deadlock: with
//! [`MAX_RUNNING`] dispatches each occupying one pool thread, their nested
//! calls would have nowhere left to run.
//!
//! So this module is intercepted in [`crate::daemon::serve::worker`],
//! before a `Job` is ever built — the same place `GET /api/events` is
//! answered in full for the same reason, and the same shape
//! `crate::daemon::serve`'s hook-depth lane generalizes for every *nested*
//! request rather than only this one. The subprocess itself runs on its own
//! detached thread, tracked in a [`DispatchRegistry`] that a `GET` polls;
//! nothing here ever touches `Serving::store`.
//!
//! # A second gate beyond the dashboard's own
//!
//! The ordinary mutation guard (`X-Storyhook` + a trusted `Host`,
//! [`crate::api::http::mutation_guard_ok`]) defeats a browser's CSRF and
//! DNS-rebinding attempts, but it is not authentication — anything that can
//! set two headers passes it, including a `curl` from any host the tailnet
//! trusts. That was an acceptable gap for editing story fields; it is not
//! one for an endpoint that spawns a terminal session running an agent. So
//! dispatch requires the daemon's bearer token too
//! ([`crate::api::rpc::token_ok`], the same constant-time check
//! `/api/v1/*` already uses), on **both** listeners — including loopback,
//! for one code path and one test matrix rather than a loopback exemption
//! that would need its own justification and its own tests.
//!
//! Full rationale, reachability chains and the review's other findings are
//! in `docs/spec/dashboard-dispatch.md`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::daemon::http1::{Header, Method};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::api::admission::named_token_ok;
use crate::api::http::{Reply, TrustedHosts, mutation_guard_ok, text_reply};
use crate::api::rpc::token_ok;
use crate::api::tokens::TokenRegistry;
use crate::daemon::bus::{Change, ChangeBus};
use crate::env::Environment;
#[cfg(test)]
use crate::service::engine::{
    CHARTER_INERT_BANNED, charter_inert_violation, classify_dispatch_files,
    prompt_override_violation,
};
use crate::service::engine::{DispatchOutcome, DispatchOutcomeState, run_shell_dispatch};
use crate::store::EngineAgent;

/// At most this many dispatches may be running at once.
///
/// Not a defense against an unauthenticated caller — the token gate already
/// closes that door — but against a token holder's own accidents: a
/// retried click, or several dashboard tabs open on the same project.
///
/// `pub(crate)` so `daemon::serve::DISPATCHERS` can be derived against it —
/// each running dispatch's `story.sh` makes several nested `story` CLI calls
/// that arrive at `hook_depth` 0, so up to this many can occupy that many
/// dispatchers simultaneously, and the pool must exceed it.
pub(crate) const MAX_RUNNING: usize = 4;

/// How many finished records the registry keeps.
///
/// `pub` since SH-361: the dispatch log serves this number **as data**
/// ([`Retention::records`]) so the sentence the dashboard renders is this
/// constant by construction rather than a second copy of it in
/// `src/web_dashboard.html`. A hand-copied number on one side of a wire is
/// the drift class SH-136 has cost this project three times, and
/// `dashboard_mutation_deadline.rs` is the precedent for pinning the pair
/// rather than trusting them.
pub const RETAIN_FINISHED: usize = 32;

/// How long a finished record is kept regardless of how many newer ones
/// have arrived.
///
/// `pub` for the same reason as [`RETAIN_FINISHED`] — it reaches the browser
/// as [`Retention::seconds`].
pub const RETAIN_FOR: Duration = Duration::from_secs(30 * 60);

/// The agent host one dispatch launches through the shared Storyhook helper.
///
/// `claude` is intentionally the public token rather than the product's
/// longer `claude-code` name. The helper preserves that older spelling only
/// on its pre-existing environment compatibility seam; new HTTP and argv
/// contracts accept and emit the canonical values here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DispatchAgent {
    #[default]
    Claude,
    Codex,
}

impl DispatchAgent {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// The taxonomy behind a non-[`Ok`](DispatchState::Ok) terminal
/// [`DispatchRecord`] (SH-232) — read by [`classify`] from `story.sh`'s own
/// `reason` field (`refuse`/`refuse_with`, `plugins/story/lib/
/// session.sh`) instead of leaving it reachable only by parsing
/// [`DispatchRecord::payload`]. A plain `fail()` refusal (no claim word, not
/// ready — see `story.sh`'s own guard order) carries no `reason` field at
/// all, so its record's `reason` stays `None`; that is a real, meaningful
/// absence, not a gap this type papers over.
///
/// [`Other`](Self::Other) is the forward-compat escape hatch: a `story.sh`
/// newer than the daemon serving it can add a reason this binary has never
/// heard of (protocol-compatible additions don't bump
/// [`REQUIRED_DISPATCH_PROTOCOL`]), and dropping that string on the floor —
/// reporting bare `refused` with nothing further — is exactly the
/// context-dropping CLAUDE.md's error-handling rule forbids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchReason {
    /// `story.sh`'s claim compare-and-swap lost a race to another dispatch.
    ClaimConflict,
    /// The tmux pane could not be confirmed to be running the launch binary
    /// within the readiness gate's timeout (I1 DISPATCH-PROVEN-OCCUPANT).
    PaneNotReady,
    /// The pane was confirmed ready, but the pasted prompt never reached its
    /// input box — nothing was submitted (I2 SUBMIT-AFTER-RECEIPT).
    HandoffUndelivered,
    /// The prompt reached the input box, but submission was never
    /// confirmed. The claim and worktree are left in place: the agent may
    /// already be working.
    HandoffUnconfirmed,
    /// This daemon refused to dispatch at all, before ever running
    /// `story.sh`, because one of its own inherited `STORY_PROMPT` /
    /// `STORY_AUTO_PROMPT` / `STORY_PROMPT_EXTRA` values violates I4
    /// CHARTER-INERT (SH-232's runtime-enforcement rider). See
    /// [`prompt_override_violation`].
    UnsafePromptOverride,
    /// A reason string this binary does not recognize, carried verbatim
    /// rather than dropped.
    Other(String),
}

impl DispatchReason {
    /// `story.sh`'s own reason string for a known variant, or the carried
    /// string for [`Other`](Self::Other) — round-trips through
    /// [`Self::parse`] byte for byte.
    fn as_str(&self) -> &str {
        match self {
            Self::ClaimConflict => "claim-conflict",
            Self::PaneNotReady => "pane-not-ready",
            Self::HandoffUndelivered => "handoff-undelivered",
            Self::HandoffUnconfirmed => "handoff-unconfirmed",
            Self::UnsafePromptOverride => "unsafe-prompt-override",
            Self::Other(raw) => raw,
        }
    }

    /// Maps a `reason` string as `story.sh` (or this module's own
    /// [`prompt_override_violation`] refusal) emits it to a typed variant,
    /// never failing: an unrecognized string becomes
    /// [`Other`](Self::Other) rather than an error, so a shell newer than
    /// this binary is forward-compatible by construction.
    fn parse(raw: &str) -> Self {
        match raw {
            "claim-conflict" => Self::ClaimConflict,
            "pane-not-ready" => Self::PaneNotReady,
            "handoff-undelivered" => Self::HandoffUndelivered,
            "handoff-unconfirmed" => Self::HandoffUnconfirmed,
            "unsafe-prompt-override" => Self::UnsafePromptOverride,
            other => Self::Other(other.to_string()),
        }
    }
}

/// Serializes as the bare reason string (`"pane-not-ready"`, not
/// `{"PaneNotReady": null}`) so a dashboard reads it exactly as `story.sh`
/// wrote it, and [`Self::Other`] costs the wire format nothing extra either.
impl Serialize for DispatchReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// The inverse of the `Serialize` impl above — round-trips
/// [`DispatchRecord`] through [`persist_dispatch_history`] /
/// [`load_dispatch_history`] without a lossy detour through a generic JSON
/// enum representation.
impl<'de> Deserialize<'de> for DispatchReason {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(deserializer)?))
    }
}

/// One dispatch's lifecycle, as reported to a polling client.
///
/// `finished_at`, `payload`, `error` and `reason` are absent while
/// [`DispatchState`] is [`Running`](DispatchState::Running) —
/// `skip_serializing_if` rather than always emitting `null`, so a client's
/// presence check (`"finished_at" in record`) is the same idiom the rest of
/// this API already uses for an absent value. `#[serde(default)]` alongside
/// it on every one of those fields is what makes that omission round-trip:
/// [`load_dispatch_history`] deserializes exactly what
/// [`persist_dispatch_history`] wrote, and an absent key must not become a
/// parse error there.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DispatchRecord {
    /// Opaque id a client polls with. Never reused.
    pub handle: String,
    /// The project slug this dispatch was asked to act on.
    pub project: String,
    /// The story id this dispatch was asked to act on.
    pub story: String,
    /// Which provider the shared dispatch helper was told to launch. Missing
    /// in records persisted by older daemons, which therefore deserialize as
    /// Claude — the only provider those daemons could have launched.
    #[serde(default)]
    pub agent: DispatchAgent,
    /// Whether this dispatch runs `story.sh`'s autonomous charter (SH-208,
    /// SH-511): Plan mode is retained but approved automatically, and
    /// everything after it — deciding open questions, merging its own PR,
    /// closing the story, reclaiming its own worktree — runs unattended. Never
    /// `skip_serializing_if`, unlike the fields below: this is never
    /// absent, only ever true or false, for the lifetime of the record.
    pub auto: bool,
    pub state: DispatchState,
    /// RFC3339, when this record was created.
    pub started_at: String,
    /// RFC3339, set once `state` leaves [`Running`](DispatchState::Running).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// `story.sh`'s own JSON result, relayed verbatim — present for
    /// [`Ok`](DispatchState::Ok) and [`Refused`](DispatchState::Refused),
    /// since a refusal is a well-formed result the script chose to report,
    /// not a failure of the script itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Set only for [`Failed`](DispatchState::Failed): the script could not
    /// be run, did not finish, or exited without printing a result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The typed reason behind a [`Refused`](DispatchState::Refused) record
    /// (SH-232) — see [`DispatchReason`]. Absent for
    /// [`Ok`](DispatchState::Ok), for [`Failed`](DispatchState::Failed), and
    /// for a `fail()`-shaped refusal that carries no `reason` field at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<DispatchReason>,
}

/// Where one dispatch attempt stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchState {
    /// The subprocess is still running.
    Running,
    /// `story.sh` reported `"ok": true`.
    Ok,
    /// `story.sh` reported `"ok": false` — a well-formed business refusal
    /// (not ready, already in-progress, a claim conflict, a worktree
    /// collision, ...), relayed exactly as the CLI itself would report it.
    Refused,
    /// The script could not be found or run, was killed for overrunning the
    /// shared dispatcher timeout, or exited without printing a result.
    Failed,
}

/// The envelope every dispatch reply is wrapped in — the one shape
/// `POST`'s 202 and `GET`'s 200 both use, so a client's response handling
/// does not need to special-case which verb it came from.
#[derive(Serialize)]
struct DispatchEnvelope<'a> {
    result: &'static str,
    dispatch: &'a DispatchRecord,
}

fn envelope_json(record: &DispatchRecord) -> String {
    serde_json::to_string(&DispatchEnvelope {
        result: "ok",
        dispatch: record,
    })
    .expect("DispatchRecord holds no type that can fail to serialize")
}

/// One registry entry: the record a client sees, plus the bookkeeping it
/// does not — when a finished entry becomes eligible for eviction.
struct Entry {
    record: DispatchRecord,
    /// Set alongside `record.finished_at`; a monotonic clock rather than a
    /// second parse of `finished_at`, so eviction timing cannot be skewed
    /// by whatever produced that RFC3339 string.
    finished_instant: Option<Instant>,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    /// Story id -> handle, but ONLY while that dispatch is `Running`. This
    /// is what makes a repeated `POST` for the same story idempotent
    /// against a concurrently running attempt, and what a finished
    /// dispatch stops blocking: a later `POST` for the same story is a
    /// genuinely new attempt, which `story.sh`'s own already-in-progress
    /// guard (or ready-gate, or claim CAS) is the authority on, not this
    /// registry.
    running_by_story: HashMap<String, String>,
    /// Finished handles, oldest first — [`DispatchRegistry::evict`]'s
    /// working set. A running dispatch is never in here.
    finished_order: VecDeque<String>,
    /// How many finished records [`DispatchRegistry::evict`] has dropped
    /// since this registry was built (SH-361).
    ///
    /// **A floor, never a total, and the wire says so.** It counts only
    /// what *this* process collected: it starts at zero on every daemon
    /// restart, and [`DispatchRegistry::log`] filters expired-but-uncollected
    /// entries out of its answer without counting them here, because that
    /// read must not mutate anything (see that method). Both gaps
    /// under-report in the same direction, which is what makes "at least N"
    /// the only honest wording — SH-361's council accepted this counter
    /// solely on that condition, having first rejected an exact `evicted`
    /// count as "a silent cap wearing a badge".
    forgotten: usize,
}

/// The retention policy behind one [`DispatchLog`], reported so a reader is
/// told why the list ends where it does (SH-361).
///
/// This project's rule is that a bounded view which reads as complete is a
/// vacuous pass, so the log discloses the **rule** — which cannot be wrong —
/// and only then a floor on what has already gone.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Retention {
    /// [`RETAIN_FINISHED`], served as data rather than restated in the page.
    pub records: usize,
    /// [`RETAIN_FOR`] in whole seconds, likewise.
    pub seconds: u64,
    /// A **lower bound** on how many finished records are already gone. See
    /// [`Inner::forgotten`] for the two reasons it under-reports; a client
    /// must render it as "at least N", never as a count.
    pub forgotten: usize,
}

/// Everything `GET /api/dispatch-log` answers: the finished records this
/// daemon still retains, newest first, and the policy that bounds them.
#[derive(Serialize)]
struct DispatchLogEnvelope<'a> {
    result: &'static str,
    dispatches: &'a [DispatchRecord],
    retention: Retention,
}

/// The registry's answer to a dispatch-log read (SH-361).
pub struct DispatchLog {
    /// Finished records, **newest first** — `finished_order` reversed.
    pub dispatches: Vec<DispatchRecord>,
    /// The policy that bounds `dispatches`.
    pub retention: Retention,
}

/// What a `POST` should do about a request to dispatch `story`.
enum StartOutcome {
    /// No dispatch was running for this story; a new record was created
    /// with this handle, and the caller must now actually spawn it.
    Started(String),
    /// A dispatch for this story was already running; here is its handle.
    /// The caller must not spawn a second one.
    AlreadyRunning(String),
    /// `MAX_RUNNING` dispatches are already in flight, for other stories.
    AtCapacity,
}

/// Tracks every dispatch this daemon has started. Every record starts life
/// in memory only; [`Self::load`] (SH-232) is what makes a *finished* one
/// survive a restart, by seeding fresh in-memory state from
/// [`Environment::dispatch_history`] rather than by reaching across
/// restarts some other way — the persisted file is a snapshot [`Self::finish`]
/// writes, read back exactly once, at the moment a new registry is built.
pub struct DispatchRegistry {
    inner: Mutex<Inner>,
    /// `Some` only for a registry built via [`Self::load`] — the real
    /// daemon's own registry. `None` for every [`Self::new`] (every test in
    /// this module, and any future caller that wants a registry with no
    /// filesystem footprint at all): [`Self::finish`] persists nothing when
    /// this is `None`, so a plain `new()` stays exactly as pure and
    /// in-memory as it always was.
    persist_env: Option<Environment>,
}

impl DispatchRegistry {
    pub fn new() -> Self {
        DispatchRegistry {
            inner: Mutex::new(Inner::default()),
            persist_env: None,
        }
    }

    /// A registry seeded from whatever the previous daemon on this store
    /// last persisted (SH-232), and that itself persists every completion
    /// from here on — the constructor the real daemon uses; every test in
    /// this module keeps using the pure in-memory [`Self::new`] instead.
    ///
    /// Only ever loads a *terminal* record: [`Environment::dispatch_history`]'s
    /// own doc explains why a dispatch still
    /// [`Running`](DispatchState::Running) when the previous daemon exited
    /// is never in that file to begin with. Defensive rather than trusting
    /// (skips a `Running` record if one is somehow found anyway) because
    /// resurrecting a "running" dispatch this daemon never started, with no
    /// child process behind it, would strand a client polling a handle that
    /// can never move again.
    pub fn load(env: &Environment) -> Self {
        let mut registry = Self::new();
        {
            let mut inner = registry.inner.lock().expect("dispatch registry lock");
            for record in load_dispatch_history(env) {
                if record.state == DispatchState::Running {
                    continue;
                }
                inner.finished_order.push_back(record.handle.clone());
                inner.entries.insert(
                    record.handle.clone(),
                    Entry {
                        record,
                        // The real elapsed time since the *previous*
                        // daemon's finish is unrecoverable -- `Instant` is
                        // process-local and does not survive serialization.
                        // Treating a freshly-loaded record as "just
                        // finished" only widens its RETAIN_FOR window
                        // slightly across a restart; RETAIN_FINISHED's
                        // count-based cap is unaffected either way.
                        finished_instant: Some(Instant::now()),
                    },
                );
            }
        }
        registry.persist_env = Some(env.clone());
        registry
    }

    /// Reserves a slot for a new dispatch of `story` under `project`, or
    /// reports why one could not be reserved. `started_at` is supplied
    /// rather than read here so every clock read in this module comes from
    /// the caller's [`Environment`], matching how the rest of the daemon
    /// tells time.
    ///
    /// `auto` is only consulted on the [`Started`](StartOutcome::Started)
    /// path. A story already running is deduped by story id regardless of
    /// mode — a repeated `POST` (with or without `?auto=1`) for a story
    /// already dispatching returns that attempt's own handle rather than
    /// starting a second script, exactly as it did before SH-208 added a
    /// second mode. The returned record's `auto` therefore always reports
    /// which mode is *actually running*, which can disagree with what a
    /// later, deduped `POST` asked for — a caller that cares must poll
    /// `auto` on the handle it gets back, not assume it matches its own
    /// request.
    fn try_start_for_agent(
        &self,
        project: &str,
        story: &str,
        agent: DispatchAgent,
        auto: bool,
        started_at: String,
    ) -> StartOutcome {
        let mut inner = self.inner.lock().expect("dispatch registry lock");
        if let Some(existing) = inner.running_by_story.get(story) {
            return StartOutcome::AlreadyRunning(existing.clone());
        }
        if inner.running_by_story.len() >= MAX_RUNNING {
            return StartOutcome::AtCapacity;
        }
        let handle = uuid::Uuid::new_v4().simple().to_string();
        inner.entries.insert(
            handle.clone(),
            Entry {
                record: DispatchRecord {
                    handle: handle.clone(),
                    project: project.to_string(),
                    story: story.to_string(),
                    agent,
                    auto,
                    state: DispatchState::Running,
                    started_at,
                    finished_at: None,
                    payload: None,
                    error: None,
                    reason: None,
                },
                finished_instant: None,
            },
        );
        inner
            .running_by_story
            .insert(story.to_string(), handle.clone());
        StartOutcome::Started(handle)
    }

    #[cfg(test)]
    fn try_start(
        &self,
        project: &str,
        story: &str,
        auto: bool,
        started_at: String,
    ) -> StartOutcome {
        self.try_start_for_agent(project, story, DispatchAgent::Claude, auto, started_at)
    }

    /// A copy of `handle`'s record, or `None` if it never existed or has
    /// aged out.
    fn get(&self, handle: &str) -> Option<DispatchRecord> {
        self.inner
            .lock()
            .expect("dispatch registry lock")
            .entries
            .get(handle)
            .map(|entry| entry.record.clone())
    }

    /// Returns the current attempt for `story`, if any. This read happens
    /// before provider-specific helper resolution on a repeated POST: an
    /// already-running Claude attempt must still be reusable when a later
    /// caller asks for Codex on a machine where only the Claude plugin is
    /// installed (and vice versa). `try_start_for_agent` remains the atomic
    /// authority after resolution, closing the race between this advisory
    /// read and a concurrent first request.
    fn running_handle(&self, story: &str) -> Option<String> {
        self.inner
            .lock()
            .expect("dispatch registry lock")
            .running_by_story
            .get(story)
            .cloned()
    }

    /// Records `handle`'s outcome and releases its story back to the
    /// running set, so the next `POST` for that story starts fresh.
    /// `classification` takes [`run_child`]/[`classify`]'s own return shape
    /// directly (rather than four trailing parameters) so a caller passes
    /// through exactly what it got, and so this signature does not grow a
    /// fifth positional argument the next time [`Classification`] does.
    ///
    /// Persists the bounded finished set to disk (SH-232) when this
    /// registry was built via [`Self::load`] — the snapshot is taken, and
    /// the mutex released, before the write happens, so a slow or full
    /// filesystem never holds up `try_start`/`get`/`finish` on another
    /// thread (each holds `inner`'s lock too, from an HTTP handler's own
    /// call stack for the first two).
    ///
    /// Two concurrent `finish()` calls (up to [`MAX_RUNNING`] can overlap)
    /// therefore have no ordering guarantee between their two *disk writes*
    /// — each snapshot is correct as of its own mutation, but the file
    /// system could apply them in either order, so the persisted file could
    /// transiently reflect the earlier of the two. Deliberately not fixed
    /// by writing inside the lock: this is a snapshot of the in-memory
    /// registry, not the registry itself, so a stale disk copy self-heals
    /// on the very next dispatch to finish, whenever that is — and holding
    /// `inner`'s lock across a filesystem write would make an unrelated
    /// `POST`/`GET` briefly hostage to that write.
    fn finish(
        &self,
        handle: &str,
        story: &str,
        finished_at: String,
        classification: Classification,
    ) {
        let (state, payload, error, reason) = classification;
        let snapshot = {
            let mut inner = self.inner.lock().expect("dispatch registry lock");
            if let Some(entry) = inner.entries.get_mut(handle) {
                entry.record.state = state;
                entry.record.finished_at = Some(finished_at);
                entry.record.payload = payload;
                entry.record.error = error;
                entry.record.reason = reason;
                entry.finished_instant = Some(Instant::now());
            }
            inner.running_by_story.remove(story);
            inner.finished_order.push_back(handle.to_string());
            Self::evict(&mut inner);
            self.persist_env.is_some().then(|| {
                inner
                    .finished_order
                    .iter()
                    .filter_map(|h| inner.entries.get(h).map(|entry| entry.record.clone()))
                    .collect::<Vec<_>>()
            })
        };
        if let (Some(records), Some(env)) = (snapshot, &self.persist_env) {
            persist_dispatch_history(env, &records);
        }
    }

    /// Drops finished records beyond [`RETAIN_FINISHED`] or older than
    /// [`RETAIN_FOR`], oldest first. A running dispatch is never touched —
    /// it is not in `finished_order` at all.
    fn evict(inner: &mut Inner) {
        while inner.finished_order.len() > RETAIN_FINISHED {
            let Some(oldest) = inner.finished_order.pop_front() else {
                break;
            };
            inner.entries.remove(&oldest);
            inner.forgotten += 1;
        }
        while let Some(oldest) = inner.finished_order.front() {
            let expired = inner
                .entries
                .get(oldest)
                .and_then(|entry| entry.finished_instant)
                .is_some_and(|at| at.elapsed() > RETAIN_FOR);
            if !expired {
                break;
            }
            let oldest = inner.finished_order.pop_front().expect("front just peeked");
            inner.entries.remove(&oldest);
            inner.forgotten += 1;
        }
    }

    /// Everything `GET /api/dispatch-log` answers (SH-361): the finished
    /// records still retained, newest first, and the policy bounding them.
    ///
    /// **This read does not mutate the registry, deliberately.** The obvious
    /// implementation calls [`Self::evict`] first, so the list shows exactly
    /// what the policy retains — and SH-361's council rejected it, on its own
    /// author's motion. Eviction is shared state: collecting a record here
    /// makes [`handle_get`] answer **404** for a handle that would otherwise
    /// still have resolved, and `pollDispatch`'s `.catch` in
    /// `src/web_dashboard.html` retries a 404 all the way to
    /// `DISPATCH_MAX_POLLS` and then reports "Lost track of the dispatch" —
    /// a fabricated client-side failure whose actual cause was somebody
    /// opening a Settings panel. A read on one route must not change what a
    /// second route answers.
    ///
    /// Filtering in the read path yields the identical honest list with no
    /// cross-route side effect: an entry past [`RETAIN_FOR`] is omitted here
    /// whether or not a later `finish()` has collected it yet. The count
    /// bound needs no filtering at all, because `finish()` evicts eagerly, so
    /// `finished_order` never exceeds [`RETAIN_FINISHED`] between writes.
    ///
    /// The consequence is stated rather than hidden: a filtered-but-
    /// uncollected entry is **not** added to `forgotten`, which is the second
    /// of the two reasons that counter is a floor rather than a total.
    pub fn log(&self) -> DispatchLog {
        let inner = self.inner.lock().expect("dispatch registry mutex poisoned");
        // Reversed: `finished_order` is oldest-first, and this list is
        // newest-first. Ordering on insertion order rather than on
        // `finished_at` is SH-336's doctrine on this surface -- every
        // storyhook timestamp is RFC3339 at one-second precision and
        // `MAX_RUNNING` dispatches can finish inside one second, so a
        // timestamp comparator is blind exactly where this list is busiest.
        // `finished_order` is appended inside this mutex and so cannot tie.
        let dispatches = inner
            .finished_order
            .iter()
            .rev()
            .filter_map(|handle| inner.entries.get(handle))
            .filter(|entry| {
                !entry
                    .finished_instant
                    .is_some_and(|at| at.elapsed() > RETAIN_FOR)
            })
            .map(|entry| entry.record.clone())
            .collect();
        DispatchLog {
            dispatches,
            retention: Retention {
                records: RETAIN_FINISHED,
                seconds: RETAIN_FOR.as_secs(),
                forgotten: inner.forgotten,
            },
        }
    }
}

impl Default for DispatchRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Writes `records` to [`Environment::dispatch_history`], temp-plus-`rename`
/// and mode 0600 — the same shape as `daemon::lifecycle::publish_inflight`
/// and for the same reason: a client that read a half-written file mid-write
/// would see a truncated or torn JSON array.
///
/// **Best-effort, and never fatal** (mirrors `publish_inflight`'s own rule):
/// a state directory that is full, missing or read-only must still let the
/// dispatch that finished report its OWN result to the caller polling it —
/// failing here would replace a good answer with a bad diagnostic about a
/// diagnostic.
fn persist_dispatch_history(env: &Environment, records: &[DispatchRecord]) {
    let path = env.dispatch_history();
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(document) = serde_json::to_string(records) else {
        return;
    };
    let temp = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let Ok(mut file) = options.open(&temp) else {
        return;
    };
    if std::io::Write::write_all(&mut file, document.as_bytes()).is_err() {
        return;
    }
    drop(file);
    let _ = std::fs::rename(&temp, &path);
}

/// Reads back whatever the previous daemon (if any) last persisted via
/// [`persist_dispatch_history`]. Absent file (no previous daemon ever
/// finished a dispatch on this store, or nothing has been written yet) and a
/// parse failure (a stale format from an older binary — the write above is
/// atomic, so a torn read is not possible, but an incompatible one is) both
/// read as "no history" rather than as an error — the same choice
/// `daemon::lifecycle::read_inflight` makes for the same reason: a fresh
/// daemon must never refuse to start over a file it no longer understands.
fn load_dispatch_history(env: &Environment) -> Vec<DispatchRecord> {
    let Ok(raw) = std::fs::read_to_string(env.dispatch_history()) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// A project slug or story id, validated against the shape `story.sh` itself
/// requires (`valid_story_id`, `plugins/story/bin/story.sh`):
/// non-empty, alphanumeric first character, alphanumeric/hyphen/underscore
/// after. Rejects path traversal and whitespace at the one boundary where
/// a URL segment becomes a shell argument.
fn valid_segment(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Intercepts `/api/repos/{project}/story/{id}/dispatch[/{handle}]` before a
/// `Job` is ever built. `None` means "not this endpoint" — the caller's
/// ordinary REST/RPC routing continues unchanged.
///
/// Guard order mirrors [`crate::api::rpc::admission`]'s own reasoning
/// (token checked before anything route-shaped is served): the mutation
/// guard first (cheapest, and what tells a browser's CORS preflight this
/// is not answering it), then the token, then whether the handle/method
/// actually resolve to something. An unauthenticated caller therefore
/// cannot use this endpoint's responses to learn whether a given handle,
/// project or story exists. `query` (SH-208's `?auto=1`) is parsed last of
/// all, on the `POST` arm only — a malformed value is a 400, but only once
/// every prior guard has already passed, so it leaks nothing to an
/// unauthenticated caller either.
/// Whether `segments` addresses the dispatch endpoint — either
/// `POST /api/repos/{project}/story/{id}/dispatch` or the
/// `GET …/dispatch/{handle}` poll beside it.
///
/// Named once, and `pub(crate)`, because two gates need the same answer and a
/// second hand-written copy of this shape could drift from it:
/// [`intercept`] uses it to claim the request, and
/// [`crate::api::admission`] uses it to *refuse* the request the token
/// exemption it would otherwise grant. Those two disagreeing would mean a
/// route admitted by one gate and refused by the other.
pub(crate) fn is_dispatch_path(segments: &[&str]) -> bool {
    (segments.len() == 6 || segments.len() == 7)
        && segments[0] == "api"
        && segments[1] == "repos"
        && segments[3] == "story"
        && segments[5] == "dispatch"
}

/// Whether `segments` addresses the dispatch log — `GET /api/dispatch-log`
/// (SH-361).
///
/// A **separate** predicate rather than a widening of [`is_dispatch_path`],
/// which `src/api/routes.rs`'s cross-check maps onto the
/// `ProjectRoute::Dispatch | DispatchPoll` family; widening it would make
/// that assertion false. Two predicates, each cross-checked against its own
/// `Route` variant, keeps both statements true.
///
/// **Daemon-scoped, not project-scoped**, because the resource is: one
/// [`DispatchRegistry`] per daemon and one
/// [`Environment::dispatch_history`] per store, bounded by
/// [`RETAIN_FINISHED`]/[`RETAIN_FOR`] **globally**. A per-project projection
/// of a globally-bounded set cannot describe its own truncation — it would
/// silently lose entries to another project's dispatches and could not even
/// say how many — and `state.dispatchHistory` in the dashboard is reset only
/// by `dismissAllDispatchHistory`, never by `selectRepo`, so the dock
/// already shows rows across project switches that a per-project log could
/// not. Each record carries its own `project`, so labelling rows costs
/// nothing and a `?project=` filter stays available later without this
/// route having claimed anything false today.
pub(crate) fn is_dispatch_log_path(segments: &[&str]) -> bool {
    segments == ["api", "dispatch-log"]
}

#[allow(clippy::too_many_arguments)]
pub fn intercept(
    segments: &[&str],
    method: &Method,
    query: Option<&str>,
    headers: &[Header],
    trusted_hosts: &TrustedHosts,
    token: &str,
    env: &Environment,
    bus: &ChangeBus,
    registry: &Arc<DispatchRegistry>,
    tokens: &TokenRegistry,
    cookie_name: &str,
    wall_now: DateTime<Utc>,
) -> Option<Reply> {
    if is_dispatch_log_path(segments) {
        return Some(handle_log(
            method,
            headers,
            token,
            tokens,
            cookie_name,
            wall_now,
            registry,
        ));
    }

    if !is_dispatch_path(segments) {
        return None;
    }

    if !mutation_guard_ok(headers, trusted_hosts) {
        return Some(text_reply(403, "Forbidden"));
    }
    // SH-255: a named token, via header or cookie, is now an alternative to
    // the master token here too -- this route's own narrower copy of
    // `admission::admission`'s gate must recognize everything the outer gate
    // already does, or a request the outer gate admitted (a browser tab
    // authenticated by its cookie) is refused here anyway.
    if !token_ok(headers, token)
        && !named_token_ok(
            headers,
            method,
            cookie_name,
            tokens,
            wall_now,
            Instant::now(),
        )
    {
        return Some(text_reply(
            401,
            "storyhook daemon: missing or invalid token",
        ));
    }

    let project = segments[2];
    let story = segments[4];
    if !valid_segment(project) || !valid_segment(story) {
        return Some(text_reply(404, "Not found"));
    }

    match (method, segments.len()) {
        (Method::Post, 6) => {
            let auto = match parse_auto(query) {
                Ok(auto) => auto,
                Err(reply) => return Some(reply),
            };
            let agent = match parse_agent(query) {
                Ok(agent) => agent,
                Err(reply) => return Some(reply),
            };
            Some(handle_post(project, story, agent, auto, env, bus, registry))
        }
        (Method::Get, 7) => Some(handle_get(segments[6], registry)),
        _ => Some(text_reply(405, "Method Not Allowed")),
    }
}

/// `GET /api/dispatch-log` — the finished records this daemon still retains,
/// and the policy that bounds them (SH-361).
///
/// # Why this route exists
///
/// A durable **error** notice used to be the only record of the failure it
/// reported: `addDispatchHistoryRow`'s own doc comment said so — "no route
/// exposes it, this page never reads it… This row is the record" — and
/// dismissing it, deliberately or through the "Dismiss all" bar that has
/// existed since SH-323, destroyed that record. SH-339 fixed the
/// *amplification* (a held Enter walking the heir chain and clearing the
/// pile); this fixes the *loss*, for the half of it that has a record to
/// recover. The daemon has kept these records all along, and has persisted
/// them across its own restarts since SH-232. They simply had no reader.
///
/// # The gate
///
/// A `GET`, and gated like every other read on this daemon and no more.
/// [`crate::api::admission::admission`] has already run in
/// `daemon::serve::worker` before this is reached; the token is re-checked
/// here with the same expression the dispatch arms use — this module's
/// deliberate, tested redundancy. **No [`mutation_guard_ok`]**, unlike
/// [`intercept`]'s other arms: that guard is what tells a browser preflight
/// this endpoint spawns a process, and reading finished records does not.
/// Structure first (a method this path does not serve is a 405), then the
/// credential (401), so an unauthenticated caller learns nothing beyond
/// which methods the path answers — the same order the arms below use.
///
/// `.no_cache()` because a browser re-issues this on every visit to the
/// Settings screen and must never be handed a stale list.
fn handle_log(
    method: &Method,
    headers: &[Header],
    token: &str,
    tokens: &TokenRegistry,
    cookie_name: &str,
    wall_now: DateTime<Utc>,
    registry: &Arc<DispatchRegistry>,
) -> Reply {
    if !matches!(method, Method::Get) {
        return text_reply(405, "Method Not Allowed");
    }
    if !token_ok(headers, token)
        && !named_token_ok(
            headers,
            method,
            cookie_name,
            tokens,
            wall_now,
            Instant::now(),
        )
    {
        return text_reply(401, "storyhook daemon: missing or invalid token");
    }
    let log = registry.log();
    let body = serde_json::to_string(&DispatchLogEnvelope {
        result: "ok",
        dispatches: &log.dispatches,
        retention: log.retention,
    })
    .expect("DispatchLogEnvelope holds no type that can fail to serialize");
    Reply::new(200, "application/json", body).no_cache()
}

/// Parses the dispatch endpoint's `auto` query parameter (SH-208).
///
/// Absent is `false`, not a refusal — a caller that has never heard of
/// `--auto` (every non-dashboard caller, and the dashboard's own plain
/// Dispatch button) must see no behavior change. `1` and `true` are the
/// only recognized "on" spellings; anything else *present* (`auto=0`, a
/// typo, a stray empty value) is a 400 rather than a silently-accepted
/// `false` — `?auto=0` quietly reading as attended would be exactly the
/// kind of mismatch between what a caller asked for and what ran that this
/// endpoint's own idempotency wrinkle (see [`DispatchRegistry::try_start`])
/// already asks a caller to be careful about.
fn parse_auto(query: Option<&str>) -> Result<bool, Reply> {
    let Some(value) = query.and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "auto").then_some(value)
        })
    }) else {
        return Ok(false);
    };
    match value {
        "1" | "true" => Ok(true),
        _ => Err(text_reply(
            400,
            format!("dispatch: unrecognized `auto` value `{value}` — use `auto=1`"),
        )),
    }
}

/// Parses the canonical `agent=claude|codex` dispatch option.
///
/// Absence preserves the endpoint's pre-SH-436 behavior: Claude. A repeated
/// key is rejected even when both values agree, because silently choosing one
/// from an ambiguous request would make the record look more authoritative
/// than the request was. `claude-code` is not accepted here: that compatibility
/// alias predates this API and remains confined to the helper environment and
/// plugin install target where removing it would break an existing caller.
fn parse_agent(query: Option<&str>) -> Result<DispatchAgent, Reply> {
    let values = query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "agent").then_some(value)
        })
        .collect::<Vec<_>>();
    if values.len() > 1 {
        return Err(text_reply(
            400,
            "dispatch: `agent` may be specified only once",
        ));
    }
    match values.first().copied() {
        None | Some("claude") => Ok(DispatchAgent::Claude),
        Some("codex") => Ok(DispatchAgent::Codex),
        Some(value) => Err(text_reply(
            400,
            format!(
                "dispatch: unrecognized `agent` value `{value}` — use `agent=claude` or `agent=codex`"
            ),
        )),
    }
}

/// `POST /api/repos/{project}/story/{id}/dispatch`.
///
/// Resolving the dispatch script is the one check made synchronously,
/// before any record exists: it describes this *daemon* (the plugin is not
/// installed, or `STORYHOOK_DISPATCH_SCRIPT` names nothing), not this one
/// dispatch attempt, and every other dispatch would fail it identically —
/// worth a clear answer on the request that first hits it rather than a
/// poll round-trip.  Everything past that point is asynchronous: this
/// always returns 202 with a handle to poll, even for a story `story.sh`
/// will refuse outright (not ready, already claimed) — the CLI's own
/// refusal is relayed verbatim as the record's `payload` once the child
/// exits, which this endpoint deliberately does not try to predict.
#[allow(clippy::too_many_arguments)]
fn handle_post(
    project: &str,
    story: &str,
    agent: DispatchAgent,
    auto: bool,
    env: &Environment,
    bus: &ChangeBus,
    registry: &Arc<DispatchRegistry>,
) -> Reply {
    if let Some(handle) = registry.running_handle(story) {
        return accepted(&handle, registry);
    }
    let script = match resolve_dispatch_script(agent) {
        Ok(script) => script,
        Err(message) => return text_reply(500, message),
    };
    match registry.try_start_for_agent(project, story, agent, auto, env.now()) {
        StartOutcome::AtCapacity => text_reply(
            429,
            format!("{MAX_RUNNING} dispatches are already running — wait for one to finish"),
        ),
        StartOutcome::AlreadyRunning(handle) => accepted(&handle, registry),
        StartOutcome::Started(handle) => {
            spawn_dispatch(
                Arc::clone(registry),
                handle.clone(),
                script,
                project.to_string(),
                story.to_string(),
                agent,
                auto,
                env.clone(),
                bus.clone(),
            );
            accepted(&handle, registry)
        }
    }
}

/// The 202 body for a dispatch that is (now, or already) running.
fn accepted(handle: &str, registry: &DispatchRegistry) -> Reply {
    let record = registry
        .get(handle)
        .expect("try_start/handle_post always inserts before returning a handle");
    Reply::new(202, "application/json", envelope_json(&record))
}

/// `GET /api/repos/{project}/story/{id}/dispatch/{handle}`.
///
/// `project` and `story` are validated by [`intercept`] but not otherwise
/// consulted here — the handle alone identifies the record, the same way a
/// story id alone identifies a story regardless of which project's URL a
/// client happened to poll it from.
fn handle_get(handle: &str, registry: &DispatchRegistry) -> Reply {
    match registry.get(handle) {
        Some(record) => Reply::new(200, "application/json", envelope_json(&record)),
        None => text_reply(404, "Not found"),
    }
}

/// `(state, payload, error, reason)` — [`run_child`] and [`classify`]'s
/// shared return shape, named once so the growing tuple stays readable at
/// each of their several return sites.
type Classification = (
    DispatchState,
    Option<serde_json::Value>,
    Option<String>,
    Option<DispatchReason>,
);

/// Spawns `script --project <project> dispatch <story> --agent=<agent>
/// [--auto]` on a
/// detached thread and records its outcome when it finishes. Never touches
/// the store: everything this needs travels in its arguments.
#[allow(clippy::too_many_arguments)]
fn spawn_dispatch(
    registry: Arc<DispatchRegistry>,
    handle: String,
    script: PathBuf,
    project: String,
    story: String,
    agent: DispatchAgent,
    auto: bool,
    env: Environment,
    bus: ChangeBus,
) {
    std::thread::spawn(move || {
        let classification = run_child(&script, &project, &story, agent, auto, &env);
        registry.finish(&handle, &story, env.now(), classification);
        // Lets an open dashboard tab refresh without polling this endpoint
        // itself — the story moved to in-progress (or didn't), and the
        // ordinary `repo-changed` handling already knows how to react.
        bus.publish(Change::Project(project));
    });
}

fn run_child(
    script: &Path,
    project: &str,
    story: &str,
    agent: DispatchAgent,
    auto: bool,
    env: &Environment,
) -> Classification {
    let engine_agent = match agent {
        DispatchAgent::Claude => EngineAgent::Claude,
        DispatchAgent::Codex => EngineAgent::Codex,
    };
    classify_outcome(run_shell_dispatch(
        script,
        project,
        story,
        engine_agent,
        auto,
        false,
        env,
    ))
}

fn classify_outcome(result: Result<DispatchOutcome, crate::error::AppError>) -> Classification {
    match result {
        Ok(outcome) => {
            let state = match outcome.state {
                DispatchOutcomeState::Ok => DispatchState::Ok,
                DispatchOutcomeState::Refused => DispatchState::Refused,
            };
            let reason = outcome
                .payload
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(DispatchReason::parse);
            (state, Some(outcome.payload), None, reason)
        }
        Err(error) => (DispatchState::Failed, None, Some(error.to_string()), None),
    }
}

#[cfg(test)]
fn classify(stdout: std::fs::File, stderr: std::fs::File) -> Classification {
    classify_outcome(classify_dispatch_files(stdout, stderr))
}

/// The daemon<->script argv contract [`resolve_dispatch_script`] requires of
/// whichever `story.sh` it resolves (SH-196). `plugins/story/bin/
/// story.sh` declares the contract it implements in its own
/// `DISPATCH_PROTOCOL` constant; bump both together, and see that
/// constant's doc comment for the rule on when a bump is actually needed.
pub const REQUIRED_DISPATCH_PROTOCOL: u32 = 1;

/// Locates `plugins/story/bin/story.sh`, in order:
///
/// 1. `$STORYHOOK_DISPATCH_SCRIPT` — an operator's own override, and how
///    every test in this tree points dispatch at a stub.
/// 2. The plugin's own install record
///    (`~/.claude/plugins/installed_plugins.json`, keyed `story@storyhook`),
///    since the marketplace installs to a version-scoped cache directory
///    that is not otherwise discoverable.
/// 3. A dev checkout's own copy, via [`crate::plugin::dev_repo_root`] — the
///    same lookup `story plugin install` uses to prefer a local checkout
///    over the published one.
///
/// Deliberately does not check that `bash`, `jq`, `git` or `tmux` are on
/// PATH: those are the *script's* dependencies, and its own `set -euo
/// pipefail` aborts loudly if one is missing, surfacing as a `failed`
/// record with the stderr tail. That failure describes one dispatch
/// attempt; a script that cannot be found describes this daemon, which is
/// why only the latter is checked before any record is created.
///
/// Whichever candidate is found is also checked against
/// [`REQUIRED_DISPATCH_PROTOCOL`] before it is returned (SH-196) — a script
/// that predates the argv shape this daemon invokes it with is exactly as
/// wrong to run as one that cannot be found at all, and belongs to the same
/// "describes this daemon, not one dispatch attempt" category above.
fn resolve_dispatch_script(agent: DispatchAgent) -> Result<PathBuf, String> {
    resolve_dispatch_script_from_for_agent(
        std::env::var("STORYHOOK_DISPATCH_SCRIPT").ok(),
        std::env::var("HOME").ok().map(PathBuf::from),
        crate::plugin::dev_repo_root(),
        agent,
    )
}

/// Resolves the same provider-specific helper for the store-backed engine
/// controls. Kept as a narrow wrapper so SH-467 cannot grow a second opinion
/// about configured, installed, and development plugin precedence.
pub(crate) fn resolve_engine_dispatch_script(agent: EngineAgent) -> Result<PathBuf, String> {
    resolve_dispatch_script(match agent {
        EngineAgent::Claude => DispatchAgent::Claude,
        EngineAgent::Codex => DispatchAgent::Codex,
    })
}

/// [`resolve_dispatch_script`]'s logic, with every environment/filesystem
/// read injected rather than performed here — reading `std::env::var` or
/// resolving `$HOME` directly would make this untestable without mutating
/// process-global state, which a parallel test run cannot safely do
/// (`cargo test` runs `#[test]` functions across threads in one process by
/// default). This is also what makes `installed_plugin_script`'s real
/// behavior against a real `installed_plugins.json` testable at all — before
/// this, only the `configured` override branch had any coverage (SH-196).
fn resolve_dispatch_script_from_for_agent(
    configured: Option<String>,
    home: Option<PathBuf>,
    dev_root: Option<PathBuf>,
    agent: DispatchAgent,
) -> Result<PathBuf, String> {
    if let Some(configured) = configured {
        let path = PathBuf::from(configured);
        return if path.is_file() {
            check_dispatch_protocol(path)
        } else {
            Err(format!(
                "STORYHOOK_DISPATCH_SCRIPT names `{}`, which is not a file",
                path.display()
            ))
        };
    }
    let installed = home.and_then(|home| match agent {
        DispatchAgent::Claude => installed_plugin_script(&home),
        DispatchAgent::Codex => crate::plugin::codex_installed_plugin_root(&home)
            .map(|root| root.join("bin/story.sh"))
            .filter(|script| script.is_file()),
    });
    if let Some(path) = installed {
        return check_dispatch_protocol(path);
    }
    if let Some(root) = dev_root {
        let path = root.join("plugins/story/bin/story.sh");
        if path.is_file() {
            return check_dispatch_protocol(path);
        }
    }
    Err(format!(
        "could not find plugins/story/bin/story.sh for agent `{}` -- install it with \
             `story plugin install {}` or set STORYHOOK_DISPATCH_SCRIPT",
        agent.as_str(),
        agent.as_str()
    ))
}

#[cfg(test)]
fn resolve_dispatch_script_from(
    configured: Option<String>,
    home: Option<PathBuf>,
    dev_root: Option<PathBuf>,
) -> Result<PathBuf, String> {
    resolve_dispatch_script_from_for_agent(configured, home, dev_root, DispatchAgent::Claude)
}

/// Refuses `path` if its declared `DISPATCH_PROTOCOL` is older than
/// [`REQUIRED_DISPATCH_PROTOCOL`], naming the path, both numbers, and the
/// remedy — this is the SH-196 fix itself. Applied uniformly to whichever
/// candidate [`resolve_dispatch_script_from`] found, an operator's own
/// `STORYHOOK_DISPATCH_SCRIPT` override included: a stale script is exactly
/// as wrong to run regardless of how it was located, and one rule here is
/// one fewer exemption to justify. `declared >= required` (not `==`) so a
/// script that is merely *newer* than this daemon needs keeps resolving —
/// a plugin release must never have to wait on a daemon rebuild.
fn check_dispatch_protocol(path: PathBuf) -> Result<PathBuf, String> {
    let declared = declared_dispatch_protocol(&path);
    if declared >= REQUIRED_DISPATCH_PROTOCOL {
        return Ok(path);
    }
    Err(format!(
        "the dispatch script at `{}` implements dispatch protocol {declared}, but this \
         storyhook needs at least {REQUIRED_DISPATCH_PROTOCOL} -- the installed story \
         plugin is out of date. Update it with `story plugin install claude` or \
         `story plugin install codex`, matching the active provider, then retry.",
        path.display()
    ))
}

/// Reads `path`'s own declared `DISPATCH_PROTOCOL=<n>` line — the constant
/// `plugins/story/bin/story.sh` itself defines near its top — without
/// executing it. A `bash -c` probe would only echo this same constant at the
/// cost of a process spawn on every resolution, and resolution deliberately
/// runs before anything about the script is trusted (see
/// [`resolve_dispatch_script`]'s own note on not pre-checking
/// `bash`/`jq`/`git`/`tmux`). Only a line whose *first* non-whitespace
/// characters are the exact assignment counts — a mention inside a comment
/// (`# see DISPATCH_PROTOCOL=1 above`) does not. `0` for a script that
/// predates the marker entirely, or whose declaration this cannot parse —
/// both read the same to a caller: "not new enough."
///
/// `pub` so `tests/plugin_contract.rs` can pin the real
/// `plugins/story/bin/story.sh` in this repo against
/// [`REQUIRED_DISPATCH_PROTOCOL`] using the exact same parser this module
/// runs at resolution time, rather than a second implementation that could
/// drift from it.
pub fn declared_dispatch_protocol(path: &Path) -> u32 {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return 0;
    };
    contents
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("DISPATCH_PROTOCOL="))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// `story@storyhook`'s installed path under `home`, from Claude Code's own
/// plugin manifest, if that path still holds the script. The manifest is a
/// JSON array of install records per plugin key; the last one wins, matching
/// how a reinstall of the same scope is recorded (append, never
/// replace-in-place).
fn installed_plugin_script(home: &Path) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(home.join(".claude/plugins/installed_plugins.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let entries = manifest
        .get("plugins")?
        .get("story@storyhook")?
        .as_array()?;
    let install_path = entries.last()?.get("installPath")?.as_str()?;
    let script = PathBuf::from(install_path).join("bin/story.sh");
    script.is_file().then_some(script)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate, asked with no named token anywhere in play.
    ///
    /// **Shadows [`super::intercept`] on purpose.** SH-255 gave the real gate
    /// three more parameters, and every test below this line is about the
    /// two decisions that came before a named token was a second way in —
    /// the CSRF guard and the master token. Routing them through a helper
    /// that supplies an empty registry keeps all of them byte-identical
    /// across that change. The named-token half of the check has its own
    /// tests, alongside `admission.rs`'s, since `named_token_ok` is the
    /// single function both gates share.
    #[allow(clippy::too_many_arguments)]
    fn intercept(
        segments: &[&str],
        method: &Method,
        query: Option<&str>,
        headers: &[Header],
        trusted_hosts: &TrustedHosts,
        token: &str,
        env: &Environment,
        bus: &ChangeBus,
        registry: &Arc<DispatchRegistry>,
    ) -> Option<Reply> {
        super::intercept(
            segments,
            method,
            query,
            headers,
            trusted_hosts,
            token,
            env,
            bus,
            registry,
            &TokenRegistry::new(Utc::now(), Instant::now()),
            "storyhook_test",
            Utc::now(),
        )
    }

    /// A placeholder environment for tests that never touch the filesystem
    /// through it — `Environment::at` and the accessors these tests call
    /// (`now`, nothing else) do no I/O, so a directory that does not exist
    /// is fine here and avoids a real scratch directory these tests do not
    /// otherwise need.
    fn env() -> Environment {
        Environment::at("/private/tmp/storyhook-dispatch-test-placeholder")
    }

    fn headers(pairs: &[(&str, &str)]) -> Vec<Header> {
        pairs
            .iter()
            .map(|(name, value)| Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap())
            .collect()
    }

    const GUARD_HEADERS: &[(&str, &str)] = &[("X-Storyhook", "1"), ("Host", "127.0.0.1")];

    #[test]
    fn valid_segment_matches_storys_own_rule() {
        assert!(valid_segment("SH-50"));
        assert!(valid_segment("scad-caliper"));
        assert!(valid_segment("a"));
        assert!(!valid_segment(""));
        assert!(!valid_segment("-leading-hyphen"));
        assert!(!valid_segment("has spaces"));
        assert!(!valid_segment("../traversal"));
        assert!(!valid_segment("semi;colon"));
    }

    #[test]
    fn non_dispatch_paths_are_not_intercepted() {
        let registry = Arc::new(DispatchRegistry::new());
        let bus = ChangeBus::new();
        let env = env();
        for path in [
            vec!["api", "repos", "proj", "story", "SH-1", "comment"],
            vec!["api", "repos", "proj", "data"],
            vec!["api", "repos"],
        ] {
            let segments: Vec<&str> = path.iter().map(|s| s.as_ref()).collect();
            let reply = intercept(
                &segments,
                &Method::Post,
                None,
                &[],
                &TrustedHosts::default(),
                "tok",
                &env,
                &bus,
                &registry,
            );
            assert!(
                reply.is_none(),
                "{segments:?} must fall through to ordinary REST/RPC routing"
            );
        }
    }

    #[test]
    fn an_extra_path_segment_past_the_handle_is_not_intercepted() {
        let registry = Arc::new(DispatchRegistry::new());
        let bus = ChangeBus::new();
        let env = env();
        let segments = [
            "api", "repos", "proj", "story", "SH-1", "dispatch", "h", "extra",
        ];
        assert!(
            intercept(
                &segments,
                &Method::Get,
                None,
                &[],
                &TrustedHosts::default(),
                "tok",
                &env,
                &bus,
                &registry,
            )
            .is_none()
        );
    }

    #[test]
    fn missing_guard_header_is_403_before_the_token_is_even_read() {
        let registry = Arc::new(DispatchRegistry::new());
        let bus = ChangeBus::new();
        let env = env();
        let segments = ["api", "repos", "proj", "story", "SH-1", "dispatch"];
        let reply = intercept(
            &segments,
            &Method::Post,
            None,
            &headers(&[("X-Storyhook-Token", "tok")]), // no X-Storyhook, no Host
            &TrustedHosts::default(),
            "tok",
            &env,
            &bus,
            &registry,
        )
        .expect("a dispatch-shaped path is always answered here");
        assert_eq!(reply.status, 403);
    }

    #[test]
    fn spoofed_host_is_403() {
        let registry = Arc::new(DispatchRegistry::new());
        let bus = ChangeBus::new();
        let env = env();
        let segments = ["api", "repos", "proj", "story", "SH-1", "dispatch"];
        let reply = intercept(
            &segments,
            &Method::Post,
            None,
            &headers(&[
                ("X-Storyhook", "1"),
                ("Host", "evil.example"),
                ("X-Storyhook-Token", "tok"),
            ]),
            &TrustedHosts::default(),
            "tok",
            &env,
            &bus,
            &registry,
        )
        .unwrap();
        assert_eq!(reply.status, 403);
    }

    #[test]
    fn guard_header_present_but_no_token_is_401() {
        let registry = Arc::new(DispatchRegistry::new());
        let bus = ChangeBus::new();
        let env = env();
        let segments = ["api", "repos", "proj", "story", "SH-1", "dispatch"];
        let reply = intercept(
            &segments,
            &Method::Post,
            None,
            &headers(GUARD_HEADERS),
            &TrustedHosts::default(),
            "tok",
            &env,
            &bus,
            &registry,
        )
        .unwrap();
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn wrong_token_is_401() {
        let registry = Arc::new(DispatchRegistry::new());
        let bus = ChangeBus::new();
        let env = env();
        let mut h = GUARD_HEADERS.to_vec();
        h.push(("X-Storyhook-Token", "not-it"));
        let segments = ["api", "repos", "proj", "story", "SH-1", "dispatch"];
        let reply = intercept(
            &segments,
            &Method::Post,
            None,
            &headers(&h),
            &TrustedHosts::default(),
            "tok",
            &env,
            &bus,
            &registry,
        )
        .unwrap();
        assert_eq!(reply.status, 401);
    }

    /// The token is checked before the handle is even looked up: an
    /// unauthenticated GET for a handle that was never minted must answer
    /// exactly like one for a handle that exists, so a caller cannot use
    /// this endpoint to enumerate live dispatches.
    #[test]
    fn the_token_is_checked_before_the_handle_is_looked_up() {
        let registry = Arc::new(DispatchRegistry::new());
        let bus = ChangeBus::new();
        let env = env();
        let segments = [
            "api",
            "repos",
            "proj",
            "story",
            "SH-1",
            "dispatch",
            "no-such-handle",
        ];
        let reply = intercept(
            &segments,
            &Method::Get,
            None,
            &headers(GUARD_HEADERS),
            &TrustedHosts::default(),
            "tok",
            &env,
            &bus,
            &registry,
        )
        .unwrap();
        assert_eq!(
            reply.status, 401,
            "must not leak 404-vs-401 to an unauthenticated caller"
        );
    }

    #[test]
    fn an_invalid_project_or_story_segment_is_404_once_authenticated() {
        let registry = Arc::new(DispatchRegistry::new());
        let bus = ChangeBus::new();
        let env = env();
        let mut h = GUARD_HEADERS.to_vec();
        h.push(("X-Storyhook-Token", "tok"));
        let segments = ["api", "repos", "../evil", "story", "SH-1", "dispatch"];
        let reply = intercept(
            &segments,
            &Method::Post,
            None,
            &headers(&h),
            &TrustedHosts::default(),
            "tok",
            &env,
            &bus,
            &registry,
        )
        .unwrap();
        assert_eq!(reply.status, 404);
    }

    #[test]
    fn an_unknown_method_on_a_dispatch_path_is_405() {
        let registry = Arc::new(DispatchRegistry::new());
        let bus = ChangeBus::new();
        let env = env();
        let mut h = GUARD_HEADERS.to_vec();
        h.push(("X-Storyhook-Token", "tok"));
        let segments = ["api", "repos", "proj", "story", "SH-1", "dispatch"];
        let reply = intercept(
            &segments,
            &Method::Put,
            None,
            &headers(&h),
            &TrustedHosts::default(),
            "tok",
            &env,
            &bus,
            &registry,
        )
        .unwrap();
        assert_eq!(reply.status, 405);
    }

    #[test]
    fn unknown_handle_is_404() {
        let registry = DispatchRegistry::new();
        let reply = handle_get("no-such-handle", &registry);
        assert_eq!(reply.status, 404);
    }

    #[test]
    fn a_second_start_for_the_same_running_story_reuses_the_handle() {
        let registry = DispatchRegistry::new();
        let first = match registry.try_start("proj", "SH-1", false, "t0".to_string()) {
            StartOutcome::Started(handle) => handle,
            _ => panic!("first start must succeed"),
        };
        match registry.try_start("proj", "SH-1", false, "t1".to_string()) {
            StartOutcome::AlreadyRunning(handle) => assert_eq!(handle, first),
            other => panic!("expected AlreadyRunning, story is still running: {other:?}"),
        }
    }

    /// SH-208's idempotency wrinkle, pinned: a `POST ?auto=1` for a story
    /// already dispatching (plainly, without `auto`) does not start a
    /// second, autonomous script — it reuses the running attempt's handle,
    /// and that handle's own record still reports the mode that is
    /// *actually running*, not the mode the deduped request asked for.
    #[test]
    fn a_second_start_with_a_different_auto_reuses_the_first_attempts_mode() {
        let registry = DispatchRegistry::new();
        let first = match registry.try_start("proj", "SH-1", false, "t0".to_string()) {
            StartOutcome::Started(handle) => handle,
            _ => panic!("first start must succeed"),
        };
        let reused = match registry.try_start("proj", "SH-1", true, "t1".to_string()) {
            StartOutcome::AlreadyRunning(handle) => handle,
            other => panic!("expected AlreadyRunning, story is still running: {other:?}"),
        };
        assert_eq!(reused, first);
        let record = registry.get(&reused).expect("just started");
        assert!(
            !record.auto,
            "the reused record must report the FIRST attempt's mode (attended), \
             not the second, deduped request's"
        );
    }

    #[test]
    fn a_second_start_with_a_different_agent_reuses_the_first_attempts_agent() {
        let registry = DispatchRegistry::new();
        let first = match registry.try_start_for_agent(
            "proj",
            "SH-1",
            DispatchAgent::Codex,
            false,
            "t0".to_string(),
        ) {
            StartOutcome::Started(handle) => handle,
            other => panic!("expected Started: {other:?}"),
        };
        let reused = match registry.try_start_for_agent(
            "proj",
            "SH-1",
            DispatchAgent::Claude,
            false,
            "t1".to_string(),
        ) {
            StartOutcome::AlreadyRunning(handle) => handle,
            other => panic!("expected AlreadyRunning: {other:?}"),
        };
        assert_eq!(reused, first);
        assert_eq!(registry.get(&reused).unwrap().agent, DispatchAgent::Codex);
    }

    #[test]
    fn a_record_without_agent_deserializes_as_claude() {
        let record: DispatchRecord = serde_json::from_value(serde_json::json!({
            "handle": "old",
            "project": "proj",
            "story": "SH-1",
            "auto": false,
            "state": "ok",
            "started_at": "t0"
        }))
        .expect("pre-agent persisted record");
        assert_eq!(record.agent, DispatchAgent::Claude);
    }

    impl std::fmt::Debug for StartOutcome {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                StartOutcome::Started(h) => write!(f, "Started({h})"),
                StartOutcome::AlreadyRunning(h) => write!(f, "AlreadyRunning({h})"),
                StartOutcome::AtCapacity => write!(f, "AtCapacity"),
            }
        }
    }

    #[test]
    fn once_finished_a_story_can_be_started_again() {
        let registry = DispatchRegistry::new();
        let handle = match registry.try_start("proj", "SH-1", false, "t0".to_string()) {
            StartOutcome::Started(h) => h,
            other => panic!("expected Started: {other:?}"),
        };
        registry.finish(
            &handle,
            "SH-1",
            "t1".to_string(),
            (
                DispatchState::Ok,
                Some(serde_json::json!({"ok": true})),
                None,
                None,
            ),
        );
        match registry.try_start("proj", "SH-1", false, "t2".to_string()) {
            StartOutcome::Started(second) => assert_ne!(second, handle),
            other => panic!("a finished dispatch must not block a fresh one: {other:?}"),
        }
    }

    #[test]
    fn capacity_is_enforced_across_distinct_stories() {
        let registry = DispatchRegistry::new();
        for n in 0..MAX_RUNNING {
            match registry.try_start(
                &format!("proj-{n}"),
                &format!("SH-{n}"),
                false,
                "t".to_string(),
            ) {
                StartOutcome::Started(_) => {}
                other => panic!("expected room for dispatch {n}: {other:?}"),
            }
        }
        match registry.try_start("proj-extra", "SH-extra", false, "t".to_string()) {
            StartOutcome::AtCapacity => {}
            other => panic!("expected AtCapacity beyond MAX_RUNNING: {other:?}"),
        }
    }

    /// Finishes one dispatch and hands back its handle — the four-line
    /// preamble every log test below would otherwise repeat.
    fn finish_one(
        registry: &DispatchRegistry,
        story: &str,
        finished_at: &str,
        reason: Option<DispatchReason>,
    ) -> String {
        let handle = match registry.try_start("proj", story, false, "t".to_string()) {
            StartOutcome::Started(h) => h,
            other => panic!("expected Started: {other:?}"),
        };
        registry.finish(
            &handle,
            story,
            finished_at.to_string(),
            (
                DispatchState::Refused,
                Some(serde_json::json!({"display": "[story] refused: something"})),
                None,
                reason,
            ),
        );
        handle
    }

    /// SH-361 P1. The log's order is the registry's own insertion order, and
    /// the probe makes every timestamp identical so a `finished_at`
    /// comparator has nothing to sort by.
    ///
    /// This is SH-336's doctrine on this surface, and it has to be a unit
    /// test: only a caller-supplied `finished_at` can force a same-second tie
    /// deterministically, which an e2e driving a real daemon cannot.
    #[test]
    fn the_log_is_newest_first_by_insertion_order_not_by_timestamp() {
        let registry = DispatchRegistry::new();
        for story in ["SH-1", "SH-2", "SH-3"] {
            finish_one(&registry, story, "2026-01-01T00:00:00Z", None);
        }
        let stories: Vec<String> = registry
            .log()
            .dispatches
            .iter()
            .map(|record| record.story.clone())
            .collect();
        assert_eq!(
            stories,
            vec!["SH-3".to_string(), "SH-2".to_string(), "SH-1".to_string()],
            "three records sharing one timestamp must still come back newest-first"
        );
    }

    /// SH-361 P2. A running dispatch is not an outcome and is not in the log.
    ///
    /// Also the reason the log iterates `finished_order` rather than the
    /// `entries` map: doing the latter would leak running records here *and*
    /// lose the order the test above pins.
    #[test]
    fn a_running_dispatch_is_absent_from_the_log() {
        let registry = DispatchRegistry::new();
        finish_one(&registry, "SH-1", "2026-01-01T00:00:00Z", None);
        match registry.try_start("proj", "SH-2", false, "t".to_string()) {
            StartOutcome::Started(_) => {}
            other => panic!("expected Started: {other:?}"),
        }
        let log = registry.log();
        assert_eq!(log.dispatches.len(), 1);
        assert_eq!(log.dispatches[0].story, "SH-1");
    }

    /// SH-361 P3. Overflowing the count bound is disclosed, not silent.
    #[test]
    fn records_dropped_past_the_count_bound_are_counted_as_forgotten() {
        let registry = DispatchRegistry::new();
        for n in 0..(RETAIN_FINISHED + 3) {
            finish_one(&registry, &format!("SH-{n}"), "2026-01-01T00:00:00Z", None);
        }
        let log = registry.log();
        assert_eq!(log.dispatches.len(), RETAIN_FINISHED);
        assert_eq!(
            log.retention.forgotten, 3,
            "three records past the bound must be reported as gone"
        );
    }

    /// SH-361 P4, as amended by the council. Two assertions, and the second
    /// is the one the amendment exists for.
    ///
    /// The first half pins `RETAIN_FOR` itself, which **nothing tested before
    /// this story**: deleting `evict`'s time-based loop shipped green, and
    /// with it the whole `retention.seconds` half of the disclosure the
    /// dashboard renders.
    ///
    /// The second half pins that reading the log is *not* a write. The
    /// natural implementation calls `evict()` first so the list matches the
    /// policy exactly; the council rejected it because collecting a record
    /// here makes `handle_get` answer 404 for a handle that would still have
    /// resolved, and `pollDispatch` turns a 404 into "Lost track of the
    /// dispatch" after exhausting its retries — a failure invented by opening
    /// a Settings panel. Filtering in the read path gives the identical list
    /// with no such side effect, and this asserts the difference directly
    /// rather than trusting the comment above it.
    #[test]
    fn an_expired_record_leaves_the_log_without_leaving_the_poll_route() {
        let registry = DispatchRegistry::new();
        let handle = finish_one(&registry, "SH-1", "2026-01-01T00:00:00Z", None);
        {
            let mut inner = registry.inner.lock().expect("dispatch registry lock");
            let entry = inner.entries.get_mut(&handle).expect("just finished");
            entry.finished_instant = Some(
                Instant::now()
                    .checked_sub(RETAIN_FOR + Duration::from_secs(1))
                    .expect("a monotonic clock far enough from its origin"),
            );
        }
        assert!(
            registry.log().dispatches.is_empty(),
            "a record past RETAIN_FOR must not be served in the log"
        );
        assert!(
            registry.get(&handle).is_some(),
            "reading the log must not evict the record the poll route answers with"
        );
    }

    /// SH-361 P5. The numbers on the wire are the module's own constants, so
    /// the sentence the dashboard composes cannot drift from the policy this
    /// registry actually enforces — the `HOOK_TIMEOUT_CEILING_SECS` /
    /// `dashboard_mutation_deadline.rs` precedent, applied before the drift
    /// rather than after it.
    #[test]
    fn the_logs_retention_numbers_are_the_modules_own_constants() {
        let log = DispatchRegistry::new().log();
        assert_eq!(log.retention.records, RETAIN_FINISHED);
        assert_eq!(log.retention.seconds, RETAIN_FOR.as_secs());
        assert_eq!(
            log.retention.forgotten, 0,
            "a fresh registry has dropped nothing"
        );
    }

    /// SH-361. The record recovered from the log still carries the two lines
    /// the dismissed notice showed — `payload.display` and the typed
    /// `reason`, the latter serialized as the bare string `story.sh` wrote.
    #[test]
    fn the_log_keeps_the_detail_and_the_typed_reason() {
        let registry = DispatchRegistry::new();
        finish_one(
            &registry,
            "SH-1",
            "2026-01-01T00:00:00Z",
            Some(DispatchReason::parse("claim-conflict")),
        );
        let log = registry.log();
        let json = serde_json::to_string(&log.dispatches[0]).expect("a record serializes");
        assert!(
            json.contains("\"reason\":\"claim-conflict\""),
            "the typed reason must survive as a bare string: {json}"
        );
        assert!(
            json.contains("refused: something"),
            "the detail must survive: {json}"
        );
    }

    /// SH-361 P8's gate, at the unit layer: the log answers no method but
    /// `GET`, and answers nothing at all without a credential.
    #[test]
    fn the_dispatch_log_refuses_a_wrong_method_and_an_untokened_caller() {
        let registry = Arc::new(DispatchRegistry::new());
        let bus = ChangeBus::new();
        let env = env();
        let mut authed = GUARD_HEADERS.to_vec();
        authed.push(("X-Storyhook-Token", "tok"));
        let segments = ["api", "dispatch-log"];

        let post = intercept(
            &segments,
            &Method::Post,
            None,
            &headers(&authed),
            &TrustedHosts::default(),
            "tok",
            &env,
            &bus,
            &registry,
        )
        .expect("the log path is always claimed");
        assert_eq!(post.status, 405);

        let untokened = intercept(
            &segments,
            &Method::Get,
            None,
            &headers(GUARD_HEADERS),
            &TrustedHosts::default(),
            "tok",
            &env,
            &bus,
            &registry,
        )
        .expect("the log path is always claimed");
        assert_eq!(untokened.status, 401);

        let ok = intercept(
            &segments,
            &Method::Get,
            None,
            &headers(&authed),
            &TrustedHosts::default(),
            "tok",
            &env,
            &bus,
            &registry,
        )
        .expect("the log path is always claimed");
        assert_eq!(ok.status, 200);
    }

    /// SH-361. An empty log is a 200 carrying its policy, never a 404 or a
    /// 500 — the both-directions check without which "the route always
    /// errors" would satisfy every assertion above.
    #[test]
    fn an_empty_dispatch_log_is_a_200_that_still_states_its_policy() {
        let registry = Arc::new(DispatchRegistry::new());
        let bus = ChangeBus::new();
        let env = env();
        let mut authed = GUARD_HEADERS.to_vec();
        authed.push(("X-Storyhook-Token", "tok"));
        let reply = intercept(
            &["api", "dispatch-log"],
            &Method::Get,
            None,
            &headers(&authed),
            &TrustedHosts::default(),
            "tok",
            &env,
            &bus,
            &registry,
        )
        .expect("the log path is always claimed");
        assert_eq!(reply.status, 200);
        assert!(
            reply.body().contains("\"dispatches\":[]"),
            "{}",
            reply.body()
        );
        assert!(
            reply
                .body()
                .contains(&format!("\"records\":{RETAIN_FINISHED}")),
            "an empty list must still disclose the policy: {}",
            reply.body()
        );
    }

    #[test]
    fn finished_records_beyond_the_retention_count_are_evicted_oldest_first() {
        let registry = DispatchRegistry::new();
        let mut handles = Vec::new();
        for n in 0..(RETAIN_FINISHED + 3) {
            let handle = match registry.try_start(
                &format!("proj-{n}"),
                &format!("SH-{n}"),
                false,
                "t".to_string(),
            ) {
                StartOutcome::Started(h) => h,
                other => panic!("expected Started: {other:?}"),
            };
            registry.finish(
                &handle,
                &format!("SH-{n}"),
                "t".to_string(),
                (
                    DispatchState::Ok,
                    Some(serde_json::json!({"ok": true})),
                    None,
                    None,
                ),
            );
            handles.push(handle);
        }
        assert!(
            registry.get(&handles[0]).is_none(),
            "the oldest finished record must be evicted once retention overflows"
        );
        assert!(
            registry.get(handles.last().unwrap()).is_some(),
            "the newest finished record must survive"
        );
    }

    /// A plain `new()` — every test above this one — must never touch a
    /// filesystem at all, even when `finish()` is called: only [`load`]
    /// wires up `persist_env`. Verified against a home that does not exist,
    /// same as the `env()` placeholder helper's own doc: if `finish()`
    /// tried to write here it would have to create directories first, and
    /// this test would see them.
    #[test]
    fn new_never_persists_regardless_of_finish() {
        let registry = DispatchRegistry::new();
        let handle = match registry.try_start("proj", "SH-1", false, "t0".to_string()) {
            StartOutcome::Started(h) => h,
            other => panic!("expected Started: {other:?}"),
        };
        registry.finish(
            &handle,
            "SH-1",
            "t1".to_string(),
            (
                DispatchState::Ok,
                Some(serde_json::json!({"ok": true})),
                None,
                None,
            ),
        );
        assert!(
            !env().dispatch_history().exists(),
            "a plain new() registry must never write dispatch history to disk"
        );
    }

    #[test]
    fn load_with_no_prior_history_starts_empty() {
        let home = storyhook_test_support::scratch_dir();
        let registry = DispatchRegistry::load(&Environment::at(home.path()));
        match registry.try_start("proj", "SH-1", false, "t0".to_string()) {
            StartOutcome::Started(_) => {}
            other => panic!("a freshly loaded registry must start empty: {other:?}"),
        }
    }

    /// SH-232's central claim: a finished dispatch survives the daemon that
    /// finished it. `finish()` on one `load()`-backed registry persists;
    /// `load()` on a second, independent registry (a fresh daemon, same
    /// store) reads it back — including the typed `reason`, round-tripped
    /// rather than degraded to an untyped payload lookup.
    #[test]
    fn a_finished_dispatch_survives_across_a_reload() {
        let home = storyhook_test_support::scratch_dir();
        let env = Environment::at(home.path());
        let first = DispatchRegistry::load(&env);
        let handle = match first.try_start("proj", "SH-1", true, "t0".to_string()) {
            StartOutcome::Started(h) => h,
            other => panic!("expected Started: {other:?}"),
        };
        first.finish(
            &handle,
            "SH-1",
            "t1".to_string(),
            (
                DispatchState::Refused,
                Some(serde_json::json!({"ok": false, "reason": "pane-not-ready"})),
                None,
                Some(DispatchReason::PaneNotReady),
            ),
        );

        let second = DispatchRegistry::load(&env);
        let record = second
            .get(&handle)
            .expect("a finished record must survive a fresh registry load");
        assert_eq!(record.state, DispatchState::Refused);
        assert_eq!(record.reason, Some(DispatchReason::PaneNotReady));
        assert!(record.auto, "auto must survive the round trip too");
        assert_eq!(record.story, "SH-1");
    }

    /// [`Environment::dispatch_history`]'s own doc: a dispatch still
    /// `Running` has no thread left to observe it once its daemon exits, so
    /// it must never appear in a loaded registry even if a corrupted or
    /// hand-edited history file somehow contains one — resurrecting a
    /// handle nothing can ever finish would strand a polling client.
    #[test]
    fn load_skips_a_running_record_found_in_history() {
        let home = storyhook_test_support::scratch_dir();
        let env = Environment::at(home.path());
        let stray = DispatchRecord {
            handle: "stray-handle".to_string(),
            project: "proj".to_string(),
            story: "SH-9".to_string(),
            agent: DispatchAgent::Claude,
            auto: false,
            state: DispatchState::Running,
            started_at: "t0".to_string(),
            finished_at: None,
            payload: None,
            error: None,
            reason: None,
        };
        persist_dispatch_history(&env, std::slice::from_ref(&stray));

        let registry = DispatchRegistry::load(&env);
        assert!(
            registry.get("stray-handle").is_none(),
            "a Running record in history must never be resurrected"
        );
    }

    /// A daemon restart between two `finish()` calls must not lose the
    /// first one — the second `load()`'s write has to include what the
    /// first `load()` already persisted, not just its own record.
    #[test]
    fn history_accumulates_across_more_than_one_reload() {
        let home = storyhook_test_support::scratch_dir();
        let env = Environment::at(home.path());

        let first = DispatchRegistry::load(&env);
        let h1 = match first.try_start("proj", "SH-1", false, "t0".to_string()) {
            StartOutcome::Started(h) => h,
            other => panic!("expected Started: {other:?}"),
        };
        first.finish(
            &h1,
            "SH-1",
            "t1".to_string(),
            (
                DispatchState::Ok,
                Some(serde_json::json!({"ok": true})),
                None,
                None,
            ),
        );

        let second = DispatchRegistry::load(&env);
        let h2 = match second.try_start("proj", "SH-2", false, "t2".to_string()) {
            StartOutcome::Started(h) => h,
            other => panic!("expected Started: {other:?}"),
        };
        second.finish(
            &h2,
            "SH-2",
            "t3".to_string(),
            (
                DispatchState::Ok,
                Some(serde_json::json!({"ok": true})),
                None,
                None,
            ),
        );

        let third = DispatchRegistry::load(&env);
        assert!(
            third.get(&h1).is_some(),
            "the first daemon's record must survive a second restart"
        );
        assert!(
            third.get(&h2).is_some(),
            "the second daemon's record must also be present"
        );
    }

    #[test]
    fn classify_reports_ok_for_a_successful_result() {
        let (state, payload, error, reason) =
            classify(capture_of(r#"{"ok":true,"id":"SH-1"}"#), capture_of(""));
        assert_eq!(state, DispatchState::Ok);
        assert_eq!(payload.unwrap()["id"], "SH-1");
        assert!(error.is_none());
        assert!(reason.is_none(), "a success carries no reason to report");
    }

    #[test]
    fn classify_reports_refused_for_a_well_formed_refusal() {
        let (state, payload, error, reason) = classify(
            capture_of(r#"{"ok":false,"display":"not ready"}"#),
            capture_of(""),
        );
        assert_eq!(state, DispatchState::Refused);
        assert_eq!(payload.unwrap()["display"], "not ready");
        assert!(error.is_none());
        assert!(
            reason.is_none(),
            "a fail()-shaped refusal has no `reason` field to read"
        );
    }

    /// The taxonomy's whole point (SH-232): a `refuse_with`-shaped refusal's
    /// `reason` field must not be left stranded inside `payload`.
    #[test]
    fn classify_reads_a_known_reason_out_of_a_refusal() {
        let (state, payload, _error, reason) = classify(
            capture_of(r#"{"ok":false,"reason":"pane-not-ready","display":"could not confirm"}"#),
            capture_of(""),
        );
        assert_eq!(state, DispatchState::Refused);
        assert_eq!(reason, Some(DispatchReason::PaneNotReady));
        // The reason is TYPED, not REMOVED from payload -- an older client
        // reading payload.reason directly must keep working unchanged.
        assert_eq!(payload.unwrap()["reason"], "pane-not-ready");
    }

    /// Every reason `story.sh`'s `dispatch` command can actually emit
    /// (`refuse`/`refuse_with` call sites in `plugins/story/bin/
    /// story.sh` and `lib/session.sh`), pinned so a renamed reason string on
    /// either side is caught here rather than silently degrading to
    /// [`DispatchReason::Other`].
    #[test]
    fn classify_reads_every_known_reason() {
        let cases = [
            ("claim-conflict", DispatchReason::ClaimConflict),
            ("pane-not-ready", DispatchReason::PaneNotReady),
            ("handoff-undelivered", DispatchReason::HandoffUndelivered),
            ("handoff-unconfirmed", DispatchReason::HandoffUnconfirmed),
        ];
        for (raw, expected) in cases {
            let (_state, _payload, _error, reason) = classify(
                capture_of(&format!(r#"{{"ok":false,"reason":"{raw}"}}"#)),
                capture_of(""),
            );
            assert_eq!(reason, Some(expected), "reason string {raw:?}");
        }
    }

    /// Forward compatibility (SH-232's whole reason for `Other`): a
    /// `story.sh` newer than this binary can emit a reason this binary has
    /// never heard of, and the string must survive rather than vanish.
    #[test]
    fn classify_carries_an_unrecognized_reason_as_other_rather_than_dropping_it() {
        let (_state, _payload, _error, reason) = classify(
            capture_of(r#"{"ok":false,"reason":"a-future-reason","display":"..."}"#),
            capture_of(""),
        );
        assert_eq!(
            reason,
            Some(DispatchReason::Other("a-future-reason".to_string()))
        );
    }

    #[test]
    fn classify_reports_failed_for_unparseable_output_and_carries_stderr() {
        let (state, payload, error, reason) = classify(
            capture_of("not json at all"),
            capture_of("bash: jq: command not found"),
        );
        assert_eq!(state, DispatchState::Failed);
        assert!(payload.is_none());
        assert!(error.unwrap().contains("jq: command not found"));
        assert!(reason.is_none());
    }

    #[test]
    fn classify_reports_failed_with_a_generic_message_when_nothing_was_said_at_all() {
        let (state, _payload, error, reason) = classify(capture_of(""), capture_of(""));
        assert_eq!(state, DispatchState::Failed);
        assert!(error.unwrap().contains("without printing a result"));
        assert!(reason.is_none());
    }

    /// [`DispatchReason`]'s own round trip: serialize as the bare string,
    /// deserialize back to the same typed variant -- what makes
    /// `persist_dispatch_history`/`load_dispatch_history` lossless for a
    /// finished record's reason.
    #[test]
    fn dispatch_reason_round_trips_through_json_including_other() {
        for reason in [
            DispatchReason::ClaimConflict,
            DispatchReason::PaneNotReady,
            DispatchReason::HandoffUndelivered,
            DispatchReason::HandoffUnconfirmed,
            DispatchReason::UnsafePromptOverride,
            DispatchReason::Other("something-new".to_string()),
        ] {
            let json = serde_json::to_string(&reason).expect("serialize");
            let back: DispatchReason = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, reason);
        }
        assert_eq!(
            serde_json::to_string(&DispatchReason::PaneNotReady).unwrap(),
            "\"pane-not-ready\"",
            "must serialize as the bare string story.sh itself emits, not a tagged object"
        );
    }

    #[test]
    fn charter_inert_violation_flags_every_banned_character_individually() {
        for banned in CHARTER_INERT_BANNED {
            let value = format!("investigate story <n>{banned}then plan a fix");
            assert!(
                charter_inert_violation(&value),
                "{banned:?} must be flagged"
            );
        }
        assert!(
            charter_inert_violation("line one\nline two"),
            "a newline must be flagged even though it is not in CHARTER_INERT_BANNED itself"
        );
    }

    #[test]
    fn charter_inert_violation_allows_parentheses_and_quotes() {
        // I4's own stated allowance (REMEDIATION.md item 4): neither can
        // cause an embedded span to execute, so neutering them would ban
        // ordinary prose for no safety gain.
        assert!(!charter_inert_violation(
            "investigate story <n> (see \"the plan\") and report back"
        ));
    }

    #[test]
    fn charter_inert_violation_allows_clean_text() {
        assert!(!charter_inert_violation(
            "Investigate and plan a fix for story <n> in this repo."
        ));
    }

    /// The regression this module's own tests caught while writing this
    /// check: `render_template`'s four placeholder tokens are legitimate
    /// template syntax, each containing `<`/`>`, and a custom
    /// `STORY_PROMPT` is EXPECTED to use them -- flagging every one would
    /// have refused the shipped default templates themselves, which use
    /// `<n>` verbatim.
    #[test]
    fn charter_inert_violation_allows_every_sanctioned_placeholder() {
        assert!(!charter_inert_violation(
            "story <n> in worktree <name> at <dir>, then run <reap>"
        ));
    }

    /// The other half of the same fix: exempting the placeholder tokens
    /// must not accidentally exempt `<`/`>` everywhere. A stray bracket
    /// that is not part of one of the four exact tokens is still a real
    /// shell redirection risk and must still be caught.
    #[test]
    fn charter_inert_violation_still_catches_a_stray_bracket_next_to_a_placeholder() {
        assert!(charter_inert_violation("story <n> > /tmp/exfil"));
        assert!(charter_inert_violation(
            "story <nope> is not a real placeholder"
        ));
    }

    /// Every bare `$NAME` token referenced in `composition_line`, in order of
    /// first appearance, deduplicated.
    ///
    /// Skips a `$` immediately followed by `{`: `AUTO_PROMPT_TPL`'s own line
    /// opens with `${STORY_AUTO_PROMPT:-...}`, which names an *override* env
    /// var, not a shipped charter piece — the same distinction the prompt
    /// override check draws. A bare `$AUTO_PROMPT_HEAD` inside that default value,
    /// by contrast, names a piece this test must check.
    fn charter_vars_referenced(composition_line: &str) -> Vec<String> {
        let mut vars = Vec::new();
        let mut rest = composition_line;
        while let Some(dollar) = rest.find('$') {
            rest = &rest[dollar + 1..];
            if rest.starts_with('{') {
                continue;
            }
            let ident: String = rest
                .chars()
                .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                .collect();
            if !ident.is_empty() && !vars.contains(&ident) {
                vars.push(ident);
            }
        }
        vars
    }

    /// The shipped defaults must themselves pass this check -- otherwise
    /// every UNMODIFIED dispatch (`STORY_PROMPT`/`STORY_AUTO_PROMPT` never
    /// set at all) would refuse itself the moment an operator's daemon
    /// environment happened to also carry either var for an unrelated
    /// reason. Extracted directly from the checked-in script rather than
    /// copy-pasted, so a future edit to either default that reintroduces a
    /// banned character (item 4's own "nobody maintains any of this"
    /// warning) is caught here rather than only at dispatch time.
    #[test]
    fn the_shipped_default_templates_are_charter_inert() {
        let script = include_str!("../../plugins/story/bin/story.sh");

        // PROMPT_TPL is still one "${STORY_PROMPT:-literal}" default.
        let line = script
            .lines()
            .find(|l| l.starts_with("PROMPT_TPL="))
            .expect("story.sh must still define PROMPT_TPL on its own line");
        let default = line
            .strip_prefix("PROMPT_TPL=\"${STORY_PROMPT:-")
            .and_then(|rest| rest.strip_suffix("}\""))
            .expect("PROMPT_TPL's default-value shape changed -- update this extraction");
        assert!(
            !charter_inert_violation(default),
            "PROMPT_TPL's shipped default must itself be CHARTER-INERT"
        );

        // SH-219: AUTO_PROMPT_TPL and AUTO_PROMPT_SOLO_TPL are no longer
        // single literals -- each composes several pieces, themselves plain
        // literal assignments defined just above the composition. Checking
        // each piece on its own (rather than the composed line, which by now
        // names OTHER SHELL VARIABLES, not charter text) covers both
        // rendered charters transitively: a single space joins clean pieces
        // into a still-clean whole.
        //
        // The piece NAMES are derived from the composition lines themselves
        // rather than hand-listed here (SH-402): a hand-maintained list is
        // exactly the shape this project fences everywhere else it recurs
        // (`tests/priority_rubric.rs`'s module doc names SH-136 and SH-198
        // as the cost of it), and it is what let a fifth charter clause ship
        // silently unchecked the first time SH-402 added one. Floor- and
        // membership-guarded so a composition line that stopped naming its
        // pieces reads as a failure, not a vacuously short list.
        let mut vars: Vec<String> = ["AUTO_PROMPT_TPL=", "AUTO_PROMPT_SOLO_TPL="]
            .iter()
            .flat_map(|prefix| {
                let line = script
                    .lines()
                    .find(|l| l.starts_with(prefix))
                    .unwrap_or_else(|| {
                        panic!("story.sh must still define {prefix} on its own line")
                    });
                charter_vars_referenced(line)
            })
            .collect();
        vars.sort();
        vars.dedup();
        assert!(
            vars.len() >= 4,
            "the autonomous charter composition lines named only {vars:?} -- either a \
             piece was inlined or this scan has stopped seeing the composition"
        );
        for expected in ["AUTO_PROMPT_HEAD", "AUTO_PROMPT_TAIL"] {
            assert!(
                vars.iter().any(|v| v == expected),
                "the autonomous charter composition lines did not reference {expected}: {vars:?}"
            );
        }

        for var in vars {
            let prefix = format!("{var}=\"");
            let line = script
                .lines()
                .find(|l| l.starts_with(&prefix))
                .unwrap_or_else(|| panic!("story.sh must still define {var} on its own line"));
            let default = line
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix('"'))
                .unwrap_or_else(|| {
                    panic!("{var}'s literal-assignment shape changed -- update this extraction")
                });
            assert!(
                !charter_inert_violation(default),
                "{var}'s shipped text must itself be CHARTER-INERT"
            );
        }
    }

    #[test]
    fn prompt_override_violation_is_none_when_everything_is_absent() {
        assert_eq!(
            prompt_override_violation(false, None, None, None, None),
            None
        );
        assert_eq!(
            prompt_override_violation(true, None, None, None, None),
            None
        );
    }

    #[test]
    fn prompt_override_violation_is_none_when_everything_is_clean() {
        assert_eq!(
            prompt_override_violation(
                false,
                Some("a clean prompt"),
                Some("also clean"),
                Some("also clean too"),
                Some("extra")
            ),
            None
        );
        assert_eq!(
            prompt_override_violation(
                true,
                Some("a clean prompt"),
                Some("also clean"),
                Some("also clean too"),
                Some("extra")
            ),
            None
        );
    }

    #[test]
    fn prompt_override_violation_catches_story_prompt_when_attended() {
        assert_eq!(
            prompt_override_violation(false, Some("rm -rf `whoami`"), None, None, None),
            Some("STORY_PROMPT")
        );
    }

    #[test]
    fn prompt_override_violation_ignores_story_prompt_when_auto() {
        // STORY_PROMPT governs the ATTENDED template; an --auto dispatch
        // never reads it, so a bad value there must not refuse a dispatch
        // that was never going to use it.
        assert_eq!(
            prompt_override_violation(true, Some("rm -rf `whoami`"), None, None, None),
            None
        );
    }

    #[test]
    fn prompt_override_violation_catches_story_auto_prompt_only_when_auto() {
        assert_eq!(
            prompt_override_violation(true, None, Some("$(danger)"), None, None),
            Some("STORY_AUTO_PROMPT")
        );
        assert_eq!(
            prompt_override_violation(false, None, Some("$(danger)"), None, None),
            None,
            "STORY_AUTO_PROMPT must not gate an attended dispatch"
        );
    }

    /// SH-219: `council_vote_available`'s probe runs inside `story.sh`, below
    /// this daemon entirely, so a `--auto` dispatch cannot know in advance
    /// whether the COUNCIL or SOLO charter is the one about to be rendered —
    /// it must refuse on a dirty override of EITHER.
    #[test]
    fn prompt_override_violation_catches_story_auto_prompt_solo_only_when_auto() {
        assert_eq!(
            prompt_override_violation(true, None, None, Some("$(danger)"), None),
            Some("STORY_AUTO_PROMPT_SOLO")
        );
        assert_eq!(
            prompt_override_violation(false, None, None, Some("$(danger)"), None),
            None,
            "STORY_AUTO_PROMPT_SOLO must not gate an attended dispatch"
        );
    }

    #[test]
    fn prompt_override_violation_catches_story_prompt_extra_in_either_mode() {
        for auto in [false, true] {
            assert_eq!(
                prompt_override_violation(auto, None, None, None, Some("please; also do X")),
                Some("STORY_PROMPT_EXTRA"),
                "STORY_PROMPT_EXTRA applies to every mode, auto={auto}"
            );
        }
    }

    #[test]
    fn prompt_override_violation_reports_the_template_before_extra_when_both_are_dirty() {
        assert_eq!(
            prompt_override_violation(false, Some("bad `here`"), None, None, Some("also; bad")),
            Some("STORY_PROMPT")
        );
    }

    fn capture_of(content: &str) -> std::fs::File {
        use std::io::Write;
        let mut file = tempfile::tempfile().expect("a scratch file");
        file.write_all(content.as_bytes())
            .expect("writing fixture content");
        file
    }

    /// A minimal but *valid* `story.sh` stand-in: everything a resolution
    /// test needs, and nothing more — including the marker
    /// `check_dispatch_protocol` now requires, so these tests keep
    /// exercising resolution order rather than tripping the protocol check
    /// that has its own tests, below.
    const FAKE_STORY_SH: &str = "#!/usr/bin/env bash\nDISPATCH_PROTOCOL=1\n";

    #[test]
    fn resolve_dispatch_script_honours_the_env_override() {
        let mut script = tempfile::NamedTempFile::new().expect("a scratch file");
        std::io::Write::write_all(&mut script, FAKE_STORY_SH.as_bytes())
            .expect("write fixture content");
        let resolved = resolve_dispatch_script_from(
            Some(script.path().to_string_lossy().into_owned()),
            None,
            None,
        );
        assert_eq!(
            resolved.expect("the override names a real file"),
            script.path()
        );
    }

    #[test]
    fn resolve_dispatch_script_names_the_bad_path_when_the_override_is_wrong() {
        let resolved = resolve_dispatch_script_from(Some("/no/such/file".to_string()), None, None);
        let message = resolved.expect_err("a nonexistent override must not silently fall through");
        assert!(message.contains("/no/such/file"));
    }

    /// Writes `home/.claude/plugins/installed_plugins.json` naming one
    /// `story@storyhook` install record per `install_dirs`, and a real
    /// [`FAKE_STORY_SH`] under each — the shape `installed_plugin_script`
    /// expects. Returns `home` (kept alive for the caller) and the install
    /// directories in the same order they were given.
    fn fake_installed_plugin_home(install_dirs: &[&str]) -> tempfile::TempDir {
        let home = storyhook_test_support::scratch_dir();
        let records: Vec<serde_json::Value> = install_dirs
            .iter()
            .map(|dir| {
                let install_dir = home.path().join(dir);
                std::fs::create_dir_all(install_dir.join("bin")).expect("mkdir install/bin");
                std::fs::write(install_dir.join("bin/story.sh"), FAKE_STORY_SH)
                    .expect("write fake story.sh");
                serde_json::json!({"installPath": install_dir.to_string_lossy()})
            })
            .collect();
        let manifest_dir = home.path().join(".claude/plugins");
        std::fs::create_dir_all(&manifest_dir).expect("mkdir manifest dir");
        std::fs::write(
            manifest_dir.join("installed_plugins.json"),
            serde_json::json!({"plugins": {"story@storyhook": records}}).to_string(),
        )
        .expect("write installed_plugins.json");
        home
    }

    #[test]
    fn resolve_dispatch_script_finds_the_installed_plugin_via_home() {
        let home = fake_installed_plugin_home(&["plugins/cache/storyhook/story/0.5.0"]);
        let resolved = resolve_dispatch_script_from(None, Some(home.path().to_path_buf()), None);
        assert_eq!(
            resolved.expect("an installed plugin should resolve"),
            home.path()
                .join("plugins/cache/storyhook/story/0.5.0/bin/story.sh")
        );
    }

    #[test]
    fn resolve_dispatch_script_uses_the_last_install_record_when_several_exist() {
        // Matches how a reinstall of the same scope is recorded: appended,
        // never replaced in place (installed_plugin_script's own doc).
        let home = fake_installed_plugin_home(&[
            "plugins/cache/storyhook/story/0.4.0",
            "plugins/cache/storyhook/story/0.5.0",
        ]);
        let resolved = resolve_dispatch_script_from(None, Some(home.path().to_path_buf()), None);
        assert_eq!(
            resolved.expect("the last record should resolve"),
            home.path()
                .join("plugins/cache/storyhook/story/0.5.0/bin/story.sh"),
            "the newest (last) install record must win, not the first"
        );
    }

    #[test]
    fn resolve_dispatch_script_prefers_the_env_override_over_an_installed_plugin() {
        let home = fake_installed_plugin_home(&["plugins/cache/storyhook/story/0.5.0"]);
        let mut override_script = tempfile::NamedTempFile::new().expect("a scratch file");
        std::io::Write::write_all(&mut override_script, FAKE_STORY_SH.as_bytes())
            .expect("write fixture content");
        let resolved = resolve_dispatch_script_from(
            Some(override_script.path().to_string_lossy().into_owned()),
            Some(home.path().to_path_buf()),
            None,
        );
        assert_eq!(resolved.unwrap(), override_script.path());
    }

    #[test]
    fn resolve_dispatch_script_falls_back_to_dev_root_when_no_plugin_is_installed() {
        let home = storyhook_test_support::scratch_dir();
        let dev_root = storyhook_test_support::scratch_dir();
        std::fs::create_dir_all(dev_root.path().join("plugins/story/bin"))
            .expect("mkdir dev checkout script dir");
        let dev_script = dev_root.path().join("plugins/story/bin/story.sh");
        std::fs::write(&dev_script, FAKE_STORY_SH).expect("write dev story.sh");
        let resolved = resolve_dispatch_script_from(
            None,
            Some(home.path().to_path_buf()),
            Some(dev_root.path().to_path_buf()),
        );
        assert_eq!(resolved.unwrap(), dev_script);
    }

    #[test]
    fn resolve_dispatch_script_ignores_a_manifest_missing_the_story_key() {
        let home = storyhook_test_support::scratch_dir();
        let manifest_dir = home.path().join(".claude/plugins");
        std::fs::create_dir_all(&manifest_dir).expect("mkdir manifest dir");
        std::fs::write(
            manifest_dir.join("installed_plugins.json"),
            serde_json::json!({"plugins": {}}).to_string(),
        )
        .expect("write installed_plugins.json without a story@storyhook entry");
        let resolved = resolve_dispatch_script_from(None, Some(home.path().to_path_buf()), None);
        assert!(
            resolved.is_err(),
            "a manifest with no story@storyhook entry must not resolve"
        );
    }

    #[test]
    fn resolve_dispatch_script_ignores_an_install_record_whose_script_is_missing() {
        let home = storyhook_test_support::scratch_dir();
        // installPath is named in the manifest but nothing was ever written
        // under it -- e.g. a manually-edited or corrupted manifest entry.
        let install_dir = home.path().join("plugins/cache/storyhook/story/0.5.0");
        let manifest_dir = home.path().join(".claude/plugins");
        std::fs::create_dir_all(&manifest_dir).expect("mkdir manifest dir");
        std::fs::write(
            manifest_dir.join("installed_plugins.json"),
            serde_json::json!({
                "plugins": {"story@storyhook": [{"installPath": install_dir.to_string_lossy()}]}
            })
            .to_string(),
        )
        .expect("write installed_plugins.json naming a script that doesn't exist");
        let resolved = resolve_dispatch_script_from(None, Some(home.path().to_path_buf()), None);
        assert!(
            resolved.is_err(),
            "an install record whose script is missing on disk must not resolve"
        );
    }

    /// Writes `content` to a fresh scratch file and returns both the file's
    /// path and the directory guard that must outlive it -- a script
    /// [`declared_dispatch_protocol`] can read back, distinct from
    /// [`capture_of`] (a `File` handle for `classify`, unrelated to a path
    /// on disk). Bind the guard as `let (_guard, path) = ...` so the scratch
    /// directory is not removed out from under the path before the test
    /// finishes reading it.
    fn script_with_content(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = storyhook_test_support::scratch_dir();
        let path = dir.path().join("story.sh");
        std::fs::write(&path, content).expect("write scratch script");
        (dir, path)
    }

    #[test]
    fn declared_dispatch_protocol_reads_a_line_start_assignment() {
        let (_guard, path) =
            script_with_content("#!/usr/bin/env bash\nDISPATCH_PROTOCOL=3\nset -euo pipefail\n");
        assert_eq!(declared_dispatch_protocol(&path), 3);
    }

    #[test]
    fn declared_dispatch_protocol_is_zero_for_a_script_with_no_marker() {
        let (_guard, path) = script_with_content("#!/usr/bin/env bash\nset -euo pipefail\n");
        assert_eq!(declared_dispatch_protocol(&path), 0);
    }

    #[test]
    fn declared_dispatch_protocol_ignores_a_mention_inside_a_comment() {
        // Only a line whose first non-whitespace characters are the exact
        // assignment counts -- a passing reference in prose must not be
        // mistaken for the real declaration.
        let (_guard, path) = script_with_content(
            "#!/usr/bin/env bash\n# see DISPATCH_PROTOCOL=1 in the header above\n",
        );
        assert_eq!(declared_dispatch_protocol(&path), 0);
    }

    #[test]
    fn declared_dispatch_protocol_is_zero_for_a_nonexistent_path() {
        assert_eq!(
            declared_dispatch_protocol(Path::new("/no/such/story.sh")),
            0
        );
    }

    #[test]
    fn resolve_dispatch_script_refuses_a_script_older_than_required() {
        let (_guard, path) = script_with_content("#!/usr/bin/env bash\nDISPATCH_PROTOCOL=0\n");
        let resolved =
            resolve_dispatch_script_from(Some(path.to_string_lossy().into_owned()), None, None);
        let message = resolved.expect_err("a script declaring an older protocol must be refused");
        assert!(message.contains(&path.display().to_string()));
        assert!(message.contains("dispatch protocol 0"));
        assert!(message.contains(&REQUIRED_DISPATCH_PROTOCOL.to_string()));
        assert!(message.contains("out of date"));
        assert!(message.contains("story plugin install claude"));
        assert!(message.contains("story plugin install codex"));
    }

    #[test]
    fn resolve_dispatch_script_refuses_a_script_with_no_marker_at_all() {
        // The exact shape of the machine that produced SH-196: an installed
        // plugin cut before this protocol existed carries no marker line.
        let (_guard, path) = script_with_content("#!/usr/bin/env bash\nset -euo pipefail\n");
        let resolved =
            resolve_dispatch_script_from(Some(path.to_string_lossy().into_owned()), None, None);
        assert!(resolved.is_err());
    }

    #[test]
    fn resolve_dispatch_script_accepts_a_script_newer_than_required() {
        // Forward-compatible on purpose: a plugin release must never have
        // to wait on a daemon rebuild just because it moved the protocol
        // number forward.
        let (_guard, path) = script_with_content(&format!(
            "#!/usr/bin/env bash\nDISPATCH_PROTOCOL={}\n",
            REQUIRED_DISPATCH_PROTOCOL + 1
        ));
        let resolved =
            resolve_dispatch_script_from(Some(path.to_string_lossy().into_owned()), None, None);
        assert_eq!(resolved.expect("a newer script must still resolve"), path);
    }

    #[test]
    fn resolve_dispatch_script_applies_the_protocol_check_to_an_installed_plugin_too() {
        // Not just the override: an installed plugin resolved from a real
        // installed_plugins.json is checked the same way -- this is the
        // exact resolution path SH-196's own bug traveled.
        let home = storyhook_test_support::scratch_dir();
        let install_dir = home.path().join("plugins/cache/storyhook/story/0.4.0");
        std::fs::create_dir_all(install_dir.join("bin")).expect("mkdir install/bin");
        std::fs::write(
            install_dir.join("bin/story.sh"),
            "#!/usr/bin/env bash\nset -euo pipefail\n",
        )
        .expect("write a pre-protocol fake story.sh");
        let manifest_dir = home.path().join(".claude/plugins");
        std::fs::create_dir_all(&manifest_dir).expect("mkdir manifest dir");
        std::fs::write(
            manifest_dir.join("installed_plugins.json"),
            serde_json::json!({
                "plugins": {"story@storyhook": [{"installPath": install_dir.to_string_lossy()}]}
            })
            .to_string(),
        )
        .expect("write installed_plugins.json");
        let resolved = resolve_dispatch_script_from(None, Some(home.path().to_path_buf()), None);
        let message =
            resolved.expect_err("an installed plugin predating the marker must be refused");
        assert!(message.contains("out of date"));
    }
}
