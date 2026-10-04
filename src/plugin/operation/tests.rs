//! The provider's inherited lock must outlive its request-thread scope.

use super::*;
use std::process::Stdio;
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, scratch_dir};

#[test]
fn inherited_provider_lock_survives_scope_exit() {
    let scratch = scratch_dir();
    let lock_path = scratch.path().join("provider.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .unwrap();
    lock.lock_exclusive().unwrap();
    let other = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    let active = Active {
        lock,
        path: scratch.path().join("current.json"),
        record: Record {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            target: "claude".into(),
            verb: "install".into(),
            started_at: now(),
            completed_at: None,
            home: scratch.path().into(),
            pid: std::process::id(),
            actor: Actor::observe(),
            previous: Previous::Unregistered,
            intended_source: None,
            steps: vec![],
            outcome: Outcome::Incomplete,
            error: None,
        },
    };
    ACTIVE.with(|slot| *slot.borrow_mut() = Some(active));
    let scope = Scope;
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "read answer"])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    inherit_lock(&mut command);
    let mut child = ChildGuard::spawn(&mut command).unwrap();
    drop(scope);
    assert_eq!(
        other.try_lock_exclusive().unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
    child.stdin().unwrap().write_all(b"release\n").unwrap();
    let patience = storyhook_test_support::load_grace::graced_now(STORY_COMMAND_DEADLINE);
    assert!(
        child
            .wait_within(patience, || "provider lock child did not exit".into())
            .success()
    );
    other.try_lock_exclusive().unwrap();
}
