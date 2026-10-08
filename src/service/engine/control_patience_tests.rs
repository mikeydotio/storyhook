//! SH-889: the retry boundary must not replay an executed transaction.
use super::*;
use crate::store::SqliteStore;

#[test]
fn engine_control_deadline_returns_busy_without_executing_a_write() {
    let fixture = storyhook_test_support::ServiceFixture::new();
    let env = Environment::at(fixture.env().home()).busy_timeout(Duration::from_millis(100));
    let store = crate::invoke::open_store(&env).unwrap();
    let project = store
        .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
        .unwrap();
    let ctx = Ctx::new(&store, project, fixture.cwd().to_path_buf(), env.clone());
    let dispatcher = ShellDispatcher::new("unused", env);
    let blocker = SqliteStore::open(store.path()).unwrap();
    blocker
        .write(|_| {
            let service = EngineService::new(&ctx, &dispatcher)
                .with_control_deadline(Instant::now() + Duration::from_millis(60));
            let mut called = false;
            let result = service.control_write(|_| {
                called = true;
                Ok(())
            });
            assert!(matches!(result, Err(StoreError::Busy(_))));
            assert!(
                !called,
                "the deadline must leave the transaction unexecuted"
            );
            Ok(())
        })
        .unwrap();

    let expired = EngineService::new(&ctx, &dispatcher).with_control_deadline(Instant::now());
    let result: Result<(), _> =
        expired.control_write(|_| panic!("an expired admission must not execute"));
    assert!(
        matches!(result, Err(StoreError::Busy(ref detail)) if detail == "engine control write admission deadline elapsed")
    );
}

#[test]
fn engine_control_never_retries_busy_after_the_transaction_closure_runs() {
    let fixture = storyhook_test_support::ServiceFixture::new();
    let env = Environment::at(fixture.env().home());
    let store = crate::invoke::open_store(&env).unwrap();
    let project = store
        .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
        .unwrap();
    let ctx = Ctx::new(&store, project, fixture.cwd().to_path_buf(), env.clone());
    let dispatcher = ShellDispatcher::new("unused", env);
    let service = EngineService::new(&ctx, &dispatcher)
        .with_control_deadline(Instant::now() + Duration::from_secs(60));
    let mut calls = 0;
    let result: Result<(), _> = service.control_write(|_| {
        calls += 1;
        Err(StoreError::Busy("transaction already executed".into()))
    });
    assert!(
        matches!(result, Err(StoreError::Busy(ref detail)) if detail == "transaction already executed")
    );
    assert_eq!(calls, 1);
    let result: Result<(), _> = service.control_write(|_| {
        calls += 1;
        Err(StoreError::Invariant(
            "preserve the caller's refusal".into(),
        ))
    });
    assert!(
        matches!(result, Err(StoreError::Invariant(ref detail)) if detail == "preserve the caller's refusal")
    );
    assert_eq!(calls, 2);
}

#[cfg(feature = "fault-injection")]
#[test]
fn engine_control_never_replays_busy_before_commit_or_after_durable_commit() {
    use crate::store::fault::{FaultAction, FaultPoint, arm};

    for point in [FaultPoint::BeforeCommit, FaultPoint::AfterCommitBeforeAck] {
        let fixture = storyhook_test_support::ServiceFixture::new();
        let env = Environment::at(fixture.env().home());
        let store = crate::invoke::open_store(&env).unwrap();
        let project = store
            .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
            .unwrap();
        let before = store.read(|tx| tx.checkout_path(project)).unwrap();
        let changed = fixture.cwd().join("committed-without-acknowledgement");
        assert_ne!(before.as_ref(), Some(&changed));
        let ctx = Ctx::new(&store, project, fixture.cwd().to_path_buf(), env.clone());
        let dispatcher = ShellDispatcher::new("unused", env);
        let service = EngineService::new(&ctx, &dispatcher)
            .with_control_deadline(Instant::now() + Duration::from_secs(60));
        let detail = format!("busy at {}", point.as_str());
        let fault = arm(point, FaultAction::Busy(detail.clone()));
        let mut calls = 0;
        let outcome = service.control_write(|tx| {
            calls += 1;
            tx.set_checkout_path(project, Some(&changed))?;
            Ok(())
        });
        drop(fault);
        assert!(matches!(outcome, Err(StoreError::Busy(ref actual)) if actual == &detail));
        assert_eq!(calls, 1, "{point:?} must not replay the completed body");

        // Read with an independent handle: the fault before COMMIT rolls back,
        // while a lost acknowledgement leaves the successful write durable.
        let observer = SqliteStore::open(store.path()).unwrap();
        let expected = match point {
            FaultPoint::BeforeCommit => before,
            FaultPoint::AfterCommitBeforeAck => Some(changed),
            _ => unreachable!(),
        };
        assert_eq!(
            observer.read(|tx| tx.checkout_path(project)).unwrap(),
            expected
        );
    }
}

