//! SH-642: engine controls are project changes; lane observations are UI refreshes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use storyhook::daemon::bus::{Change, ChangeBus, Subscription};
use storyhook::daemon::watch::ChangeWatcher;
use storyhook::service::engine::{ConfigureRequest, EngineService, StartRequest};
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{
    Access, EngineAgent, EngineRunRecord, MigrationReport, ReadOps, SqliteStore, Store, StoreError,
    WriteOps, WriteWithSnapshot,
};
use storyhook_test_support::{FakeDispatcher, ServiceFixture};

fn start(fixture: &ServiceFixture) -> EngineRunRecord {
    EngineService::new(&fixture.ctx(), &FakeDispatcher::default())
        .start(StartRequest {
            scope: storyhook::store::EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap()
}

fn notices(subscription: &Subscription) -> Vec<Change> {
    std::iter::from_fn(|| subscription.recv(Duration::ZERO)).collect()
}

fn project_notice(
    watcher: &ChangeWatcher,
    fixture: &ServiceFixture,
    bus: &ChangeBus,
    subscription: &Subscription,
) {
    watcher.notice(fixture.store(), bus);
    assert_eq!(
        notices(subscription),
        vec![Change::Project("fixture".into())]
    );
}

#[test]
fn controls_are_attributed_even_when_the_clock_does_not_advance() {
    let fixture = ServiceFixture::new();
    let watcher = ChangeWatcher::new(fixture.store());
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    let run = start(&fixture);
    project_notice(&watcher, &fixture, &bus, &subscription);

    let ctx = fixture.ctx();
    let dispatcher = FakeDispatcher::default();
    let service = EngineService::new(&ctx, &dispatcher);
    service.pause(&run.id).unwrap();
    project_notice(&watcher, &fixture, &bus, &subscription);
    service.resume(&run.id).unwrap();
    project_notice(&watcher, &fixture, &bus, &subscription);
    service
        .configure(
            &run.id,
            ConfigureRequest {
                lanes: 2,
                agent: EngineAgent::Codex,
                model: None,
                effort: None,
                speed: None,
            },
        )
        .unwrap();
    project_notice(&watcher, &fixture, &bus, &subscription);
    service.stop(&run.id, false).unwrap();
    project_notice(&watcher, &fixture, &bus, &subscription);

    // A finished run no longer needs reconciling, but its alert still updates.
    service.acknowledge(&run.id).unwrap();
    watcher.notice(fixture.store(), &bus);
    assert_eq!(notices(&subscription), vec![Change::Resync]);
}

#[test]
fn lane_observations_remain_resyncs_and_cloned_watchers_do_not_replay() {
    let fixture = ServiceFixture::new();
    let run = start(&fixture);
    let watcher = ChangeWatcher::new(fixture.store());
    let clone = watcher.clone();
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    let mut lane = fixture
        .store()
        .read(|tx| tx.engine_lanes(&run.id))
        .unwrap()
        .remove(0);
    lane.last_observed_at = "2026-01-01T00:00:01Z".into();
    lane.probe_detail = Some("probe temporarily unanswered".into());
    fixture
        .store()
        .write(|tx| tx.put_engine_lane(&lane))
        .unwrap();
    watcher.notice(fixture.store(), &bus);
    clone.notice(fixture.store(), &bus);
    assert_eq!(notices(&subscription), vec![Change::Resync]);
    assert_eq!(
        fixture.store().read(|tx| tx.engine_lanes(&run.id)).unwrap()[0],
        lane
    );
}

#[test]
fn mixed_story_and_run_changes_publish_each_project_once() {
    let fixture = ServiceFixture::new();
    let watcher = ChangeWatcher::new(fixture.store());
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    start(&fixture);
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "new work beside a new run".into(),
            ..NewStoryInput::default()
        })
        .unwrap();
    project_notice(&watcher, &fixture, &bus, &subscription);
    watcher.notice(fixture.store(), &bus);
    assert!(notices(&subscription).is_empty());
}

#[test]
fn replacing_a_run_without_changing_the_live_count_is_attributed() {
    let fixture = ServiceFixture::new();
    let first = start(&fixture);
    let watcher = ChangeWatcher::new(fixture.store());
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    EngineService::new(&fixture.ctx(), &FakeDispatcher::default())
        .stop(&first.id, false)
        .unwrap();
    let second = start(&fixture);
    assert_ne!(first.id, second.id);
    assert_eq!(first.updated_at, second.updated_at);
    project_notice(&watcher, &fixture, &bus, &subscription);
}

