//! The process boundary used by the Full Auto engine.
//!
//! [`Dispatcher`] is deliberately synchronous. The engine decides which
//! thread owns an attempt; this module decides what one attempt means. That
//! keeps the store-pool deadlock rule in the caller while giving reconcile
//! tests a seam that never needs a worktree, tmux server, or agent process.

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use regex::Regex;
use wait_timeout::ChildExt;

use crate::env::Environment;
use crate::env::spawn_env::apply_dispatch_allowlist;
use crate::error::AppError;
use crate::store::EngineAgent;

/// How long the worktree/tmux/agent helper may run.
///
/// This is the dashboard dispatch bound moved to the shared seam: the script's
/// readiness handoff is bounded below this, while a networked `git fetch` is
/// the genuinely variable part.
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(180);

/// A tmux client normally answers in milliseconds. The shared machine-probe
/// budget bounds a wedged server without inventing another patience value.
const TMUX_TIMEOUT: Duration = crate::daemon::tailnet::TAILNET_PROBE_TIMEOUT;

/// Bounds diagnostics from a faulty helper or tmux client.
const MAX_CAPTURE_BYTES: u64 = 64 * 1024;

const PROMPT_OVERRIDE_ENV_VARS: [&str; 4] = [
    "STORY_PROMPT",
    "STORY_AUTO_PROMPT",
    "STORY_AUTO_PROMPT_SOLO",
    "STORY_PROMPT_EXTRA",
];

const TEMPLATE_PLACEHOLDERS: [&str; 5] = ["<name>", "<dir>", "<reap>", "<n>", "<done-state>"];
pub(crate) const CHARTER_INERT_BANNED: [char; 8] = ['`', '$', ';', '&', '|', '<', '>', '!'];

/// Everything the shell actuator needs to dispatch one already-selected story.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchRequest {
    pub project: String,
    pub story: String,
    pub agent: EngineAgent,
}

/// A parsed answer from `story.sh`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchOutcomeState {
    /// The helper answered `"ok": true`.
    Ok,
    /// The helper returned a well-formed refusal payload.
    Refused,
}

/// The helper's own answer, classified without replacing any of its fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchOutcome {
    pub state: DispatchOutcomeState,
    pub payload: serde_json::Value,
}

impl DispatchOutcome {
    /// Builds an outcome from the helper's complete JSON value.
    #[must_use]
    pub fn from_payload(payload: serde_json::Value) -> Self {
        let state = if payload.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
            DispatchOutcomeState::Ok
        } else {
            DispatchOutcomeState::Refused
        };
        Self { state, payload }
    }
}

/// The testability seam around worktree/tmux/agent side effects.
pub trait Dispatcher: Send + Sync {
    fn dispatch(&self, request: DispatchRequest) -> Result<DispatchOutcome, AppError>;
    fn window_alive(&self, window: &str) -> bool;
    fn kill_window(&self, window: &str) -> Result<(), AppError>;
}

/// Production dispatcher backed by Storyhook's existing shell helper and tmux.
pub struct ShellDispatcher {
    story_sh_path: PathBuf,
    env: Environment,
    tmux_program: OsString,
}

impl ShellDispatcher {
    #[must_use]
    pub fn new(story_sh_path: impl Into<PathBuf>, env: Environment) -> Self {
        Self {
            story_sh_path: story_sh_path.into(),
            env,
            tmux_program: OsString::from("tmux"),
        }
    }

    fn tmux(&self) -> Command {
        Command::new(&self.tmux_program)
    }
}

impl Dispatcher for ShellDispatcher {
    fn dispatch(&self, request: DispatchRequest) -> Result<DispatchOutcome, AppError> {
        run_shell_dispatch(
            &self.story_sh_path,
            &request.project,
            &request.story,
            request.agent,
            true,
            &self.env,
        )
    }

    fn window_alive(&self, window: &str) -> bool {
        let mut command = self.tmux();
        command.args([
            "display-message",
            "-p",
            "-t",
            window,
            "#{pane_pid}\t#{pane_current_command}\t#{pane_dead}",
        ]);
        let captured = match run_captured(command, TMUX_TIMEOUT) {
            Ok(captured) if captured.status.success() => captured,
            _ => return false,
        };
        let answer = String::from_utf8_lossy(&captured.stdout);
        let mut fields = answer.trim_end().splitn(3, '\t');
        let Some(pid) = fields.next().and_then(|raw| raw.parse::<i32>().ok()) else {
            return false;
        };
        let Some(command) = fields.next() else {
            return false;
        };
        if fields.next() != Some("0") || !pid_is_live(pid) {
            return false;
        }
        ProcessIdentity::from_process().matches(command)
    }

