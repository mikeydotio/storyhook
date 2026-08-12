//! The durable GitHub credential `story github-auth login` writes and the
//! daemon's background poll thread reads (SH-212) — the OS keychain, never a
//! file or a database column.
//!
//! # This is not the ambient-environment credential SH-153 forbade
//!
//! SH-153 ruled that the daemon must never read a GitHub token from its own
//! process environment, because that environment belongs to whichever
//! process happened to start the daemon, not to whoever is asking. A
//! keychain entry is a different thing: it is written by an explicit,
//! consented `story github-auth login` naming exactly what it grants, and it
//! is revoked by an equally explicit `story github-auth logout` — not
//! inherited from whichever shell launched the daemon.
//!
//! # Explicit stores, not the crate's global default
//!
//! `keyring_core` offers a process-global default store
//! (`set_default_store`/`get_default_store`) that every [`Entry`] falls back
//! to. This module never touches it. Every function here takes the
//! [`Arc<CredentialStore>`](CredentialStore) it should use explicitly — the
//! same shape [`crate::github::api::GithubApiFactory`] uses for the GitHub
//! client itself. A global would make every test that touches a credential
//! share one process-wide static across threads `cargo test` runs
//! concurrently; an explicit parameter lets production hold the real
//! platform store and a test hold [`keyring_core::mock::Store`] side by
//! side.
//!
//! # One entry per store, keyed by the store's own identity
//!
//! The account name is [`StoreLocation::key`](crate::env::StoreLocation::key)
//! — the same digest that already names a store's daemon-state directory —
//! not a project. SH-113's store isolation is one daemon per *store*, and a
//! store can hold many projects; keying by project would need a directory of
//! entries with no natural cleanup path when a project is deleted, for a
//! credential [`super::pr_check::run_check`] already re-validates per
//! project on every call.

use std::sync::Arc;

use keyring_core::{CredentialStore, Error as KeyringError};

use crate::domain::secret::GithubToken;
use crate::error::AppError;

/// The keychain service name every entry this module writes is filed under.
///
/// A fixed constant, not derived from anything: the account
/// ([`StoreLocation::key`](crate::env::StoreLocation::key)) is what makes
/// entries for two stores distinct, so the service name only needs to keep
/// storyhook's entries apart from every other application's in the same
/// keychain.
pub const SERVICE: &str = "storyhook-github";

/// Builds this platform's native credential store.
///
/// One call, ordinarily at process startup — [`login`], [`read`] and
/// [`logout`] all take the result rather than rebuilding it, so a daemon that
/// polls many times reuses one keychain connection instead of opening one
/// per tick.
///
/// # Errors
///
/// [`AppError::GithubAuth`] if this platform has no supported backend, or if
/// the backend exists but could not be reached — most commonly a headless
/// Linux process with no Secret Service (`gnome-keyring`, KWallet) running.
/// Callers differ deliberately in how they treat that: `story github-auth
/// login` refuses hard, because a human is at the terminal to see why; the
/// daemon's poll thread degrades to a logged no-op tick, because nobody is.
#[cfg(target_os = "macos")]
pub fn default_credential_store() -> Result<Arc<CredentialStore>, AppError> {
    apple_native_keyring_store::keychain::Store::new()
        .map(|store| store as Arc<CredentialStore>)
        .map_err(|e| AppError::GithubAuth(format!("could not open the macOS keychain: {e}")))
}

/// See [the macOS version](default_credential_store) above.
#[cfg(target_os = "windows")]
pub fn default_credential_store() -> Result<Arc<CredentialStore>, AppError> {
    windows_native_keyring_store::Store::new()
        .map(|store| store as Arc<CredentialStore>)
        .map_err(|e| {
            AppError::GithubAuth(format!(
                "could not open the Windows Credential Manager: {e}"
            ))
        })
}

/// See [the macOS version](default_credential_store) above. Secret Service
/// over D-Bus (`zbus`, pure Rust — no system `libdbus` needed), which is what
/// GNOME Keyring and KWallet both implement.
#[cfg(target_os = "linux")]
pub fn default_credential_store() -> Result<Arc<CredentialStore>, AppError> {
    zbus_secret_service_keyring_store::Store::new()
        .map(|store| store as Arc<CredentialStore>)
        .map_err(|e| {
            AppError::GithubAuth(format!(
                "could not reach the Secret Service over D-Bus: {e}\n\n\
                 This usually means no keyring daemon (gnome-keyring, KWallet) is running -- \
                 common on a headless machine, or a daemon started before login finishes. \
                 `story github-auth` needs one; `story pr-check` run by hand still works with \
                 STORYHOOK_GITHUB_TOKEN set."
            ))
        })
}

/// See [the macOS version](default_credential_store) above. No backend is
/// bundled for a platform outside the three storyhook ships prebuilt
/// binaries for.
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub fn default_credential_store() -> Result<Arc<CredentialStore>, AppError> {
    Err(AppError::GithubAuth(
        "story github-auth has no keychain backend on this platform".to_string(),
    ))
}

