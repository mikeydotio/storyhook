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

/// Clipboard-writer command candidates for the host OS, tried in order. Empty
/// on a platform outside the two storyhook ships prebuilt binaries for — a
/// `#[cfg(target_os = "windows")]` arm used to sit here, hardcoding `clip`
/// for a platform nothing has ever compiled this crate against. Removed by
/// SH-276, the same call SH-260 made for `src/github/credential_store.rs`'s
/// Windows arm: no build, no test, no way to know if the argv was even
/// right. `$STORYHOOK_CLIPBOARD_CMD` still overrides on every platform, so
/// the caller turns an empty candidate list into a clear, actionable error
/// rather than a silent no-op.
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
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
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

/// The OSC 52 escape sequence that hands `text` to the terminal emulator's
/// clipboard: `ESC ] 52 ; c ; <base64> BEL`.
///
/// `c` is the clipboard selection (as opposed to `p`, the primary selection).
/// The payload is base64 because the sequence is terminated by a control
/// character, so the text has to be encoded into an alphabet that cannot
/// contain one.
///
/// Terminated with `BEL` (`\x07`) rather than the equally-legal `ESC \`:
/// both are accepted by every terminal that implements OSC 52 at all, and
/// `BEL` survives more intermediaries (notably `screen`) intact.
///
/// Returned as a `String` rather than written anywhere, so the sequence itself
/// is a pure function that can be asserted on byte for byte — the encoding is
/// the part that can be wrong, and it cannot be checked by looking at a
/// terminal.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// Standard base64, no line breaks and no dependency.
///
/// Hand-rolled because this is the only base64 in the codebase and a crate for
/// twenty lines is a poor trade — but it is exact: the standard alphabet, and
/// `=` padding, both of which a terminal's decoder requires.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let triple = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((triple >> (18 - 6 * i)) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Whether a copy reached anything, and by which route.
///
/// Not a `Result`: "the local `pbcopy` is missing but the terminal took it
/// over OSC 52" is a success, and so is the reverse, so neither mechanism's
/// failure is by itself an error worth failing a command over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Copied {
    /// The clipboard of the machine this process runs on.
    pub local: bool,
    /// The terminal emulator's clipboard, via OSC 52 — the machine the user is
    /// actually sitting at, which over SSH or Mosh is a different one.
    pub terminal: bool,
}

impl Copied {
    /// Whether the text reached a clipboard by any route at all.
    pub fn anywhere(self) -> bool {
        self.local || self.terminal
    }

    /// How to describe what happened, for a human, or `None` if nothing
    /// worked and there is nothing to claim.
    pub fn describe(self) -> Option<&'static str> {
        match (self.local, self.terminal) {
            (true, true) => Some("copied to the clipboard"),
            (false, true) => Some("copied to the terminal's clipboard"),
            (true, false) => Some("copied to this machine's clipboard"),
            (false, false) => None,
        }
    }
}

/// Copy `text` to both clipboards that might be the user's: this machine's,
/// and the terminal emulator's over OSC 52.
///
/// **Both, unconditionally, rather than detecting which is wanted.** There is
/// no reliable way to ask "is this session remote?" — `$SSH_TTY` and
/// `$SSH_CONNECTION` are absent under Mosh, which is one of the two ways this
/// machine is driven. Attempting both costs one process spawn and a few bytes
/// down the terminal, and between them they cover the local and remote cases
/// without having to tell them apart.
///
/// `escapes` is where the OSC 52 sequence is written. It must be the terminal
/// and never stdout when stdout is carrying a value a caller will parse: the
/// bytes are invisible on a terminal and corrupting anywhere else.
pub fn copy_everywhere(text: &str, escapes: &mut impl Write) -> Copied {
    let local = copy_to_clipboard(text).is_ok();
    let terminal = escapes
        .write_all(osc52(text).as_bytes())
        .and_then(|()| escapes.flush())
        .is_ok();
    Copied { local, terminal }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        // The RFC 4648 test vectors. A terminal's decoder is not ours, so
        // "it round-trips through our own decoder" would prove nothing --
        // these are the bytes the rest of the world agrees on.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encodes_a_token_shaped_value() {
        // A real token is a hex UUID; this is the shape that actually travels.
        let token = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
        assert_eq!(
            base64(token.as_bytes()),
            "M2YyNTA0ZTAtNGY4OS0xMWQzLTlhMGMtMDMwNWU4MmMzMzAx"
        );
    }

    #[test]
    fn osc52_wraps_the_payload_in_the_clipboard_selection_sequence() {
        assert_eq!(osc52("foo"), "\u{1b}]52;c;Zm9v\u{7}");
    }

    #[test]
    fn osc52_is_the_only_thing_written_and_carries_no_newline() {
        // A trailing newline would be printed by the terminal rather than
        // consumed by the sequence, putting a blank line in a command's
        // output for no reason.
        let sequence = osc52("anything");
        assert!(sequence.starts_with("\u{1b}]52;c;"));
        assert!(sequence.ends_with('\u{7}'));
        assert!(!sequence.contains('\n'));
    }

    #[test]
    fn copy_everywhere_reports_the_terminal_even_when_no_utility_exists() {
        // The SSH/Mosh case in miniature: the local clipboard command is
        // missing (or fails), and the copy still reached the user because the
        // terminal took it. That must read as a success, not a failure.
        let mut sink = Vec::new();
        temp_env_clipboard_cmd("/nonexistent/storyhook-clipboard-test", || {
            let copied = copy_everywhere("secret", &mut sink);
            assert!(!copied.local);
            assert!(copied.terminal);
            assert!(copied.anywhere());
            assert_eq!(
                copied.describe(),
                Some("copied to the terminal's clipboard")
            );
        });
        assert_eq!(String::from_utf8(sink).unwrap(), osc52("secret"));
    }

    #[test]
    fn describe_says_nothing_when_nothing_worked() {
        let nowhere = Copied {
            local: false,
            terminal: false,
        };
        assert_eq!(nowhere.describe(), None);
        assert!(!nowhere.anywhere());
    }

    /// Runs `body` with `$STORYHOOK_CLIPBOARD_CMD` set, then restores it.
    ///
    /// The process environment is shared by every test in this binary, so this
    /// serializes on a mutex rather than trusting that no other test reads the
    /// same variable at the same moment.
    fn temp_env_clipboard_cmd(value: &str, body: impl FnOnce()) {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = env::var("STORYHOOK_CLIPBOARD_CMD").ok();
        // SAFETY: guarded by LOCK above, and every other reader of this
        // variable in this binary is inside this same helper.
        unsafe { env::set_var("STORYHOOK_CLIPBOARD_CMD", value) };
        body();
        match previous {
            Some(v) => unsafe { env::set_var("STORYHOOK_CLIPBOARD_CMD", v) },
            None => unsafe { env::remove_var("STORYHOOK_CLIPBOARD_CMD") },
        }
    }
}
