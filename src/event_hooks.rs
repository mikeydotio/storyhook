use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;
use wait_timeout::ChildExt;

/// How much of a hook's stderr is read back for the failure diagnostic.
const DIAGNOSTIC_PREFIX_BYTES: u64 = 4096;

/// An unlinked temporary file handed to a hook in place of a pipe.
///
/// # Why a file rather than a pipe
///
/// `fire_hook` used to drive three concurrent pipes **sequentially** — write the
/// payload to stdin, then wait, then read stderr — which is the classic pipe
/// deadlock [`std::process::Child::wait_with_output`] exists to prevent. Two
/// unbounded waits followed (SH-141):
///
/// * the payload write blocked once the pipe buffer filled, if nothing was
///   draining it — and it ran *before* the timeout was consulted, so
///   `timeout_seconds` was never reached at all;
/// * the stderr read blocked waiting for an end-of-file that only arrives when
///   the last descendant holding the write end dies.
///
/// Neither was bounded by anything storyhook controls. A merely *verbose* hook
/// wedged the daemon with no misbehaviour and no descendant: measured, a hook
/// writing 200 KiB to stderr and then reading its payload never returns, while
/// the same hook at 32 KiB returns in 12.9 ms — the difference is one 64 KiB
/// pipe buffer.
///
/// A regular file has no end-of-file rendezvous and no reader/writer handshake,
/// so neither operation can wait on a descriptor whose holder storyhook does not
/// control, however many descendants inherit it and however long they live. The
/// wedge stops being *representable* rather than being bounded by a new
/// deadline — which is why this fix carries no timeout of its own, no thread and
/// no `unsafe`.
///
/// Unnamed ([`tempfile::tempfile`]) rather than named: no path ever exists, so
/// there is nothing to clean up, nothing left behind after a panic or a SIGKILL,
/// and nothing another process can open.
///
/// # The invariant this type exists to own
///
/// The file position. Every accessor rewinds internally and the inner
/// [`std::fs::File`] never escapes, so the omission that would hand a hook an
/// empty payload — or read an empty diagnostic back — cannot be written at any
/// call site. It was the one silent-failure mode the file design would otherwise
/// have introduced, and it would have read exactly like "the hook said nothing".
///
/// # Accepted limitation: no backpressure
///
/// A pipe's 64 KiB buffer throttled a runaway hook; a file does not. A
/// descendant that inherits fd 2 and writes forever grows an **unlinked** file,
/// which no `du` or `ls` can find — only `lsof +L1`. Three things answer that,
/// and a hard cap is deliberately not one of them: `RLIMIT_FSIZE` would need
/// `unsafe` and a `#[cfg]`, and its `SIGXFSZ` would kill the backgrounded job,
/// breaking the promise that a hook's descendants are left alone.
///
/// **Redesign trigger:** the first report of a hook filling a disk, or the first
/// need to bound a hook child's resources for any other reason. The limitation
/// is recorded on SH-174, which already owns "hooks are unbounded inside the
/// request handler"; bounding `timeout_seconds` there bounds this too, since the
/// worst case is write bandwidth × `timeout_seconds`.
struct ScratchFile(std::fs::File);

impl ScratchFile {
    /// A scratch file holding `bytes`, positioned at its start.
    ///
    /// The written length is verified against `bytes`, so a payload that did not
    /// land is a loud error at the moment it happens rather than an empty
    /// document a hook acts on with total confidence.
    fn holding(bytes: &[u8]) -> std::io::Result<Self> {
        let mut file = tempfile::tempfile()?;
        file.write_all(bytes)?;
        file.flush()?;
        let written = file.metadata()?.len();
        if written != bytes.len() as u64 {
            return Err(std::io::Error::other(format!(
                "wrote {written} of {} payload bytes",
                bytes.len()
            )));
        }
        file.seek(SeekFrom::Start(0))?;
        Ok(Self(file))
    }

    /// An empty scratch file for a child to write into.
    fn empty() -> std::io::Result<Self> {
        Ok(Self(tempfile::tempfile()?))
    }

    /// This file as a child's stdio, consuming the handle.
    ///
    /// Consuming on purpose: the payload is never read back, so letting the
    /// handle survive would leave an un-rewound file nobody needs and a way to
    /// misuse it.
    fn into_stdio(self) -> Stdio {
        Stdio::from(self.0)
    }

    /// A duplicate of this file for a child to write into, keeping ours.
    ///
    /// The duplicate shares the file *offset* with our handle, which is why
    /// [`Self::read_prefix`] rewinds rather than assuming a position.
    fn for_child(&self) -> std::io::Result<Stdio> {
        Ok(Stdio::from(self.0.try_clone()?))
    }

