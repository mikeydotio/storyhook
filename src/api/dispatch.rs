//! The dashboard's dispatch endpoint (SH-50).
//!
//! `POST /api/repos/{project}/story/{id}/dispatch` runs the same
//! `plugin/claude-code/bin/story.sh dispatch` the CLI's `/story do` uses:
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
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::daemon::http1::{Header, Method};
use serde::Serialize;
use wait_timeout::ChildExt;

use crate::api::http::{Reply, mutation_guard_ok, text_reply};
use crate::api::rpc::token_ok;
use crate::daemon::bus::{Change, ChangeBus};
use crate::env::Environment;

/// How long a dispatch may run before its whole process group is killed.
///
/// `story.sh`'s own documented worst case for the readiness gate and prompt
/// submission is under 35 seconds; the genuinely unbounded part is `git
/// fetch`, which the child's `GIT_TERMINAL_PROMPT=0` keeps off an
/// interactive credential prompt but not off a slow or unreachable remote.
/// 180 seconds is generous headroom over the documented worst case without
/// being unbounded.
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(180);

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
const RETAIN_FINISHED: usize = 32;

/// How long a finished record is kept regardless of how many newer ones
/// have arrived.
const RETAIN_FOR: Duration = Duration::from_secs(30 * 60);

/// The most stdout or stderr this module reads back from a dispatch child.
///
/// Bounds memory against a runaway script within the timeout window above;
/// `story.sh`'s own JSON result and diagnostic tail are a few hundred bytes
/// at most; a hard `set -e` abort's stderr is rarely more than a few lines.
const MAX_CAPTURE_BYTES: u64 = 64 * 1024;

/// One dispatch's lifecycle, as reported to a polling client.
///
/// `finished_at`, `payload` and `error` are absent while [`DispatchState`]
/// is [`Running`](DispatchState::Running) and present in every terminal
/// state — `skip_serializing_if` rather than always emitting `null`, so a
/// client's presence check (`"finished_at" in record`) is the same idiom
/// the rest of this API already uses for an absent value.
#[derive(Clone, Debug, Serialize)]
pub struct DispatchRecord {
    /// Opaque id a client polls with. Never reused.
    pub handle: String,
    /// The project slug this dispatch was asked to act on.
    pub project: String,
    /// The story id this dispatch was asked to act on.
    pub story: String,
    /// Whether this dispatch runs `story.sh`'s autonomous charter (SH-208):
    /// plan approval is still the one human interaction, but everything
    /// past it — council-voting open questions, merging its own PR, closing
    /// the story, reclaiming its own worktree — runs unattended. Never
    /// `skip_serializing_if`, unlike the fields below: this is never
    /// absent, only ever true or false, for the lifetime of the record.
    pub auto: bool,
    pub state: DispatchState,
    /// RFC3339, when this record was created.
    pub started_at: String,
    /// RFC3339, set once `state` leaves [`Running`](DispatchState::Running).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// `story.sh`'s own JSON result, relayed verbatim — present for
    /// [`Ok`](DispatchState::Ok) and [`Refused`](DispatchState::Refused),
    /// since a refusal is a well-formed result the script chose to report,
    /// not a failure of the script itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Set only for [`Failed`](DispatchState::Failed): the script could not
    /// be run, did not finish, or exited without printing a result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Where one dispatch attempt stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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
    /// The script could not be found or run, was killed for overrunning
    /// [`DISPATCH_TIMEOUT`], or exited without printing a result.
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

/// Tracks every dispatch this daemon has started, in memory only. Restarting
/// the daemon forgets every record — a dispatch that outlives the request
/// that started it does not need its bookkeeping to outlive the daemon.
pub struct DispatchRegistry {
    inner: Mutex<Inner>,
}

impl DispatchRegistry {
    pub fn new() -> Self {
        DispatchRegistry {
            inner: Mutex::new(Inner::default()),
        }
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
    fn try_start(
        &self,
        project: &str,
        story: &str,
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
                    auto,
                    state: DispatchState::Running,
                    started_at,
                    finished_at: None,
                    payload: None,
                    error: None,
                },
                finished_instant: None,
            },
        );
        inner
            .running_by_story
            .insert(story.to_string(), handle.clone());
        StartOutcome::Started(handle)
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

