//! Member workspace locks, the member-aware reset quiesce, and the shell
//! actuator's batch process boundary (SH-831).

use super::*;
use crate::service::NewStoryInput;
use crate::store::{SqliteStore, StoreError};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use storyhook_test_support::ServiceFixture;

/// Bound on a scoped helper thread's wait in these tests; never a claim
/// about how fast anything runs.
const PATIENCE: Duration = Duration::from_secs(30);

fn repository() -> tempfile::TempDir {
    let root = storyhook_test_support::scratch_dir();
    let status = crate::env::git_env::command(root.path())
        .args(["init", "-q"])
        .status()
        .unwrap();
    assert!(status.success());
    root
}

fn lock_path(root: &Path, story: &str) -> std::path::PathBuf {
    root.join(".git/storyhook/workspace-locks")
        .join(format!("{story}.lock"))
}

#[test]
fn member_locks_are_tried_in_story_number_order_and_a_held_one_is_skipped() {
    let root = repository();
    let _nine = WorkspaceLock::try_acquire(root.path(), "SH-9")
        .unwrap()
        .unwrap();
    let _ten = WorkspaceLock::try_acquire(root.path(), "SH-10")
        .unwrap()
        .unwrap();
    let members = [
        (StoryNo::new(10), "SH-10".to_owned()),
        (StoryNo::new(11), "SH-11".to_owned()),
        (StoryNo::new(9), "SH-9".to_owned()),
    ];

    let (mut locks, busy) = MemberLocks::acquire(root.path(), &members).unwrap();

    assert_eq!(busy, ["SH-9", "SH-10"], "numeric order, never string order");
    assert!(locks.get("SH-11").is_some());
    assert!(locks.get("SH-9").is_none());
    assert!(
        WorkspaceLock::try_acquire(root.path(), "SH-11")
            .unwrap()
            .is_none(),
        "the batch holds SH-11"
    );
    locks.release("SH-11");
    assert!(
        WorkspaceLock::try_acquire(root.path(), "SH-11")
            .unwrap()
            .is_some(),
        "a member that leaves releases its lock at once"
    );
}

#[test]
fn member_locks_are_released_when_the_batch_ends() {
    let root = repository();
    let members = [(StoryNo::new(2), "SH-2".to_owned())];
    let (locks, busy) = MemberLocks::acquire(root.path(), &members).unwrap();
    assert!(busy.is_empty());
    // A second batch, or a manual action, never waits: it is told busy.
    let (_, busy) = MemberLocks::acquire(root.path(), &members).unwrap();
    assert_eq!(busy, ["SH-2"]);
    drop(locks);
    let (_, busy) = MemberLocks::acquire(root.path(), &members).unwrap();
    assert!(busy.is_empty());
}

fn head(store: &SqliteStore, env: &Environment, project: ProjectId) -> VerificationCandidate {
    let ctx = Ctx::new(store, project, env.home(), env.clone()).no_hooks(true);
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Batch head".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    VerificationQueue::new(store).next().unwrap().unwrap()
}

#[test]
fn a_reset_of_a_batch_member_ends_the_batch_not_the_heads_attempt() {
    let fixture = ServiceFixture::new();
    let store = SqliteStore::open(fixture.store().path()).unwrap();
    let project = ProjectId::new(fixture.project().get());
    let env = Environment::at(fixture.cwd());
    let candidate = head(&store, &env, project);
    let activity = VerificationActivity::new();
    let guard = activity.acquire(&candidate, env.now());
    let batch = Cancellation::default();
    let membership = guard.enter_batch(BTreeSet::from(["SH-7".to_owned()]), batch.clone());

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + PATIENCE;
            while !batch.is_cancelled() {
                assert!(Instant::now() < deadline, "the batch was never cancelled");
                std::thread::sleep(Duration::from_millis(10));
            }
            drop(membership);
        });
        activity
            .cancel_story_and_wait(project, "SH-7", Instant::now() + PATIENCE)
            .unwrap();
    });

    assert!(!guard.is_cancelled(), "the head's attempt goes on");
    assert!(activity.active_for(project).is_some());
    activity
        .cancel_story_and_wait(project, "SH-8", Instant::now())
        .expect("neither head nor member: nothing to wait for");
    let _stuck = guard.enter_batch(BTreeSet::from(["SH-7".to_owned()]), Cancellation::default());
    assert!(
        activity
            .cancel_story_and_wait(project, "SH-7", Instant::now())
            .is_err(),
        "a batch that does not release in time is reported, never waited on forever"
    );
}

