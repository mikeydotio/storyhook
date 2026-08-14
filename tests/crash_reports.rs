//! End-to-end: a daemon that crashes gets classified, ledgered, and — when
//! there is real defect evidence and storyhook's own project is registered —
//! files itself a bug story with a redacted excerpt of its own log (SH-287).
//!
//! Filing is gated behind `STORYHOOK_CRASH_FILE=1` in every test build
//! (`crash::should_file`), so every test that wants it to run sets that
//! variable explicitly on the one command it matters for — proving both that
//! the override works and, by its absence elsewhere, that a test build never
//! files without it.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use storyhook::daemon::crash::{self, CrashClassification, FiledOutcome};
use storyhook::daemon::lifecycle;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Stops whatever daemon `env` is running, even if the test panics first.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment(), lifecycle::StopMode::Force);
    }
}

/// Blocks until `ready`, or fails the test.
fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

/// Registers storyhook's own project — the one `crash::file_pending` targets
/// — in `env`'s store, at exactly the uuid this repository's own committed
/// `.storyhook.toml` declares.
///
/// `story project new` cannot mint that uuid on its own (it always generates
/// a fresh one): but `ProjectService::init` adopts an *existing* pointer
/// file's uuid rather than replacing it — the mechanism a clone of an
/// already-registered repository relies on. Writing that pointer by hand
/// before running the command is what reaches the same code path without a
/// second repository to clone.
fn register_self_project(env: &TestEnv, dir: &Path) {
    std::fs::write(
        dir.join(".storyhook.toml"),
        "schema = 1\nuuid = \"291ea25f-3363-4b5d-9051-66636c1066f9\"\nprefix = \"SH\"\n",
    )
    .expect("writing the pointer file");
    env.story(dir)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
}

/// Spawns a real `story daemon --serve` that panics on its own shortly after
/// publishing its portfile, and waits for it to die.
///
/// Stands down whatever daemon is already running first — `register_self_project`
/// auto-spawns one, and `claim_pidfile` refuses a second one outright, which
/// would make the "crash" spawned here fail to even start rather than panic.
fn panic_the_daemon(env: &TestEnv, cwd: &Path) {
    panic_the_daemon_with_marker(env, cwd, "1");
}