    /// Records `handle`'s outcome and releases its story back to the
    /// running set, so the next `POST` for that story starts fresh.
    fn finish(
        &self,
        handle: &str,
        story: &str,
        state: DispatchState,
        finished_at: String,
        payload: Option<serde_json::Value>,
        error: Option<String>,
    ) {
        let mut inner = self.inner.lock().expect("dispatch registry lock");
        if let Some(entry) = inner.entries.get_mut(handle) {
            entry.record.state = state;
            entry.record.finished_at = Some(finished_at);
            entry.record.payload = payload;
            entry.record.error = error;
            entry.finished_instant = Some(Instant::now());
        }
        inner.running_by_story.remove(story);
        inner.finished_order.push_back(handle.to_string());
        Self::evict(&mut inner);
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
        }
    }
}

impl Default for DispatchRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// A project slug or story id, validated against the shape `story.sh` itself
/// requires (`valid_story_id`, `plugin/claude-code/bin/story.sh`):
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
#[allow(clippy::too_many_arguments)]
pub fn intercept(
    segments: &[&str],
    method: &Method,
    query: Option<&str>,
    headers: &[Header],
    trusted_hosts: &[String],
    token: &str,
    env: &Environment,
    bus: &ChangeBus,
    registry: &Arc<DispatchRegistry>,
) -> Option<Reply> {
    if segments.len() < 6
        || segments.len() > 7
        || segments[0] != "api"
        || segments[1] != "repos"
        || segments[3] != "story"
        || segments[5] != "dispatch"
    {
        return None;
    }

    if !mutation_guard_ok(headers, trusted_hosts) {
        return Some(text_reply(403, "Forbidden"));
    }
    if !token_ok(headers, token) {
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
            Some(handle_post(project, story, auto, env, bus, registry))
        }
        (Method::Get, 7) => Some(handle_get(segments[6], registry)),
        _ => Some(text_reply(405, "Method Not Allowed")),
    }
}

/// Parses the dispatch endpoint's one query parameter, `auto` (SH-208).
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
    auto: bool,
    env: &Environment,
    bus: &ChangeBus,
    registry: &Arc<DispatchRegistry>,
) -> Reply {
    let script = match resolve_dispatch_script() {
        Ok(script) => script,
        Err(message) => return text_reply(500, message),
    };
    match registry.try_start(project, story, auto, env.now()) {
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

/// Spawns `script --project <project> dispatch <story> [--auto]` on a
/// detached thread and records its outcome when it finishes. Never touches
/// the store: everything this needs travels in its arguments.
#[allow(clippy::too_many_arguments)]
fn spawn_dispatch(
    registry: Arc<DispatchRegistry>,
    handle: String,
    script: PathBuf,
    project: String,
    story: String,
    auto: bool,
    env: Environment,
    bus: ChangeBus,
) {
    std::thread::spawn(move || {
        let (state, payload, error) = run_child(&script, &project, &story, auto, &env);
        registry.finish(&handle, &story, state, env.now(), payload, error);
        // Lets an open dashboard tab refresh without polling this endpoint
        // itself — the story moved to in-progress (or didn't), and the
        // ordinary `repo-changed` handling already knows how to react.
        bus.publish(Change::Project(project));
    });
}

/// Runs one dispatch child to completion (or to [`DISPATCH_TIMEOUT`]) and
/// classifies its outcome. Never returns [`DispatchState::Running`].
fn run_child(
    script: &Path,
    project: &str,
    story: &str,
    auto: bool,
    env: &Environment,
) -> (DispatchState, Option<serde_json::Value>, Option<String>) {
    let stdout_file = match tempfile::tempfile() {
        Ok(file) => file,
        Err(e) => {
            return (
                DispatchState::Failed,
                None,
                Some(format!("could not stage dispatch output: {e}")),
            );
        }
    };
    let stderr_file = match tempfile::tempfile() {
        Ok(file) => file,
        Err(e) => {
            return (
                DispatchState::Failed,
                None,
                Some(format!("could not stage dispatch output: {e}")),
            );
        }
    };
    let (child_stdout, child_stderr) = match (stdout_file.try_clone(), stderr_file.try_clone()) {
        (Ok(out), Ok(err)) => (out, err),
        (Err(e), _) | (_, Err(e)) => {
            return (
                DispatchState::Failed,
                None,
                Some(format!("could not stage dispatch output: {e}")),
            );
        }
    };

    // The daemon's own binary, so the child's `story` calls run the exact
    // build serving this request rather than whatever `story` PATH
    // resolves to — the same reasoning `daemon::lifecycle::spawn_child`
    // already applies to itself.
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("story"));

    let mut command = Command::new("bash");
    command
        .arg(script)
        .arg("--project")
        .arg(project)
        .arg("dispatch")
        .arg(story);
    if auto {
        // The one argument this endpoint adds to story.sh's own dispatch
        // argv (SH-208) — everything else about the child is identical
        // between the two modes; `story.sh` itself is what swaps the
        // handoff prompt for the autonomous charter on seeing this flag.
        command.arg("--auto");
    }
    command
        // Never inherited from the daemon's own cwd, which a daemon spawned
        // from a since-deleted directory (a build's temp dir, a cleaned-up
        // worktree) can hold indefinitely — bash's own startup calls
        // `getcwd()` before the script's first line runs, and fails loudly
        // on stderr if that directory is gone. `story.sh`'s own
        // `enter_checkout` immediately `cd`s away from whatever this is, so
        // its only job is to always exist.
        .current_dir(env.home())
        .env("STORY_BIN", &exe)
        .env("STORYHOOK_STORE_PATH", env.store_path())
        // A dashboard caller has no tmux session of its own to name ahead
        // of time (SH-50) — one session per project, created if it is not
        // there yet.
        .env("STORY_TARGET_SESSION", project)
        .env("STORY_CREATE_SESSION", "1")
        // `story.sh` does not set this itself; the daemon must, since a
        // blocked credential prompt has nobody to answer it.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(child_stdout)
        .stderr(child_stderr);
    // Its own process group, so a timeout kills whatever the script started
    // (git, tmux, a claude probe) along with the script itself — killing
    // only the leader would orphan the rest, still running.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return (
                DispatchState::Failed,
                None,
                Some(format!("failed to start the dispatch script: {e}")),
            );
        }
    };
    let pid = child.id();

    match child.wait_timeout(DISPATCH_TIMEOUT) {
        Ok(Some(_status)) => classify(stdout_file, stderr_file),
        Ok(None) => {
            // Negative pid = the whole process group established above, so
            // nothing the script spawned survives it.
            #[cfg(unix)]
            // SAFETY: the group id of a process this thread just spawned
            // and has not yet reaped, so it cannot have been recycled onto
            // an unrelated group.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            (
                DispatchState::Failed,
                None,
                Some(format!(
                    "dispatch did not finish within {}s and was terminated",
                    DISPATCH_TIMEOUT.as_secs()
                )),
            )
        }
        Err(e) => (
            DispatchState::Failed,
            None,
            Some(format!("could not wait for the dispatch process: {e}")),
        ),
    }
}

