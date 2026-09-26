//! Regressions for the production view process boundary and catalog authority.

use super::*;
use crate::store::{SqliteStore, WriteOps};
use storyhook_test_support::{STORY_COMMAND_DEADLINE, ServiceFixture, load_grace, scratch_dir};

#[test]
fn probe_budget_leaves_one_third_for_interpreter_start_and_exit() {
    assert_eq!(
        crate::process::plugin_probe_budget() * 3,
        VIEW_RECONCILE_TIMEOUT * 2
    );
}

#[test]
fn a_pass_longer_than_the_old_outer_bound_can_complete() {
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 6; printf completed"]);
    let result = run_view(command, &AtomicBool::new(false))
        .unwrap_or_else(|error| panic!("{}", error.detail()));
    assert!(result.status.success());
    assert_eq!(result.stdout, b"completed");
}

#[test]
fn shutdown_interrupts_an_active_view_without_spending_its_deadline() {
    let root = scratch_dir();
    let ready = root.path().join("ready");
    let stop = AtomicBool::new(false);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut command = Command::new("sh");
            command
                .args(["-c", "sleep 600 & printf ready > \"$1\"; wait", "probe"])
                .arg(&ready);
            sender.send(run_view(command, &stop)).unwrap();
        });
        let found = std::panic::catch_unwind(|| {
            load_grace::wait_for(
                load_grace::Patience::new(STORY_COMMAND_DEADLINE),
                STOP_POLL,
                || "view helper did not publish its marker".into(),
                || ready.exists().then_some(()),
            )
        });
        stop.store(true, Ordering::Release);
        // The assertion distinguishes cancellation from waiting out 45 s.
        let result = receiver.recv_timeout(VIEW_RECONCILE_TIMEOUT / 3).unwrap();
        assert!(matches!(result, Err(CaptureError::Cancelled)));
        assert!(found.is_ok());
    });
}

#[test]
fn requests_resolve_the_current_catalog_without_requiring_an_existing_journal() {
    let fixture = ServiceFixture::new();
    let store = SqliteStore::open(fixture.store().path()).unwrap();
    let project = ProjectId::new(fixture.project().get());
    store
        .write(|tx| tx.set_checkout_path(project, Some(fixture.cwd())))
        .unwrap();
    let views = registered_views(&store).unwrap();
    assert!(
        views.iter().any(|(id, _, directory)| *id == project
            && *directory == fixture.cwd().join(".storyhook/logs"))
    );
    assert!(!fixture.cwd().join(".storyhook/logs").exists());
    store
        .write(|tx| tx.set_checkout_path(project, Some(&fixture.cwd().join("absent"))))
        .unwrap();
    assert!(
        registered_views(&store)
            .unwrap()
            .iter()
            .all(|(id, _, _)| *id != project)
    );
    store
        .write(|tx| tx.set_checkout_path(project, None))
        .unwrap();
    assert!(
        registered_views(&store)
            .unwrap()
            .iter()
            .all(|(id, _, _)| *id != project)
    );
}

#[test]
fn a_phase_request_activates_a_missing_journal_and_disabled_mirroring_does_nothing() {
    let fixture = ServiceFixture::new();
    let store = SqliteStore::open(fixture.store().path()).unwrap();
    let project = ProjectId::new(fixture.project().get());
    store
        .write(|tx| tx.set_checkout_path(project, Some(fixture.cwd())))
        .unwrap();
    let requests = Requests::default();
    let env = Environment::at(fixture.cwd());
    let stop = AtomicBool::new(false);
    requests.request(project);
    poll_with(&store, &env, &stop, &requests, |_, _, _, _| {
        panic!("disabled view")
    });
    let mut seen = Vec::new();
    poll_with(
        &store,
        &env.with_test_verifier_mirror(),
        &stop,
        &requests,
        |_, _, directory, stop| {
            seen.push(directory.to_path_buf());
            assert!(!directory.exists());
            // Demand sent while the external operation runs must survive it.
            requests.request(project);
            stop.store(true, Ordering::Release);
        },
    );
    assert_eq!(seen, vec![fixture.cwd().join(".storyhook/logs")]);
    assert_eq!(requests.take(), std::collections::BTreeSet::from([project]));
}

#[test]
fn a_failed_first_attempt_is_retried_without_a_journal_or_another_phase() {
    let fixture = ServiceFixture::new();
    let store = SqliteStore::open(fixture.store().path()).unwrap();
    let project = ProjectId::new(fixture.project().get());
    store
        .write(|tx| tx.set_checkout_path(project, Some(fixture.cwd())))
        .unwrap();
    let requests = Requests::default();
    requests.request(project);
    let env = Environment::at(fixture.cwd()).with_test_verifier_mirror();
    let stop = AtomicBool::new(false);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            poll_with(&store, &env, &stop, &requests, |_, _, directory, _| {
                assert!(!directory.exists());
                sender.send(()).unwrap();
            })
        });
        let patience = load_grace::graced_now(RECONCILE_INTERVAL * 3);
        let first = receiver.recv_timeout(patience);
        let second = receiver.recv_timeout(patience);
        stop.store(true, Ordering::Release);
        assert!(
            first.is_ok() && second.is_ok(),
            "activation was lost after a failed first attempt"
        );
    });
}
