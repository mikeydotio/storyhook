//! The client-side half of `story claim`'s comment (SH-476).
//!
//! A claim comments by default. The default sentence names the *caller's*
//! host and tmux window, and the daemon that executes the claim is in
//! neither: `story` parses locally and sends an
//! [`crate::invoke::InvokeRequest`] over `/api/v1/invoke`, `$TMUX` is
//! per-process, and one daemon serves every client of its store. The
//! hostname would coincidentally agree today — one daemon per store, local —
//! but the tmux window never will, and a coincidence is not a fact.
//!
//! So the parser records only the *choice*
//! ([`ClaimComment`](crate::cli::ClaimComment)) and this module resolves it,
//! in the process that belongs to the person who typed the command, before
//! the request is built. That keeps `parse_invocation` pure, which is what
//! lets `tests/trailing_arguments.rs` provoke every verb in the grammar with
//! no side effects at all.

use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::cli::{ClaimComment, Invocation};

/// What the daemon says to a request that still carries
/// [`ClaimComment::Default`].
///
/// The daemon refuses rather than filling it in. It cannot see the caller's
/// tmux session, and answering with its own host would be a fabrication
/// dressed as an answer — which is precisely what SH-372 settled absence must
/// never be promoted to.
pub const UNRESOLVED_REFUSAL: &str = "internal: `story claim` reached the store with an \
                                      unresolved default comment. The default names the \
                                      caller's own host and tmux window, so only the client \
                                      can compose it — send `--comment <text>` or \
                                      `--no-comment` instead.";

/// The sentence a claim posts when the caller named no text of their own.
///
/// `window` is the caller's `session:window`, absent when the claiming
/// process is not inside tmux — a script, an MCP caller, the Full Auto
/// engine's own claims. There the tmux clause is **omitted entirely** rather
/// than completed dishonestly: never an empty window name, never a
/// placeholder, never a fabricated session (user determination, 2026-08-25).
/// An absent clause states nothing, which is the correct reading of absence.
#[must_use]
pub fn default_comment(host: &str, window: Option<&str>) -> String {
    match window {
        Some(window) => format!("Starting work on this story in {host} tmux window {window}"),
        None => format!("Starting work on this story on {host}"),
    }
}

/// Replaces a [`ClaimComment::Default`] with the text this process can
/// compose, leaving every other invocation untouched.
///
/// Called once, in `main`, immediately before the request is built — beside
/// where `$STORYHOOK_ACTOR`, the piped stdin and the GitHub credential are
/// read, and for the identical reason.
#[must_use]
pub fn resolve(invocation: Invocation) -> Invocation {
    match invocation {
        Invocation::Claim {
            target,
            comment: ClaimComment::Default,
            dry_run,
        } => Invocation::Claim {
            target,
            comment: ClaimComment::Custom(default_comment(
                &hostname().unwrap_or_else(|| "an unknown host".to_string()),
                tmux_window().as_deref(),
            )),
            dry_run,
        },
        other => other,
    }
}

