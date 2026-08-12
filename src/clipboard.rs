//! Putting text on the user's clipboard.
//!
//! Lifted out of [`crate::web`], where it was private and served one caller
//! (`story web address`). It has two callers now — `story daemon token` copies
//! the token as well — so it lives where both can reach it rather than one
//! reaching into the other.
//!
//! # Two clipboards, because there may be two machines
//!
//! A clipboard utility (`pbcopy`, `xclip`, `wl-copy`) writes to the clipboard
//! of the machine the process runs on. That is the right answer when the
//! terminal is on that machine and the wrong one whenever it is not: this
//! machine is driven over SSH and Mosh, where `pbcopy` would put a value on
//! the clipboard of the box nobody is sitting at.
//!
//! [`osc52`] is the other half. It asks the *terminal emulator* — the program
//! actually in front of the user, wherever that is — to take the text, over
//! the same connection the session already has. Neither mechanism subsumes the
//! other: a terminal may refuse OSC 52 (it is off by default in some, and tmux
//! needs `set-clipboard on`), and a remote `pbcopy` reaches the wrong machine.
//! So [`copy_everywhere`] does both and reports whether *either* worked.

use std::env;
use std::io::Write;
use std::process::Command;

use crate::error::AppError;

/// Clipboard-writer command candidates for the host OS, tried in order. Empty on
/// unsupported platforms (the caller turns that into a clear error).
#[cfg(target_os = "macos")]
fn default_clipboard_argv() -> Vec<Vec<String>> {
    vec![vec!["pbcopy".to_string()]]
}
#[cfg(target_os = "linux")]
fn default_clipboard_argv() -> Vec<Vec<String>> {
    vec![
        vec![
            "xclip".to_string(),
            "-selection".to_string(),
            "clipboard".to_string(),
        ],
        vec![
            "xsel".to_string(),
            "--clipboard".to_string(),
            "--input".to_string(),
        ],
    ]
}
#[cfg(target_os = "windows")]
fn default_clipboard_argv() -> Vec<Vec<String>> {
    vec![vec!["clip".to_string()]]
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn default_clipboard_argv() -> Vec<Vec<String>> {
    Vec::new()
}

/// Pipe `text` to a clipboard command's stdin. Stdout/stderr are discarded. The
/// caller distinguishes a missing binary (`ErrorKind::NotFound`) to try the next
/// candidate.
fn pipe_to_command(argv: &[String], text: &str) -> std::io::Result<std::process::ExitStatus> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
        // `stdin` drops here, closing the pipe so the child sees EOF.
    }
    child.wait()
}

/// Copy `text` to the system clipboard of the machine this process runs on.
///
/// Honors `$STORYHOOK_CLIPBOARD_CMD` (a non-empty value is split on whitespace
/// and used verbatim, e.g. `wl-copy`); otherwise tries the platform utilities
/// in order. A total absence of any clipboard utility maps to an actionable
/// error.
pub fn copy_to_clipboard(text: &str) -> Result<(), AppError> {
    let candidates: Vec<Vec<String>> = match env::var("STORYHOOK_CLIPBOARD_CMD") {
        Ok(c) if !c.trim().is_empty() => {
            vec![c.split_whitespace().map(str::to_string).collect()]
        }
        _ => default_clipboard_argv(),
    };
    if candidates.is_empty() {
        return Err(AppError::Storage(
            "copying to the clipboard isn't supported on this platform — set $STORYHOOK_CLIPBOARD_CMD to a clipboard command"
                .to_string(),
        ));
    }

    let mut tried = Vec::new();
    for argv in &candidates {
        match pipe_to_command(argv, text) {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                return Err(AppError::Storage(format!(
                    "clipboard command `{}` exited with status {status}",
                    argv[0]
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tried.push(argv[0].clone());
            }
            Err(e) => {
                return Err(AppError::Storage(format!(
                    "failed to run clipboard command `{}`: {e}",
                    argv[0]
                )));
            }
        }
    }
    Err(AppError::Storage(format!(
        "no clipboard utility found (tried: {}). Set $STORYHOOK_CLIPBOARD_CMD to your clipboard command (e.g. wl-copy).",
        tried.join(", ")
    )))
}
