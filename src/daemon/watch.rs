//! Attributes a store-wide change to specific projects, for anything that
//! notices "the store moved" but does not already know what moved.
//!
//! [`ChangeWatcher`] is the one place that attribution lives. Two callers in
//! `daemon::serve` share one watcher against one baseline (SH-202), so a
//! commit both of them notice publishes exactly once rather than twice:
//!
//! * the request boundary, immediately after `rpc::route`'s
//!   `POST /api/v1/invoke` arm answers — the only way an ordinary `story`
//!   command reaches the store, and the one route that carries no signal of
//!   its own for what it changed the way `rest::route`'s `Changed` does.
//!   `rest::route`'s own arm deliberately does **not** also call this: every
//!   mutating REST route already has exhaustive `Changed` coverage, and
//!   calling `notice()` there too was found, empirically, to risk attributing
//!   an unrelated out-of-band commit to the wrong response and colliding with
//!   [`ChangeBus`]'s coalescing window (SH-202's own council record; the
//!   surviving hazard — the coalescing window is blind to *cause*, not just
//!   to this call site — is filed as SH-216).
//! * `poll_change_token`'s low-frequency `PRAGMA data_version` poll, the
//!   safety net for a write this daemon did not itself serve — a `story tui`
//!   session on the same store, a second machine, a `sqlite3` prompt.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::daemon::bus::{Change, ChangeBus};
use crate::store::{ReadOps, Store};

/// What the store looked like the last time [`ChangeWatcher::notice`] looked.
struct Baseline {
    /// The store's `PRAGMA data_version` the last time it was read, or `None`
    /// if that read has never once succeeded.
    token: Option<u64>,
    /// Every project's slug and the highest `global_seq` its event log held.
    seqs: BTreeMap<String, i64>,
}

/// Diffs the store against its own last-seen state and publishes what moved.
///
/// Cloning shares the same baseline — a clone answers "what changed since the
/// *last* call from any clone of this watcher," never its own private
/// history — which is what lets two independent callers share one baseline
/// without one of them re-publishing a commit the other already reported.
#[derive(Clone)]
pub struct ChangeWatcher {
    baseline: Arc<Mutex<Baseline>>,
}

impl ChangeWatcher {
    /// A watcher baselined against `store`'s state right now.
    ///
    /// Must be built before anything can write through this daemon — see
    /// `daemon::serve::serve`'s call, ahead of `thread::scope` — so that a
    /// write which landed before the daemon started is history, not news.
    /// Publishing for it would be wrong twice over: a write nobody here is
    /// watching for is not what "the instant it commits" promises, and a
    /// freshly connected client has just fetched that state anyway.
    /// [`crate::tui::event`]'s
    /// `a_write_that_landed_before_the_daemon_started_is_not_replayed` pins
    /// the client-side half of the same property.
    pub fn new<S: Store>(store: &S) -> Self {
        ChangeWatcher {
            baseline: Arc::new(Mutex::new(Baseline {
                token: store.change_token().ok(),
                seqs: project_sequences(store),
            })),
        }
    }

