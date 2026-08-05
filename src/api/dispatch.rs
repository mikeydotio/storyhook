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
//! [`crate::daemon::serve::dispatch`] is the single thread that owns the
//! store, fed by a rendezvous channel of capacity zero — every other
//! request queues behind whichever [`crate::api::rest::route`] or
//! [`crate::api::rpc::route`] call is in flight. A dispatch takes 15-35
//! seconds even on the happy path (worktree creation, then waiting for
//! claude's TUI to accept a pasted prompt), and it gets there by making
//! several of its own `story` CLI calls — each of which, since store
//! isolation landed, reaches this *same* daemon over its own
//! `/api/v1/invoke` connection. Answering a dispatch request on the store
//! thread would therefore deadlock on the first nested call: the request
//! occupying that thread would be waiting on a child that is waiting on
//! that same thread.
//!
//! So this module is intercepted in [`crate::daemon::serve::worker`],
//! before a `Job` is ever built — the same place `GET /api/events` is
//! answered in full for the same reason. The subprocess itself runs on its
//! own detached thread, tracked in a [`DispatchRegistry`] that a `GET`
//! polls; nothing here ever touches `Serving::store`.
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

use serde::Serialize;
use tiny_http::{Header, Method};
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
const MAX_RUNNING: usize = 4;

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
    fn try_start(&self, project: &str, story: &str, started_at: String) -> StartOutcome {
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
/// project or story exists.
#[allow(clippy::too_many_arguments)]
pub fn intercept(
    segments: &[&str],
    method: &Method,
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
        (Method::Post, 6) => Some(handle_post(project, story, env, bus, registry)),
        (Method::Get, 7) => Some(handle_get(segments[6], registry)),
        _ => Some(text_reply(405, "Method Not Allowed")),
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
fn handle_post(
    project: &str,
    story: &str,
    env: &Environment,
    bus: &ChangeBus,
    registry: &Arc<DispatchRegistry>,
) -> Reply {
    let script = match resolve_dispatch_script() {
        Ok(script) => script,
        Err(message) => return text_reply(500, message),
    };
    match registry.try_start(project, story, env.now()) {
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

/// Spawns `script --project <project> dispatch <story>` on a detached
/// thread and records its outcome when it finishes. Never touches the
/// store: everything this needs travels in its arguments.
fn spawn_dispatch(
    registry: Arc<DispatchRegistry>,
    handle: String,
    script: PathBuf,
    project: String,
    story: String,
    env: Environment,
    bus: ChangeBus,
) {
    std::thread::spawn(move || {
        let (state, payload, error) = run_child(&script, &project, &story, &env);
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
        .arg(story)
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
fn resolve_dispatch_script() -> Result<PathBuf, String> {
    resolve_dispatch_script_from(std::env::var("STORYHOOK_DISPATCH_SCRIPT").ok())
}

/// [`resolve_dispatch_script`]'s logic, with the environment variable read
/// injected rather than performed here — reading `std::env::var` directly
/// would make this untestable without mutating process-global state, which
/// a parallel test run cannot safely do (`cargo test` runs `#[test]`
/// functions across threads in one process by default).
fn resolve_dispatch_script_from(configured: Option<String>) -> Result<PathBuf, String> {
    if let Some(configured) = configured {
        let path = PathBuf::from(configured);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "STORYHOOK_DISPATCH_SCRIPT names `{}`, which is not a file",
                path.display()
            ))
        };
    }
    if let Some(path) = installed_plugin_script() {
        return Ok(path);
    }
    if let Some(root) = crate::plugin::dev_repo_root() {
        let path = root.join("plugin/claude-code/bin/story.sh");
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(
        "could not find plugin/claude-code/bin/story.sh -- install the plugin \
         (`story plugin install <target>`) or set STORYHOOK_DISPATCH_SCRIPT"
            .to_string(),
    )
}

/// `story@storyhook`'s installed path, from Claude Code's own plugin
/// manifest, if that path still holds the script. The manifest is a JSON
/// array of install records per plugin key; the last one wins, matching how
/// a reinstall of the same scope is recorded (append, never replace-in-place).
fn installed_plugin_script() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let raw =
        std::fs::read_to_string(PathBuf::from(home).join(".claude/plugins/installed_plugins.json"))
            .ok()?;
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
        let first = match registry.try_start("proj", "SH-1", "t0".to_string()) {
            StartOutcome::Started(handle) => handle,
            _ => panic!("first start must succeed"),
        };
        match registry.try_start("proj", "SH-1", "t1".to_string()) {
            StartOutcome::AlreadyRunning(handle) => assert_eq!(handle, first),
            other => panic!("expected AlreadyRunning, story is still running: {other:?}"),
        }
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
        let handle = match registry.try_start("proj", "SH-1", "t0".to_string()) {
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
        match registry.try_start("proj", "SH-1", "t2".to_string()) {
            StartOutcome::Started(second) => assert_ne!(second, handle),
            other => panic!("a finished dispatch must not block a fresh one: {other:?}"),
        }
    }

    #[test]
    fn capacity_is_enforced_across_distinct_stories() {
        let registry = DispatchRegistry::new();
        for n in 0..MAX_RUNNING {
            match registry.try_start(&format!("proj-{n}"), &format!("SH-{n}"), "t".to_string()) {
                StartOutcome::Started(_) => {}
                other => panic!("expected room for dispatch {n}: {other:?}"),
            }
        }
        match registry.try_start("proj-extra", "SH-extra", "t".to_string()) {
            StartOutcome::AtCapacity => {}
            other => panic!("expected AtCapacity beyond MAX_RUNNING: {other:?}"),
        }
    }

    #[test]
    fn finished_records_beyond_the_retention_count_are_evicted_oldest_first() {
        let registry = DispatchRegistry::new();
        let mut handles = Vec::new();
        for n in 0..(RETAIN_FINISHED + 3) {
            let handle =
                match registry.try_start(&format!("proj-{n}"), &format!("SH-{n}"), "t".to_string())
                {
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

    #[test]
    fn resolve_dispatch_script_honours_the_env_override() {
        let script = tempfile::NamedTempFile::new().expect("a scratch file");
        let resolved =
            resolve_dispatch_script_from(Some(script.path().to_string_lossy().into_owned()));
        assert_eq!(
            resolved.expect("the override names a real file"),
            script.path()
        );
    }

    #[test]
    fn resolve_dispatch_script_names_the_bad_path_when_the_override_is_wrong() {
        let resolved = resolve_dispatch_script_from(Some("/no/such/file".to_string()));
        let message = resolved.expect_err("a nonexistent override must not silently fall through");
        assert!(message.contains("/no/such/file"));
    }
}