/// This machine's hostname, as the OS reports it.
///
/// `libc::gethostname` rather than a new dependency: `libc` is already a
/// direct dependency of this crate. `None` when the call fails or the name is
/// empty — an unnamed host is reported as unknown rather than as an empty
/// clause in the middle of a sentence.
fn hostname() -> Option<String> {
    // POSIX permits truncation without a terminator, so the buffer carries one
    // spare byte that is never written and always read as the terminator.
    const LEN: usize = 256;
    let mut buffer = [0_i8; LEN + 1];
    // SAFETY: `buffer` is `LEN + 1` bytes and `LEN` is passed as the bound, so
    // the call can write at most `LEN` bytes and the final byte stays zero.
    let rc = unsafe { libc::gethostname(buffer.as_mut_ptr().cast::<libc::c_char>(), LEN) };
    if rc != 0 {
        return None;
    }
    let bytes: Vec<u8> = buffer
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    let name = String::from_utf8_lossy(&bytes).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// How long the tmux probe is given before it is killed and the sentence
/// degrades to the host alone.
///
/// A `tmux display-message` against a live server answers in milliseconds —
/// it is one round trip over a unix socket to a process that is already
/// running, since `$TMUX` being set is what got us here. The bound is not
/// about how long that should take, then; it is about a *wedged* server, and
/// it is derived rather than picked: the same value
/// [`TAILNET_PROBE_TIMEOUT`](crate::daemon::tailnet::TAILNET_PROBE_TIMEOUT)
/// gives the other "ask the machine about itself, degrade quietly if it will
/// not say" probe in this codebase. Two probes of the same kind should not
/// have two opinions about how patient to be.
const TMUX_PROBE_TIMEOUT: Duration = crate::daemon::tailnet::TAILNET_PROBE_TIMEOUT;

/// The caller's `session:window`, or `None` when this process is not in tmux
/// or tmux cannot be asked.
///
/// `$TMUX` is the detector — it is set by tmux in every process it starts and
/// is per-process, so a daemon reading it would answer about whoever happened
/// to start it. The window itself has to come from tmux, since no environment
/// variable carries it: `$TMUX_PANE` names the pane, not the window it
/// currently lives in, and a pane moves between windows.
///
/// Every failure degrades to `None`, which
/// [`default_comment`] renders as the host-only sentence. A claim is not
/// worth failing over a comment, and a *guessed* window is worse than no
/// window at all.
fn tmux_window() -> Option<String> {
    std::env::var_os("TMUX")?;
    let mut command = Command::new("tmux");
    command.arg("display-message").arg("-p");
    if let Some(pane) = std::env::var_os("TMUX_PANE") {
        command.arg("-t").arg(pane);
    }
    command
        .arg("#{session_name}:#{window_index}")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    // Its own process group, so a timeout kills whatever the probe started as
    // well as the probe itself — killing the leader alone leaves its children
    // orphaned, still running, and still holding the pipe this process is
    // reading. Modelled on `daemon::tailnet::tailscale_status_json`, which is
    // the same shape of question and carries the same hazard.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let child = command.spawn().ok()?;
    let pid = child.id();

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    let output = match rx.recv_timeout(TMUX_PROBE_TIMEOUT) {
        Ok(Ok(output)) if output.status.success() => output,
        Ok(_) => return None,
        Err(_) => {
            // Reported, never silent: a probe that wedged is why the comment
            // is about to name no window, and a reader who is inside tmux
            // deserves to know which of the two happened.
            eprintln!(
                "warning: `tmux display-message` did not answer within {}s; the claim's \
                 comment will name this host only",
                TMUX_PROBE_TIMEOUT.as_secs()
            );
            #[cfg(unix)]
            // SAFETY: the group id of a process this process just spawned and
            // has not yet reaped, so it cannot have been recycled onto an
            // unrelated group. The reaper thread above is still in
            // `wait_with_output`, so the killed process is collected rather
            // than left a zombie.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            return None;
        }
    };
    let window = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // A tmux that answered with nothing, or with only the separator, has told
    // us no window — the same as not being asked.
    (!window.is_empty() && window != ":").then_some(window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ClaimTarget;

    #[test]
    fn in_tmux_the_default_names_the_window() {
        assert_eq!(
            default_comment("psamathe", Some("main:3")),
            "Starting work on this story in psamathe tmux window main:3"
        );
    }

    /// The whole of the 2026-08-25 determination: outside tmux the clause is
    /// gone, not blank. A reader must never be shown a window that does not
    /// exist, and must never be shown the word "window" with nothing after it.
    #[test]
    fn outside_tmux_the_default_omits_the_tmux_clause_entirely() {
        let text = default_comment("psamathe", None);
        assert_eq!(text, "Starting work on this story on psamathe");
        assert!(
            !text.contains("tmux"),
            "the tmux clause must be absent, not empty: {text}"
        );
        assert!(
            !text.contains("window"),
            "the tmux clause must be absent, not empty: {text}"
        );
    }

    #[test]
    fn resolve_replaces_only_the_default() {
        let custom = Invocation::Claim {
            target: ClaimTarget::Story("SH-1".to_string()),
            comment: ClaimComment::Custom("mine".to_string()),
            dry_run: false,
        };
        assert_eq!(resolve(custom.clone()), custom);

        let suppressed = Invocation::Claim {
            target: ClaimTarget::Next { phase: None },
            comment: ClaimComment::Suppressed,
            dry_run: false,
        };
        assert_eq!(resolve(suppressed.clone()), suppressed);
    }

    /// The property the daemon's refusal depends on: after this step, no
    /// `Default` survives.
    #[test]
    fn resolve_leaves_no_default_behind() {
        let resolved = resolve(Invocation::Claim {
            target: ClaimTarget::Story("SH-1".to_string()),
            comment: ClaimComment::Default,
            dry_run: false,
        });
        let Invocation::Claim { comment, .. } = resolved else {
            panic!("resolve must not change the variant");
        };
        match comment {
            ClaimComment::Custom(text) => assert!(
                text.starts_with("Starting work on this story "),
                "unexpected default text: {text}"
            ),
            other => panic!("a Default must be resolved to Custom, got {other:?}"),
        }
    }

    /// Every other verb passes through untouched — this step is a claim's,
    /// not a general rewrite of the invocation.
    #[test]
    fn resolve_passes_other_invocations_through() {
        let show = Invocation::Show {
            id: "SH-1".to_string(),
        };
        assert_eq!(resolve(show.clone()), show);
    }

    /// `hostname()` answers on every platform this project builds for; the
    /// unknown-host fallback exists for the failure, not for the ordinary
    /// case.
    #[test]
    fn the_host_is_readable_on_this_machine() {
        let host = hostname().expect("this machine has a hostname");
        assert!(!host.is_empty());
        assert!(!host.contains('\0'), "the name must be trimmed at the NUL");
    }
}