/// A member the batch left out (busy lock, refused submission, moved head)
/// is no longer the batch's: before the fix it stayed listed on the slot, so
/// a reset of that story cancelled the whole batch and then waited for it.
#[test]
fn a_member_left_out_of_the_batch_is_neither_waited_for_nor_ends_it() {
    let fixture = ServiceFixture::new();
    let store = SqliteStore::open(fixture.store().path()).unwrap();
    let project = ProjectId::new(fixture.project().get());
    let env = Environment::at(fixture.cwd());
    let candidate = head(&store, &env, project);
    let activity = VerificationActivity::new();
    let guard = activity.acquire(&candidate, env.now());
    let batch = Cancellation::default();
    let membership = guard.enter_batch(
        BTreeSet::from(["SH-7".to_owned(), "SH-8".to_owned()]),
        batch.clone(),
    );

    membership.leave("SH-8");

    activity
        .cancel_story_and_wait(project, "SH-8", Instant::now())
        .expect("a left-out story is not the batch's to wait for");
    assert!(!batch.is_cancelled(), "its reset does not end the batch");
    assert!(
        activity
            .cancel_story_and_wait(project, "SH-7", Instant::now())
            .is_err(),
        "a member still in the batch is waited for"
    );
    assert!(batch.is_cancelled());
    drop(membership);
}

#[test]
fn a_live_batch_is_abandoned_once_and_an_ended_one_is_left_alone() {
    let fixture = ServiceFixture::new();
    let store = SqliteStore::open(fixture.store().path()).unwrap();
    let project = ProjectId::new(fixture.project().get());
    let env = Environment::at(fixture.cwd());
    let first = head(&store, &env, project);
    let second = head_named(&store, &env, project, "Batch partner");
    let batch = record(project, &[&first, &second]);
    store
        .write(|tx| tx.insert_verification_batch(&batch))
        .unwrap();

    assert_eq!(
        abandon_interrupted_batches(&store, &env, project).unwrap(),
        std::slice::from_ref(&batch.id)
    );
    let stored = store.read(|tx| tx.verification_batches(project)).unwrap();
    assert_eq!(stored[0].phase, BatchPhase::Abandoned);
    assert_eq!(stored[0].detail.as_deref(), Some(INTERRUPTED));
    assert!(
        abandon_interrupted_batches(&store, &env, project)
            .unwrap()
            .is_empty()
    );
}

/// A store that counts the write transactions it opens, so a test can prove
/// that a path writes nothing when it has nothing to write (SH-693).
struct CountingStore {
    inner: SqliteStore,
    writes: std::sync::atomic::AtomicUsize,
}