    /// Checks whether `store` has moved since the last call from any clone of
    /// this watcher and, if so, publishes what changed to `bus`.
    ///
    /// The token is the cheap gate — one pragma, whatever the store holds.
    /// Only when it has moved does this pay for the sharper question: which
    /// projects' histories grew? Those get a precise [`Change::Project`], and
    /// a project appearing or disappearing gets [`Change::Catalog`].
    ///
    /// **A change it cannot attribute becomes [`Change::Resync`].** Editing a
    /// state definition appends no story event, so nothing's sequence moves
    /// and the only honest answer is "something changed, refetch." Guessing
    /// at a project would be worse than the resync: a dashboard showing
    /// something untrue is the failure this whole feed exists to prevent.
    pub fn notice<S: Store>(&self, store: &S, bus: &ChangeBus) {
        let Ok(token) = store.change_token() else {
            return;
        };
        let mut baseline = self.baseline.lock().unwrap_or_else(PoisonError::into_inner);
        if baseline.token == Some(token) {
            return;
        }
        baseline.token = Some(token);

        let fresh = project_sequences(store);
        // A change nobody is listening for is still a change: the baseline
        // has to move, or the first client to connect — which has just
        // fetched everything — would be told to refetch it again.
        if bus.subscriber_count() == 0 {
            baseline.seqs = fresh;
            return;
        }

        let mut attributed = false;
        for (slug, seq) in &fresh {
            if baseline.seqs.get(slug) != Some(seq) {
                bus.publish(Change::Project(slug.clone()));
                attributed = true;
            }
        }
        if fresh.len() != baseline.seqs.len() {
            bus.publish(Change::Catalog);
            attributed = true;
        }
        if !attributed {
            bus.publish(Change::Resync);
        }
        baseline.seqs = fresh;
    }
}