    /// The first `max` bytes, and the file's true length.
    ///
    /// Loops to `max` or end-of-file: a short read is legal on a regular file
    /// too, so a single `read` gives an arbitrary prefix rather than a
    /// deterministic one — which is the defect the previous `read_stderr_limited`
    /// had, independently of the wedge.
    ///
    /// The length is returned rather than inferred from the bytes because it is
    /// what makes silence *provable*: `0` means the hook genuinely said nothing,
    /// and a positive length with no bytes read is an internally inconsistent
    /// state the caller can name instead of misreporting.
    fn read_prefix(&self, max: u64) -> std::io::Result<(Vec<u8>, u64)> {
        let len = self.0.metadata()?.len();
        let mut handle = self.0.try_clone()?;
        handle.seek(SeekFrom::Start(0))?;
        let mut buf = Vec::new();
        handle.take(max).read_to_end(&mut buf)?;
        Ok((buf, len))
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct HooksConfig {
    #[serde(default)]
    pub settings: HooksSettings,
    #[serde(default)]
    pub on_create: Option<HookDef>,
    #[serde(default)]
    pub on_state_change: Option<HookDef>,
    #[serde(default)]
    pub on_close: Option<HookDef>,
    #[serde(default)]
    pub on_comment: Option<HookDef>,
    #[serde(default)]
    pub on_priority_change: Option<HookDef>,
    #[serde(default)]
    pub on_label_change: Option<HookDef>,
    #[serde(default)]
    pub on_relationship_change: Option<HookDef>,
}

#[derive(Debug, Deserialize)]
pub struct HooksSettings {
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_timeout() -> u64 {
    10
}
fn default_enabled() -> bool {
    true
}

/// The upper bound on `hooks.settings.timeout_seconds` and any per-hook
/// [`HookDef::timeout_seconds`] override.
///
/// # Why 60s
///
/// Event hooks run inside the daemon's single request handler, so this is
/// also the longest any other queued client blocks behind one hook — 2x that
/// for the `set-state`/`set-fields` transition pair ([`transition_pair_timeout`]
/// fires both `on_state_change` and `on_close` serially), which composes
/// exactly with [`crate::daemon::lifecycle::SERVED_DEADLINE`]'s own 120s. 6x
/// the 10s default is enough for a webhook POST, a fast `git push`, or
/// kicking off a CI trigger without waiting for it to finish; a hook that
/// genuinely needs longer backgrounds the slow part with `cmd &` — the idiom
/// [`fire_hook`]'s own docs name as the sanctioned outlet for "do not wait
/// for this" — rather than being waited on directly.
///
/// **Redesign trigger** (SH-174, closed by this ceiling): the first real hook
/// that legitimately needs more than this and cannot be restructured to
/// background the slow part.
pub const HOOK_TIMEOUT_CEILING_SECS: u64 = 60;

/// `Some(reason)` if `secs`, under `label`, exceeds [`HOOK_TIMEOUT_CEILING_SECS`].
fn ceiling_violation(label: &str, secs: u64) -> Option<String> {
    (secs > HOOK_TIMEOUT_CEILING_SECS)
        .then(|| format!("{label} = {secs}s exceeds the {HOOK_TIMEOUT_CEILING_SECS}s ceiling"))
}

/// The first `timeout_seconds` — the project default or any of the seven
/// per-hook overrides — that exceeds [`HOOK_TIMEOUT_CEILING_SECS`], named so
/// the caller can say which field to fix rather than only that something is
/// wrong.
fn timeout_ceiling_violation(config: &HooksConfig) -> Option<String> {
    if let Some(reason) =
        ceiling_violation("settings.timeout_seconds", config.settings.timeout_seconds)
    {
        return Some(reason);
    }
    let slots: [(&str, Option<&HookDef>); 7] = [
        ("on_create", config.on_create.as_ref()),
        ("on_state_change", config.on_state_change.as_ref()),
        ("on_close", config.on_close.as_ref()),
        ("on_comment", config.on_comment.as_ref()),
        ("on_priority_change", config.on_priority_change.as_ref()),
        ("on_label_change", config.on_label_change.as_ref()),
        (
            "on_relationship_change",
            config.on_relationship_change.as_ref(),
        ),
    ];
    slots.into_iter().find_map(|(name, hook)| {
        let secs = hook?.timeout_seconds?;
        ceiling_violation(&format!("{name}.timeout_seconds"), secs)
    })
}

impl Default for HooksSettings {
    fn default() -> Self {
        Self {
            timeout_seconds: 10,
            enabled: true,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct HookDef {
    pub command: String,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub enum HookEventType {
    Create,
    StateChange,
    Close,
    Comment,
    PriorityChange,
    LabelChange,
    RelationshipChange,
}

impl HookEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::StateChange => "state_change",
            Self::Close => "close",
            Self::Comment => "comment",
            Self::PriorityChange => "priority_change",
            Self::LabelChange => "label_change",
            Self::RelationshipChange => "relationship_change",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "create" => Some(Self::Create),
            "state_change" | "state-change" => Some(Self::StateChange),
            "close" => Some(Self::Close),
            "comment" => Some(Self::Comment),
            "priority_change" | "priority-change" => Some(Self::PriorityChange),
            "label_change" | "label-change" => Some(Self::LabelChange),
            "relationship_change" | "relationship-change" => Some(Self::RelationshipChange),
            _ => None,
        }
    }
}

/// The repository's event-hook configuration, from whichever of its two homes
/// it lives in.
///
/// The committed pointer file's `[hooks]` table is consulted first and the
/// legacy `.storyhook/hooks.toml` second. Both, rather than one, because the
/// two storage models coexist until the legacy web daemon is retired: a
/// repository that has been moved into the store carries its hooks in the
/// pointer, and one that has not still carries them in the directory. A
/// repository with both is answered by the pointer, which is the file its
/// current storyhook writes about and reads.
pub fn load_hooks_config(root: &Path) -> Option<HooksConfig> {
    match load_hooks_config_result(root) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("warning: {message}");
            None
        }
    }
}

/// [`load_hooks_config`]'s full result, before it collapses "misconfigured"
/// and "absent" into the same `None`.
///
/// `Ok(None)`: no `[hooks]` table exists anywhere for this project. `Ok(Some(_))`:
/// a valid, ceiling-respecting config. `Err(_)`: a `[hooks]` table exists but
/// is unusable — a TOML parse failure or a [`HOOK_TIMEOUT_CEILING_SECS`]
/// violation — worded for a human.
///
/// [`load_hooks_config`] is the right choice for every hook-firing and
/// deadline-computing caller, which all treat "misconfigured" and "absent"
/// identically on purpose: neither fires anything. [`list_hooks`] and
/// [`test_hook`] are the two surfaces that explain themselves to a human, and
/// a user who raised `timeout_seconds` past the ceiling deserves to be told
/// that specifically rather than shown the same message an empty project
/// gets — the SH-174 council's binding addendum against shipping a second
/// silent-refusal path in the same story that deleted the first one.
fn load_hooks_config_result(root: &Path) -> Result<Option<HooksConfig>, String> {
    if let Some(table) = crate::service::project::pointer_hooks(root) {
        return match table.try_into::<HooksConfig>() {
            Ok(config) => match timeout_ceiling_violation(&config) {
                None => Ok(Some(config)),
                Some(reason) => Err(format!(
                    "the [hooks] table in .storyhook.toml refuses to load: {reason}"
                )),
            },
            Err(e) => Err(format!(
                "the [hooks] table in .storyhook.toml failed to parse: {e}"
            )),
        };
    }
    let path = root.join(".storyhook/hooks.toml");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    match toml::from_str::<HooksConfig>(&raw) {
        Ok(config) => match timeout_ceiling_violation(&config) {
            None => Ok(Some(config)),
            Some(reason) => Err(format!("hooks.toml refuses to load: {reason}")),
        },
        Err(e) => Err(format!("hooks.toml failed to parse: {e}")),
    }
}

/// The largest timeout configured for any hook at `root`'s project, or `None`
/// if it has no hooks configured at all.
///
/// Used to widen a served-command deadline (SH-173) for every command except
/// the two [`transition_pair_timeout`] covers instead. `hook_depth` caps
/// *nesting* at one — a hook's own child cannot itself trigger a hook — but
/// that bounds recursion, not how many distinct hook types one served
/// command can fire in sequence: `StoryService::fire_transition_hooks`
/// (`service/story.rs`) fires `on_state_change` and then `on_close`, serially,
/// in the same call whenever a transition closes a story (SH-174). For every
/// command that reaches only one hook type per call — which is every command
/// except `set-state`/`set-fields` — a command's own completion time really
/// is bounded above by *some* configured hook's timeout, so a deadline that
/// ignores hook configuration entirely is what turns a legitimately slow
/// hook into an abandoned command. Deliberately the *largest* configured
/// value rather than the one hook the command would actually fire: resolving
/// that precisely would mean rebuilding the CLI-command-to-hook-event mapping
/// a second time on the daemon's cold, request-only path, for a number that
/// is already a generous upper bound either way.
#[must_use]
pub fn max_configured_timeout(root: &Path) -> Option<Duration> {
    let config = load_hooks_config(root)?;
    let default = config.settings.timeout_seconds;
    let slots: [Option<&HookDef>; 7] = [
        config.on_create.as_ref(),
        config.on_state_change.as_ref(),
        config.on_close.as_ref(),
        config.on_comment.as_ref(),
        config.on_priority_change.as_ref(),
        config.on_label_change.as_ref(),
        config.on_relationship_change.as_ref(),
    ];
    slots
        .into_iter()
        .flatten()
        .map(|hook| hook.timeout_seconds.unwrap_or(default))
        .max()
        .map(Duration::from_secs)
}

/// `on_state_change`'s configured timeout plus `on_close`'s, or `None` if
/// neither is configured — the one pair of hooks a single served command can
/// fire serially (SH-174).
///
/// `StoryService::fire_transition_hooks` always fires `on_state_change` for a
/// transition, and also fires `on_close` when the transition closes the
/// story — both `set-state` (`story move`) and `set-fields` (`story set`)
/// reach it. [`max_configured_timeout`]'s max-of-one-hook widening
/// under-counts this specific pair: a widening based on the larger of the
/// two alone can be exceeded by their sum. A sum, not a max, because unlike
/// every other command's single hook firing, these two genuinely both run,
/// one after the other, in the worst case.
#[must_use]
pub fn transition_pair_timeout(root: &Path) -> Option<Duration> {
    let config = load_hooks_config(root)?;
    let default = config.settings.timeout_seconds;
    if config.on_state_change.is_none() && config.on_close.is_none() {
        return None;
    }
    let secs: u64 = [config.on_state_change.as_ref(), config.on_close.as_ref()]
        .into_iter()
        .flatten()
        .map(|hook| hook.timeout_seconds.unwrap_or(default))
        .sum();
    Some(Duration::from_secs(secs))
}

fn resolve_hook(config: &HooksConfig, event_type: HookEventType) -> Option<&HookDef> {
    match event_type {
        HookEventType::Create => config.on_create.as_ref(),
        HookEventType::StateChange => config.on_state_change.as_ref(),
        HookEventType::Close => config.on_close.as_ref(),
        HookEventType::Comment => config.on_comment.as_ref(),
        HookEventType::PriorityChange => config.on_priority_change.as_ref(),
        HookEventType::LabelChange => config.on_label_change.as_ref(),
        HookEventType::RelationshipChange => config.on_relationship_change.as_ref(),
    }
}

/// How deep inside an event hook this process is running.
///
/// A hook shells out to `story`, and the child must not fire the hook that
/// spawned it. Anything unparseable counts as "inside a hook", because the safe
/// answer to an ambiguous depth is to fire nothing.
///
/// Read here for a process that *is* the CLI. A daemon must not use it: the
/// depth belongs to the invocation, which arrived over a wire, and the daemon's
/// own process environment says nothing about it.
#[must_use]
pub fn depth_from_env() -> u32 {
    match std::env::var("STORYHOOK_HOOK_DEPTH") {
        Ok(raw) => raw.trim().parse().unwrap_or(1),
        Err(_) => 0,
    }
}

/// Fires one hook, at recursion depth `depth`.
///
/// `depth` is a parameter rather than an environment read, and that is what
/// makes hooks terminate under a daemon. The CLI's depth comes from its
/// environment; a daemon's comes from the request envelope, and its own process
/// environment — which never has the variable set — would say every invocation
/// was a fresh one. A hook that shells out to `story` would then fire the hook
/// that spawned it, forever.
///
/// The child is told `depth + 1`, so the chain is describable rather than merely
/// stopped.
///
/// # The environment contract (SH-193)
///
/// **This command's environment is the daemon's, not yours.** No
/// `env_clear` runs before `hook.command` executes, deliberately — see
/// [`crate::env::spawn_env`]'s header for why an arbitrary user-authored
/// command cannot safely have its environment narrowed the way
/// [`crate::env::git_env`] narrows a trusted `git` invocation. That means a
/// hook inherits whatever the daemon itself was holding: possibly a
/// credential exported in a shell that started this daemon for an entirely
/// different project, hours or days before this hook fired, still present
/// because the daemon outlives the client that spawned it. Do not assume the
/// environment is fresh, and do not assume it belongs to whoever triggered
/// this event — write a hook that names every variable it depends on rather
/// than relying on ambient state.
pub fn fire_hook(
    root: &Path,
    config: &HooksConfig,
    event_type: HookEventType,
    payload_json: &str,
    depth: u32,
) {
    if !config.settings.enabled {
        return;
    }

    // Loop prevention.
    if depth >= 1 {
        return;
    }

    let Some(hook) = resolve_hook(config, event_type) else {
        return;
    };
    let timeout = Duration::from_secs(
        hook.timeout_seconds
            .unwrap_or(config.settings.timeout_seconds),
    );
    let event_name = event_type.as_str();

    // Both scratch files are created before the spawn, and a failure to create
    // either means the hook does not run. Firing anyway would hand it a
    // valid-looking empty payload, and it would take a silently wrong action —
    // worse than not firing and saying so.
    let payload = match ScratchFile::holding(payload_json.as_bytes()) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("warning: {event_name} hook did not run: could not stage its payload: {e}");
            return;
        }
    };
    let diagnostics = match ScratchFile::empty() {
        Ok(file) => file,
        Err(e) => {
            eprintln!(
                "warning: {event_name} hook did not run: could not stage its diagnostics: {e}"
            );
            return;
        }
    };
    let child_stderr = match diagnostics.for_child() {
        Ok(stdio) => stdio,
        Err(e) => {
            eprintln!(
                "warning: {event_name} hook did not run: could not stage its diagnostics: {e}"
            );
            return;
        }
    };