#[test]
fn deleting_a_project_with_a_live_run_reports_removal_and_catalog() {
    let fixture = ServiceFixture::new();
    start(&fixture);
    // Deletion belongs in a standalone store: ServiceFixture's drop check
    // requires its project to exist so it can compare the event/read models.
    let copy_dir = storyhook_test_support::scratch_dir();
    let copy = fixture
        .store()
        .snapshot(copy_dir.path(), "deletion")
        .unwrap();
    let store = SqliteStore::open(copy).unwrap();
    let watcher = ChangeWatcher::new(&store);
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    store
        .write(|tx| tx.delete_project(fixture.project()))
        .unwrap();
    watcher.notice(&store, &bus);
    assert_eq!(
        notices(&subscription),
        vec![Change::Project("fixture".into()), Change::Catalog]
    );
}

#[test]
fn unwatched_run_changes_advance_the_shared_baseline() {
    let fixture = ServiceFixture::new();
    let watcher = ChangeWatcher::new(fixture.store());
    let bus = ChangeBus::new();
    start(&fixture);
    watcher.notice(fixture.store(), &bus);
    let subscription = bus.subscribe();
    watcher.notice(fixture.store(), &bus);
    assert!(notices(&subscription).is_empty());
}

/// Injects only read-transaction failure; successful reads and all writes use SQLite.
struct ReadFaultStore {
    inner: SqliteStore,
    fail: AtomicBool,
}

impl ReadFaultStore {
    fn new(fixture: &ServiceFixture) -> Self {
        Self {
            inner: SqliteStore::open(fixture.env().store_path()).unwrap(),
            fail: AtomicBool::new(false),
        }
    }
}

impl Store for ReadFaultStore {
    fn access(&self) -> Access {
        self.inner.access()
    }
    type ReadTx<'a> = <SqliteStore as Store>::ReadTx<'a>;
    type WriteTx<'a> = <SqliteStore as Store>::WriteTx<'a>;

    fn read<T>(
        &self,
        f: impl FnOnce(&Self::ReadTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(StoreError::Invariant("injected read outage".into()));
        }
        self.inner.read(f)
    }
    fn write<T>(
        &self,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.inner.write(f)
    }
    fn migrate(&self) -> Result<MigrationReport, StoreError> {
        self.inner.migrate()
    }
    fn change_token(&self) -> Result<u64, StoreError> {
        self.inner.change_token()
    }
    fn snapshot(&self, dir: &Path, label: &str) -> Result<PathBuf, StoreError> {
        self.inner.snapshot(dir, label)
    }
    fn write_with_snapshot<T>(
        &self,
        dir: &Path,
        label: &str,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<WriteWithSnapshot<T>, StoreError> {
        self.inner.write_with_snapshot(dir, label, f)
    }
}

#[test]
fn failed_snapshot_neither_announces_removals_nor_consumes_a_control() {
    let fixture = ServiceFixture::new();
    let store = ReadFaultStore::new(&fixture);
    let watcher = ChangeWatcher::new(&store);
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    start(&fixture);
    let changed_token = store.change_token().unwrap();
    store.fail.store(true, Ordering::SeqCst);
    watcher.notice(&store, &bus);
    assert!(
        notices(&subscription).is_empty(),
        "failed reads are not evidence of project removal"
    );
    store.fail.store(false, Ordering::SeqCst);
    assert_eq!(
        store.change_token().unwrap(),
        changed_token,
        "retry must need no further write"
    );
    watcher.notice(&store, &bus);
    assert_eq!(
        notices(&subscription),
        vec![Change::Project("fixture".into())]
    );
}

#[test]
fn an_initial_read_failure_does_not_mark_the_unread_token_as_seen() {
    let fixture = ServiceFixture::new();
    let store = ReadFaultStore::new(&fixture);
    start(&fixture);
    store.fail.store(true, Ordering::SeqCst);
    let watcher = ChangeWatcher::new(&store);
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    store.fail.store(false, Ordering::SeqCst);
    watcher.notice(&store, &bus);
    assert_eq!(
        notices(&subscription),
        vec![Change::Project("fixture".into()), Change::Catalog]
    );
}