    fn kill_window(&self, window: &str) -> Result<(), AppError> {
        let mut command = self.tmux();
        command.args(["kill-window", "-t", window]);
        match run_captured(command, TMUX_TIMEOUT) {
            Ok(captured) if captured.status.success() => Ok(()),
            Ok(captured) => {
                let detail = String::from_utf8_lossy(&captured.stderr).trim().to_string();
                let suffix = if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {detail}")
                };
                Err(AppError::Storage(format!(
                    "tmux refused to kill window `{window}`{suffix}"
                )))
            }
            Err(CaptureError::Timeout) => Err(AppError::Storage(format!(
                "tmux did not answer while killing window `{window}` within {}s",
                TMUX_TIMEOUT.as_secs()
            ))),
            Err(error) => Err(AppError::Storage(format!(
                "could not kill tmux window `{window}`: {}",
                error.detail()
            ))),
        }
    }
}

/// Runs one helper invocation. The dashboard uses `auto` from its request;
/// [`ShellDispatcher`] always supplies true for engine lanes.
pub(crate) fn run_shell_dispatch(
    script: &Path,
    project: &str,
    story: &str,
    agent: EngineAgent,
    auto: bool,
    env: &Environment,
) -> Result<DispatchOutcome, AppError> {
    let [
        story_prompt,
        story_auto_prompt,
        story_auto_prompt_solo,
        story_prompt_extra,
    ] = PROMPT_OVERRIDE_ENV_VARS.map(|name| std::env::var(name).ok());
    if let Some(name) = prompt_override_violation(
        auto,
        story_prompt.as_deref(),
        story_auto_prompt.as_deref(),
        story_auto_prompt_solo.as_deref(),
        story_prompt_extra.as_deref(),
    ) {
        let display = format!(
            "[story] refused to dispatch {story} — this daemon's own ${name} environment value \
             contains a character CHARTER-INERT bans (one of ` $ ; & | < > ! or a newline) \
             and would be pasted into a live shell-backed pane verbatim. Fix ${name} in the \
             daemon's own environment and restart it, then retry."
        );
        return Ok(DispatchOutcome::from_payload(serde_json::json!({
            "ok": false,
            "reason": "unsafe-prompt-override",
            "display": display,
            "env_var": name,
        })));
    }

    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("story"));
    let mut command = Command::new("bash");
    command
        .arg(script)
        .arg("--project")
        .arg(project)
        .arg("dispatch")
        .arg(story)
        .arg(format!("--agent={}", agent.as_str()));
    if auto {
        command.arg("--auto");
    }
    apply_dispatch_allowlist(&mut command);
    command
        .current_dir(env.home())
        .env("STORY_BIN", exe)
        .env("STORYHOOK_STORE_PATH", env.store_path())
        .env("STORY_TARGET_SESSION", project)
        .env("STORY_CREATE_SESSION", "1")
        .env("GIT_TERMINAL_PROMPT", "0");

    let captured = run_captured(command, DISPATCH_TIMEOUT).map_err(|error| match error {
        CaptureError::Stage(detail) => {
            AppError::Storage(format!("could not stage dispatch output: {detail}"))
        }
        CaptureError::Spawn(detail) => {
            AppError::Storage(format!("failed to start the dispatch script: {detail}"))
        }
        CaptureError::Wait(detail) => {
            AppError::Storage(format!("could not wait for the dispatch process: {detail}"))
        }
        CaptureError::Timeout => AppError::Storage(format!(
            "dispatch did not finish within {}s and was terminated",
            DISPATCH_TIMEOUT.as_secs()
        )),
    })?;
    classify_dispatch_bytes(&captured.stdout, &captured.stderr)
}

fn classify_dispatch_bytes(stdout: &[u8], stderr: &[u8]) -> Result<DispatchOutcome, AppError> {
    match serde_json::from_slice::<serde_json::Value>(trim_ascii(stdout)) {
        Ok(payload) => Ok(DispatchOutcome::from_payload(payload)),
        Err(_) => {
            let stderr = String::from_utf8_lossy(stderr).trim().to_string();
            let message = if stderr.is_empty() {
                "the dispatch script exited without printing a result".to_string()
            } else {
                stderr
            };
            Err(AppError::Storage(message))
        }
    }
}

