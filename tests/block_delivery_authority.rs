//! Revocable block episodes and workspace exclusion at the delivery boundary.
use fs4::FileExt;
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use storyhook::daemon::block_delivery::process_one;
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{BlockAction, BlockDelivery, DeliveryStatus, ReadOps, Store, WriteOps};
use storyhook_test_support::ServiceFixture;

/// Run lock-release contracts without descriptors inherited by sibling tests.
/// CLOEXEC closes at exec, so an unrelated concurrent fork can otherwise keep
/// an owner's lock alive after the delivery helper and its descendants exit.
fn isolated_ownership_test(name: &str) -> bool {
    const CHILD: &str = "STORYHOOK_DELIVERY_AUTHORITY_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return false;
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD, name);
    let output = storyhook_test_support::run_bounded(command, name, Duration::from_secs(60));
    assert!(
        output.status.success(),
        "isolated {name} failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

fn fixture() -> ServiceFixture {
    let f = ServiceFixture::new();
    let mut git = storyhook::env::git_env::command(f.cwd());
    let out = git.args(["init", "-b", "main"]).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    f.store()
        .write(|tx| tx.set_checkout_path(f.project(), Some(f.cwd())))
        .unwrap();
    f
}
fn active(f: &ServiceFixture, title: &str) -> String {
    let ctx = f.ctx();
    let service = StoryService::new(&ctx);
    let id = service
        .create(&NewStoryInput {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    service
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    id
}
fn rows(f: &ServiceFixture) -> Vec<BlockDelivery> {
    f.store()
        .read(|tx| tx.block_deliveries(f.project()))
        .unwrap()
}
fn lock_file(f: &ServiceFixture, id: &str) -> File {
    let directory = f.cwd().join(".git/storyhook/workspace-locks");
    std::fs::create_dir_all(&directory).unwrap();
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(format!("{id}.lock")))
        .unwrap()
}
fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "barrier {} was not reached",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct ReleaseOnDrop(std::path::PathBuf);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, "release barrier");
    }
}

#[test]
fn ending_a_block_revokes_pending_interrupt_even_without_resume() {
    for destination in ["todo", "done"] {
        let f = fixture();
        let id = active(&f, "Revocable hold");
        let ctx = f.ctx();
        let service = StoryService::new(&ctx);
        service.set_awaiting(&id, "Wait for approval").unwrap();
        service
            .set_state(&id, destination, None, None, None)
            .unwrap();
        if destination == "todo" {
            service.clear_awaiting(&id).unwrap();
        }
        assert_eq!(rows(&f)[0].status, DeliveryStatus::Superseded);
        assert!(!process_one(f.store(), f.env(), Some(Path::new("/must-not-run"))).unwrap());
        assert!(rows(&f).iter().all(|row| row.action != BlockAction::Resume));
    }
}

#[test]
fn unblock_then_reblock_retires_the_old_episode_without_duplicate_hold_effects() {
    let f = fixture();
    let id = active(&f, "Current episode");
    let ctx = f.ctx();
    let service = StoryService::new(&ctx);
    service.set_awaiting(&id, "First hold").unwrap();
    service.clear_awaiting(&id).unwrap();
    service.set_awaiting(&id, "Second hold").unwrap();
    service.set_awaiting(&id, "Second hold again").unwrap();
    let all = rows(&f);
    assert_eq!(all.len(), 3);
    assert!(
        all[..2]
            .iter()
            .all(|row| row.status == DeliveryStatus::Superseded)
    );
    assert_eq!(all[2].action, BlockAction::Interrupt);
    assert_eq!(all[2].status, DeliveryStatus::Pending);
}