impl CountingStore {
    fn writes(&self) -> usize {
        self.writes.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Store for CountingStore {
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
        self.writes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.write(f)
    }
    fn migrate(&self) -> Result<crate::store::MigrationReport, StoreError> {
        self.inner.migrate()
    }
    fn change_token(&self) -> Result<u64, StoreError> {
        self.inner.change_token()
    }
    fn snapshot(&self, _dir: &Path, _label: &str) -> Result<std::path::PathBuf, StoreError> {
        // These tests take no backups. A standalone copy is a backup site
        // under SH-297 (tests/coupled_snapshot.rs), and this is not one.
        unreachable!("the write-counting store takes no snapshot")
    }
    fn write_with_snapshot<T>(
        &self,
        dir: &Path,
        label: &str,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<crate::store::WriteWithSnapshot<T>, StoreError> {
        self.writes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.write_with_snapshot(dir, label, f)
    }
}

#[test]
fn a_starting_worker_opens_no_write_transaction_without_a_live_batch() {
    let fixture = ServiceFixture::new();
    let store = CountingStore {
        inner: SqliteStore::open(fixture.store().path()).unwrap(),
        writes: std::sync::atomic::AtomicUsize::new(0),
    };
    let project = ProjectId::new(fixture.project().get());
    let env = Environment::at(fixture.cwd());

    assert!(
        abandon_interrupted_batches(&store, &env, project)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.writes(), 0, "no batch: nothing may be written");

    let first = head(&store.inner, &env, project);
    let second = head_named(&store.inner, &env, project, "Batch partner");
    let batch = record(project, &[&first, &second]);
    store
        .inner
        .write(|tx| tx.insert_verification_batch(&batch))
        .unwrap();
    let mut ended = batch
        .advance(BatchPhase::Released, "2026-01-01T00:00:01Z")
        .unwrap();
    ended.retired = true;
    store
        .inner
        .write(|tx| tx.update_verification_batch(&ended, 0))
        .unwrap();
    assert!(
        abandon_interrupted_batches(&store, &env, project)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.writes(),
        0,
        "only ended batches: nothing may be written"
    );
}

/// A batch in `landing` is recovered from its landing intents, never
/// abandoned (B10, SH-832 D3): a restart that finds only such a batch writes
/// nothing, and the batch stays `landing`.
#[test]
fn a_starting_worker_leaves_a_landing_batch_to_its_intents() {
    let fixture = ServiceFixture::new();
    let store = CountingStore {
        inner: SqliteStore::open(fixture.store().path()).unwrap(),
        writes: std::sync::atomic::AtomicUsize::new(0),
    };
    let project = ProjectId::new(fixture.project().get());
    let env = Environment::at(fixture.cwd());
    let first = head(&store.inner, &env, project);
    let second = head_named(&store.inner, &env, project, "Batch partner");
    let batch = record(project, &[&first, &second]);
    assert_eq!(batch.phase, BatchPhase::Gating);
    let landing = batch
        .advance(BatchPhase::Landing, "2026-01-01T00:00:01Z")
        .unwrap();
    store
        .inner
        .write(|tx| {
            tx.insert_verification_batch(&batch)?;
            assert!(tx.update_verification_batch(&landing, 0)?);
            Ok(())
        })
        .unwrap();

    assert!(
        abandon_interrupted_batches(&store, &env, project)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.writes(),
        0,
        "only a landing batch: nothing is written"
    );
    assert_eq!(
        store
            .inner
            .read(|tx| tx.verification_batches(project))
            .unwrap()[0]
            .phase,
        BatchPhase::Landing
    );
}

fn head_named(
    store: &SqliteStore,
    env: &Environment,
    project: ProjectId,
    title: &str,
) -> VerificationCandidate {
    let ctx = Ctx::new(store, project, env.home(), env.clone()).no_hooks(true);
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    VerificationQueue::new(store)
        .ordered_for(project)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.story_id == id)
        .unwrap()
}

fn record(project: ProjectId, members: &[&VerificationCandidate]) -> VerificationBatch {
    let id = BatchId::generate();
    VerificationBatch {
        branch: id.branch(),
        id,
        project,
        project_slug: members[0].project_slug.clone(),
        head: members[0].story_id.clone(),
        base_branch: "dev".into(),
        base_commit: "a".repeat(40),
        tip: "b".repeat(40),
        pull_request: None,
        phase: BatchPhase::Gating,
        members: members
            .iter()
            .enumerate()
            .map(|(position, member)| BatchMember {
                story: StoryNo::parse_id("SH", &member.story_id).unwrap(),
                story_id: member.story_id.clone(),
                generation: member.verifying_generation.unwrap(),
                head_commit: "c".repeat(40),
                pull_request: format!("https://github.com/acme/widgets/pull/{}", position + 1),
                position: position as u32,
            })
            .collect(),
        excluded: Vec::new(),
        gate: None,
        detail: None,
        retired: false,
        revision: 0,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    }
}

/// Writes an executable fixture script.
fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/bash\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A registered GitHub checkout, a verifying story with a lease in it, and
/// a shell actuator whose helper, verifier and batch scripts are fixtures
/// that record what they were given.
struct Boundary {
    fixture: ServiceFixture,
    store: SqliteStore,
    env: Environment,
    candidate: VerificationCandidate,
    root: std::path::PathBuf,
    record: std::path::PathBuf,
}

impl Boundary {
    fn new() -> Self {
        let fixture = ServiceFixture::new();
        let root = fixture.github_checkout("https://github.com/acme/widgets");
        let store = SqliteStore::open(fixture.store().path()).unwrap();
        let project = ProjectId::new(fixture.project().get());
        let env = Environment::at(&root);
        let mut candidate = head(&store, &env, project);
        candidate.checkout = root.clone();
        candidate.cleanup_lease = Some(crate::domain::StoryCleanupLease {
            version: CLEANUP_LEASE_VERSION,
            project_slug: candidate.project_slug.clone(),
            story_id: candidate.story_id.clone(),
            repository_path: root.clone(),
            worktree_path: root.join(".claude/worktrees").join(&candidate.story_id),
            branch: format!("worktree-{}", candidate.story_id),
            tmux: crate::domain::TmuxCleanupTarget {
                socket_path: root.join("tmux.sock"),
            },
        });
        let record = root.join("recorded");
        Self {
            fixture,
            store,
            env,
            candidate,
            root,
            record,
        }
    }