#[cfg(test)]
pub(crate) fn classify_dispatch_files(
    stdout: File,
    stderr: File,
) -> Result<DispatchOutcome, AppError> {
    classify_dispatch_bytes(&read_capture(stdout), &read_capture(stderr))
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

pub(crate) fn charter_inert_violation(value: &str) -> bool {
    let mut stripped = value.to_string();
    for token in TEMPLATE_PLACEHOLDERS {
        stripped = stripped.replace(token, "");
    }
    stripped.contains(CHARTER_INERT_BANNED) || stripped.contains('\n')
}

pub(crate) fn prompt_override_violation(
    auto: bool,
    story_prompt: Option<&str>,
    story_auto_prompt: Option<&str>,
    story_auto_prompt_solo: Option<&str>,
    story_prompt_extra: Option<&str>,
) -> Option<&'static str> {
    let mut candidates = Vec::with_capacity(3);
    if auto {
        candidates.push(("STORY_AUTO_PROMPT", story_auto_prompt));
        candidates.push(("STORY_AUTO_PROMPT_SOLO", story_auto_prompt_solo));
    } else {
        candidates.push(("STORY_PROMPT", story_prompt));
    }
    candidates.push(("STORY_PROMPT_EXTRA", story_prompt_extra));
    candidates
        .into_iter()
        .find(|(_, value)| value.is_some_and(charter_inert_violation))
        .map(|(name, _)| name)
}

struct Captured {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

enum CaptureError {
    Stage(std::io::Error),
    Spawn(std::io::Error),
    Wait(std::io::Error),
    Timeout,
}

impl CaptureError {
    fn detail(&self) -> String {
        match self {
            Self::Stage(error) | Self::Spawn(error) | Self::Wait(error) => error.to_string(),
            Self::Timeout => "the process timed out".to_string(),
        }
    }
}

fn run_captured(mut command: Command, timeout: Duration) -> Result<Captured, CaptureError> {
    let stdout_file = tempfile::tempfile().map_err(CaptureError::Stage)?;
    let stderr_file = tempfile::tempfile().map_err(CaptureError::Stage)?;
    let child_stdout = stdout_file.try_clone().map_err(CaptureError::Stage)?;
    let child_stderr = stderr_file.try_clone().map_err(CaptureError::Stage)?;
    command
        .stdin(Stdio::null())
        .stdout(child_stdout)
        .stderr(child_stderr);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().map_err(CaptureError::Spawn)?;
    let pid = child.id();
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            kill_process_group(pid);
            let _ = child.wait();
            return Err(CaptureError::Timeout);
        }
        Err(error) => {
            kill_process_group(pid);
            let _ = child.wait();
            return Err(CaptureError::Wait(error));
        }
    };
    Ok(Captured {
        status,
        stdout: read_capture(stdout_file),
        stderr: read_capture(stderr_file),
    })
}

fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    // SAFETY: this is the process group created for the child immediately
    // above; it has not been reaped and therefore cannot have been recycled.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

fn read_capture(mut file: File) -> Vec<u8> {
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(0)).is_ok() {
        let _ = file.take(MAX_CAPTURE_BYTES).read_to_end(&mut bytes);
    }
    bytes
}

fn pid_is_live(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 does not alter the target; it asks the kernel whether
    // the process exists and whether this caller may signal it.
    let status = unsafe { libc::kill(pid, 0) };
    status == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

struct ProcessIdentity {
    pattern: Option<Regex>,
    launch_binaries: Vec<PathBuf>,
}

impl ProcessIdentity {
    fn from_process() -> Self {
        let pattern = std::env::var("STORY_READY_PROCESS_PATTERN")
            .unwrap_or_else(|_| "^(claude|node|codex)$".to_string());
        let launch_words = match std::env::var("STORY_LAUNCH_CMD") {
            Ok(command) => command
                .split_whitespace()
                .next()
                .map(|word| vec![word.to_string()])
                .unwrap_or_default(),
            Err(_) => vec!["claude".to_string(), "codex".to_string()],
        };
        Self {
            pattern: Regex::new(&pattern).ok(),
            launch_binaries: launch_words
                .iter()
                .filter_map(|word| resolve_executable(word))
                .collect(),
        }
    }

    fn matches(&self, observed: &str) -> bool {
        let observed = Path::new(observed)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(observed);
        if observed.is_empty() {
            return false;
        }
        if self
            .pattern
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(observed))
        {
            return true;
        }
        self.launch_binaries.iter().any(|resolved| {
            let Some(base) = resolved.file_name().and_then(|name| name.to_str()) else {
                return false;
            };
            if observed == base {
                return true;
            }
            if !is_version_name(base) || !is_version_name(observed) {
                return false;
            }
            resolved
                .parent()
                .map(|parent| parent.join(observed))
                .is_some_and(|sibling| is_executable(&sibling))
        })
    }
}

