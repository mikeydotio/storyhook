//! The daemon's unattended GitHub poll (SH-212) — watching every project's
//! linked pull requests for a merge, with no client ever asking.
//!
//! # What this reuses, and why nothing here re-detects a merge
//!
//! [`pr_check::run_check`] is `story pr-check`'s whole engine: it reads a
//! project's registered GitHub remotes fresh, re-validates every linked PR's
//! `(owner, repo)` against that set (the cross-repo spoofing guard SH-49
//! built in), and applies `StoryPrMerged`/`StoryPrClosed`. This module calls
//! it unmodified, once per project per tick — the alternative, a second
//! merge-detection path, is exactly how that guard would end up silently
//! missing from one of the two.
//!
//! # The credential: read fresh, never cached
//!
//! [`credential_store::read`] is called on every tick rather than once at
//! startup. A cached token would keep this thread polling GitHub on the
//! user's behalf for the rest of the daemon's life after
//! `story github-auth logout` — the daemon would have to notice the logout
//! and stop, which nothing here does or should have to. Reading fresh is
//! what makes "logout takes effect on the next tick, no restart" simply
//! true, the same discipline [`pr_check::run_check`] already applies to a
//! project's remote configuration.
//!
//! The [`Arc<CredentialStore>`](CredentialStore) itself — the open
//! connection to the platform keychain — is a different question and is
//! cached, once built successfully: rebuilding it every tick would mean a
//! fresh D-Bus round trip every five minutes for no reason. If it was never
//! built (no backend on this platform, or a headless Linux box with no
//! Secret Service running at daemon startup), a build is retried on every
//! tick rather than given up on — the same "however late" self-heal
//! [`super::serve::tailnet_reprobe`] uses for a login-time-missed tailnet
//! interface. Once it succeeds, it is never rebuilt again.
//!
//! # Per-project isolation
//!
//! One project's error — no remote registered (the common case: most
//! projects never link-pr'd against GitHub), a revoked token, a rate limit,
//! a network blip — must never stop the tick for the rest. [`tick`] discards
//! each project's `Result` rather than propagating it, deliberately: there is
//! nobody to report a background failure to, and one bad project silently
//! starving every other project's close-on-merge would be worse than the
//! quiet skip.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use keyring_core::CredentialStore;

use super::serve::SHUTDOWN_CHECK;
use crate::env::Environment;
use crate::error::AppError;
use crate::github::api::GithubApiFactory;
use crate::github::credential_store;
use crate::service::github::RealGithubApiFactory;
use crate::service::{Ctx, pr_check};
use crate::store::{ReadOps, Store};

/// How often the poll thread wakes up to check the stored credential and
/// sweep every project's linked pull requests.
///
/// Overridable via `STORYHOOK_GITHUB_POLL_MS`, the same seam
/// [`super::serve::heartbeat_interval`] and
/// [`super::serve::change_poll_interval`] already use. Five minutes by
/// default: GitHub's REST rate limit (5,000 requests/hour for an
/// authenticated token) comfortably absorbs one request per open linked PR
/// every five minutes for any store this daemon plausibly serves, and a
/// merge is not time-critical enough to poll faster than that by default.
fn github_poll_interval() -> Duration {
    std::env::var("STORYHOOK_GITHUB_POLL_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(300))
}

/// Polls GitHub for merged pull requests on [`github_poll_interval`] until
/// `stop` is signalled. Spawned in [`super::serve::serve`]'s `thread::scope`
/// beside `heartbeat`, `poll_change_token` and `watch_parent`.
pub(crate) fn poll_github<S: Store>(store: &S, env: &Environment, stop: &AtomicBool) {
    poll_github_with(
        store,
        env,
        stop,
        credential_store::default_credential_store,
        &RealGithubApiFactory,
    );
}

/// [`poll_github`]'s engine, taking the credential-store builder and the
/// [`GithubApiFactory`] as parameters — the same seam
/// [`pr_check::run_check`] itself takes its factory through, so a test can
/// substitute [`keyring_core::mock::Store`] and
/// `storyhook_test_support::FakeGithubApiFactory` without a keychain or a
/// network. [`poll_github`] is the production wrapper, fixed to the real
/// platform keychain and [`RealGithubApiFactory`].
fn poll_github_with<S: Store>(
    store: &S,
    env: &Environment,
    stop: &AtomicBool,
    build_credential_store: impl Fn() -> Result<Arc<CredentialStore>, AppError>,
    api_factory: &dyn GithubApiFactory,
) {
    let interval = github_poll_interval();
    let mut waited = Duration::ZERO;
    let mut credential_store: Option<Arc<CredentialStore>> = None;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(SHUTDOWN_CHECK);
        waited += SHUTDOWN_CHECK;
        if waited < interval {
            continue;
        }
        waited = Duration::ZERO;

        let store_handle = match &credential_store {
            Some(existing) => Arc::clone(existing),
            None => match build_credential_store() {
                Ok(built) => {
                    credential_store = Some(Arc::clone(&built));
                    built
                }
                // No backend on this platform, or one that could not be
                // reached this time (e.g. no Secret Service session yet) —
                // documented degrade, not a crash. Retried next tick.
                Err(_) => continue,
            },
        };
        tick(store, env, &store_handle, api_factory);
    }
}

/// One poll: the stored credential, if any, spent against every project that
/// has one worth spending it on.
///
/// `pub` rather than `pub(crate)`, for the reason [`pr_check::run_check`]
/// itself is: `tests/daemon_github_poll.rs` needs it directly, and a
/// dev-dependency on `storyhook-test-support` (which itself depends on this
/// crate) makes an integration test the only place a fixture built from
/// [`crate::store::sqlite::SqliteStore`] and this function's own types agree
/// on being the same crate.
pub fn tick<S: Store>(
    store: &S,
    env: &Environment,
    credential_store: &Arc<CredentialStore>,
    api_factory: &dyn GithubApiFactory,
) {
    let account = env.store().key();
    let token = match credential_store::read(credential_store, &account) {
        Ok(Some(token)) => token,
        // Nobody has run `story github-auth login`, or the read failed —
        // either way, nothing to spend this tick. Not logged: an unconfigured
        // credential is this feature's default, ordinary state, not a fault.
        Ok(None) | Err(_) => return,
    };
    let Ok(projects) = store.read(|tx| tx.projects()) else {
        return;
    };
    for project in projects {
        let ctx = Ctx::new(store, project.id, env.home(), env.clone())
            .with_github_token(Some(token.clone()));
        // Discarded deliberately — see the module doc's "Per-project
        // isolation". `id: None` checks every open linked PR the project has,
        // the same scope a bare `story pr-check` uses.
        let _ = pr_check::run_check(&ctx, api_factory, None);
    }
}