    fn actuator(&self, activity: &VerificationActivity) -> ShellVerificationActuator {
        let helper = self.root.join("helper.sh");
        // A submit helper that echoes a valid receipt and records the inode
        // of the workspace lock it inherited.
        script(
            &helper,
            &format!(
                r#"python3 -c 'import os; print(os.fstat(int(os.environ["STORY_WORKSPACE_LOCK_FD"])).st_ino)' > '{record}.lock'
lease="$STORYHOOK_REAP_LEASE_V1"
story=$(printf '%s' "$lease" | jq -r .story_id)
jq -n --argjson lease "$lease" --arg story "$story" \
  '{{ok:true, receipt_version:1, story_id:$story, lease:$lease, pushed:true,
    pull_request:{{url:"https://github.com/acme/widgets/pull/7", number:7, base:"dev",
      head_oid:"{head}", adopted:true}}, display:"fixture"}}'"#,
                record = self.record.display(),
                head = "d".repeat(40),
            ),
        );
        let verifier = self.root.join("verify-pr.sh");
        script(
            &verifier,
            &format!(
                r#"env | grep '^STORYHOOK_REPAIR_\|^STORYHOOK_CERTIFY_ONLY=' | sort > '{record}.gate'
printf '%s\n' "$1" >> '{record}.gate'
printf '%s\n' '{{"result":"certified","head":"{head}","tree":"{tree}","gate":"make test","detail":"ok"}}'"#,
                record = self.record.display(),
                head = "d".repeat(40),
                tree = "e".repeat(40),
            ),
        );
        let batch = self.root.join("verify-batch.sh");
        script(
            &batch,
            &format!(
                r#"{{ pwd -P; printf '%s\n' "$STORYHOOK_GITHUB_AUTHORITY" "$STORYHOOK_GATE_PROGRESS" "$@"; }} > '{record}.batch'
case "$1" in
publish) printf '%s\n' '{{"ok":true,"url":"https://github.com/acme/widgets/pull/900","number":900,"base":"dev","head_oid":"'"$3"'","adopted":false,"pushed":true}}' ;;
retire) printf '%s\n' '{{"ok":true,"closed":true,"merged":false,"deleted":true}}' ;;
esac"#,
                record = self.record.display(),
            ),
        );
        ShellVerificationActuator::with_paths(
            self.env.clone(),
            helper,
            std::path::PathBuf::from("/usr/bin/true"),
        )
        .with_activity(activity.clone())
        .with_verifier_script(verifier)
        .with_batch_script(batch)
        .with_batching()
    }

    fn read(&self, suffix: &str) -> String {
        std::fs::read_to_string(self.record.with_extension(suffix)).unwrap()
    }
}

#[test]
fn a_member_submission_inherits_that_members_own_lock() {
    let boundary = Boundary::new();
    let activity = VerificationActivity::new();
    let _head = activity.acquire(&boundary.candidate, boundary.env.now());
    let actuator = boundary.actuator(&activity);
    let (locks, busy) =
        MemberLocks::acquire(&boundary.root, &[(StoryNo::new(41), "SH-41".to_owned())]).unwrap();
    assert!(busy.is_empty());

    let receipt = actuator
        .batch()
        .expect("with_batching offers batch operations")
        .submit_member(
            &boundary.candidate,
            MemberOwner(locks.get("SH-41").unwrap()),
            &Cancellation::default(),
        )
        .unwrap();

    assert_eq!(receipt.number, 7);
    let inherited: u64 = boundary.read("lock").trim().parse().unwrap();
    let member_lock = std::fs::metadata(lock_path(&boundary.root, "SH-41")).unwrap();
    assert_eq!(
        inherited,
        member_lock.ino(),
        "the member's own lock, not the head's"
    );
}