/// The same as [`panic_the_daemon`], with a distinct panic message — so
/// several crashes in one test do not fingerprint identically and dedupe
/// into each other.
fn panic_the_daemon_with_marker(env: &TestEnv, cwd: &Path, marker: &str) {
    env.stop_daemon();
    assert!(
        !env.daemon_is_live(),
        "a daemon must not be running before arming a crash"
    );
    let mut panicking = env
        .raw_story(cwd)
        .env("STORYHOOK_TEST_PANIC", marker)
        .args(["daemon", "--serve", "--port", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning a daemon armed to panic");
    let status = panicking.wait().expect("reaping the panicking daemon");
    assert!(
        !status.success(),
        "a panicking daemon must not exit cleanly"
    );
}

/// Starts a fresh daemon and, when `file` is set, waits until every
/// `Pending` crash has resolved one way or another.
///
/// **Read the ledger only *after* starting the daemon, not before.**
/// `harvest` runs synchronously inside this same `daemon start` — the crash
/// this call is meant to process does not exist in the ledger at all until
/// the new daemon's `lifecycle::run` writes it. Capturing "what was pending"
/// beforehand would capture an empty set and make the wait below vacuously
/// true, resolving before `file_pending`'s background thread has run at all.
fn restart_and_wait_for_filing(env: &TestEnv, cwd: &Path, file: bool) {
    let mut cmd = env.story(cwd);
    cmd.args(["daemon", "start"]);
    if file {
        cmd.env("STORYHOOK_CRASH_FILE", "1");
    }
    cmd.assert().success();

    if !file {
        // Nothing to wait for: `should_file` is false, so `file_pending`
        // returns immediately without touching the ledger.
        return;
    }
    wait_for("crash filing to resolve every pending record", || {
        crash::read_crashes(&env.environment())
            .iter()
            .all(|c| c.filed != FiledOutcome::Pending)
    });
}

/// A `story show <id> --json` result, parsed.
fn show(env: &TestEnv, cwd: &Path, id: &str) -> serde_json::Value {
    let output = env
        .story(cwd)
        .args(["show", id, "--json"])
        .output()
        .expect("running story show");
    assert!(output.status.success(), "story show {id} failed");
    serde_json::from_slice(&output.stdout).expect("parsing story show's JSON")
}

/// Every open story's id and labels, for a project — enough to find whatever
/// a crash may have filed without already knowing its id.
fn list_with_labels(env: &TestEnv, cwd: &Path) -> Vec<serde_json::Value> {
    let output = env
        .story(cwd)
        .args(["list", "--json"])
        .output()
        .expect("running story list");
    assert!(output.status.success(), "story list failed");
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parsing story list's JSON");
    parsed["stories"].as_array().cloned().unwrap_or_default()
}

/// An unclean exit with no panic record is not evidence of a defect, and must
/// never be filed — the daemon abandoned mid-flight ledger already exists for
/// "something might not have finished"; this is specifically "something is
/// wrong with the code", and a `kill -9` alone does not say that.
#[test]
fn an_unclean_exit_with_no_evidence_files_nothing() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();
    register_self_project(&env, dir.path());
    let before = list_with_labels(&env, dir.path()).len();

    // A crash with no panic hook involved: a bare `SIGKILL`, delivered by
    // hand rather than through `story daemon stop`, leaves a residue portfile
    // and no `PanicRecord` — exactly what the next start must read as an
    // unclean exit rather than a defect. `register_self_project` already
    // auto-spawned a daemon, so it must be stood down first, the same as
    // `panic_the_daemon` does.
    env.stop_daemon();
    assert!(
        !env.daemon_is_live(),
        "a daemon must not be running before arming a crash"
    );
    let mut started = env
        .raw_story(dir.path())
        .args(["daemon", "--serve", "--port", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning a daemon");
    wait_for("the daemon to publish a portfile", || {
        env.daemon().is_some()
    });
    // SIGKILL, not a panic: no PanicRecord, so the next start must classify
    // this as an unclean exit rather than a defect.
    unsafe {
        libc::kill(started.id() as libc::pid_t, libc::SIGKILL);
    }
    started.wait().expect("reaping the killed daemon");
    wait_for("the pidfile lock to release", || {
        !lifecycle::is_live(&env.environment())
    });

    restart_and_wait_for_filing(&env, dir.path(), true);

    let ledger = crash::read_crashes(&env.environment());
    assert_eq!(
        ledger.len(),
        1,
        "exactly one crash must be ledgered: {ledger:?}"
    );
    assert_eq!(ledger[0].classification, CrashClassification::UncleanExit);
    assert!(
        matches!(&ledger[0].filed, FiledOutcome::Withheld(reason) if reason.contains("not evidence")),
        "an unclean exit must be withheld, not filed: {:?}",
        ledger[0].filed
    );

    let after = list_with_labels(&env, dir.path()).len();
    assert_eq!(
        before, after,
        "no story may be created for a bare unclean exit"
    );
}

/// The central claim: a real panic, in a real daemon, becomes a real bug
/// story — labelled, prioritized, and carrying what the evidence says.
#[test]
fn a_panic_files_a_high_priority_bug_story() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();
    register_self_project(&env, dir.path());

    panic_the_daemon(&env, dir.path());
    restart_and_wait_for_filing(&env, dir.path(), true);

    let ledger = crash::read_crashes(&env.environment());
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].classification, CrashClassification::Panicked);
    let story_id = match &ledger[0].filed {
        FiledOutcome::Filed(id) => id.clone(),
        other => panic!("expected the crash to be filed, got {other:?}"),
    };

    let story = show(&env, dir.path(), &story_id);
    let fields = &story["story"]["story"];
    assert_eq!(fields["priority"], "high");
    let labels: Vec<&str> = fields["labels"]
        .as_array()
        .expect("labels array")
        .iter()
        .map(|l| l.as_str().expect("a label is a string"))
        .collect();
    assert!(
        labels.contains(&"crash"),
        "labels must include `crash`: {labels:?}"
    );
    assert!(
        labels.contains(&"auto-filed"),
        "labels must include `auto-filed`: {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.starts_with("crash:")),
        "labels must include a fingerprint: {labels:?}"
    );
    assert!(
        fields["title"]
            .as_str()
            .expect("a title")
            .starts_with("Daemon panic:"),
        "title: {:?}",
        fields["title"]
    );
    let description = fields["description"].as_str().unwrap_or_default();
    assert!(
        description.contains("STORYHOOK_TEST_PANIC"),
        "the description must carry the panic's own message: {description}"
    );
}