    // The payload is delivered in full, with a real end-of-file, from the
    // instant the hook starts — rather than written into a pipe afterwards,
    // which is what a hook reading stdin early used to block on.
    let child = Command::new("sh")
        .args(["-c", &hook.command])
        .stdin(payload.into_stdio())
        .stdout(Stdio::null())
        .stderr(child_stderr)
        .current_dir(root)
        .env("STORYHOOK_HOOK_DEPTH", (depth + 1).to_string())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: {event_name} hook failed to spawn: {e}");
            return;
        }
    };

    let outcome = match child.wait_timeout(timeout) {
        Ok(Some(status)) if status.success() => return,
        Ok(Some(status)) => HookOutcome::Exited {
            status: status.to_string(),
            said: Diagnostics::read(&diagnostics),
        },
        Ok(None) => {
            // Kills `sh` only. Whatever it backgrounded is left alone: those
            // descendants hold nothing of storyhook's now beyond an unlinked
            // file nobody reads, and `cmd &` is the one idiom a hook author has
            // for "do not wait for this".
            let _ = child.kill();
            let _ = child.wait();
            // Read *after* the kill, so the diagnostic carries everything the
            // hook managed to say before its deadline. The old code discarded
            // the pipe unread on this branch, which threw away the explanation
            // for the kill on the one branch where the user most needs it.
            HookOutcome::TimedOut {
                after: timeout,
                said: Diagnostics::read(&diagnostics),
            }
        }
        Err(e) => HookOutcome::NotWaitable(e.to_string()),
    };
    eprintln!("warning: {event_name} hook {outcome}");
}

