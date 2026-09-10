//! `story daemon gc` reclaims the runtime directory of a store that no longer
//! exists — and only that (SH-638).
//!
//! `~/.local/state/storyhook/daemons/<key>/` is created for every store a
//! daemon ever served and was never removed: 1,207 on the filing machine, 3
//! with a live store. The sweeper is conservative by design — every directory
//! it cannot *prove* throwaway is kept and named with a reason code — and it
//! reports before it removes, through the same confirmation door
//! `story project delete` uses.
//!
//! Every fixture here is a directory planted by hand, in the exact shape a
//! real daemon leaves (a `daemon.log` whose `holding` line names the store,
//! or a `daemon.json`), aged past the reclaim floor by setting mtimes rather
//! than sleeping. The one real-daemon test at the end proves the planted
//! shape is the real one. Lock-held cases hold the `flock` in this process:
//! a daemon deliberately left serving a deleted store is a shape the standing
//! rule forbids and `check-no-orphan-servers.sh` would reap mid-test.
//!
//! Design of record: `docs/spec/store-isolation.md`, the SH-638 amendment.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fs4::FileExt;
use predicates::prelude::*;
use storyhook::daemon::agent::LAUNCHD_LABEL;
use storyhook::daemon::gc::RECLAIM_AGE_FLOOR;
use storyhook::env::{StoreLocation, canonical_ish};
use storyhook::service::project::is_under_temp;
use storyhook_test_support::{TestEnv, non_temporary_dir, scratch_dir};

/// How far past the floor a planted directory is aged. Twice the floor
/// rather than the floor plus a margin, so the assertion never sits within a
/// clock tick of the boundary it is meant to be clear of.
const AGED_PAST_THE_FLOOR: Duration = RECLAIM_AGE_FLOOR.saturating_mul(2);

/// The line a real daemon writes first, verbatim shape
/// (`src/daemon/lifecycle.rs`, `holding`).
fn holding_line(store: &Path) -> String {
    format!(
        "storyhook daemon 2.4.2 on http://127.0.0.1:50684 (pid 34732) holding {}\n",
        store.display()
    )
}

/// The canonical form of a store path under `env`'s scratch root — what a
/// daemon keys its directory by, whether or not the file exists yet.
fn temp_store(env: &TestEnv, name: &str) -> PathBuf {
    let literal = env.home().join("stores").join(name);
    fs::create_dir_all(literal.parent().unwrap()).expect("the store's parent");
    let store = canonical_ish(&literal).expect("a canonical path under the scratch root");
    assert!(
        is_under_temp(&store),
        "positive control: the fixture store {} must sit under a temp root",
        store.display()
    );
    store
}

/// The runtime directory `store` keys, under `env`'s state home.
fn runtime_dir(env: &TestEnv, store: &Path) -> PathBuf {
    env.environment()
        .daemons_dir()
        .join(StoreLocation::key_for_path(store))
}

/// Plants the directory a cleanly-stopped daemon leaves for `store`: a
/// `daemon.log` naming it, a zero-byte pidfile and spawn lock, and one backup
/// snapshot — then ages it past the reclaim floor.
fn plant(env: &TestEnv, store: &Path) -> PathBuf {
    let dir = runtime_dir(env, store);
    plant_with_log(&dir, &holding_line(store));
    dir
}

fn plant_with_log(dir: &Path, log: &str) {
    fs::create_dir_all(dir.join("backups")).expect("the runtime directory");
    fs::write(dir.join("daemon.log"), log).expect("daemon.log");
    fs::write(dir.join("daemon.pid"), b"").expect("daemon.pid");
    fs::write(dir.join("daemon.spawn.lock"), b"").expect("daemon.spawn.lock");
    fs::write(
        dir.join("backups/storyhook-20260815T235412.214Z-snapshot.db"),
        b"not really sqlite",
    )
    .expect("a snapshot");
    age(dir, AGED_PAST_THE_FLOOR);
}

/// Sets the mtime of `dir` and everything directly inside it to `by` ago —
/// the newest of those is what the sweeper reads as the directory's age.
fn age(dir: &Path, by: Duration) {
    let then = SystemTime::now() - by;
    let mut targets = vec![dir.to_path_buf()];
    targets.extend(
        fs::read_dir(dir)
            .expect("reading the runtime directory")
            .flatten()
            .map(|entry| entry.path()),
    );
    for target in targets {
        File::open(&target)
            .and_then(|file| file.set_modified(then))
            .unwrap_or_else(|e| panic!("aging {}: {e}", target.display()));
    }
}

/// Holds an exclusive `flock` on `path`, as the daemon holds its pidfile and
/// a spawning client holds the spawn lock. Released on drop.
fn hold(path: &Path) -> File {
    let held = File::options()
        .read(true)
        .write(true)
        .open(path)
        .expect("opening the lock file");
    held.try_lock_exclusive()
        .expect("this test must be the one holding it");
    held
}

fn gc(env: &TestEnv) -> assert_cmd::Command {
    let mut cmd = env.story(env.home());
    cmd.args(["daemon", "gc"]);
    cmd
}