#[test]
fn the_batch_gate_claims_no_repair_admission_while_the_heads_own_gate_does() {
    let boundary = Boundary::new();
    let activity = VerificationActivity::new();
    let _head = activity.acquire(&boundary.candidate, boundary.env.now());
    let actuator = boundary.actuator(&activity);
    let batch_pr = PrLink {
        owner: "acme".into(),
        repo: "widgets".into(),
        number: 900,
        url: "https://github.com/acme/widgets/pull/900".into(),
        close_on_merge: false,
        status: "open".into(),
        linked_at: "2026-01-01T00:00:00Z".into(),
        last_checked_at: None,
    };
    let cancellation = Cancellation::default();

    let own = actuator.verify_cancellable(&boundary.candidate, &batch_pr, &cancellation);
    assert!(
        matches!(own, VerificationOutcome::Certified { .. }),
        "{own:?}"
    );
    let own_env = boundary.read("gate");
    assert!(
        own_env.contains("STORYHOOK_REPAIR_ADMISSION=1"),
        "{own_env}"
    );

    let batch = actuator
        .batch()
        .unwrap()
        .gate(&boundary.candidate, &batch_pr, &cancellation);
    assert!(
        matches!(batch, VerificationOutcome::Certified { .. }),
        "{batch:?}"
    );
    let batch_env = boundary.read("gate");
    assert!(!batch_env.contains("STORYHOOK_REPAIR_"), "{batch_env}");
    assert!(
        batch_env.contains("STORYHOOK_CERTIFY_ONLY=1"),
        "{batch_env}"
    );
    assert!(
        batch_env.ends_with("https://github.com/acme/widgets/pull/900\n"),
        "{batch_env}"
    );
}

#[test]
fn publish_and_retire_run_the_batch_script_from_the_lease_repository() {
    let boundary = Boundary::new();
    let activity = VerificationActivity::new();
    let _head = activity.acquire(&boundary.candidate, boundary.env.now());
    let actuator = boundary.actuator(&activity);
    let batching = actuator.batch().unwrap();
    let tip = "f".repeat(40);
    let publication = BatchPublication {
        branch: "storyhook/verify-batch/0123456789ab".into(),
        tip: tip.clone(),
        base: "dev".into(),
        title: "Verification batch 0123456789ab: SH-1, SH-2".into(),
        body: "members".into(),
    };

    let published = batching
        .publish(&boundary.candidate, &publication, &Cancellation::default())
        .unwrap();

    assert_eq!(
        published,
        BatchPullRequest {
            url: "https://github.com/acme/widgets/pull/900".into(),
            number: 900
        }
    );
    let lines: Vec<String> = boundary.read("batch").lines().map(str::to_owned).collect();
    assert_eq!(
        std::path::Path::new(&lines[0]),
        boundary.root.canonicalize().unwrap(),
        "cwd is the head's lease repository"
    );
    assert_eq!(std::path::Path::new(&lines[1]), boundary.root);
    assert_eq!(
        std::path::Path::new(&lines[2]),
        journal_path(&boundary.env, &boundary.candidate)
    );
    assert_eq!(
        lines[3..],
        [
            "publish".to_owned(),
            publication.branch.clone(),
            tip,
            "dev".into(),
            publication.title.clone(),
            "members".into()
        ]
    );

    let mut batch = record(
        boundary.candidate.project,
        &[&boundary.candidate, &boundary.candidate],
    );
    batch.pull_request = Some(published);
    let retirement = batching
        .retire(&boundary.candidate, &batch, "ended")
        .unwrap();
    assert!(retirement.closed && retirement.deleted && !retirement.merged);
    let lines: Vec<String> = boundary.read("batch").lines().map(str::to_owned).collect();
    assert_eq!(
        lines[3..],
        [
            "retire".to_owned(),
            batch.branch.clone(),
            "https://github.com/acme/widgets/pull/900".into(),
            "ended".into()
        ]
    );
    drop(boundary.store);
    drop(boundary.fixture);
}
