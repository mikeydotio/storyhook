//! The credentials storyhook takes out of its own environment, so that no child
//! it starts can inherit one (SH-153).
//!
//! # Why taking rather than reading
//!
//! [`git_env::scrub_this_process`](super::git_env::scrub_this_process) is the
//! sibling of this module and settles the argument for the git case: a daemon
//! outlives the client that started it and holds that client's environment for
//! its whole life, so a variable exported in one shell decides what every later
//! command on the machine sees.
//!
//! A credential makes that worse in a way a `GIT_DIR` does not. `spawn_child`
//! builds its `Command` with no `env_clear`, so a GitHub token in the client's
//! shell is installed in the daemon's environment — and from there the daemon
//! hands it to **every child it starts**: a user's own event-hook script
//! (`event_hooks.rs`, `sh -c`), the dashboard's dispatch (`api/dispatch.rs`,
//! `bash`), and `claude` (`plugin.rs`). None of those asked for a credential and
//! none of them should have one.
//!
//! So this does not merely *read* the variable, it **removes** it in the same
//! call. From the first statement of `main` there is exactly one copy of the
//! credential in the process, it is a value rather than an environment entry,
//! and nothing this process spawns can inherit it.
//!
//! # The ordering that makes it work, and the objection it answers
//!
//! Read first, remove second, both inside one call. The obvious alternative —
//! a bare scrub next to the git one — was proposed and rejected during SH-153's
//! council for a good reason: `main` is *also* the process that legitimately
//! reads this variable, so a scrub placed before the read would leave the client
//! with nothing to send. Taking closes that gap by construction, because the
//! caller cannot get the ordering wrong.
//!
//! # What this list does **not** claim
//!
//! It covers **storyhook's own** credential variables — today, one. It says
//! nothing about `AWS_SECRET_ACCESS_KEY`, `OPENAI_API_KEY`, or anything else a
//! user happens to have exported, all of which the daemon still inherits and
//! still forwards to those same three kinds of child. Adding a second name here
//! would not make the daemon credential-clean, and anybody who reads this list
//! as a security boundary has read it wrong. SH-193 carries that general
//! problem.

use std::fmt;

use crate::domain::secret::{GITHUB_TOKEN_VAR, GithubToken};
use crate::error::AppError;

/// The environment variables that hold a credential storyhook itself defines.
///
/// A denylist rather than an allowlist, because — exactly as
/// [`git_env`](super::git_env)'s process layer argues — storyhook's children
/// are not all storyhook's, and clearing the environment of a user's own hook
/// script is not available to us.
const CREDENTIALS: [&str; 1] = [GITHUB_TOKEN_VAR];

/// What the client took out of its own environment.
///
/// Carries the raw value rather than a parsed [`GithubToken`] because the
/// removal has to happen at the top of `main`, before the global flags are
/// split — and a *malformed* credential has to be reported with the caller's
/// chosen rendering, which is not known yet. So this holds it, and
/// [`github_token`](Self::github_token) validates when there is somewhere to
/// report a failure.
pub struct Credentials {
    github_token: Option<String>,
}

impl Credentials {
    /// The caller's GitHub credential, validated.
    ///
    /// `Ok(None)` means the caller had none, which is not an error here: the
    /// refusal belongs where the work runs, so that the dashboard and a
    /// hand-built request meet it too.
    ///
    /// # Errors
    ///
    /// [`AppError::GithubAuth`] if the variable was set to a blank value.
    pub fn github_token(&self) -> Result<Option<GithubToken>, AppError> {
        self.github_token.as_ref().map(GithubToken::new).transpose()
    }
}

/// Deliberately prints nothing, for the reason [`GithubToken`]'s own `Debug`
/// does: this is a struct somebody will one day include in a diagnostic.
impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field(
                "github_token",
                &self.github_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// Removes storyhook's credential variables from this process and returns what
/// they held.
///
/// Call this **once, at the top of `main`**, before anything is parsed and long
/// before anything is spawned. After it returns, the credential exists only as
/// the value it handed back.
///
/// # Safety
///
/// `remove_var` is unsound only in the presence of concurrent readers. This runs
/// in `main` before storyhook has started a thread, opened a store or spawned
/// anything — the same window, and the same argument, as
/// [`git_env::scrub_this_process`](super::git_env::scrub_this_process) and
/// `main.rs::publish_store_path`.
#[must_use]
pub fn take_credentials() -> Credentials {
    let github_token = std::env::var(GITHUB_TOKEN_VAR).ok();
    for name in CREDENTIALS {
        // SAFETY: single-threaded, as the doc comment above establishes.
        unsafe { std::env::remove_var(name) };
    }
    Credentials { github_token }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list is what it says it is. Mirrors `git_env`'s own pin: a name
    /// added here is a deliberate edit, and a name removed is too.
    #[test]
    fn the_credential_list_is_exactly_what_it_says() {
        assert_eq!(CREDENTIALS, [GITHUB_TOKEN_VAR]);
        assert_eq!(
            CREDENTIALS.len(),
            1,
            "see the module docs before growing this"
        );
    }

    /// A blank value is refused when it is asked for, not silently dropped —
    /// the two mistakes are different and the messages say so.
    #[test]
    fn a_blank_credential_is_refused_when_it_is_read() {
        let blank = Credentials {
            github_token: Some("   ".to_string()),
        };
        let error = blank
            .github_token()
            .expect_err("a blank credential is unusable");
        assert_eq!(error.exit_code(), 6);

        let absent = Credentials { github_token: None };
        assert!(
            absent
                .github_token()
                .expect("absence is not an error")
                .is_none(),
            "having no credential is the daemon's refusal to raise, not this one's"
        );
    }

    /// And the holder does not print what it holds.
    #[test]
    fn credentials_do_not_print_themselves() {
        let held = Credentials {
            github_token: Some("ghp_must_not_appear".to_string()),
        };
        let printed = format!("{held:?}");
        assert!(!printed.contains("ghp_must_not_appear"), "{printed}");
        assert!(printed.contains("redacted"), "{printed}");
    }
}
