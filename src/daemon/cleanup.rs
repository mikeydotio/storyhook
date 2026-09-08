//! Scheduled project-workspace cleanup (SH-594).
//!
//! The scheduler only decides when a project is due. `CleanupService` owns
//! candidate discovery and every destructive invariant, so the CLI and daemon
//! cannot disagree about what is safe to remove.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

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

/// Polls until daemon shutdown, running overdue projects immediately.
pub(crate) fn poll_cleanup<S: Store>(store: &S, env: &Environment, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        tick(store, env);
        let interval = poll_interval();
        let mut waited = Duration::ZERO;
        while waited < interval && !stop.load(Ordering::Relaxed) {
            let slice = super::serve::SHUTDOWN_CHECK.min(interval - waited);
            thread::sleep(slice);
            waited += slice;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{NewProject, ProjectSettings, SqliteStore, WriteOps};

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