/// A second, identical panic must not mint a second story — it folds a "seen
/// again" comment into the first one instead.
#[test]
fn the_same_panic_twice_dedupes_into_one_story() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();
    register_self_project(&env, dir.path());

    panic_the_daemon(&env, dir.path());
    restart_and_wait_for_filing(&env, dir.path(), true);
    let first_ledger = crash::read_crashes(&env.environment());
    let story_id = match &first_ledger[0].filed {
        FiledOutcome::Filed(id) => id.clone(),
        other => panic!("expected the first crash to be filed, got {other:?}"),
    };

    panic_the_daemon(&env, dir.path());
    restart_and_wait_for_filing(&env, dir.path(), true);

    let ledger = crash::read_crashes(&env.environment());
    assert_eq!(ledger.len(), 2, "both crashes must be ledgered: {ledger:?}");
    assert_eq!(
        ledger[1].filed,
        FiledOutcome::Deduped(story_id.clone()),
        "the repeat must be deduped into the first story, not filed anew"
    );

    let stories = list_with_labels(&env, dir.path());
    let crash_stories: Vec<&serde_json::Value> = stories
        .iter()
        .filter(|s| {
            s["story"]["labels"]
                .as_array()
                .is_some_and(|labels| labels.iter().any(|l| l.as_str() == Some("crash")))
        })
        .collect();
    assert_eq!(
        crash_stories.len(),
        1,
        "exactly one crash story must exist after two identical panics: {crash_stories:?}"
    );

    let story = show(&env, dir.path(), &story_id);
    let comments = story["story"]["story"]["comments"]
        .as_array()
        .expect("comments array");
    assert!(
        comments.iter().any(|c| c["text"]
            .as_str()
            .unwrap_or_default()
            .contains("seen again")),
        "the second occurrence must be recorded as a comment: {comments:?}"
    );
}

/// Nothing is filed anywhere until storyhook's own project is registered in
/// this store — the crash is still ledgered, and says why.
#[test]
fn without_the_self_project_registered_everything_is_withheld() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();
    // Deliberately no `register_self_project` call.
    env.story(dir.path())
        .args(["project", "new", "--prefix", "ZZ"])
        .assert()
        .success();

    panic_the_daemon(&env, dir.path());
    restart_and_wait_for_filing(&env, dir.path(), true);

    let ledger = crash::read_crashes(&env.environment());
    assert_eq!(ledger.len(), 1);
    assert!(
        matches!(&ledger[0].filed, FiledOutcome::Withheld(reason) if reason.contains("not registered")),
        "must name the missing registration: {:?}",
        ledger[0].filed
    );
    assert!(
        list_with_labels(&env, dir.path()).is_empty(),
        "no story may exist in an unrelated project"
    );
}

/// The gate itself: without `STORYHOOK_CRASH_FILE=1`, a test build never
/// files — even with real defect evidence and a registered project.
#[test]
fn a_test_build_files_nothing_without_the_explicit_override() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();
    register_self_project(&env, dir.path());

    panic_the_daemon(&env, dir.path());
    restart_and_wait_for_filing(&env, dir.path(), false);

    let ledger = crash::read_crashes(&env.environment());
    assert_eq!(ledger.len(), 1);
    assert_eq!(
        ledger[0].filed,
        FiledOutcome::Pending,
        "without the override, filing must not have run at all"
    );
    assert!(list_with_labels(&env, dir.path()).is_empty());
}

/// `daemon.log` is 0600 precisely because it can carry a GitHub token
/// (SH-153) — this proves the *pipeline*, not just `redact` in isolation: a
/// secret genuinely present in a crash's own preserved log must not survive
/// into the story a human reads.
#[test]
fn a_secret_in_the_daemons_log_never_reaches_the_filed_story() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();
    register_self_project(&env, dir.path());

    panic_the_daemon(&env, dir.path());

    // Seeded after the daemon has died and before the next spawn rotates the
    // file — deterministic and safe, since nothing is writing to it at this
    // moment. Exercises the real pipeline (rotate -> harvest -> preserve_log
    // -> redact -> embed), not a fabricated `CrashRecord`.
    {
        use std::io::Write as _;
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(env.environment().daemon_log())
            .expect("opening the dead daemon's log to seed a secret into it");
        writeln!(log, "X-Storyhook-Token: totally-secret-bearer-value").unwrap();
        writeln!(
            log,
            "using ghp_abcdefghijklmnopqrstuvwxyz0123456789 for a github call"
        )
        .unwrap();
    }

    restart_and_wait_for_filing(&env, dir.path(), true);

    let ledger = crash::read_crashes(&env.environment());
    let story_id = match &ledger[0].filed {
        FiledOutcome::Filed(id) => id.clone(),
        other => panic!("expected the crash to be filed, got {other:?}"),
    };
    let story = show(&env, dir.path(), &story_id);
    let description = story["story"]["story"]["description"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !description.contains("totally-secret-bearer-value"),
        "the bearer token leaked into the filed story: {description}"
    );
    assert!(
        !description.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
        "the GitHub token leaked into the filed story: {description}"
    );
    assert!(
        description.contains("[REDACTED]") && description.contains("[REDACTED-GITHUB-TOKEN]"),
        "redaction markers must survive into the description: {description}"
    );
}