/// What a hook's stderr turned out to hold.
///
/// The length is carried rather than inferred, because it is the only thing
/// that distinguishes "the hook said nothing" from "the hook said something
/// storyhook failed to read back". Reporting the second as the first is how the
/// wedge this file was rewritten for stayed invisible for so long.
enum Diagnostics {
    /// The hook said nothing at all — a length of zero, not an empty read.
    Silent,
    /// What the hook said, and the total it wrote if that is more than was read.
    Said { text: String, truncated_from: u64 },
    /// The hook wrote `len` bytes that could not be read back. Storyhook's
    /// fault, and it must not be reported as the hook's silence.
    Unreadable { len: u64, error: String },
}

impl Diagnostics {
    fn read(file: &ScratchFile) -> Self {
        match file.read_prefix(DIAGNOSTIC_PREFIX_BYTES) {
            Ok((bytes, len)) => {
                let text = String::from_utf8_lossy(&bytes).trim().to_string();
                if text.is_empty() {
                    if len == 0 {
                        Self::Silent
                    } else {
                        Self::Unreadable {
                            len,
                            error: "read back empty".to_string(),
                        }
                    }
                } else {
                    Self::Said {
                        text,
                        truncated_from: if len > DIAGNOSTIC_PREFIX_BYTES {
                            len
                        } else {
                            0
                        },
                    }
                }
            }
            Err(e) => Self::Unreadable {
                len: 0,
                error: e.to_string(),
            },
        }
    }
}