#[test]
fn workspace_contention_does_not_claim_or_starve_another_story() {
    let f = fixture();
    let first = active(&f, "Busy workspace");
    let second = active(&f, "Available workspace");
    let ctx = f.ctx();
    let service = StoryService::new(&ctx);
    service.set_awaiting(&first, "First hold").unwrap();
    service.set_awaiting(&second, "Second hold").unwrap();
    let lock = lock_file(&f, &first);
    lock.try_lock_exclusive().unwrap();
    let script = f.cwd().join("notify.sh");
    std::fs::write(&script, "printf '%s' \"$4\" > reached\nprintf '{\"ok\":true,\"target\":\"session\",\"display\":\"interrupted\"}'\n").unwrap();
    assert!(process_one(f.store(), f.env(), Some(&script)).unwrap());
    assert_eq!(
        std::fs::read_to_string(f.cwd().join("reached")).unwrap(),
        second
    );
    assert_eq!(rows(&f)[0].status, DeliveryStatus::Pending);
    assert_eq!(rows(&f)[1].status, DeliveryStatus::Delivered);
    {
        use storyhook::store::fault::{FaultAction, arm};
        let _fault = arm(
            storyhook::store::FaultPoint::BeforeCommit,
            FaultAction::Fail("busy pass must remain a read".into()),
        );
        assert!(!process_one(f.store(), f.env(), Some(&script)).unwrap());
    }
    service.clear_awaiting(&first).unwrap();
    drop(lock);
    assert_eq!(rows(&f)[0].status, DeliveryStatus::Superseded);
    process_one(f.store(), f.env(), Some(&script)).unwrap();
    assert_eq!(
        std::fs::read_to_string(f.cwd().join("reached")).unwrap(),
        second
    );
}

#[test]
fn admitted_helper_and_its_descendants_retain_exclusion_until_acknowledgement() {
    if isolated_ownership_test(
        "admitted_helper_and_its_descendants_retain_exclusion_until_acknowledgement",
    ) {
        return;
    }
    for orphan in [false, true] {
        let f = fixture();
        let id = active(&f, "Delivery barrier");
        StoryService::new(&f.ctx())
            .set_awaiting(&id, "Wait for approval")
            .unwrap();
        let script = f.cwd().join("notify.sh");
        let wait = "touch captured; while [ ! -e release ]; do sleep 0.02; done";
        std::fs::write(
            f.cwd().join("reply.json"),
            r#"{"ok":true,"target":"original-session","display":"interrupted"}"#,
        )
        .unwrap();
        let body = if orphan {
            format!("( {wait}; cat reply.json ) &\n")
        } else {
            format!("{wait}\ncat reply.json\n")
        };
        std::fs::write(&script, body).unwrap();
        std::thread::scope(|scope| {
            let _release = ReleaseOnDrop(f.cwd().join("release"));
            let (sent, received) = mpsc::channel();
            let fixture = &f;
            let helper = &script;
            scope.spawn(move || {
                sent.send(process_one(fixture.store(), fixture.env(), Some(helper)))
                    .unwrap()
            });
            wait_for(&f.cwd().join("captured"));
            let contender = lock_file(&f, &id);
            let lock_error = contender.try_lock_exclusive().unwrap_err();
            assert_eq!(lock_error.kind(), std::io::ErrorKind::WouldBlock);
            assert!(
                received.recv_timeout(Duration::from_millis(150)).is_err(),
                "helper descendants must quiesce before acknowledgement"
            );
            assert_eq!(rows(&f)[0].status, DeliveryStatus::Attempting);
            StoryService::new(&f.ctx()).clear_awaiting(&id).unwrap();
            assert_eq!(rows(&f)[0].status, DeliveryStatus::Attempting);
            std::fs::write(f.cwd().join("release"), "continue").unwrap();
            assert!(
                received
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap()
            );
            contender.try_lock_exclusive().unwrap();
        });
        assert_eq!(rows(&f)[0].status, DeliveryStatus::Delivered);
        assert_eq!(rows(&f)[0].target.as_deref(), Some("original-session"));
    }
}

