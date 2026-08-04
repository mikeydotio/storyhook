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
    if let Some(table) = crate::service::project::pointer_hooks(root) {
        return match table.try_into::<HooksConfig>() {
            Ok(config) => Some(config),
            Err(e) => {
                eprintln!("warning: failed to parse the [hooks] table in .storyhook.toml: {e}");
                None
            }
        };
    }
    let path = root.join(".storyhook/hooks.toml");
    let raw = std::fs::read_to_string(&path).ok()?;
    match toml::from_str(&raw) {
        Ok(config) => Some(config),
        Err(e) => {
            eprintln!("warning: failed to parse hooks.toml: {e}");
            None
        }
    }
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

    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            if !status.success() {
                match diagnostics.read_prefix(DIAGNOSTIC_PREFIX_BYTES) {
                    Ok((bytes, len)) => {
                        let said = String::from_utf8_lossy(&bytes).trim().to_string();
                        if said.is_empty() {
                            // `len` is what distinguishes the two: zero is proof
                            // the hook said nothing, and a positive length we
                            // could not read back is a fault of storyhook's that
                            // must not be reported as the hook's silence.
                            if len == 0 {
                                eprintln!("warning: {event_name} hook exited with {status}");
                            } else {
                                eprintln!(
                                    "warning: {event_name} hook exited with {status}, and wrote \
                                     {len} bytes to stderr that storyhook could not read back"
                                );
                            }
                        } else {
                            eprintln!("warning: {event_name} hook failed: {said}");
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "warning: {event_name} hook exited with {status}; its stderr could \
                             not be read: {e}"
                        );
                    }
                }
            }
        }
        Ok(None) => {
            // Kills `sh` only. Whatever it backgrounded is left alone: those
            // descendants hold nothing of storyhook's now beyond an unlinked
            // file nobody reads, and `cmd &` is the one idiom a hook author has
            // for "do not wait for this".
            let _ = child.kill();
            let _ = child.wait();
            eprintln!(
                "warning: {event_name} hook timed out after {}s",
                timeout.as_secs()
            );
        }
        Err(e) => {
            eprintln!("warning: {event_name} hook error: {e}");
        }
    }
}

pub fn build_payload(fields: serde_json::Value) -> String {
    serde_json::to_string(&fields).unwrap_or_default()
}

pub fn list_hooks(root: &Path) -> String {
    match load_hooks_config(root) {
        None => "no hooks configured (no `[hooks]` table in .storyhook.toml)".to_string(),
        Some(config) => {
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

    let config = load_hooks_config(root).ok_or_else(|| {
        crate::error::AppError::NotFound(
            "no event hooks configured; add a `[hooks]` table to .storyhook.toml".to_string(),
        )
    })?;

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
}