impl std::fmt::Display for Diagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Silent => Ok(()),
            Self::Said {
                text,
                truncated_from: 0,
            } => write!(f, ": {text}"),
            Self::Said {
                text,
                truncated_from,
            } => write!(
                f,
                ": {text} (showing the first {DIAGNOSTIC_PREFIX_BYTES} of {truncated_from} bytes)"
            ),
            Self::Unreadable { len: 0, error } => {
                write!(f, "; its stderr could not be read: {error}")
            }
            Self::Unreadable { len, error } => write!(
                f,
                "; it wrote {len} bytes to stderr that storyhook could not read back: {error}"
            ),
        }
    }
}

/// How a hook ended, when it did not end well.
///
/// Private, and `fire_hook` still returns `()`. A public type here would be a
/// new surface with seven call sites that legitimately discard it — and a `pub
/// fn` returning a private type trips `private_interfaces`, which `-D warnings`
/// makes a build failure, so the split is what lets this stay private at all.
enum HookOutcome {
    /// Ran to completion and failed. The status and what it said are reported
    /// **together**: they used to be mutually exclusive, so a hook that
    /// explained itself had its exit code silently discarded.
    Exited { status: String, said: Diagnostics },
    /// Outlived its deadline and was killed.
    TimedOut { after: Duration, said: Diagnostics },
    /// Storyhook could not wait for it, which is a fault of storyhook's rather
    /// than of the hook.
    NotWaitable(String),
}

impl std::fmt::Display for HookOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The status is named whether or not the hook explained itself. It
            // used to be discarded whenever stderr was non-empty, so the two
            // most useful facts about a failure were mutually exclusive.
            Self::Exited { status, said } => write!(f, "failed ({status}){said}"),
            Self::TimedOut { after, said } => write!(
                f,
                "timed out after {}s; sh was killed and anything it backgrounded is still running{said}",
                after.as_secs()
            ),
            Self::NotWaitable(error) => write!(f, "could not be waited for: {error}"),
        }
    }
}

pub fn build_payload(fields: serde_json::Value) -> String {
    serde_json::to_string(&fields).unwrap_or_default()
}

pub fn list_hooks(root: &Path) -> String {
    match load_hooks_config_result(root) {
        Err(reason) => format!("hooks are configured but not active: {reason}"),
        Ok(None) => "no hooks configured (no `[hooks]` table in .storyhook.toml)".to_string(),
        Ok(Some(config)) => {
            let mut lines = Vec::new();
            let events: [(&str, &Option<HookDef>); 7] = [
                ("on_create", &config.on_create),
                ("on_state_change", &config.on_state_change),
                ("on_close", &config.on_close),
                ("on_comment", &config.on_comment),
                ("on_priority_change", &config.on_priority_change),
                ("on_label_change", &config.on_label_change),
                ("on_relationship_change", &config.on_relationship_change),
            ];
            for (name, hook) in events {
                if let Some(h) = hook {
                    let timeout = h.timeout_seconds.unwrap_or(config.settings.timeout_seconds);
                    lines.push(format!(
                        "{name}: {cmd} (timeout: {timeout}s)",
                        cmd = h.command
                    ));
                }
            }
            if lines.is_empty() {
                "hooks.toml exists but no event hooks are configured".to_string()
            } else {
                if !config.settings.enabled {
                    lines.insert(
                        0,
                        "hooks are DISABLED (settings.enabled = false)".to_string(),
                    );
                }
                lines.join("\n")
            }
        }
    }
}

