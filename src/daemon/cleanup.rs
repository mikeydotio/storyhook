//! Scheduled project-workspace cleanup (SH-594).
//!
//! The scheduler only decides when a project is due. `CleanupService` owns
//! candidate discovery and every destructive invariant, so the CLI and daemon
//! cannot disagree about what is safe to remove.

use super::bus::{Change, ChangeBus};
use crate::error::AppError;
use crate::service::cleanup::requests;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::domain::parse_duration;
use crate::env::Environment;
use crate::service::{CleanupService, Ctx};
use crate::store::{ReadOps, Store};

/// Default automatic-cleanup cadence: one day.
pub const DEFAULT_INTERVAL_SECS: u64 = 24 * 60 * 60;

fn poll_interval() -> Duration {
    std::env::var("STORYHOOK_CLEANUP_POLL_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(60))
}

fn configured_interval(raw: Option<&str>) -> Duration {
    raw.and_then(parse_duration)
        .and_then(|value| value.to_std().ok())
        .unwrap_or(Duration::from_secs(DEFAULT_INTERVAL_SECS))
}

fn due(stamp: &std::path::Path, interval: Duration) -> bool {
    std::fs::metadata(stamp)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_none_or(|age| age >= interval)
}

fn stamp(env: &Environment, slug: &str) -> std::path::PathBuf {
    env.daemon_state_dir().join("cleanup").join(slug)
}

fn record_attempt(env: &Environment, slug: &str) {
    let path = stamp(env, slug);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, env.now());
}

/// Runs every due project's cleanup once.
pub fn tick<S: Store>(store: &S, env: &Environment) {
    let Ok(projects) = store.read(|tx| tx.projects()) else {
        return;
    };
    for project in projects {
        let Ok((settings, checkout)) =
            store.read(|tx| Ok((tx.settings(project.id)?, tx.checkout_path(project.id)?)))
        else {
            continue;
        };
        if settings.cleanup_auto == Some(false) {
            continue;
        }
        let interval = configured_interval(settings.cleanup_interval.as_deref());
        if !due(&stamp(env, &project.slug), interval) {
            continue;
        }
        let Some(checkout) = checkout else {
            record_attempt(env, &project.slug);
            continue;
        };
        let ctx = Ctx::new(store, project.id, checkout, env.clone()).no_hooks(true);
        let context = format!("project={}", project.slug);
        match CleanupService::new(&ctx).run(false) {
            Ok(report) => super::activity::emit(
                if report.failed.is_empty() {
                    "INFO"
                } else {
                    "ERROR"
                },
                "cleanup",
                "event",
                &context,
                &format!(
                    "removed={} skipped={} failed={} reclaimed_bytes={}",
                    report.removed.len(),
                    report.skipped.len(),
                    report.failed.len(),
                    report.reclaimed_bytes
                ),
            ),
            Err(error) => {
                super::activity::emit("ERROR", "cleanup", "event", &context, &error.to_string())
            }
        }
        // A failed attempt waits for the next configured cadence rather than
        // retrying destructive network work every scheduler tick.
        record_attempt(env, &project.slug);
    }
}

