//! SH-730: remediation reuses ownership instead of contending with its verifier.

use super::*;
use crate::service::NewStoryInput;
use crate::service::workspace_lock::{WorkspaceLock, git};
use crate::store::{SqliteStore, StoryNo};
use storyhook_test_support::ServiceFixture;

fn candidate(store: &SqliteStore, env: &Environment, project: ProjectId) -> VerificationCandidate {
    let ctx = Ctx::new(store, project, env.home(), env.clone()).no_hooks(true);
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Workspace remediation".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    VerificationQueue::new(store).next().unwrap().unwrap()
}

fn helper(root: &std::path::Path) -> PathBuf {
    let path = root.join("notify.sh");
    std::fs::write(
        &path,
        format!(
            "{}\n{}",
            include_str!("../../../plugins/story/lib/workspace.sh"),
            r#"
fail() { printf '%s\n' "$*" >&2; exit 1; }
[ "$3" = notify ] || fail "unexpected verb $3"
for boundary in discovery delivery submission; do
    reserve_story_workspace "$4"
done
python3 - "$4" "$5" <<'PY'
import fcntl, json, sys
with open('.git/storyhook/workspace-locks/' + sys.argv[1] + '.lock', 'a') as rival:
    try:
        fcntl.flock(rival, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        pass
    else:
        raise AssertionError('notification lost workspace exclusion')
with open('delivered', 'w') as receipt:
    receipt.write(sys.argv[2])
print(json.dumps({'ok': True}))
PY
"#
        ),
    )
    .unwrap();
    path
}

#[test]
fn remediation_reuses_verifier_lock_without_parking_or_interrupting_agent() {
    let fixture = ServiceFixture::new();
    let store = SqliteStore::open(fixture.store().path()).unwrap();
    let project = ProjectId::new(fixture.project().get());
    let env = Environment::at(fixture.cwd());
    let mut candidate = candidate(&store, &env, project);
    candidate.checkout = fixture.cwd().to_path_buf();
    git(&candidate.checkout, &["init", "-q"], None).unwrap();
    let activity = VerificationActivity::new();
    let guard = activity
        .try_acquire(&store, &candidate, env.now())
        .unwrap()
        .unwrap();
    let actuator = ShellVerificationActuator::with_paths(
        env.clone(),
        helper(fixture.cwd()),
        "unused-story".into(),
    )
    .with_activity(activity.clone());
    let ctx = Ctx::new(&store, project, env.home(), env.clone()).no_hooks(true);
    let diagnosis =
        "CENTRAL VERIFICATION RED — repair the failed assertion.\nPreserve this diagnosis.";
    let result = return_for_repair(
        &VerificationQueue::new(&store),
        &ctx,
        &actuator,
        &candidate,
        diagnosis,
        &activity.cancellation_for(project),
    )
    .unwrap();
    let row = store
        .read(|tx| tx.story(project, StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(
        matches!(result, GenerationWrite::Applied(true)),
        "{:?}",
        row.awaiting
    );
    assert_eq!(row.state, "in-progress");
    assert!(row.awaiting.is_none());
    assert!(
        store
            .read(|tx| tx.block_deliveries(project))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        std::fs::read_to_string(fixture.cwd().join("delivered")).unwrap(),
        diagnosis
    );
    assert!(
        WorkspaceLock::try_acquire(fixture.cwd(), &candidate.story_id)
            .unwrap()
            .is_none()
    );
    drop(guard);
    assert!(
        WorkspaceLock::try_acquire(fixture.cwd(), &candidate.story_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn notification_without_verifier_ownership_acquires_its_own_lock() {
    let fixture = ServiceFixture::new();
    let store = SqliteStore::open(fixture.store().path()).unwrap();
    let project = ProjectId::new(fixture.project().get());
    let env = Environment::at(fixture.cwd());
    let mut candidate = candidate(&store, &env, project);
    candidate.checkout = fixture.cwd().to_path_buf();
    git(fixture.cwd(), &["init", "-q"], None).unwrap();
    let actuator =
        ShellVerificationActuator::with_paths(env, helper(fixture.cwd()), "unused-story".into());
    assert_eq!(
        actuator.notify(&candidate, "diagnosis").unwrap(),
        NotifyDelivery::Delivered
    );
    assert!(
        WorkspaceLock::try_acquire(fixture.cwd(), &candidate.story_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn control_runner_replaces_stale_markers_only_with_current_ownership() {
    let fixture = ServiceFixture::new();
    let store = SqliteStore::open(fixture.store().path()).unwrap();
    let project = ProjectId::new(fixture.project().get());
    let env = Environment::at(fixture.cwd());
    let mut candidate = candidate(&store, &env, project);
    candidate.checkout = fixture.cwd().to_path_buf();
    git(fixture.cwd(), &["init", "-q"], None).unwrap();
    let activity = VerificationActivity::new();
    let actuator = ShellVerificationActuator::new(env.clone()).with_activity(activity.clone());
    for owned in [false, true] {
        let _guard = owned.then(|| {
            activity
                .try_acquire(&store, &candidate, env.now())
                .unwrap()
                .unwrap()
        });
        let mut command = Command::new("python3");
        command
            .args([
                "-c",
                r#"
import os, sys
marker = os.environ.get('STORY_WORKSPACE_LOCK_FD')
if sys.argv[1] == 'owned':
    expected = os.stat('.git/storyhook/workspace-locks/SH-1.lock')
    actual = os.fstat(int(marker))
    assert (actual.st_dev, actual.st_ino) == (expected.st_dev, expected.st_ino)
else:
    assert marker is None, marker
"#,
                if owned { "owned" } else { "unowned" },
            ])
            .current_dir(fixture.cwd())
            .env("STORY_WORKSPACE_LOCK_FD", "999999");
        let output = actuator
            .run_control_command(
                command,
                "verifier-notify",
                "ownership",
                project,
                "ownership probe",
            )
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn shell_reservation_rejects_invalid_and_wrong_workspace_descriptors() {
    let root = storyhook_test_support::scratch_dir();
    git(root.path(), &["init", "-q"], None).unwrap();
    let owner = WorkspaceLock::acquire(root.path(), "SH-1").unwrap();
    for marker in ["closed", "wrong-story", "not-a-descriptor", "2"] {
        let mut command = Command::new("bash");
        command.arg("-c").arg(format!(
            "{}\nfail() {{ printf '%s\\n' \"$*\" >&2; exit 1; }}\nreserve_story_workspace \"$1\"\n",
            include_str!("../../../plugins/story/lib/workspace.sh")
        )).arg("probe").arg(if marker == "wrong-story" { "SH-2" } else { "SH-1" })
            .current_dir(root.path());
        // Materialize the wrong target so rejection proves identity mismatch.
        std::fs::write(
            root.path().join(".git/storyhook/workspace-locks/SH-2.lock"),
            "",
        )
        .unwrap();
        if marker == "wrong-story" {
            owner.dispatch_command(&mut command);
        } else {
            command.env(
                "STORY_WORKSPACE_LOCK_FD",
                if marker == "closed" { "999999" } else { marker },
            );
        }
        let output = crate::process::run_captured(command, Duration::from_secs(5))
            .unwrap_or_else(|error| panic!("{}", error.detail()));
        assert!(!output.status.success(), "accepted {marker}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains(if marker == "wrong-story" {
                "workspace lock identity changed"
            } else {
                "descriptor"
            }),
            "{marker}: {error}"
        );
    }
}

#[test]
fn control_runner_keeps_ownership_through_timeout_and_cancellation_cleanup() {
    for cancel in [false, true] {
        let fixture = ServiceFixture::new();
        let store = SqliteStore::open(fixture.store().path()).unwrap();
        let project = ProjectId::new(fixture.project().get());
        let env = Environment::at(fixture.cwd());
        let mut candidate = candidate(&store, &env, project);
        candidate.checkout = fixture.cwd().to_path_buf();
        git(fixture.cwd(), &["init", "-q"], None).unwrap();
        let activity = VerificationActivity::new();
        let guard = activity
            .try_acquire(&store, &candidate, env.now())
            .unwrap()
            .unwrap();
        let cancellation = activity.cancellation_for(project);
        let mut actuator = ShellVerificationActuator::new(env).with_activity(activity);
        actuator.control_timeout = Duration::from_secs(2);
        actuator.termination_grace = Duration::from_secs(2);
        let mut command = Command::new("python3");
        command
            .args([
                "-c",
                r#"
import fcntl, os, signal, time
from pathlib import Path
def terminate(signum, frame):
    with open('.git/storyhook/workspace-locks/SH-1.lock', 'a') as rival:
        try:
            fcntl.flock(rival, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            Path('terminated').write_text('excluded')
        else:
            Path('terminated').write_text('ownership lost')
    raise SystemExit(0)
signal.signal(signal.SIGTERM, terminate)
# Drop the child copy: the runner must retain its own owner during cleanup.
os.close(int(os.environ['STORY_WORKSPACE_LOCK_FD']))
Path('ready').touch()
while True:
    time.sleep(0.01)
"#,
            ])
            .current_dir(fixture.cwd());
        std::thread::scope(|scope| {
            let running = scope.spawn(|| {
                actuator.run_control_command(
                    command,
                    "verifier-notify",
                    "termination",
                    project,
                    "termination probe",
                )
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while !fixture.cwd().join("ready").exists() {
                assert!(Instant::now() < deadline, "child never became ready");
                std::thread::sleep(Duration::from_millis(10));
            }
            drop(guard);
            assert!(
                WorkspaceLock::try_acquire(fixture.cwd(), "SH-1")
                    .unwrap()
                    .is_none()
            );
            if cancel {
                cancellation.cancel();
            }
            let error = running
                .join()
                .unwrap()
                .err()
                .expect("control must stop")
                .to_string();
            assert!(
                error.contains(if cancel { "cancel" } else { "did not finish" }),
                "{error}"
            );
        });
        assert_eq!(
            std::fs::read_to_string(fixture.cwd().join("terminated")).unwrap(),
            "excluded"
        );
        assert!(
            WorkspaceLock::try_acquire(fixture.cwd(), "SH-1")
                .unwrap()
                .is_some()
        );
    }
}
