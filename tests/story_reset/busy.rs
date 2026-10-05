//! A reset waits out store contention at every write and never reports it (SH-886).
//!
//! SH-801's card reset removed its window, worktree and branch, then lost its
//! final write to SQLite's busy timeout and stayed reserved. These tests give
//! every other write the same `Busy` result, before its closure runs, exactly
//! where `BEGIN IMMEDIATE` reports contention.
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use storyhook::service::reset::{ResetCaller, reset_story};
use storyhook::service::story_reset::StoryResetService;
use storyhook::service::{Ctx, NewStoryInput, StoryService};
use storyhook::store::{ReadOps, SqliteStore, Store, StoreError, StoryNo, WriteOps};
use storyhook_test_support::ServiceFixture;

/// Refuses every other write as contention once armed; reads stay real.
struct ContendedWrites {
    inner: SqliteStore,
    armed: AtomicBool,
    calls: AtomicUsize,
    refused: AtomicUsize,
}

impl ContendedWrites {
    fn open(fixture: &ServiceFixture) -> Self {
        Self {
            inner: SqliteStore::open(fixture.store().path()).unwrap(),
            armed: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            refused: AtomicUsize::new(0),
        }
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    fn refused(&self) -> usize {
        self.refused.load(Ordering::SeqCst)
    }
}

impl Store for ContendedWrites {
    fn access(&self) -> storyhook::store::Access {
        self.inner.access()
    }
    type ReadTx<'a> = <SqliteStore as Store>::ReadTx<'a>;
    type WriteTx<'a> = <SqliteStore as Store>::WriteTx<'a>;
    fn read<T>(
        &self,
        f: impl FnOnce(&Self::ReadTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.inner.read(f)
    }
    fn write<T>(
        &self,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        if self.armed.load(Ordering::SeqCst) && self.calls.fetch_add(1, Ordering::SeqCst) % 2 == 0 {
            self.refused.fetch_add(1, Ordering::SeqCst);
            return Err(StoreError::Busy(
                "timed out waiting for the project write lock (beginning a write)".into(),
            ));
        }
        self.inner.write(f)
    }
    fn migrate(&self) -> Result<storyhook::store::MigrationReport, StoreError> {
        self.inner.migrate()
    }
    fn change_token(&self) -> Result<u64, StoreError> {
        self.inner.change_token()
    }
    fn snapshot(&self, dir: &Path, label: &str) -> Result<std::path::PathBuf, StoreError> {
        self.inner.snapshot(dir, label)
    }
    fn write_with_snapshot<T>(
        &self,
        dir: &Path,
        label: &str,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<storyhook::store::WriteWithSnapshot<T>, StoreError> {
        self.inner.write_with_snapshot(dir, label, f)
    }
}

fn in_progress(fixture: &ServiceFixture, title: &str) -> String {
    StoryService::new(&fixture.ctx().no_hooks(true))
        .create(&NewStoryInput {
            title: title.into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn state(fixture: &ServiceFixture) -> String {
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .state
}

#[test]
fn card_reset_completes_when_every_write_after_reservation_meets_contention() {
    let fixture = ServiceFixture::new();
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), None))
        .unwrap();
    let id = in_progress(&fixture, "Contended card reset");
    let store = ContendedWrites::open(&fixture);
    let ctx = Ctx::new(
        &store,
        fixture.project(),
        fixture.cwd().to_path_buf(),
        fixture.env().clone(),
    )
    .no_hooks(true);
    let service = StoryResetService::new(&ctx);
    let reset = service.reserve(&id, &id).unwrap();
    store.arm();
    let done = service
        .execute(&id, &reset.token, || Ok(()))
        .expect("store contention must be waited out, never reported");
    assert!(done.completed);
    assert!(
        store.refused() >= 3,
        "each write after reservation met contention: {}",
        store.refused()
    );
    assert_eq!(state(&fixture), "todo");
    let stored = fixture
        .store()
        .read(|tx| tx.story_reset(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(stored.completed && stored.failure.is_none(), "{stored:?}");
}

#[test]
fn card_reservation_waits_out_contention() {
    let fixture = ServiceFixture::new();
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), None))
        .unwrap();
    let id = in_progress(&fixture, "Contended reservation");
    let store = ContendedWrites::open(&fixture);
    let ctx = Ctx::new(
        &store,
        fixture.project(),
        fixture.cwd().to_path_buf(),
        fixture.env().clone(),
    )
    .no_hooks(true);
    store.arm();
    let reset = StoryResetService::new(&ctx)
        .reserve(&id, &id)
        .expect("a reservation must wait out store contention");
    assert_eq!(store.refused(), 1);
    assert!(!reset.completed);
}

#[test]
fn native_reset_completes_when_every_write_meets_contention() {
    let fixture = ServiceFixture::new();
    let repo = fixture.cwd().canonicalize().unwrap();
    let init = storyhook::env::git_env::command(&repo)
        .args(["init", "--initial-branch=main"])
        .output()
        .unwrap();
    assert!(init.status.success(), "{init:?}");
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(&repo)))
        .unwrap();
    let id = in_progress(&fixture, "Contended native reset");
    let store = ContendedWrites::open(&fixture);
    let ctx = Ctx::new(
        &store,
        fixture.project(),
        fixture.cwd().to_path_buf(),
        fixture.env().clone(),
    )
    .no_hooks(true);
    store.arm();
    reset_story(&ctx, &id, false, &ResetCaller::default())
        .expect("store contention must be waited out, never reported");
    assert!(store.refused() >= 2, "{}", store.refused());
    assert_eq!(state(&fixture), "todo");
    assert!(
        fixture
            .store()
            .read(|tx| tx.story_resets(fixture.project()))
            .unwrap()
            .is_empty()
    );
}