/// A crashloop must not flood the tracker: at most three *new* stories per
/// daemon start (the cap is a private constant in `crash.rs`, exercised
/// behaviorally here rather than imported).
#[test]
fn a_crashloop_is_capped_at_three_new_stories_per_start() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();
    register_self_project(&env, dir.path());

    // Four distinct, non-deduplicating crashes, accumulated as `Pending`
    // without ever triggering filing.
    for marker in ["one", "two", "three", "four"] {
        panic_the_daemon_with_marker(&env, dir.path(), marker);
        restart_and_wait_for_filing(&env, dir.path(), false);
    }
    let before = crash::read_crashes(&env.environment());
    assert_eq!(before.len(), 4);
    assert!(before.iter().all(|c| c.filed == FiledOutcome::Pending));

    // One more restart, this time with filing on, processes all four at once.
    env.stop_daemon();
    let mut cmd = env.story(dir.path());
    cmd.args(["daemon", "start"])
        .env("STORYHOOK_CRASH_FILE", "1");
    cmd.assert().success();
    wait_for("all four pending crashes to resolve", || {
        crash::read_crashes(&env.environment())
            .iter()
            .all(|c| c.filed != FiledOutcome::Pending)
    });

    let after = crash::read_crashes(&env.environment());
    let filed = after
        .iter()
        .filter(|c| matches!(c.filed, FiledOutcome::Filed(_)))
        .count();
    let withheld_for_the_cap = after
        .iter()
        .filter(
            |c| matches!(&c.filed, FiledOutcome::Withheld(reason) if reason.contains("more than")),
        )
        .count();
    assert_eq!(
        filed, 3,
        "exactly three of the four must be filed: {after:?}"
    );
    assert_eq!(
        withheld_for_the_cap, 1,
        "the fourth must be withheld and say why: {after:?}"
    );

    let crash_story_count = list_with_labels(&env, dir.path())
        .iter()
        .filter(|s| {
            s["story"]["labels"]
                .as_array()
                .is_some_and(|labels| labels.iter().any(|l| l.as_str() == Some("crash")))
        })
        .count();
    assert_eq!(crash_story_count, 3);
}

/// `story doctor crashes` — the CLI surface a human reviews the ledger
/// through, and `story doctor`'s own advisory pointer to it (SH-287).
#[test]
fn story_doctor_crashes_lists_clears_and_is_advised_from_plain_doctor() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();
    register_self_project(&env, dir.path());

    let empty = env
        .story(dir.path())
        .args(["doctor", "crashes"])
        .output()
        .expect("running story doctor crashes");
    assert!(empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stdout).contains("no crashes"));

    panic_the_daemon(&env, dir.path());
    restart_and_wait_for_filing(&env, dir.path(), true);

    let ledger = crash::read_crashes(&env.environment());
    let crash_id = ledger[0].id.clone();
    let story_id = match &ledger[0].filed {
        FiledOutcome::Filed(id) => id.clone(),
        other => panic!("expected the crash to be filed, got {other:?}"),
    };

    let listing = env
        .story(dir.path())
        .args(["doctor", "crashes"])
        .output()
        .expect("running story doctor crashes");
    assert!(listing.status.success());
    let listing_text = String::from_utf8_lossy(&listing.stdout);
    assert!(
        listing_text.contains(&crash_id),
        "the listing must name the crash: {listing_text}"
    );
    assert!(
        listing_text.contains("panicked"),
        "the listing must name the classification: {listing_text}"
    );
    assert!(
        listing_text.contains(&format!("filed as `{story_id}`")),
        "the listing must say what the crash became: {listing_text}"
    );

    // `story doctor` (no subcommand) only advises — it does not fail the
    // project just because a daemon once crashed and got a bug filed for it.
    let plain_doctor = env
        .story(dir.path())
        .args(["doctor"])
        .output()
        .expect("running story doctor");
    assert!(plain_doctor.status.success());
    let plain_doctor_text = String::from_utf8_lossy(&plain_doctor.stdout);
    assert!(
        plain_doctor_text.contains("crash") && plain_doctor_text.contains("story doctor crashes"),
        "plain `story doctor` must point at the crash ledger: {plain_doctor_text}"
    );

    env.story(dir.path())
        .args(["doctor", "crashes", "clear", "--all"])
        .assert()
        .success();
    let cleared = env
        .story(dir.path())
        .args(["doctor", "crashes"])
        .output()
        .expect("running story doctor crashes after clearing");
    assert!(String::from_utf8_lossy(&cleared.stdout).contains("no crashes"));
}
