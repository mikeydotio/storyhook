//! `story daemon token` over a real terminal (SH-250).
//!
//! # Why a PTY
//!
//! The command's whole contract is a split between two streams that depends on
//! whether a *human* is watching: stdout is always the bare token, and the
//! clipboard copy, the OSC 52 escape sequence and the rotation notice happen
//! only when stderr is a terminal. A piped harness — which is every other test
//! of this command, and `daemon_lifecycle.rs`'s own
//! `daemon_token_writes_nothing_but_the_token_when_it_is_piped` — can prove
//! the *suppressed* half and nothing else. `IsTerminal` answers false, so the
//! interactive branch has never run.
//!
//! `storyhook_test_support::Pty` puts a real `openpty` in the way, so the
//! branch executes. This file is the only place it does.
//!
//! # What is asserted, and what cannot be
//!
//! That the escape sequence is *emitted*, correctly encoded, and that the
//! platform clipboard command was invoked with the token on its stdin. What no
//! test can assert is that a terminal emulator on the far end of an SSH
//! connection honoured OSC 52 and updated a macOS pasteboard — that depends on
//! the emulator's own settings (`set-clipboard on` in tmux, and off by default
//! in several others). The encoding is the part that can be *wrong*, and
//! `src/clipboard.rs`'s unit tests pin it against the RFC 4648 vectors.
//!
//! # Why this file cannot wedge the gate
//!
//! Every `expect` carries its own deadline and fails with the transcript,
//! [`watchdog`] bounds the whole file, and each child carries
//! `daemon_containment()` — enforced by `Pty::spawn` — because killing a child
//! does not kill a daemon it started.

use std::path::Path;
use std::time::Duration;

use storyhook_test_support::{DaemonGuard, Pty, TestEnv, Watchdog, scratch_dir, watchdog};

/// The wall-clock bound on this whole file: two short conversations, each a
/// daemon start plus one command. Generous against the sporadic multi-second
/// stall `EXPECT_TIMEOUT` documents, not against the work itself.
fn guard() -> Watchdog {
    watchdog("daemon_token_clipboard", Duration::from_secs(90))
}

/// A `STORYHOOK_CLIPBOARD_CMD` that records what it was handed.
///
/// `tee` is the whole implementation: it copies stdin to the named file, which
/// is exactly what "the clipboard received this" means for a test. Standard on
/// every platform this suite runs on, so there is no shim script to keep
/// executable.
fn recording_clipboard(at: &Path) -> String {
    format!("tee {}", at.display())
}

/// Starts a daemon and returns its token.
fn start_daemon(env: &TestEnv, cwd: &Path) -> String {
    env.story(cwd).args(["daemon", "start"]).assert().success();
    env.daemon()
        .expect("a started daemon must publish a portfile")
        .token
}

/// The interactive contract, all of it, in one conversation: the token on
/// stdout, the OSC 52 sequence emitted, the local clipboard command fed, and
/// the notice printed.
#[test]
fn on_a_terminal_the_token_is_printed_copied_and_announced() {
    let _watchdog = guard();
    let env = TestEnv::isolated();
    // `dir` first: the guard stops the daemon from this directory when it
    // drops, so the directory has to outlive it. Passing `scratch_dir().path()`
    // inline would delete it at the end of that statement.
    let dir = scratch_dir();
    let _daemon = DaemonGuard::new(&env, dir.path());
    let clipboard_log = dir.path().join("clipboard.txt");
    let token = start_daemon(&env, dir.path());

    let mut command = env.raw_story(dir.path());
    command
        .env(
            "STORYHOOK_CLIPBOARD_CMD",
            recording_clipboard(&clipboard_log),
        )
        .args(["daemon", "token"]);
    let mut pty = Pty::spawn("daemon token on a tty", command);

    // The token itself, on stdout.
    pty.expect(&token);
    // The notice, on stderr -- same device under a PTY, which is the point:
    // a human sees both.
    pty.expect("note:");
    pty.expect("rotates");
    let status = pty.wait();
    assert!(status.success(), "transcript:\n{}", pty.transcript());

    let transcript = pty.transcript();

    // The OSC 52 sequence, byte for byte, with this daemon's actual token
    // base64-encoded inside it. Reconstructing the expected sequence from the
    // token is what makes this an assertion about the *encoding* rather than
    // about the presence of an escape character.
    let expected = storyhook::clipboard::osc52(&token);
    assert!(
        transcript.contains(&expected),
        "the OSC 52 sequence for this token was not emitted.\n\
         expected to find: {expected:?}\ntranscript: {transcript:?}"
    );

    // And the local clipboard command really ran, with the token on its stdin
    // -- the half OSC 52 does not cover, for a user sitting at this machine.
    let recorded = std::fs::read_to_string(&clipboard_log)
        .expect("the clipboard command must have been run and written its file");
    assert_eq!(
        recorded.trim(),
        token,
        "the clipboard command was handed something other than the token"
    );
}

/// A terminal with no working clipboard utility still prints the token and
/// still emits OSC 52 — which is precisely the SSH and Mosh case, where the
/// far-end terminal is the only clipboard that matters and a local `pbcopy`
/// would have reached the wrong machine anyway.
///
/// It must not fail the command: the token was delivered.
#[test]
fn a_terminal_with_no_clipboard_utility_still_reaches_the_terminals_clipboard() {
    let _watchdog = guard();
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let _daemon = DaemonGuard::new(&env, dir.path());
    let token = start_daemon(&env, dir.path());

    let mut command = env.raw_story(dir.path());
    command
        .env(
            "STORYHOOK_CLIPBOARD_CMD",
            "/nonexistent/storyhook-no-such-clipboard",
        )
        .args(["daemon", "token"]);
    let mut pty = Pty::spawn("daemon token with no clipboard utility", command);

    pty.expect(&token);
    pty.expect("terminal's clipboard");
    let status = pty.wait();
    assert!(
        status.success(),
        "a missing clipboard utility must not fail the command -- the token \
         still reached the terminal.\ntranscript:\n{}",
        pty.transcript()
    );

    assert!(
        pty.transcript()
            .contains(&storyhook::clipboard::osc52(&token)),
        "transcript:\n{}",
        pty.transcript()
    );
}