#[test]
fn engine_control_deadline_also_bounds_the_shared_handle_write_mutex() {
    let fixture = storyhook_test_support::ServiceFixture::new();
    let env = Environment::at(fixture.env().home());
    let store = crate::invoke::open_store(&env).unwrap();
    let project = store
        .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
        .unwrap();
    let ctx = Ctx::new(&store, project, fixture.cwd().to_path_buf(), env.clone());
    let dispatcher = ShellDispatcher::new("unused", env);
    std::thread::scope(|scope| {
        let (entered, held) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let store_ref = &store;
        let writer = scope.spawn(move || {
            store_ref.write(|_| {
                entered.send(()).unwrap();
                // Dropping `release` on an assertion failure also frees the writer.
                let _ = released.recv();
                Ok(())
            })
        });
        held.recv_timeout(storyhook_test_support::load_grace::PATIENCE_CEILING)
            .unwrap();
        let (done, result) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let service = EngineService::new(&ctx, &dispatcher)
                .with_control_deadline(Instant::now() + Duration::from_millis(60));
            let mut called = false;
            let outcome = service.control_write(|_| {
                called = true;
                Ok(())
            });
            done.send((outcome, called)).unwrap();
        });
        // This is harness patience, not the admission deadline. The other
        // writer is still holding the local mutex when the result must arrive.
        let outcome = result.recv_timeout(storyhook_test_support::load_grace::graced_now(
            Duration::from_secs(5),
        ));
        drop(release);
        writer.join().unwrap().unwrap();
        let (outcome, called) =
            outcome.expect("local admission must not wait for the writer to release");
        assert!(matches!(outcome, Err(StoreError::Busy(_))));
        assert!(!called);
    });
}

#[test]
fn engine_control_busy_body_rolls_back_and_preserves_nested_write_refusal() {
    let fixture = storyhook_test_support::ServiceFixture::new();
    let env = Environment::at(fixture.env().home());
    let store = crate::invoke::open_store(&env).unwrap();
    let project = store
        .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
        .unwrap();
    let ctx = Ctx::new(&store, project, fixture.cwd().to_path_buf(), env.clone());
    let dispatcher = ShellDispatcher::new("unused", env);
    let service = EngineService::new(&ctx, &dispatcher)
        .with_control_deadline(Instant::now() + Duration::from_secs(60));
    let before = store.read(|tx| tx.checkout_path(project)).unwrap();
    let mut calls = 0;
    let outcome: Result<(), _> = service.control_write(|tx| {
        calls += 1;
        tx.set_checkout_path(project, Some(Path::new("/must-roll-back")))?;
        Err(StoreError::Busy("body refusal".into()))
    });
    assert!(matches!(outcome, Err(StoreError::Busy(ref detail)) if detail == "body refusal"));
    assert_eq!(calls, 1);
    assert_eq!(store.read(|tx| tx.checkout_path(project)).unwrap(), before);
    store
        .write(|_| {
            let outcome: Result<(), _> =
                service.control_write(|_| panic!("nested write body must not run"));
            assert!(matches!(outcome, Err(StoreError::NestedWrite)));
            Ok(())
        })
        .unwrap();
}

// A scheduler barrier after a real BEGIN, before the caller's operation. It
// makes the late-admission detector independent of when the worker is scheduled.
struct AdmittedStore {
    inner: SqliteStore,
    admitted: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl Store for AdmittedStore {
    fn access(&self) -> crate::store::Access {
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
        self.inner.write(f)
    }
    fn try_write<T>(
        &self,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.inner.try_write(|tx| {
            self.admitted.send(()).unwrap();
            let _ = self.release.lock().unwrap().recv();
            f(tx)
        })
    }
    fn migrate(&self) -> Result<crate::store::MigrationReport, StoreError> {
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
    ) -> Result<crate::store::WriteWithSnapshot<T>, StoreError> {
        self.inner.write_with_snapshot(dir, label, f)
    }
}

#[test]
fn engine_control_rechecks_deadline_after_successful_begin() {
    let fixture = storyhook_test_support::ServiceFixture::new();
    let env = Environment::at(fixture.env().home());
    let inner = crate::invoke::open_store(&env).unwrap();
    let project = inner
        .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
        .unwrap();
    let (admitted, admission) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let store = AdmittedStore {
        inner,
        admitted,
        release: std::sync::Mutex::new(wait),
    };
    let ctx = Ctx::new(&store, project, fixture.cwd().to_path_buf(), env.clone());
    let dispatcher = ShellDispatcher::new("unused", env);
    std::thread::scope(|scope| {
        // Long enough to reach a local BEGIN; actual expiry is synchronized with
        // the barrier, rather than inferred from a pre-admission sleep.
        let deadline =
            Instant::now() + storyhook_test_support::load_grace::graced_now(Duration::from_secs(1));
        let ctx = &ctx;
        let dispatcher = &dispatcher;
        let worker = scope.spawn(move || {
            let service = EngineService::new(ctx, dispatcher).with_control_deadline(deadline);
            let mut called = false;
            let result = service.control_write(|_| {
                called = true;
                Ok(())
            });
            (result, called)
        });
        let admitted = admission.recv_timeout(storyhook_test_support::load_grace::PATIENCE_CEILING);
        if admitted.is_ok() {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
        drop(release);
        let (result, called) = worker.join().unwrap();
        admitted.expect("the detector must reach BEGIN before expiry");
        assert!(matches!(result, Err(StoreError::Busy(_))));
        assert!(
            !called,
            "an admitted transaction resumed past its deadline must not mutate"
        );
    });
}