/// Every project's slug and the highest `global_seq` its event log holds.
///
/// An unreadable store yields an empty map rather than an error: both of
/// [`ChangeWatcher`]'s callers are background-safety-net shaped even when one
/// of them runs at the request boundary — a transient read failure here must
/// not fail the request that is *answering*, only skip its notification.
fn project_sequences<S: Store>(store: &S) -> BTreeMap<String, i64> {
    store
        .read(|tx| {
            let mut seqs: BTreeMap<String, i64> = BTreeMap::new();
            for project in tx.projects()? {
                seqs.insert(project.slug, tx.max_global_seq(project.id)?.get());
            }
            Ok(seqs)
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::StoryEvent;
    use crate::store::{ExpectedSeq, NewProject, ProjectId, SqliteStore, StoryNo, WriteOps};
    use std::time::Duration;

    fn store() -> (tempfile::TempDir, SqliteStore) {
        let dir = tempfile::Builder::new()
            .prefix("storyhook-watch-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory");
        let store = SqliteStore::open(dir.path().join("store.db")).expect("opening a store");
        store.migrate().expect("migrating");
        (dir, store)
    }

    fn a_project(store: &SqliteStore, slug: &str) -> ProjectId {
        store
            .write(|tx| {
                tx.create_project(&NewProject {
                    uuid: format!("{slug}-uuid"),
                    slug: slug.to_string(),
                    name: slug.to_string(),
                    prefix: "SH".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                })
            })
            .expect("creating a project")
    }

    /// The core promise: a project whose event log grew is published by
    /// slug, not swallowed into an unattributed resync.
    #[test]
    fn a_projects_growth_is_published_by_slug() {
        let (_dir, store) = store();
        let project = a_project(&store, "existing");
        let watcher = ChangeWatcher::new(&store);
        let bus = ChangeBus::new();
        let subscriber = bus.subscribe();

        store
            .write(|tx| {
                tx.append_events(
                    project,
                    StoryNo::new(1),
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryCreated {
                        at: "2026-01-01T00:00:00Z".into(),
                        title: "watched".into(),
                        state: "backlog".into(),
                    }],
                    &crate::domain::provenance::Provenance::unrecorded(),
                )
            })
            .expect("appending an event");

        watcher.notice(&store, &bus);
        assert_eq!(
            subscriber.recv(Duration::from_millis(200)),
            Some(Change::Project("existing".into()))
        );
    }

    /// A brand new project is not in the baseline at all, so the diff loop
    /// reports it by slug the same as any other growth — and the count
    /// mismatch *also* publishes `Catalog`, since the set of projects moved
    /// too. Both, in that order: this is `poll_change_token`'s own diff,
    /// carried over unchanged.
    #[test]
    fn a_new_project_is_published_by_slug_and_as_catalog() {
        let (_dir, store) = store();
        let watcher = ChangeWatcher::new(&store);
        let bus = ChangeBus::new();
        let subscriber = bus.subscribe();

        a_project(&store, "fresh");

        watcher.notice(&store, &bus);
        assert_eq!(
            subscriber.recv(Duration::from_millis(200)),
            Some(Change::Project("fresh".into()))
        );
        assert_eq!(
            subscriber.recv(Duration::from_millis(200)),
            Some(Change::Catalog)
        );
    }

    /// A change the diff cannot attribute to any project or to the catalog —
    /// a configuration edit, which appends no event — still has to be
    /// reported, honestly, as "something changed."
    #[test]
    fn an_unattributable_change_is_a_resync() {
        let (dir, store) = store();
        let project = a_project(&store, "existing");
        let watcher = ChangeWatcher::new(&store);
        let bus = ChangeBus::new();
        let subscriber = bus.subscribe();

        // A configuration write: it moves `PRAGMA data_version` without
        // touching anything `project_sequences` can see move. Through
        // `ConfigService::add_type` rather than `WriteOps::put_types`
        // directly — `tests/type_slug_call_sites.rs` allowlists every raw
        // writer of story types, and a test fixture manufacturing a change
        // is not one of the allowed reasons a real service gets to be.
        let env = crate::env::Environment::at(dir.path());
        let ctx = crate::service::Ctx::new(&store, project, dir.path(), env);
        crate::service::config::ConfigService::new(&ctx)
            .add_type("spike", None, None)
            .expect("a configuration write");

        watcher.notice(&store, &bus);
        assert_eq!(
            subscriber.recv(Duration::from_millis(200)),
            Some(Change::Resync)
        );
    }

    /// A store that has not moved at all publishes nothing.
    #[test]
    fn an_unmoved_store_publishes_nothing() {
        let (_dir, store) = store();
        let watcher = ChangeWatcher::new(&store);
        let bus = ChangeBus::new();
        let subscriber = bus.subscribe();

        watcher.notice(&store, &bus);
        assert_eq!(subscriber.recv(Duration::from_millis(50)), None);
    }

    /// A change with no subscribers still advances the baseline, so the
    /// first client to connect afterward — which has just fetched everything
    /// — is not immediately told to refetch it again.
    #[test]
    fn a_change_with_no_subscribers_still_advances_the_baseline() {
        let (_dir, store) = store();
        let watcher = ChangeWatcher::new(&store);
        let bus = ChangeBus::new();

        a_project(&store, "unwatched");
        watcher.notice(&store, &bus); // nobody subscribed yet

        let subscriber = bus.subscribe();
        watcher.notice(&store, &bus); // nothing new since the call above
        assert_eq!(
            subscriber.recv(Duration::from_millis(50)),
            None,
            "the baseline must already reflect the unwatched change, not \
             replay it to the first subscriber"
        );
    }

    /// The property the shared baseline exists for: two callers noticing the
    /// same commit — the request boundary and the safety-net poll racing
    /// each other — publish it once, not twice.
    #[test]
    fn two_notices_of_one_commit_publish_once() {
        let (_dir, store) = store();
        let project = a_project(&store, "existing");
        let watcher = ChangeWatcher::new(&store);
        let bus = ChangeBus::new();
        let subscriber = bus.subscribe();

        store
            .write(|tx| {
                tx.append_events(
                    project,
                    StoryNo::new(1),
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryCreated {
                        at: "2026-01-01T00:00:00Z".into(),
                        title: "raced".into(),
                        state: "backlog".into(),
                    }],
                    &crate::domain::provenance::Provenance::unrecorded(),
                )
            })
            .expect("appending an event");

        watcher.notice(&store, &bus);
        watcher.notice(&store, &bus);

        assert_eq!(
            subscriber.recv(Duration::from_millis(200)),
            Some(Change::Project("existing".into()))
        );
        assert_eq!(
            subscriber.recv(Duration::from_millis(50)),
            None,
            "the second notice of the same commit must not publish again"
        );
    }
}