fn resolve_executable(word: &str) -> Option<PathBuf> {
    let candidate = PathBuf::from(word);
    let found = if candidate.components().count() > 1 {
        candidate
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|directory| directory.join(word))
            .find(|path| is_executable(path))?
    };
    let resolved = std::fs::canonicalize(found).ok()?;
    is_executable(&resolved).then_some(resolved)
}

fn is_version_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt as _;

        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn dispatcher_with_tmux(root: &Path, tmux_program: &Path) -> ShellDispatcher {
        ShellDispatcher {
            story_sh_path: root.join("story.sh"),
            env: Environment::at(root.join("home")),
            tmux_program: tmux_program.as_os_str().to_owned(),
        }
    }

    #[test]
    fn payload_classification_preserves_the_whole_answer() {
        let payload = serde_json::json!({
            "ok": false,
            "reason": "future-refusal",
            "future": {"nested": true}
        });
        assert_eq!(
            DispatchOutcome::from_payload(payload.clone()),
            DispatchOutcome {
                state: DispatchOutcomeState::Refused,
                payload,
            }
        );
    }

    #[test]
    fn process_identity_accepts_resolved_and_installed_sibling_versions_only() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let root = storyhook_test_support::scratch_dir();
        let versions = root.path().join("versions");
        std::fs::create_dir(&versions).unwrap();
        for version in ["2.1.227", "2.1.228"] {
            let path = versions.join(version);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        symlink(versions.join("2.1.228"), root.path().join("claude")).unwrap();
        let identity = ProcessIdentity {
            pattern: Regex::new("^(claude|node)$").ok(),
            launch_binaries: vec![std::fs::canonicalize(root.path().join("claude")).unwrap()],
        };
        assert!(identity.matches("2.1.228"));
        assert!(identity.matches("2.1.227"));
        assert!(identity.matches("node"));
        assert!(!identity.matches("9.9.9"));
        assert!(!identity.matches("zsh"));
    }

    #[test]
    fn process_identity_does_not_widen_a_plain_named_install_to_any_version() {
        let identity = ProcessIdentity {
            pattern: None,
            launch_binaries: vec![PathBuf::from("/usr/local/bin/claude")],
        };
        assert!(identity.matches("claude"));
        assert!(!identity.matches("2.1.228"));
    }

    #[test]
    fn charter_inert_check_preserves_placeholders_but_rejects_shell_syntax() {
        assert!(!charter_inert_violation(
            "Work <n> in <name> at <dir>; reap token is omitted"
                .replace(';', ",")
                .as_str()
        ));
        assert!(charter_inert_violation("story <n> > /tmp/exfil"));
        assert!(charter_inert_violation("line one\nline two"));
    }

    #[test]
    fn charter_inert_check_accepts_the_rendered_completion_state_placeholder() {
        assert!(!charter_inert_violation(
            "move story <n> to <done-state>, then run <reap>"
        ));
    }

    #[test]
    fn bounded_capture_terminates_a_process_group() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        assert!(matches!(
            run_captured(command, Duration::from_millis(20)),
            Err(CaptureError::Timeout)
        ));
    }

    #[test]
    fn shell_window_probe_requires_a_live_pid_and_agent_identity() {
        let root = storyhook_test_support::scratch_dir();
        let live = root.path().join("tmux-live");
        executable(
            &live,
            &format!("printf '{}\\tcodex\\t0\\n'", std::process::id()),
        );
        assert!(dispatcher_with_tmux(root.path(), &live).window_alive("@7"));

        let shell = root.path().join("tmux-shell");
        executable(
            &shell,
            &format!("printf '{}\\tzsh\\t0\\n'", std::process::id()),
        );
        assert!(!dispatcher_with_tmux(root.path(), &shell).window_alive("@7"));

        let dead = root.path().join("tmux-dead");
        executable(
            &dead,
            &format!("printf '{}\\tcodex\\t1\\n'", std::process::id()),
        );
        assert!(!dispatcher_with_tmux(root.path(), &dead).window_alive("@7"));
    }

    #[test]
    fn shell_kill_targets_the_exact_window_and_carries_tmux_diagnostics() {
        let root = storyhook_test_support::scratch_dir();
        let log = root.path().join("args");
        let tmux = root.path().join("tmux");
        executable(
            &tmux,
            &format!("printf '%s\\n' \"$*\" > '{}'; exit 23", log.display()),
        );
        let error = dispatcher_with_tmux(root.path(), &tmux)
            .kill_window("@exact")
            .unwrap_err();
        assert_eq!(
            std::fs::read_to_string(log).unwrap().trim(),
            "kill-window -t @exact"
        );
        assert!(
            error
                .to_string()
                .contains("tmux refused to kill window `@exact`")
        );
    }
}