/// Classifies a finished child's captured stdout/stderr. `story.sh` always
/// emits exactly one JSON object on stdout on any deliberate exit path
/// (`fail`/`refuse`/success alike); a `set -e` abort before that point is
/// the one case with nothing parseable, which is what distinguishes
/// [`DispatchState::Failed`] from a business [`DispatchState::Refused`].
fn classify(
    stdout_file: std::fs::File,
    stderr_file: std::fs::File,
) -> (DispatchState, Option<serde_json::Value>, Option<String>) {
    let stdout = read_capture(stdout_file);
    match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        Ok(value) => {
            let ok = value.get("ok").and_then(serde_json::Value::as_bool);
            let state = if ok == Some(true) {
                DispatchState::Ok
            } else {
                DispatchState::Refused
            };
            (state, Some(value), None)
        }
        Err(_) => {
            let stderr = read_capture(stderr_file);
            let message = if stderr.trim().is_empty() {
                "the dispatch script exited without printing a result".to_string()
            } else {
                stderr.trim().to_string()
            };
            (DispatchState::Failed, None, Some(message))
        }
    }
}

/// Rewinds and reads up to [`MAX_CAPTURE_BYTES`] of `file`, lossily —
/// diagnostic text, never data this module acts on, so a stray non-UTF-8
/// byte must not turn "the script said something" into "nothing was read
/// at all".
fn read_capture(mut file: std::fs::File) -> String {
    let mut buf = Vec::new();
    if file.seek(SeekFrom::Start(0)).is_ok() {
        let _ = file.take(MAX_CAPTURE_BYTES).read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// The daemon<->script argv contract [`resolve_dispatch_script`] requires of
/// whichever `story.sh` it resolves (SH-196). `plugin/claude-code/bin/
/// story.sh` declares the contract it implements in its own
/// `DISPATCH_PROTOCOL` constant; bump both together, and see that
/// constant's doc comment for the rule on when a bump is actually needed.
pub const REQUIRED_DISPATCH_PROTOCOL: u32 = 1;

/// Locates `plugin/claude-code/bin/story.sh`, in order:
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
fn resolve_dispatch_script() -> Result<PathBuf, String> {
    resolve_dispatch_script_from(
        std::env::var("STORYHOOK_DISPATCH_SCRIPT").ok(),
        std::env::var("HOME").ok().map(PathBuf::from),
        crate::plugin::dev_repo_root(),
    )
}

/// [`resolve_dispatch_script`]'s logic, with every environment/filesystem
/// read injected rather than performed here — reading `std::env::var` or
/// resolving `$HOME` directly would make this untestable without mutating
/// process-global state, which a parallel test run cannot safely do
/// (`cargo test` runs `#[test]` functions across threads in one process by
/// default). This is also what makes `installed_plugin_script`'s real
/// behavior against a real `installed_plugins.json` testable at all — before
/// this, only the `configured` override branch had any coverage (SH-196).
fn resolve_dispatch_script_from(
    configured: Option<String>,
    home: Option<PathBuf>,
    dev_root: Option<PathBuf>,
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
    if let Some(path) = home.and_then(|home| installed_plugin_script(&home)) {
        return check_dispatch_protocol(path);
    }
    if let Some(root) = dev_root {
        let path = root.join("plugin/claude-code/bin/story.sh");
        if path.is_file() {
            return check_dispatch_protocol(path);
        }
    }
    Err(
        "could not find plugin/claude-code/bin/story.sh -- install the plugin \
         (`story plugin install <target>`) or set STORYHOOK_DISPATCH_SCRIPT"
            .to_string(),
    )
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
         plugin is out of date. Update it with `story plugin install claude-code` (or \
         `claude plugin update story@storyhook`), then retry.",
        path.display()
    ))
}