#[test]
fn an_unusable_checkout_is_acknowledged_without_poisoning_other_projects() {
    let f = fixture();
    let first = active(&f, "Missing checkout");
    let other = f.add_project("healthy", "HD");
    let ctx = f.ctx_for(other);
    let service = StoryService::new(&ctx);
    let second = service
        .create(&NewStoryInput {
            title: "Healthy delivery".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    service
        .set_state(&second, "in-progress", None, None, None)
        .unwrap();
    f.store()
        .write(|tx| {
            tx.set_checkout_path(f.project(), Some(&f.cwd().join("missing")))?;
            tx.set_checkout_path(other, Some(f.cwd()))
        })
        .unwrap();
    StoryService::new(&f.ctx())
        .set_awaiting(&first, "Wait for repair")
        .unwrap();
    service.set_awaiting(&second, "Wait for approval").unwrap();
    let script = f.cwd().join("notify.sh");
    std::fs::write(&script, "printf '%s' \"$4\" > reached\nprintf '{\"ok\":true,\"target\":\"session\",\"display\":\"interrupted\"}'\n").unwrap();
    assert!(process_one(f.store(), f.env(), Some(&script)).unwrap());
    assert_eq!(rows(&f)[0].status, DeliveryStatus::Unreached);
    assert!(rows(&f)[0].detail.contains("workspace exclusion"));
    assert!(rows(&f)[0].detail.contains("missing"));
    assert!(!f.cwd().join("reached").exists());
    assert!(process_one(f.store(), f.env(), Some(&script)).unwrap());
    assert_eq!(
        std::fs::read_to_string(f.cwd().join("reached")).unwrap(),
        second
    );
    assert_eq!(
        f.store().read(|tx| tx.block_deliveries(other)).unwrap()[0].status,
        DeliveryStatus::Delivered
    );
}

/// Pauses only the first write door; all reads and transactions remain real SQLite.
struct ClaimWriteGate {
    inner: storyhook::store::SqliteStore,
    entered: mpsc::Sender<()>,
    release: std::sync::Mutex<Option<mpsc::Receiver<()>>>,
}

struct ReleaseClaim(Option<mpsc::Sender<()>>);
impl ReleaseClaim {
    fn release(mut self) {
        self.0
            .take()
            .unwrap()
            .send(())
            .expect("release the pending claim");
    }
}
impl Drop for ReleaseClaim {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

impl ClaimWriteGate {
    fn new(f: &ServiceFixture) -> (Self, mpsc::Receiver<()>, ReleaseClaim) {
        let (entered, observation) = mpsc::channel();
        let (release, permission) = mpsc::channel();
        (
            Self {
                inner: storyhook::store::SqliteStore::open(f.store().path()).unwrap(),
                entered,
                release: std::sync::Mutex::new(Some(permission)),
            },
            observation,
            ReleaseClaim(Some(release)),
        )
    }
}

impl Store for ClaimWriteGate {
    fn access(&self) -> storyhook::store::Access {
        self.inner.access()
    }
    type ReadTx<'a> = <storyhook::store::SqliteStore as Store>::ReadTx<'a>;
    type WriteTx<'a> = <storyhook::store::SqliteStore as Store>::WriteTx<'a>;
    fn read<T>(
        &self,
        f: impl FnOnce(&Self::ReadTx<'_>) -> Result<T, storyhook::store::StoreError>,
    ) -> Result<T, storyhook::store::StoreError> {
        self.inner.read(f)
    }
    fn write<T>(
        &self,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, storyhook::store::StoreError>,
    ) -> Result<T, storyhook::store::StoreError> {
        let release = self.release.lock().unwrap().take();
        if let Some(release) = release {
            self.entered.send(()).expect("announce the claim write");
            release
                .recv_timeout(Duration::from_secs(5))
                .expect("release the claim write");
        }
        self.inner.write(f)
    }
    fn migrate(&self) -> Result<storyhook::store::MigrationReport, storyhook::store::StoreError> {
        self.inner.migrate()
    }
    fn change_token(&self) -> Result<u64, storyhook::store::StoreError> {
        self.inner.change_token()
    }
    fn snapshot(
        &self,
        dir: &Path,
        label: &str,
    ) -> Result<std::path::PathBuf, storyhook::store::StoreError> {
        self.inner.snapshot(dir, label)
    }
    fn write_with_snapshot<T>(
        &self,
        dir: &Path,
        label: &str,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, storyhook::store::StoreError>,
    ) -> Result<storyhook::store::WriteWithSnapshot<T>, storyhook::store::StoreError> {
        self.inner.write_with_snapshot(dir, label, f)
    }
}

#[test]
fn project_identity_is_revalidated_after_workspace_acquisition_before_claim() {
    for column in ["uuid", "slug", "prefix", "checkout_path"] {
        let f = fixture();
        let id = active(&f, "Claim identity");
        StoryService::new(&f.ctx())
            .set_awaiting(&id, "Wait for approval")
            .unwrap();
        let (gated, entered, release) = ClaimWriteGate::new(&f);
        std::thread::scope(|scope| {
            // Drop sends permission on panic, before the scoped worker is joined.
            let permit = release;
            let conn = rusqlite::Connection::open(f.store().path()).unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            let original: String = conn
                .query_row(
                    &format!("SELECT {column} FROM projects WHERE id=?1"),
                    [f.project().get()],
                    |row| row.get(0),
                )
                .unwrap();
            let worker =
                scope.spawn(|| process_one(&gated, f.env(), Some(Path::new("/must-not-run"))));
            entered
                .recv_timeout(Duration::from_secs(5))
                .expect("worker must reach the final claim write");
            // Observe only after the worker is paused beyond its nonblocking
            // acquire. Polling with a lock would itself make the worker return busy.
            let contender = lock_file(&f, &id);
            let error = contender
                .try_lock_exclusive()
                .expect_err("worker must own the workspace before the SQL writer");
            assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
            conn.execute(
                &format!("UPDATE projects SET {column}=?1 WHERE id=?2"),
                rusqlite::params!["changed-identity", f.project().get()],
            )
            .unwrap();
            conn.execute_batch("COMMIT").unwrap();
            permit.release();
            assert!(
                !worker.join().unwrap().unwrap(),
                "{column} changed after exclusion was acquired"
            );
            assert_eq!(rows(&f)[0].status, DeliveryStatus::Pending);
            conn.execute(
                &format!("UPDATE projects SET {column}=?1 WHERE id=?2"),
                rusqlite::params![original, f.project().get()],
            )
            .unwrap();
        });
    }
}

#[test]
fn recovery_waits_for_inherited_ownership_and_keeps_other_work_runnable() {
    if isolated_ownership_test(
        "recovery_waits_for_inherited_ownership_and_keeps_other_work_runnable",
    ) {
        return;
    }
    let f = fixture();
    let first = active(&f, "Interrupted helper");
    let second = active(&f, "Independent helper");
    let ctx = f.ctx();
    let service = StoryService::new(&ctx);
    service.set_awaiting(&first, "Wait for approval").unwrap();
    let mut attempting = rows(&f).remove(0);
    attempting.status = DeliveryStatus::Attempting;
    attempting.target = Some("original target".into());
    f.store()
        .write(|tx| tx.update_block_delivery(&attempting, DeliveryStatus::Pending))
        .unwrap();
    let lock = lock_file(&f, &first);
    lock.try_lock_exclusive().unwrap();
    {
        use storyhook::store::fault::{FaultAction, arm};
        let _fault = arm(
            storyhook::store::FaultPoint::BeforeCommit,
            FaultAction::Fail("busy recovery must remain a read".into()),
        );
        storyhook::daemon::block_delivery::recover(f.store(), f.env()).unwrap();
    }
    assert_eq!(rows(&f)[0], attempting);
    service.set_awaiting(&second, "Wait for repair").unwrap();
    let script = f.cwd().join("notify.sh");
    std::fs::write(&script, "printf '%s' \"$4\" > reached\nprintf '{\"ok\":true,\"target\":\"new target\",\"display\":\"interrupted\"}'\n").unwrap();
    assert!(process_one(f.store(), f.env(), Some(&script)).unwrap());
    assert_eq!(rows(&f)[0], attempting);
    assert_eq!(rows(&f)[1].status, DeliveryStatus::Delivered);
    drop(lock);
    assert!(!process_one(f.store(), f.env(), Some(&script)).unwrap());
    assert_eq!(rows(&f)[0].status, DeliveryStatus::Uncertain);
    assert_eq!(rows(&f)[0].target, attempting.target);
    assert_eq!(
        std::fs::read_to_string(f.cwd().join("reached")).unwrap(),
        second
    );
}
