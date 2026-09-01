//! `story doctor install` reports the installed set (SH-530).
//!
//! The load-bearing property is that it answers **when the store will not open
//! normally**. That is its own headline — a machine whose store was carried
//! past every release by an unreleased build is exactly the machine that needs
//! to be told so — and a verb that resolved a store before speaking could never
//! deliver it. So the degraded case here is not an edge case; it is the point.

use std::process::Output;

use storyhook_test_support::TestEnv;

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn it_reports_the_installed_set_on_an_ordinary_machine() {
    let env = TestEnv::isolated();
    let project = env.project().build();

    let out = env
        .story(project.path())
        .args(["doctor", "install"])
        .output()
        .expect("running `story doctor install`");

    assert!(out.status.success(), "{}", text(&out));
    let report = text(&out);
    for row in ["running", "binary", "store", "edit guard"] {
        assert!(
            report.contains(row),
            "the report must name `{row}`:\n{report}"
        );
    }
}

#[test]
fn it_still_answers_when_the_store_is_from_a_newer_storyhook() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    env.stop_daemon();
    let future = storyhook::store::current_schema_version() + 1;
    rusqlite::Connection::open(env.store_path())
        .expect("opening the store")
        .execute_batch(&format!("PRAGMA user_version = {future}"))
        .expect("claiming a future schema");

    let out = env
        .story(project.path())
        .args(["doctor", "install"])
        .output()
        .expect("running `story doctor install`");

    assert!(
        out.status.success(),
        "the verb whose headline is `your store is out of range` must not need \
         that store to say so:\n{}",
        text(&out)
    );
    let report = text(&out);
    assert!(
        report.contains("READ-ONLY"),
        "it must name the condition:\n{report}"
    );
    assert!(
        report.contains(&future.to_string()),
        "it must name the version found:\n{report}"
    );
}

#[test]
fn a_pending_one_way_migration_is_reported_before_it_happens() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    env.stop_daemon();
    // A store one version BEHIND this build: the shape that, on 2026-08-28,
    // silently carried this project's real tracker past every published
    // release. It is reported here before anything runs it.
    rusqlite::Connection::open(env.store_path())
        .expect("opening the store")
        .execute_batch("PRAGMA user_version = 1")
        .expect("planting an older schema");

    let out = env
        .story(project.path())
        .args(["doctor", "install"])
        .output()
        .expect("running `story doctor install`");

    let report = text(&out);
    assert!(
        report.contains("PENDING") && report.contains("one-way"),
        "a pending migration must be named, and named as irreversible:\n{report}"
    );
}

#[test]
fn the_summary_never_tells_anyone_to_revert_their_work() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    env.stop_daemon();
    rusqlite::Connection::open(env.store_path())
        .expect("opening the store")
        .execute_batch("PRAGMA user_version = 1")
        .expect("planting an older schema");

    let report = text(
        &env.story(project.path())
            .args(["doctor", "install"])
            .output()
            .expect("running `story doctor install`"),
    );

    // The whole doctrine of this verb in one assertion: a change sitting in a
    // checkout is aimed at the next release, so the remedy is always the
    // release. A detector that advised throwing the work away would be worse
    // than no detector.
    assert!(
        report.contains("never to revert"),
        "the summary must say the work survives:\n{report}"
    );
    assert!(
        !report.to_lowercase().contains("discard"),
        "nothing here may suggest discarding work:\n{report}"
    );
}