fn gc_force(env: &TestEnv) -> assert_cmd::Command {
    let mut cmd = gc(env);
    cmd.arg("--force");
    cmd
}

/// `gc --force` about some *other* store, so the environment's own store —
/// which is also its default store — is judged as the default rather than
/// as "the store this command is about".
fn gc_force_about_another_store(env: &TestEnv) -> assert_cmd::Command {
    let other = temp_store(env, "another.db");
    let mut cmd = env.story(env.home());
    cmd.args([
        "--store-path",
        other.to_str().unwrap(),
        "daemon",
        "gc",
        "--force",
    ]);
    cmd
}

#[test]
fn gc_refuses_without_force_and_names_what_it_would_remove() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "gone.db");
    let dir = plant(&env, &store);

    gc(&env)
        .assert()
        .failure()
        .stderr(predicate::str::contains(StoreLocation::key_for_path(
            &store,
        )))
        .stderr(predicate::str::contains(store.display().to_string()))
        .stderr(predicate::str::contains("backup snapshot"))
        .stderr(predicate::str::contains("--force"));
    assert!(dir.is_dir(), "an unconfirmed gc must remove nothing");
}

#[test]
fn gc_force_removes_the_directory_and_leaves_the_default_stores() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "gone.db");
    let dir = plant(&env, &store);
    // The default store's own directory, its store absent too — planted in
    // the identical shape so only the exclusion can tell them apart.
    let default_dir = plant(&env, env.environment().store_path());

    gc_force_about_another_store(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("removed 1"))
        .stdout(predicate::str::contains(StoreLocation::key_for_path(
            &store,
        )))
        .stdout(predicate::str::contains("[default_store]"));
    assert!(!dir.exists(), "the reclaimable directory must be gone");
    assert!(
        default_dir.is_dir(),
        "the default store's directory is never reclaimed"
    );
}

#[test]
fn gc_reports_nothing_to_reclaim_on_a_clean_state_home() {
    let env = TestEnv::isolated();
    gc(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to reclaim"));
}

#[test]
fn gc_keeps_a_directory_whose_store_exists() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "live.db");
    fs::write(&store, b"present").expect("the store");
    let dir = plant(&env, &store);

    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("[store_exists]"));
    assert!(dir.is_dir());
}

#[test]
fn gc_keeps_this_stores_directory_even_when_the_store_is_gone() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "mine.db");
    let dir = plant(&env, &store);

    let mut cmd = env.story(env.home());
    cmd.args([
        "--store-path",
        store.to_str().unwrap(),
        "daemon",
        "gc",
        "--force",
    ]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("[this_store]"));
    assert!(dir.is_dir());
}

#[test]
fn gc_keeps_the_default_stores_directory_even_when_the_store_is_gone() {
    let env = TestEnv::isolated();
    let dir = plant(&env, env.environment().store_path());

    gc_force_about_another_store(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("[default_store]"));
    assert!(dir.is_dir());
}

#[test]
fn gc_keeps_a_directory_whose_pidfile_is_held() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "serving.db");
    let dir = plant(&env, &store);
    let held = hold(&dir.join("daemon.pid"));

    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("[daemon_live]"));
    assert!(dir.is_dir(), "a held pidfile is a live daemon");

    drop(held);
    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("removed 1"));
    assert!(!dir.exists(), "released, the same directory is reclaimable");
}

#[test]
fn gc_keeps_a_directory_whose_spawn_lock_is_held() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "spawning.db");
    let dir = plant(&env, &store);
    let _held = hold(&dir.join("daemon.spawn.lock"));

    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("[spawn_in_progress]"));
    assert!(dir.is_dir());
}

#[test]
fn gc_keeps_an_absent_store_outside_a_temp_root() {
    let env = TestEnv::isolated();
    let root = non_temporary_dir("daemon-gc");
    let store = canonical_ish(&root.join("offline/store.db")).expect("a canonical path");
    assert!(
        !is_under_temp(&store),
        "positive control: {}",
        store.display()
    );
    let dir = plant(&env, &store);

    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("[not_under_temp]"))
        .stdout(predicate::str::contains("offline"));
    assert!(
        dir.is_dir(),
        "an absent store outside a temp root may be an offline volume"
    );
}

#[test]
fn gc_keeps_a_directory_it_cannot_identify() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "anonymous.db");
    let dir = runtime_dir(&env, &store);
    fs::create_dir_all(&dir).expect("the runtime directory");
    fs::write(dir.join("daemon.pid"), b"").expect("daemon.pid");
    age(&dir, AGED_PAST_THE_FLOOR);

    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("[unidentified]"));
    assert!(dir.is_dir());
}

#[test]
fn gc_keeps_a_directory_whose_identity_does_not_hash_to_its_name() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "keyed.db");
    let other = temp_store(&env, "named.db");
    // Keyed by one store, claiming another: the parse is not to be trusted.
    let dir = runtime_dir(&env, &store);
    plant_with_log(&dir, &holding_line(&other));

    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("[identity_mismatch]"));
    assert!(dir.is_dir());
}

