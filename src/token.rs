//! `story token new|list|revoke` (SH-255) — the named, persistent, revocable
//! credential that authenticates the dashboard.
//!
//! Process management, not project data, the same shape as [`crate::web`]'s
//! own commands: a token record lives in the daemon's `tokens.json` sidecar
//! (`crate::env::Environment::daemon_state_dir`), not the store, so every
//! verb here speaks to the running daemon's control route — loopback, the
//! portfile's master token — rather than through `/api/v1/invoke`. See
//! [`crate::api::tokens`] for the registry these calls reach, and
//! [`crate::daemon::lifecycle::mint_named_token`] and its two siblings for
//! the wire calls themselves.

use crate::daemon::lifecycle::{self, DaemonInfo};
use crate::env::Environment;
use crate::error::AppError;

/// The environment these commands act in.
///
/// Re-resolved rather than threaded, and `None` for the flag on purpose —
/// the same reasoning as [`crate::web`]'s identical helper: `main` publishes
/// any `--store-path` into `$STORYHOOK_STORE_PATH` before anything
/// dispatches, so this reads the store the caller named.
fn environment() -> Result<Environment, AppError> {
    Environment::from_process(None)
}

/// The running daemon, if there is one that published where it is.
fn running_daemon(env: &Environment) -> Option<DaemonInfo> {
    lifecycle::is_live(env)
        .then(|| lifecycle::read_info(env))
        .flatten()
}

/// Help-like summary of the `token` command group, shown when a command needs
/// the daemon running but it isn't.
const TOKEN_COMMANDS_SUMMARY: &str = "\
token commands:
  story token new <name>       mint a fresh named token
  story token list              show every live token
  story token revoke <name>     end one token immediately
";

/// The error returned when a `token` command requires a running daemon but
/// there isn't one — a token record is the daemon's own state, so there is
/// nothing to mint, list or revoke without it.
fn daemon_not_running_error() -> AppError {
    AppError::Usage(format!(
        "storyhook daemon is not running — start it with: story daemon start\n\n{TOKEN_COMMANDS_SUMMARY}"
    ))
}

/// `story token new <name>` — mint a fresh token.
///
/// **Standard output is the raw secret, and only the secret** (SH-250's rule
/// for `story daemon token`, carried over here): a script can do
/// `TOKEN=$(story token new laptop)` and get exactly the value it asked for,
/// nothing appended. Everything else this call has to say — when the token
/// expires, how to end it sooner — goes to stderr. After this call returns,
/// nothing the daemon holds can reproduce the secret; a caller who loses it
/// has to mint a new one under a new name.
pub fn handle_new(name: &str) -> Result<String, AppError> {
    let env = environment()?;
    let info = running_daemon(&env).ok_or_else(daemon_not_running_error)?;
    let minted = lifecycle::mint_named_token(&info, name)?;
    eprintln!(
        "note: token \"{}\" expires {}. Save it now — it will not be shown again. \
         `story token revoke {}` ends it sooner.",
        minted.name, minted.expires_at, minted.name
    );
    Ok(minted.token)
}

/// `story token list` — every live token, oldest first. Never a secret: the
/// registry cannot produce one after mint time, so there is nothing here to
/// withhold beyond what it already lacks.
pub fn handle_list() -> Result<String, AppError> {
    let env = environment()?;
    let info = running_daemon(&env).ok_or_else(daemon_not_running_error)?;
    let tokens = lifecycle::list_named_tokens(&info)?;
    if tokens.is_empty() {
        return Ok("No named tokens exist. Mint one with `story token new <name>`.".to_string());
    }
    let mut lines = vec![format!(
        "{} named token{}:",
        tokens.len(),
        if tokens.len() == 1 { "" } else { "s" }
    )];
    for t in &tokens {
        lines.push(format!(
            "  {} ({}…)  created {}  expires {}",
            t.name, t.prefix, t.created_at, t.expires_at
        ));
    }
    Ok(lines.join("\n"))
}

/// `story token revoke <name>` — end one token immediately, whether or not
/// it has expired.
pub fn handle_revoke(name: &str) -> Result<String, AppError> {
    let env = environment()?;
    let info = running_daemon(&env).ok_or_else(daemon_not_running_error)?;
    lifecycle::revoke_named_token(&info, name)?;
    Ok(format!("Revoked token \"{name}\"."))
}
