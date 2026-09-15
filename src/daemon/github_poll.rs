//! Opt-in, project-isolated GitHub monitoring through gh.
use super::serve::SHUTDOWN_CHECK;
use crate::env::Environment;
use crate::github::api::GithubApiFactory;
use crate::service::github::RealGithubApiFactory;
use crate::service::{Ctx, github_repository, pr_check};
use crate::store::{ReadOps, Store};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

fn github_poll_interval() -> Duration {
    std::env::var("STORYHOOK_GITHUB_POLL_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(300))
}

/// Polls opted-in projects until the daemon shuts down.
pub(crate) fn poll_github<S: Store>(store: &S, env: &Environment, stop: &AtomicBool) {
    let interval = github_poll_interval();
    let mut waited = Duration::ZERO;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(SHUTDOWN_CHECK);
        waited += SHUTDOWN_CHECK;
        if waited >= interval {
            waited = Duration::ZERO;
            tick(store, env, &RealGithubApiFactory);
        }
    }
}

/// Checks each opted-in project without sharing destinations or hiding failures.
pub fn tick<S: Store>(store: &S, env: &Environment, factory: &dyn GithubApiFactory) {
    let projects = match store.read(|tx| tx.projects()) {
        Ok(projects) => projects,
        Err(error) => {
            eprintln!("GitHub polling could not read projects: {error}");
            return;
        }
    };
    for project in projects {
        let ctx = Ctx::new(store, project.id, env.home(), env.clone());
        let result = (|| {
            if store.read(|tx| tx.open_pr_links(project.id))?.is_empty() {
                return Ok(());
            }
            if github_repository::poll_enabled(&ctx)? {
                pr_check::run_check(&ctx, factory, None)?;
            }
            Ok::<_, crate::error::AppError>(())
        })();
        if let Err(error) = result {
            eprintln!(
                "GitHub polling for project {} failed: {error}",
                project.slug
            );
        }
    }
}