#[test]
fn gc_reads_a_store_path_containing_a_space_whole() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "Ada Lovelace/store.db");
    fs::create_dir_all(store.parent().unwrap()).expect("the spaced parent");
    let dir = plant(&env, &store);

    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("removed 1"));
    assert!(
        !dir.exists(),
        "a path is read to its line end, never its first space"
    );
}

#[test]
fn gc_finds_the_holding_line_below_a_banner() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "banner.db");
    let dir = runtime_dir(&env, &store);
    let log = format!(
        "storyhook daemon: wrote a backup to {}/backups/x.db\n{}",
        dir.display(),
        holding_line(&store)
    );
    plant_with_log(&dir, &log);

    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("removed 1"));
    assert!(!dir.exists());
}

#[test]
fn gc_prefers_the_portfile_over_the_log() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "portfile.db");
    let dir = runtime_dir(&env, &store);
    // A log naming nothing usable; the portfile carries the truth.
    plant_with_log(
        &dir,
        "storyhook daemon: parent process 1 is gone; exiting\n",
    );
    fs::write(
        dir.join("daemon.json"),
        serde_json::json!({
            "pid": 1, "port": 1, "version": "2.4.2", "protocol": 1,
            "exe": "/usr/local/bin/story", "exe_mtime": 0,
            "started_at": "2026-08-03T19:46:22Z", "token": "t",
            "store_path": store,
        })
        .to_string(),
    )
    .expect("daemon.json");
    age(&dir, AGED_PAST_THE_FLOOR);

    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("removed 1"));
    assert!(!dir.exists());
}

#[test]
fn gc_keeps_a_directory_younger_than_the_spawn_lock_deadline() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "fresh.db");
    let dir = plant(&env, &store);
    // Planted aged; touch one file back to now, as a spawn in flight would.
    File::open(dir.join("daemon.log"))
        .and_then(|f| f.set_modified(SystemTime::now()))
        .expect("touching the log");

    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("[too_young]"));
    assert!(dir.is_dir());
}

#[test]
fn gc_keeps_a_store_a_login_agent_still_names() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "agent.db");
    let dir = plant(&env, &store);
    let agents = env.home().join("Library/LaunchAgents");
    fs::create_dir_all(&agents).expect("the LaunchAgents directory");
    fs::write(
        agents.join(format!("{LAUNCHD_LABEL}.deadbeefdeadbeef.plist")),
        format!(
            "<plist><dict><key>ProgramArguments</key><array><string>/usr/local/bin/story\
             </string><string>--store-path</string><string>{}</string><string>daemon\
             </string><string>--serve</string></array></dict></plist>",
            store.display()
        ),
    )
    .expect("planting the agent");

    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("[login_agent]"))
        .stdout(predicate::str::contains("daemon uninstall"));
    assert!(
        dir.is_dir(),
        "launchd would recreate it at login; uninstall is the remedy"
    );
}

#[test]
fn gc_never_touches_an_entry_that_is_not_a_key() {
    let env = TestEnv::isolated();
    let daemons = env.environment().daemons_dir();
    fs::create_dir_all(daemons.join("not-a-key")).expect("a stray directory");
    fs::write(daemons.join("README"), b"hello").expect("a stray file");
    age(&daemons.join("not-a-key"), AGED_PAST_THE_FLOOR);

    gc_force(&env).assert().success();
    assert!(daemons.join("not-a-key").is_dir());
    assert!(daemons.join("README").is_file());
}

#[test]
fn daemon_status_names_reclaimable_directories_only_when_there_are_any() {
    let env = TestEnv::isolated();
    let mut quiet = env.story(env.home());
    quiet
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("story daemon gc").not());

    plant(&env, &temp_store(&env, "gone.db"));
    let mut hinted = env.story(env.home());
    hinted
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 runtime director"))
        .stdout(predicate::str::contains("story daemon gc"));
}

/// The planted shape is the real one: a daemon started for a temp store,
/// stopped, its store deleted, is reclaimed by the same predicate.
#[test]
fn gc_reclaims_a_real_daemons_directory_once_its_temp_store_is_gone() {
    let env = TestEnv::isolated();
    let store = temp_store(&env, "real.db");
    let cwd = scratch_dir();
    let mut start = env.story(cwd.path());
    start
        .args(["--store-path", store.to_str().unwrap(), "daemon", "start"])
        .assert()
        .success();
    let dir = runtime_dir(&env, &store);
    assert!(dir.is_dir(), "a started daemon keys {}", dir.display());
    let mut stop = env.story(cwd.path());
    stop.args(["--store-path", store.to_str().unwrap(), "daemon", "stop"])
        .assert()
        .success();
    fs::remove_file(&store).expect("deleting the store");

    // Younger than the floor: kept, and said so.
    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("[too_young]"));
    assert!(dir.is_dir());

    age(&dir, AGED_PAST_THE_FLOOR);
    gc(&env)
        .assert()
        .failure()
        .stderr(predicate::str::contains(store.display().to_string()))
        .stderr(predicate::str::contains("--force"));
    assert!(dir.is_dir());
    gc_force(&env)
        .assert()
        .success()
        .stdout(predicate::str::contains("removed 1"));
    assert!(!dir.exists());
}