pub fn test_hook(root: &Path, event_type_str: &str) -> Result<String, crate::error::AppError> {
    let event_type = HookEventType::parse(event_type_str).ok_or_else(|| {
        crate::error::AppError::Validation(format!(
            "unknown event type `{event_type_str}` (valid: create, state_change, close, comment, priority_change, label_change, relationship_change)"
        ))
    })?;

    let config = match load_hooks_config_result(root) {
        Err(reason) => {
            return Err(crate::error::AppError::Validation(format!(
                "hooks are configured but not active: {reason}"
            )));
        }
        Ok(None) => {
            return Err(crate::error::AppError::NotFound(
                "no event hooks configured; add a `[hooks]` table to .storyhook.toml".to_string(),
            ));
        }
        Ok(Some(config)) => config,
    };

    let hook = resolve_hook(&config, event_type).ok_or_else(|| {
        crate::error::AppError::NotFound(format!("no hook configured for {event_type_str}"))
    })?;

    let payload = serde_json::json!({
        "event_type": event_type.as_str(),
        "story_id": "TEST-1",
        "timestamp": crate::service::Clock::System.now(),
        "story_title": "Test Story",
        "test": true
    });
    let payload_str = serde_json::to_string(&payload).unwrap();

    fire_hook(root, &config, event_type, &payload_str, depth_from_env());

    Ok(format!("fired {} hook: {}", event_type_str, hook.command))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch file has no name, which is what makes it clean up itself.
    ///
    /// A link count of zero *is* the property: nothing to remove on the happy
    /// path, no residue after a panic or a SIGKILL, and no path another process
    /// can open while the file holds a payload's `comment_text`. Asserting it
    /// directly rather than by watching a temp directory, because a directory's
    /// contents move for reasons unrelated to this file.
    #[cfg(unix)]
    #[test]
    fn a_scratch_file_has_no_name() {
        use std::os::unix::fs::MetadataExt;
        let spool = ScratchFile::holding(b"payload").expect("staging a payload");
        assert_eq!(
            spool.0.metadata().expect("stat").nlink(),
            0,
            "the scratch file is linked into the filesystem, so it has a path — \
             which brings back the cleanup, the residue after a crash, and the \
             disclosure surface that unlinking removes"
        );
    }

    /// A staged payload is readable from its first byte.
    ///
    /// The rewind is the whole invariant. Forgetting it hands a hook an
    /// immediate end-of-file, which is indistinguishable from an empty payload
    /// and would make the hook take a silently wrong action — so the type
    /// rewinds internally and this proves it does.
    #[test]
    fn a_staged_payload_is_positioned_at_its_start() {
        let spool = ScratchFile::holding(b"{\"event_type\":\"create\"}").expect("staging");
        let (bytes, len) = spool.read_prefix(4096).expect("reading back");
        assert_eq!(bytes, b"{\"event_type\":\"create\"}");
        assert_eq!(len, 23);
    }

    /// `read_prefix` returns exactly `max` bytes and the file's *true* length.
    ///
    /// Both halves matter. The prefix must be deterministic, which needs a loop:
    /// a short read is legal on a regular file too, so a single `read` gives an
    /// arbitrary amount. And the length has to be the file's, not the prefix's,
    /// because that is what lets a caller tell "the hook said nothing" from
    /// "the hook said something storyhook failed to read".
    #[test]
    fn read_prefix_loops_to_its_limit_and_reports_the_true_length() {
        let content = vec![b'z'; 200_000];
        let spool = ScratchFile::holding(&content).expect("staging");
        let (bytes, len) = spool.read_prefix(4096).expect("reading back");
        assert_eq!(bytes.len(), 4096, "the prefix must be the full limit");
        assert!(bytes.iter().all(|b| *b == b'z'));
        assert_eq!(len, 200_000, "the length is the file's, not the prefix's");
    }

    /// An empty scratch file reports a zero length, which is what makes silence
    /// provable rather than inferred.
    #[test]
    fn an_empty_scratch_file_proves_the_hook_said_nothing() {
        let spool = ScratchFile::empty().expect("staging");
        let (bytes, len) = spool.read_prefix(4096).expect("reading back");
        assert!(bytes.is_empty());
        assert_eq!(len, 0);
    }

    /// What a child writes is what storyhook reads back, from byte zero.
    ///
    /// `for_child` hands out a `try_clone`, which shares the file *offset* with
    /// our handle — so a child that writes 100 bytes leaves the shared position
    /// at 100. This is the case that would silently return nothing if
    /// `read_prefix` trusted the position instead of rewinding.
    #[test]
    fn a_childs_writes_are_read_back_from_the_start() {
        let spool = ScratchFile::empty().expect("staging");
        let stdio = spool.for_child().expect("a descriptor for the child");
        let status = Command::new("sh")
            .args(["-c", "printf 'the hook is unhappy' >&2"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stdio)
            .status()
            .expect("running a child");
        assert!(status.success());

        let (bytes, len) = spool.read_prefix(4096).expect("reading back");
        assert_eq!(String::from_utf8_lossy(&bytes), "the hook is unhappy");
        assert_eq!(len, 19);
    }

    /// A failing hook reports its status **and** its words, not one or the
    /// other.
    ///
    /// They were mutually exclusive: a non-empty stderr took the
    /// `hook failed: {stderr}` branch and the exit status was dropped, so the
    /// hooks that explained themselves were exactly the ones whose exit code
    /// nobody saw.
    #[test]
    fn a_failing_hook_names_its_status_even_when_it_printed_something() {
        let rendered = HookOutcome::Exited {
            status: "exit status: 7".to_string(),
            said: Diagnostics::Said {
                text: "noisy".to_string(),
                truncated_from: 0,
            },
        }
        .to_string();
        assert_eq!(rendered, "failed (exit status: 7): noisy");
    }

    /// A hook that failed silently still names its status, and says nothing
    /// more.
    #[test]
    fn a_silent_failure_reports_only_its_status() {
        let rendered = HookOutcome::Exited {
            status: "exit status: 1".to_string(),
            said: Diagnostics::Silent,
        }
        .to_string();
        assert_eq!(rendered, "failed (exit status: 1)");
    }

    /// Truncation is announced with a real byte count.
    ///
    /// The 4 KiB cap was silent, so a reader had no way to tell a hook that
    /// said little from one whose explanation was cut off mid-sentence.
    #[test]
    fn a_truncated_diagnostic_says_how_much_was_withheld() {
        let rendered = Diagnostics::Said {
            text: "the beginning".to_string(),
            truncated_from: 91_234,
        }
        .to_string();
        assert_eq!(
            rendered,
            ": the beginning (showing the first 4096 of 91234 bytes)"
        );
    }

    /// A timeout carries what the hook managed to say before it was killed, and
    /// says plainly that its descendants were not.
    ///
    /// This branch read no stderr at all before SH-141 — it discarded the pipe
    /// unread — so the explanation for the kill was thrown away on the one
    /// branch where a user most needs it.
    #[test]
    fn a_timeout_reports_what_the_hook_said_before_the_deadline() {
        let rendered = HookOutcome::TimedOut {
            after: Duration::from_secs(10),
            said: Diagnostics::Said {
                text: "halfway through".to_string(),
                truncated_from: 0,
            },
        }
        .to_string();
        assert!(rendered.starts_with("timed out after 10s"), "{rendered}");
        assert!(
            rendered.contains("anything it backgrounded is still running"),
            "the message must say what was *not* killed, because that is the \
             promise a hook author is relying on: {rendered}"
        );
        assert!(rendered.ends_with(": halfway through"), "{rendered}");
    }

    /// Bytes storyhook could not read back are never reported as the hook's
    /// silence.
    ///
    /// That distinction is the whole reason `read_prefix` returns the file's
    /// true length: the two states are indistinguishable from the bytes alone,
    /// and reporting the second as the first is the shape of failure that kept
    /// the original wedge invisible.
    #[test]
    fn unreadable_diagnostics_are_not_reported_as_silence() {
        let rendered = Diagnostics::Unreadable {
            len: 512,
            error: "read back empty".to_string(),
        }
        .to_string();
        assert!(rendered.contains("512 bytes"), "{rendered}");
        assert!(rendered.contains("could not read back"), "{rendered}");
    }

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("storyhook-event-hooks-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory")
    }

    fn write_hooks_toml(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir.join(".storyhook")).expect("the hooks directory");
        std::fs::write(dir.join(".storyhook/hooks.toml"), body).expect("writing hooks.toml");
    }

    /// No hooks configured at all is `None`, matching
    /// [`max_configured_timeout`]'s own "nothing configured" case.
    #[test]
    fn transition_pair_timeout_is_none_with_no_hooks_configured() {
        let dir = scratch();
        assert_eq!(transition_pair_timeout(dir.path()), None);
    }

    /// Neither `on_state_change` nor `on_close` configured is `None`, even
    /// when other hook types are — the pair this function reports on is
    /// exactly those two, not "any hook at all".
    #[test]
    fn transition_pair_timeout_ignores_every_hook_but_the_pair() {
        let dir = scratch();
        write_hooks_toml(
            dir.path(),
            "[settings]\ntimeout_seconds = 10\n\n\
             [on_create]\ncommand = \"true\"\ntimeout_seconds = 30\n\n\
             [on_comment]\ncommand = \"true\"\ntimeout_seconds = 30\n",
        );
        assert_eq!(transition_pair_timeout(dir.path()), None);
    }

    /// The pair sums, rather than taking the larger of the two — the whole
    /// point of this function existing separately from
    /// [`max_configured_timeout`]: `fire_transition_hooks` fires both, one
    /// after the other, when a transition closes a story, so the real
    /// worst-case cost is their total, not either one alone.
    #[test]
    fn transition_pair_timeout_sums_state_change_and_close() {
        let dir = scratch();
        write_hooks_toml(
            dir.path(),
            "[settings]\ntimeout_seconds = 10\n\n\
             [on_state_change]\ncommand = \"true\"\ntimeout_seconds = 40\n\n\
             [on_close]\ncommand = \"true\"\ntimeout_seconds = 15\n",
        );
        assert_eq!(
            transition_pair_timeout(dir.path()),
            Some(Duration::from_secs(55)),
            "55 = 40 (on_state_change) + 15 (on_close), not max(40, 15)"
        );
    }

    /// Only one of the pair configured falls back to the project default for
    /// the other slot's *absence* correctly: an unconfigured hook contributes
    /// nothing, not the settings default — `fire_hook` never runs a hook that
    /// has no `HookDef` at all (`resolve_hook` returns `None` and it's a
    /// no-op), so counting a phantom default-timeout firing would overcount.
    #[test]
    fn transition_pair_timeout_with_only_one_hook_configured_is_that_hooks_alone() {
        let dir = scratch();
        write_hooks_toml(
            dir.path(),
            "[settings]\ntimeout_seconds = 10\n\n\
             [on_state_change]\ncommand = \"true\"\ntimeout_seconds = 45\n",
        );
        assert_eq!(
            transition_pair_timeout(dir.path()),
            Some(Duration::from_secs(45)),
            "on_close is not configured, so fire_hook never runs it — it must \
             contribute 0s, not the settings default"
        );
    }

    /// A hook with no timeout of its own falls back to the project's
    /// `settings.timeout_seconds`, matching every other hook-timeout
    /// resolution in this file.
    #[test]
    fn transition_pair_timeout_falls_back_to_the_settings_default() {
        let dir = scratch();
        write_hooks_toml(
            dir.path(),
            "[settings]\ntimeout_seconds = 20\n\n\
             [on_state_change]\ncommand = \"true\"\n\n\
             [on_close]\ncommand = \"true\"\ntimeout_seconds = 5\n",
        );
        assert_eq!(
            transition_pair_timeout(dir.path()),
            Some(Duration::from_secs(25)),
            "25 = 20 (on_state_change, defaulted) + 5 (on_close, explicit)"
        );
    }

    /// A value exactly at the ceiling is accepted — the check is `>`, not
    /// `>=`, so the ceiling names the largest *usable* value rather than the
    /// smallest refused one.
    #[test]
    fn a_settings_timeout_at_the_ceiling_loads() {
        let dir = scratch();
        write_hooks_toml(
            dir.path(),
            &format!("[settings]\ntimeout_seconds = {HOOK_TIMEOUT_CEILING_SECS}\n"),
        );
        assert!(load_hooks_config(dir.path()).is_some());
    }

    /// `settings.timeout_seconds` past the ceiling refuses the whole config —
    /// not a silent clamp back down to the ceiling, which would leave a user
    /// believing they configured more than they got (SH-174).
    #[test]
    fn a_settings_timeout_over_the_ceiling_refuses_to_load() {
        let dir = scratch();
        write_hooks_toml(
            dir.path(),
            &format!(
                "[settings]\ntimeout_seconds = {}\n",
                HOOK_TIMEOUT_CEILING_SECS + 1
            ),
        );
        assert!(load_hooks_config(dir.path()).is_none());
    }

    /// A per-hook override past the ceiling refuses the *whole* config, even
    /// though `settings.timeout_seconds` itself is fine — the ceiling applies
    /// to every `timeout_seconds` field, not just the project default.
    #[test]
    fn a_per_hook_override_over_the_ceiling_refuses_the_whole_config() {
        let dir = scratch();
        write_hooks_toml(
            dir.path(),
            &format!(
                "[settings]\ntimeout_seconds = 10\n\n\
                 [on_create]\ncommand = \"true\"\ntimeout_seconds = {}\n",
                HOOK_TIMEOUT_CEILING_SECS + 1
            ),
        );
        assert!(load_hooks_config(dir.path()).is_none());
    }

    /// [`timeout_ceiling_violation`] names the specific offending field, not
    /// just "invalid config" — the whole point of checking at config-load
    /// time instead of only refusing to fire is that the message can say
    /// which line to fix.
    #[test]
    fn timeout_ceiling_violation_names_the_offending_field() {
        let dir = scratch();
        write_hooks_toml(
            dir.path(),
            "[settings]\ntimeout_seconds = 10\n\n\
             [on_close]\ncommand = \"true\"\ntimeout_seconds = 61\n",
        );
        let reason = load_hooks_config_result(dir.path()).expect_err("must refuse");
        assert!(
            reason.contains("on_close.timeout_seconds"),
            "must name the offending field: {reason}"
        );
        assert!(
            reason.contains("61"),
            "must name the offending value: {reason}"
        );
        assert!(
            reason.contains(&HOOK_TIMEOUT_CEILING_SECS.to_string()),
            "must name the ceiling itself: {reason}"
        );
    }

    /// The binding addendum from the SH-174 council: a ceiling-refused config
    /// must not read identically to a project with no hooks at all.
    /// `list_hooks` is one of the two surfaces (with [`test_hook`]) that
    /// explains itself to a human, so it must say *which* of those two very
    /// different states it is in.
    #[test]
    fn list_hooks_distinguishes_a_ceiling_refusal_from_no_hooks_at_all() {
        let absent = scratch();
        let absent_message = list_hooks(absent.path());
        assert!(
            absent_message.contains("no hooks configured"),
            "{absent_message}"
        );

        let refused = scratch();
        write_hooks_toml(
            refused.path(),
            &format!(
                "[settings]\ntimeout_seconds = {}\n",
                HOOK_TIMEOUT_CEILING_SECS + 1
            ),
        );
        let refused_message = list_hooks(refused.path());
        assert_ne!(
            absent_message, refused_message,
            "a misconfigured project must not read identically to an absent one"
        );
        assert!(
            !refused_message.contains("no hooks configured"),
            "a ceiling refusal is not the same claim as \"no hooks configured\": {refused_message}"
        );
        assert!(
            refused_message.contains("ceiling"),
            "must name the actual problem: {refused_message}"
        );
    }

    /// Same distinction as `list_hooks`, for `story hooks test`'s own error —
    /// a user testing a hook they just misconfigured must be told why it
    /// refused to load, not given the same "add a `[hooks]` table" advice an
    /// empty project gets.
    #[test]
    fn test_hook_names_the_ceiling_violation_rather_than_no_hooks_configured() {
        let dir = scratch();
        write_hooks_toml(
            dir.path(),
            &format!(
                "[settings]\ntimeout_seconds = 10\n\n\
                 [on_create]\ncommand = \"true\"\ntimeout_seconds = {}\n",
                HOOK_TIMEOUT_CEILING_SECS + 1
            ),
        );
        let error = test_hook(dir.path(), "create").expect_err("must refuse");
        let message = error.to_string();
        assert!(
            message.contains("ceiling"),
            "must name the actual problem, not the generic \"no hooks\" message: {message}"
        );
        assert!(
            !message.contains("add a `[hooks]` table"),
            "that advice is for an absent config, not a refused one: {message}"
        );
    }
}