/// Reads `path`'s own declared `DISPATCH_PROTOCOL=<n>` line — the constant
/// `plugin/claude-code/bin/story.sh` itself defines near its top — without
/// executing it. A `bash -c` probe would only echo this same constant at the
/// cost of a process spawn on every resolution, and resolution deliberately
/// runs before anything about the script is trusted (see
/// [`resolve_dispatch_script`]'s own note on not pre-checking
/// `bash`/`jq`/`git`/`tmux`). Only a line whose *first* non-whitespace
/// characters are the exact assignment counts — a mention inside a comment
/// (`# see DISPATCH_PROTOCOL=1 above`) does not. `0` for a script that
/// predates the marker entirely, or whose declaration this cannot parse —
/// both read the same to a caller: "not new enough."
fn declared_dispatch_protocol(path: &Path) -> u32 {
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
                &[],
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
                &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            DispatchState::Ok,
            "t1".to_string(),
            Some(serde_json::json!({"ok": true})),
            None,
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
                DispatchState::Ok,
                "t".to_string(),
                Some(serde_json::json!({"ok": true})),
                None,
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

    #[test]
    fn classify_reports_ok_for_a_successful_result() {
        let (state, payload, error) =
            classify(capture_of(r#"{"ok":true,"id":"SH-1"}"#), capture_of(""));
        assert_eq!(state, DispatchState::Ok);
        assert_eq!(payload.unwrap()["id"], "SH-1");
        assert!(error.is_none());
    }

    #[test]
    fn classify_reports_refused_for_a_well_formed_refusal() {
        let (state, payload, error) = classify(
            capture_of(r#"{"ok":false,"display":"not ready"}"#),
            capture_of(""),
        );
        assert_eq!(state, DispatchState::Refused);
        assert_eq!(payload.unwrap()["display"], "not ready");
        assert!(error.is_none());
    }

    #[test]
    fn classify_reports_failed_for_unparseable_output_and_carries_stderr() {
        let (state, payload, error) = classify(
            capture_of("not json at all"),
            capture_of("bash: jq: command not found"),
        );
        assert_eq!(state, DispatchState::Failed);
        assert!(payload.is_none());
        assert!(error.unwrap().contains("jq: command not found"));
    }

    #[test]
    fn classify_reports_failed_with_a_generic_message_when_nothing_was_said_at_all() {
        let (state, _payload, error) = classify(capture_of(""), capture_of(""));
        assert_eq!(state, DispatchState::Failed);
        assert!(error.unwrap().contains("without printing a result"));
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
        std::fs::create_dir_all(dev_root.path().join("plugin/claude-code/bin"))
            .expect("mkdir dev checkout script dir");
        let dev_script = dev_root.path().join("plugin/claude-code/bin/story.sh");
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
        assert!(message.contains("story plugin install claude-code"));
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