/// Attempts each due closed lifecycle, even when periodic cleanup is disabled.
pub fn tick_closures<S: Store>(store: &S, env: &Environment) -> Result<(), AppError> {
    let projects = store.read(|tx| tx.projects())?;
    let mut failures = Vec::new();
    for project in projects {
        let pending = store.read(|tx| tx.closure_cleanups(project.id))?;
        let pending: Vec<_> = pending
            .into_iter()
            .filter(|r| requests::due(r, &env.now()))
            .collect();
        if pending.is_empty() {
            continue;
        }
        let checkout = store.read(|tx| tx.checkout_path(project.id))?;
        let ctx = Ctx::new(
            store,
            project.id,
            checkout.unwrap_or_else(|| env.home().to_path_buf()),
            env.clone(),
        )
        .no_hooks(true);
        match CleanupService::new(&ctx).run_pending() {
            Ok(report) => {
                for issue in &report.skipped {
                    super::activity::emit(
                        "WARN",
                        "cleanup",
                        "event",
                        &format!("project={} story={}", project.slug, issue.story_id),
                        &format!("{}: {}", issue.reason, issue.detail),
                    );
                }
                for failure in &report.failed {
                    super::activity::emit(
                        "ERROR",
                        "cleanup",
                        "event",
                        &format!("project={} story={}", project.slug, failure.story_id),
                        &format!("{}: {}", failure.reason, failure.detail),
                    );
                }
            }
            Err(error) => {
                for request in pending {
                    let issue = crate::service::CleanupSkip {
                        story_id: request.story.to_id(&project.prefix),
                        reason: "cleanup-unavailable".into(),
                        detail: error.to_string(),
                    };
                    if let Err(persist) = requests::finish(&ctx, &request, Some(&issue)) {
                        failures.push(format!(
                            "{}: {error}; recording cleanup failure: {persist}",
                            issue.story_id
                        ));
                    }
                }
                failures.push(format!("{}: {error}", project.slug));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(AppError::Storage(failures.join("; ")))
    }
}

/// Wakes on committed project changes; durable requests recover lost notifications.
pub(crate) fn poll_cleanup<S: Store>(
    store: &S,
    env: &Environment,
    bus: &ChangeBus,
    stop: &AtomicBool,
) {
    let subscription = bus.subscribe();
    while !stop.load(Ordering::Relaxed) {
        if let Err(error) = tick_closures(store, env) {
            super::activity::emit(
                "ERROR",
                "cleanup",
                "event",
                "closure requests",
                &error.to_string(),
            );
        }
        tick(store, env);
        let deadline = Instant::now()
            + poll_interval().min(Duration::from_secs(requests::RETRY_SECONDS as u64));
        while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            let wait = super::serve::SHUTDOWN_CHECK
                .min(deadline.saturating_duration_since(Instant::now()));
            if matches!(
                subscription.recv(wait),
                Some(Change::Project(_) | Change::Catalog | Change::Resync | Change::Reload)
            ) {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{NewProject, ProjectSettings, SqliteStore, WriteOps};

    #[test]
    fn startup_and_change_feed_drain_committed_closures() {
        use crate::service::{NewStoryInput, StoryService};
        use crate::store::StoryNo;
        use storyhook_test_support::load_grace::{Patience, wait_for};
        let fixture = storyhook_test_support::ServiceFixture::new();
        let store = SqliteStore::open(fixture.env().store_path()).unwrap();
        let env = Environment::at(fixture.env().home());
        let project = store
            .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
            .unwrap();
        store
            .write(|tx| {
                tx.set_checkout_path(project, None)?;
                tx.put_settings(
                    project,
                    &ProjectSettings {
                        cleanup_auto: Some(false),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        let ctx = Ctx::new(&store, project, fixture.cwd(), env.clone()).no_hooks(true);
        let stories = StoryService::new(&ctx);
        let close = || {
            let story = stories
                .create(&NewStoryInput {
                    title: "Lifecycle wake".into(),
                    ..Default::default()
                })
                .unwrap();
            stories
                .set_state(&story.id, "done", None, None, None)
                .unwrap();
            StoryNo::parse_id("SH", &story.id).unwrap()
        };
        let first = close();
        let bus = ChangeBus::new();
        let stop = AtomicBool::new(false);
        struct StopOnDrop<'a>(&'a AtomicBool, &'a ChangeBus);
        impl Drop for StopOnDrop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
                self.1.publish(Change::Reload);
            }
        }
        std::thread::scope(|scope| {
            scope.spawn(|| poll_cleanup(&store, &env, &bus, &stop));
            let _stop = StopOnDrop(&stop, &bus);
            let completed = |story| {
                wait_for(
                    Patience::new(Duration::from_secs(requests::RETRY_SECONDS as u64)),
                    super::super::serve::SHUTDOWN_CHECK,
                    || format!("closure worker did not finish {story:?}"),
                    || {
                        store
                            .read(|tx| tx.closure_cleanup(project, story))
                            .unwrap()
                            .filter(|r| r.completed)
                    },
                )
            };
            completed(first);
            for change in [Change::Project("fixture".into()), Change::Resync] {
                let next = close();
                bus.publish(change);
                completed(next);
            }
        });
    }

    #[test]
    fn default_and_configured_intervals_are_exact() {
        assert_eq!(
            configured_interval(None),
            Duration::from_secs(DEFAULT_INTERVAL_SECS)
        );
        assert_eq!(
            configured_interval(Some("2h")),
            Duration::from_secs(2 * 60 * 60)
        );
    }

    #[test]
    fn a_recorded_attempt_survives_a_scheduler_restart() {
        let root = tempfile::tempdir_in("/private/tmp").unwrap();
        let env = Environment::at(root.path());
        let path = stamp(&env, "fixture");
        assert!(due(&path, Duration::from_secs(60)));

        record_attempt(&env, "fixture");

        let restarted = Environment::at(root.path());
        assert!(!due(&stamp(&restarted, "fixture"), Duration::from_secs(60)));
    }

    #[test]
    fn automatic_cleanup_is_enabled_by_default_and_can_be_disabled() {
        let root = tempfile::tempdir_in("/private/tmp").unwrap();
        let env = Environment::at(root.path());
        let store = SqliteStore::open(env.store_path()).unwrap();
        store.migrate().unwrap();
        store
            .write(|tx| {
                let enabled = tx.create_project(&NewProject {
                    uuid: "enabled".into(),
                    slug: "enabled".into(),
                    name: "Enabled".into(),
                    prefix: "EN".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                })?;
                tx.set_checkout_path(enabled, Some(root.path().join("missing-enabled").as_path()))?;
                let enabled_two = tx.create_project(&NewProject {
                    uuid: "enabled-two".into(),
                    slug: "enabled-two".into(),
                    name: "Enabled two".into(),
                    prefix: "ET".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                })?;
                tx.set_checkout_path(
                    enabled_two,
                    Some(root.path().join("missing-enabled-two").as_path()),
                )?;
                let disabled = tx.create_project(&NewProject {
                    uuid: "disabled".into(),
                    slug: "disabled".into(),
                    name: "Disabled".into(),
                    prefix: "DIS".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                })?;
                tx.set_checkout_path(
                    disabled,
                    Some(root.path().join("missing-disabled").as_path()),
                )?;
                tx.put_settings(
                    disabled,
                    &ProjectSettings {
                        cleanup_auto: Some(false),
                        ..ProjectSettings::default()
                    },
                )?;
                Ok(())
            })
            .unwrap();

        tick(&store, &env);

        assert!(stamp(&env, "enabled").exists());
        assert!(
            stamp(&env, "enabled-two").exists(),
            "one project's failure must not stop the next project"
        );
        assert!(!stamp(&env, "disabled").exists());
    }
}