/// Stores `token` in `store` under `account`, replacing whatever was there.
///
/// # Errors
///
/// [`AppError::GithubAuth`] if the underlying store refuses the write.
pub fn login(
    store: &Arc<CredentialStore>,
    account: &str,
    token: &GithubToken,
) -> Result<(), AppError> {
    entry(store, account)?
        .set_password(token.expose())
        .map_err(keyring_error)
}

/// The token stored under `account`, or `None` if nothing is stored.
///
/// Called fresh on every daemon poll tick rather than cached — see the
/// module doc: that is what makes [`logout`] take effect on the very next
/// tick with no daemon restart.
///
/// # Errors
///
/// [`AppError::GithubAuth`] if the underlying store fails for a reason other
/// than "nothing is stored" — a locked keychain, a lost D-Bus connection.
pub fn read(store: &Arc<CredentialStore>, account: &str) -> Result<Option<GithubToken>, AppError> {
    match entry(store, account)?.get_password() {
        Ok(password) => GithubToken::new(password).map(Some),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(e) => Err(keyring_error(e)),
    }
}

/// Deletes whatever is stored under `account`.
///
/// Idempotent: deleting an already-absent credential succeeds rather than
/// erroring, because `logout` must be safe to run twice — a user unsure
/// whether `login` ever succeeded should be able to run it to be sure,
/// without an error implying something went wrong.
///
/// # Errors
///
/// [`AppError::GithubAuth`] if the underlying store fails for a reason other
/// than "nothing is stored".
pub fn logout(store: &Arc<CredentialStore>, account: &str) -> Result<(), AppError> {
    match entry(store, account)?.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(e) => Err(keyring_error(e)),
    }
}

fn entry(store: &Arc<CredentialStore>, account: &str) -> Result<keyring_core::Entry, AppError> {
    store.build(SERVICE, account, None).map_err(keyring_error)
}

fn keyring_error(e: KeyringError) -> AppError {
    AppError::GithubAuth(format!("keychain error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock store, isolated per test so concurrent tests never share state
    /// — unlike `keyring_core::set_default_store`, which would make every
    /// test in this binary race over one process-wide static.
    fn mock_store() -> Arc<CredentialStore> {
        keyring_core::mock::Store::new().expect("building a mock store never fails")
    }

    #[test]
    fn a_token_round_trips_through_login_and_read() {
        let store = mock_store();
        let token = GithubToken::new("ghp_test_token").expect("a usable token");
        login(&store, "account-a", &token).expect("login writes the entry");
        let read_back = read(&store, "account-a")
            .expect("reading does not error")
            .expect("the entry exists");
        assert_eq!(read_back, token);
    }

    #[test]
    fn reading_an_absent_account_is_none_not_an_error() {
        let store = mock_store();
        assert!(
            read(&store, "never-logged-in")
                .expect("absence is not an error")
                .is_none()
        );
    }

    #[test]
    fn logout_removes_what_login_wrote() {
        let store = mock_store();
        let token = GithubToken::new("ghp_test_token").expect("a usable token");
        login(&store, "account-b", &token).expect("login writes the entry");
        logout(&store, "account-b").expect("logout deletes it");
        assert!(
            read(&store, "account-b")
                .expect("reading does not error")
                .is_none(),
            "the entry must be gone after logout"
        );
    }

    #[test]
    fn logout_is_idempotent() {
        let store = mock_store();
        logout(&store, "never-logged-in").expect("logging out nothing is not an error");
        logout(&store, "never-logged-in").expect("nor is doing it twice");
    }

    #[test]
    fn two_accounts_in_one_store_do_not_collide() {
        let store = mock_store();
        let token_a = GithubToken::new("ghp_a").expect("a usable token");
        let token_b = GithubToken::new("ghp_b").expect("a usable token");
        login(&store, "account-a", &token_a).expect("login a");
        login(&store, "account-b", &token_b).expect("login b");
        assert_eq!(read(&store, "account-a").unwrap().unwrap(), token_a);
        assert_eq!(read(&store, "account-b").unwrap().unwrap(), token_b);
        logout(&store, "account-a").expect("logout a");
        assert!(read(&store, "account-a").unwrap().is_none());
        assert_eq!(
            read(&store, "account-b").unwrap().unwrap(),
            token_b,
            "logging out one account must not disturb the other"
        );
    }

    /// [`login`] overwrites, rather than erroring on, an existing entry — a
    /// re-run of `story github-auth login` after a PAT was rotated must
    /// replace the old one, not refuse.
    #[test]
    fn logging_in_again_overwrites_the_previous_token() {
        let store = mock_store();
        let old = GithubToken::new("ghp_old").expect("a usable token");
        let new = GithubToken::new("ghp_new").expect("a usable token");
        login(&store, "account-a", &old).expect("first login");
        login(&store, "account-a", &new).expect("second login overwrites");
        assert_eq!(read(&store, "account-a").unwrap().unwrap(), new);
    }
}
