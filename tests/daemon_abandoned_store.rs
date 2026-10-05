//! A daemon whose store is deleted stops, and nothing it writes on the way out
//! brings the deleted tree back.
//!
//! Plugin tests delete their home when they end. A daemon still serving that
//! home used to keep running, listening and answering from an unlinked store,
//! because nothing told it the store was gone; and when it did exit, its
//! "daemon stopped" journal record recreated the deleted home directory by
//! directory. Both are pinned here against a real daemon process.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use storyhook::daemon::lifecycle::{self, ABANDONED_STORE_CHECK};
use storyhook_test_support::load_grace::Patience;
use storyhook_test_support::{ChildGuard, TestEnv, scratch_dir};

/// How long a daemon gets to notice its store is gone: eight of its own checks,
/// with the shared load grace on top. One check would do on an idle machine;
/// the rest is scheduling slack, not a second chance for the behaviour.
const ABANDONMENT_PATIENCE: Duration = ABANDONED_STORE_CHECK.saturating_mul(8);

/// A running daemon, recorded by pid and start token so a test can watch it
/// after its pidfile has been deleted with its home. A daemon that outlives the
/// test is killed here, because nothing else can find it any more.
struct Daemon {
    pid: u32,
    start_time: Option<String>,
}

impl Daemon {
    fn is_live(&self) -> bool {
        lifecycle::process_identity_is_live(self.pid, self.start_time.as_deref())
    }

    fn exits_within_patience(&self) -> bool {
        let mut patience = Patience::new(ABANDONMENT_PATIENCE);
        while self.is_live() {
            if patience.expired() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        true
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.is_live() {
            let pid = libc::pid_t::try_from(self.pid).expect("a pid fits pid_t");
            // SAFETY: `kill` only sends a signal to the recorded daemon, whose
            // identity was rechecked just above.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

fn start_daemon(env: &TestEnv, cwd: &Path, extra: &[(&str, String)]) -> Daemon {
    env.story(cwd)
        .envs(extra.iter().map(|(name, value)| (*name, value.as_str())))
        .args(["daemon", "start"])
        .assert()
        .success();
    let info = env
        .daemon()
        .expect("the daemon published its portfile once it was healthy");
    Daemon {
        pid: info.pid,
        start_time: lifecycle::process_start_time(info.pid),
    }
}

/// Deletes `home` the way a finished test does. One retry covers a directory
/// a daemon thread created in the instant between listing and removal.
fn delete_tree(home: &Path) {
    if std::fs::remove_dir_all(home).is_err() {
        std::fs::remove_dir_all(home).expect("deleting the fixture home");
    }
}

/// The daemon's owner is this live test, so its parent watch never fires: only
/// the daemon noticing that its store is gone can stop it.
#[test]
fn a_daemon_whose_home_is_deleted_exits_and_recreates_nothing() {
    let env = TestEnv::isolated();
    let cwd = scratch_dir();
    let daemon = start_daemon(&env, cwd.path(), &[]);

    delete_tree(env.home());

    assert!(
        daemon.exits_within_patience(),
        "daemon pid {} still serves the deleted store {}",
        daemon.pid,
        env.store_path().display()
    );
    assert!(
        !env.home().exists(),
        "the exiting daemon recreated the deleted home {}",
        env.home().display()
    );
}

/// The incident's order: the test deletes its home, then the test process
/// ends. Whichever watch stops the daemon, its orderly exit writes nothing
/// back into the deleted tree.
#[test]
fn a_daemon_orphaned_after_its_home_was_deleted_leaves_nothing_behind() {
    let env = TestEnv::isolated();
    let cwd = scratch_dir();
    let mut owner =
        ChildGuard::spawn(Command::new("sleep").arg("30")).expect("spawning a stand-in owner");
    let token = lifecycle::process_start_time(owner.pid())
        .expect("the stand-in owner's native start token");
    let daemon = start_daemon(
        &env,
        cwd.path(),
        &[
            ("STORYHOOK_PARENT_PID", owner.pid().to_string()),
            ("STORYHOOK_PARENT_START_TIME", token),
        ],
    );

    delete_tree(env.home());
    owner.kill_and_reap();

    assert!(
        daemon.exits_within_patience(),
        "daemon pid {} outlived both its owner and its store",
        daemon.pid
    );
    assert!(
        !env.home().exists(),
        "the exiting daemon recreated the deleted home {}",
        env.home().display()
    );
}
