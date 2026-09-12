//! Complete service mutations produce durable effective-block edges.
use std::path::Path;
use storyhook::service::{NewStoryInput, RelationService, StoryService};
use storyhook::store::{BlockAction, BlockDelivery, ReadOps, Store};
use storyhook_test_support::ServiceFixture;

fn story(f: &ServiceFixture, title: &str) -> String {
    StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
        .id
}
fn deliveries(f: &ServiceFixture) -> Vec<BlockDelivery> {
    f.store()
        .read(|tx| tx.block_deliveries(f.project()))
        .unwrap()
}
fn actions(f: &ServiceFixture) -> Vec<BlockAction> {
    deliveries(f).into_iter().map(|d| d.action).collect()
}
#[test]
fn rapid_prose_block_and_unblock_preserve_both_edges() {
    let f = ServiceFixture::new();
    let id = story(&f, "Rapid hold");
    let ctx = f.ctx();
    let svc = StoryService::new(&ctx);
    svc.set_state(&id, "in-progress", None, None, None).unwrap();
    svc.set_awaiting(&id, "repair").unwrap();
    svc.clear_awaiting(&id).unwrap();
    assert_eq!(actions(&f), [BlockAction::Interrupt, BlockAction::Resume]);
    assert!(deliveries(&f)[0].id < deliveries(&f)[1].id);
}
#[test]
fn duplicate_and_partial_unblocks_do_not_resume() {
    let f = ServiceFixture::new();
    let id = story(&f, "Held");
    let a = story(&f, "A");
    let b = story(&f, "B");
    let ctx = f.ctx();
    let svc = StoryService::new(&ctx);
    let relations = RelationService::new(&ctx);
    svc.set_state(&id, "in-progress", None, None, None).unwrap();
    relations
        .block_on(&id, &[a.clone(), b.clone()], Some("repair"))
        .unwrap();
    relations
        .block_on(&id, std::slice::from_ref(&a), None)
        .unwrap();
    svc.set_awaiting(&id, "independent hold").unwrap();
    relations.unblock_from(&id, &[a, b]).unwrap();
    assert_eq!(actions(&f), [BlockAction::Interrupt]);
    svc.clear_awaiting(&id).unwrap();
    assert_eq!(actions(&f), [BlockAction::Interrupt, BlockAction::Resume]);
}
#[test]
fn reopened_dependency_interrupts_its_active_dependent() {
    let f = ServiceFixture::new();
    let id = story(&f, "Dependent");
    let blocker = story(&f, "Closed dependency");
    let ctx = f.ctx();
    let svc = StoryService::new(&ctx);
    svc.set_state(&blocker, "done", None, None, None).unwrap();
    RelationService::new(&ctx)
        .relate(&id, "blocked-by", &blocker, false)
        .unwrap();
    svc.set_state(&id, "in-progress", None, None, None).unwrap();
    assert!(deliveries(&f).is_empty());
    svc.reopen(&blocker).unwrap();
    assert_eq!(actions(&f), [BlockAction::Interrupt]);
}
#[test]
fn failed_relation_batch_leaves_no_delivery_or_partial_hold() {
    let f = ServiceFixture::new();
    let id = story(&f, "Rollback");
    let blocker = story(&f, "Dependency");
    assert!(
        RelationService::new(&f.ctx())
            .block_on(&id, &[blocker, "SH-99999".into()], None)
            .is_err()
    );
    assert!(deliveries(&f).is_empty());
}
#[test]
fn clearing_into_todo_and_closing_do_not_send_resume() {
    for close in [false, true] {
        let f = ServiceFixture::new();
        let id = story(&f, "Parked");
        let ctx = f.ctx();
        let svc = StoryService::new(&ctx);
        svc.set_awaiting(&id, "hold").unwrap();
        if close {
            svc.set_state(&id, "done", None, None, None).unwrap();
        } else {
            svc.clear_awaiting(&id).unwrap();
        }
        assert_eq!(actions(&f), [BlockAction::Interrupt]);
    }
}

#[test]
fn blocked_state_and_dependency_closure_use_the_same_edges() {
    let f = ServiceFixture::new();
    let id = story(&f, "State hold");
    let ctx = f.ctx();
    let svc = StoryService::new(&ctx);
    svc.set_state(&id, "blocked", None, None, None).unwrap();
    svc.set_state(&id, "in-progress", None, None, None).unwrap();
    assert_eq!(actions(&f), [BlockAction::Interrupt, BlockAction::Resume]);
    let blocker = story(&f, "Dependency");
    RelationService::new(&ctx)
        .block_on(&id, std::slice::from_ref(&blocker), None)
        .unwrap();
    svc.set_state(&blocker, "done", None, None, None).unwrap();
    assert_eq!(
        actions(&f),
        [
            BlockAction::Interrupt,
            BlockAction::Resume,
            BlockAction::Interrupt,
            BlockAction::Resume
        ]
    );
}

#[test]
fn failed_and_missing_agent_deliveries_are_recorded_without_replay_or_resume() {
    use storyhook::store::{DeliveryStatus, WriteOps};
    for (reply, expected) in [
        (
            r#"{"ok":false,"reason":"pane-unavailable","display":"no agent reached"}"#,
            DeliveryStatus::Unreached,
        ),
        (
            r#"{"ok":false,"reason":"pane-provider-unknown","display":"unmarked pane"}"#,
            DeliveryStatus::Unreached,
        ),
        (
            r#"{"ok":false,"reason":"interruption-failed","display":"cleanup unconfirmed"}"#,
            DeliveryStatus::Uncertain,
        ),
        ("not json", DeliveryStatus::Uncertain),
    ] {
        let f = ServiceFixture::new();
        f.store()
            .write(|tx| tx.set_checkout_path(f.project(), Some(f.cwd())))
            .unwrap();
        let script = f.cwd().join("notify.sh");
        std::fs::write(&script, format!("cat <<'REPLY'\n{reply}\nREPLY\n")).unwrap();
        let id = story(&f, "Unreached resume");
        let ctx = f.ctx();
        let svc = StoryService::new(&ctx);
        svc.set_state(&id, "in-progress", None, None, None).unwrap();
        svc.set_awaiting(&id, "hold").unwrap();
        svc.clear_awaiting(&id).unwrap();
        assert!(
            storyhook::daemon::block_delivery::process_one(f.store(), f.env(), Some(&script))
                .unwrap()
        );
        assert_eq!(deliveries(&f)[0].status, expected);
        // Resume must refuse before invoking any helper if the interrupt had no target.
        assert!(
            storyhook::daemon::block_delivery::process_one(
                f.store(),
                f.env(),
                Some(Path::new("/unused"))
            )
            .unwrap()
        );
        assert_eq!(deliveries(&f)[1].status, DeliveryStatus::Unreached);
        assert!(
            !storyhook::daemon::block_delivery::process_one(f.store(), f.env(), Some(&script))
                .unwrap()
        );
    }
}

#[test]
fn delivery_reports_missing_checkout_without_undoing_block() {
    use storyhook::store::WriteOps;
    let f = ServiceFixture::new();
    f.store()
        .write(|tx| tx.set_checkout_path(f.project(), None))
        .unwrap();
    let id = story(&f, "Unreached agent");
    StoryService::new(&f.ctx())
        .set_awaiting(&id, "repair")
        .unwrap();
    assert!(
        storyhook::daemon::block_delivery::process_one(
            f.store(),
            f.env(),
            Some(std::path::Path::new("/unused"))
        )
        .unwrap()
    );
    let rows = deliveries(&f);
    assert_eq!(rows[0].status, storyhook::store::DeliveryStatus::Unreached);
    assert!(rows[0].detail.contains("no agent reached"));
    assert!(
        !storyhook::daemon::block_delivery::process_one(
            f.store(),
            f.env(),
            Some(std::path::Path::new("/unused"))
        )
        .unwrap()
    );
    assert!(
        StoryService::new(&f.ctx())
            .clear_awaiting(&id)
            .unwrap()
            .comments
            .iter()
            .any(|c| c.text.contains("no agent reached"))
    );
}

#[test]
fn recovery_records_uncertain_delivery_and_never_replays_it() {
    use storyhook::store::{DeliveryStatus, WriteOps};
    let f = ServiceFixture::new();
    let id = story(&f, "Interrupted delivery");
    StoryService::new(&f.ctx())
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    StoryService::new(&f.ctx())
        .set_awaiting(&id, "repair")
        .unwrap();
    let mut delivery = deliveries(&f).remove(0);
    delivery.status = DeliveryStatus::Attempting;
    f.store()
        .write(|tx| tx.update_block_delivery(&delivery, DeliveryStatus::Pending))
        .unwrap();
    storyhook::daemon::block_delivery::recover(f.store(), f.env()).unwrap();
    assert_eq!(deliveries(&f)[0].status, DeliveryStatus::Uncertain);
    storyhook::daemon::block_delivery::recover(f.store(), f.env()).unwrap();
    assert!(
        !storyhook::daemon::block_delivery::process_one(
            f.store(),
            f.env(),
            Some(std::path::Path::new("/unused"))
        )
        .unwrap()
    );
}

#[test]
fn delivered_interrupt_binds_the_exact_resume_prompt_to_its_target() {
    use storyhook::store::{DeliveryStatus, WriteOps};
    let f = ServiceFixture::new();
    let id = story(&f, "Resume exact session");
    f.store()
        .write(|tx| tx.set_checkout_path(f.project(), Some(f.cwd())))
        .unwrap();
    let script = f.cwd().join("provider.sh");
    std::fs::write(
        &script,
        r#"if [ "$5" = --interrupt ]; then
printf '{"ok":true,"target":"session-token","display":"interrupted"}'
else
printf '%s' "$5" > prompt
[ "$6" = --expected-target ] && [ "$7" = session-token ] || exit 22
printf '{"ok":true,"display":"prompt delivered"}'
fi
"#,
    )
    .unwrap();
    let ctx = f.ctx();
    let svc = StoryService::new(&ctx);
    svc.set_state(&id, "in-progress", None, None, None).unwrap();
    svc.set_awaiting(&id, "repair").unwrap();
    svc.clear_awaiting(&id).unwrap();
    for _ in 0..2 {
        assert!(
            storyhook::daemon::block_delivery::process_one(f.store(), f.env(), Some(&script))
                .unwrap()
        );
    }
    assert_eq!(
        std::fs::read_to_string(f.cwd().join("prompt")).unwrap(),
        storyhook::service::block_delivery::UNBLOCK_PROMPT
    );
    assert!(
        deliveries(&f)
            .iter()
            .all(|d| d.status == DeliveryStatus::Delivered)
    );
}

#[test]
fn cli_block_and_unblock_reach_the_daemon_delivery_worker() {
    use std::time::{Duration, Instant};
    use storyhook_test_support::TestEnv;
    let env = TestEnv::isolated();
    struct Stop<'a>(&'a TestEnv);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.stop_daemon();
        }
    }
    let _stop = Stop(&env);
    let script = env.home().join("notify.sh");
    std::fs::write(
        &script,
        r#"DISPATCH_PROTOCOL=5
if [ "$5" = --interrupt ]; then
  printf sent > native-interrupt
  printf '{"ok":true,"target":"daemon-session","display":"native interrupt acknowledged"}'
else
  [ "$6" = --expected-target ] && [ "$7" = daemon-session ] || exit 22
  printf '%s' "$5" > resume-prompt
  printf '{"ok":true,"display":"resume acknowledged"}'
fi
"#,
    )
    .unwrap();
    env.story(env.home())
        .args(["daemon", "start"])
        .env("STORYHOOK_DISPATCH_SCRIPT", &script)
        .assert()
        .success();
    let p = env
        .project()
        .prefix("BLK")
        .seed_story("Daemon delivery")
        .build();
    p.story()
        .args(["move", "BLK-1", "in-progress"])
        .assert()
        .success();
    p.story()
        .args(["block", "BLK-1", "temporary repair"])
        .assert()
        .success();
    p.story().args(["unblock", "BLK-1"]).assert().success();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !p.path().join("resume-prompt").exists() {
        assert!(
            Instant::now() < deadline,
            "daemon never delivered the queued effects"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(p.path().join("native-interrupt").exists());
    assert_eq!(
        std::fs::read_to_string(p.path().join("resume-prompt")).unwrap(),
        storyhook::service::block_delivery::UNBLOCK_PROMPT
    );
}

/// A daemon start with nothing interrupted must not open a write transaction.
///
/// Every store fault point fires inside every commit, an empty transaction
/// included, so a housekeeping write on a fresh daemon kills any armed daemon
/// before it accepts its first connection. That is how SH-690 turned `dev` red
/// (SH-693): the crash suite arms `before_commit` for the client's one command,
/// and `recover()` reached the point first, on its own. Arming the same point
/// here, in process and on this thread, makes the write observable without a
/// daemon: a transaction that should not exist surfaces as the injected error,
/// and `Ok` proves there was none.
///
/// Two stores, because "no rows at all" and "rows, none of them interrupted"
/// are different gates and a fix that only checks for an empty table passes
/// the first and fails the second.
#[test]
fn recovery_with_nothing_interrupted_opens_no_write_transaction() {
    use storyhook::store::FaultPoint;
    use storyhook::store::fault::{FaultAction, arm};
    for seeded in [false, true] {
        let f = ServiceFixture::new();
        if seeded {
            let id = story(&f, "Settled hold");
            StoryService::new(&f.ctx())
                .set_awaiting(&id, "repair")
                .unwrap();
            assert_eq!(
                deliveries(&f)[0].status,
                storyhook::store::DeliveryStatus::Pending,
                "the seeded row must be real work for the delivery pass, and not for recovery"
            );
        }
        let outcome = {
            let _fault = arm(
                FaultPoint::BeforeCommit,
                FaultAction::Fail("an idle recovery pass opened a write transaction".into()),
            );
            storyhook::daemon::block_delivery::recover(f.store(), f.env())
        };
        assert!(
            outcome.is_ok(),
            "seeded={seeded}: recovery with no interrupted delivery must not open a write \
             transaction — every store fault point fires inside every commit, so this write \
             kills an armed daemon during its own start-up (SH-693): {outcome:?}"
        );
    }
}

/// The steady-state half of the same promise: an idle delivery pass is a read.
///
/// `process_one` runs on the daemon's first pass, right after recovery, and
/// then once per [`storyhook::daemon::block_delivery::IDLE_POLL`] for the
/// daemon's life. A write transaction on every one of those passes holds
/// `BEGIN IMMEDIATE` against every client once a second for nothing, and fires
/// every armed fault point on a daemon that has been given no work.
#[test]
fn an_idle_delivery_pass_opens_no_write_transaction() {
    use storyhook::store::fault::{FaultAction, arm};
    use storyhook::store::{DeliveryStatus, FaultPoint, WriteOps};
    let f = ServiceFixture::new();
    f.store()
        .write(|tx| tx.set_checkout_path(f.project(), None))
        .unwrap();
    let id = story(&f, "Settled");
    StoryService::new(&f.ctx())
        .set_awaiting(&id, "repair")
        .unwrap();
    // Drain the one pending row the ordinary way, so the store holds a
    // delivery that is real but is no longer work.
    assert!(
        storyhook::daemon::block_delivery::process_one(
            f.store(),
            f.env(),
            Some(Path::new("/unused"))
        )
        .unwrap()
    );
    assert_eq!(deliveries(&f)[0].status, DeliveryStatus::Unreached);

    let outcome = {
        let _fault = arm(
            FaultPoint::BeforeCommit,
            FaultAction::Fail("an idle delivery pass opened a write transaction".into()),
        );
        storyhook::daemon::block_delivery::process_one(
            f.store(),
            f.env(),
            Some(Path::new("/unused")),
        )
    };
    assert!(
        matches!(outcome, Ok(false)),
        "a delivery pass with nothing pending must be a read, not a write (SH-693): {outcome:?}"
    );
}

/// The other half of the gate: when a delivery WAS interrupted, recovery must
/// still take the write path.
///
/// Pinned so the two tests above cannot be satisfied by never writing at all.
/// Under the armed fault the repair fails at its commit and rolls back, which
/// is the proof a transaction was opened; disarmed, the same call lands.
#[test]
fn recovery_still_writes_when_a_delivery_was_interrupted() {
    use storyhook::store::fault::{FaultAction, arm};
    use storyhook::store::{DeliveryStatus, FaultPoint, WriteOps};
    let f = ServiceFixture::new();
    let id = story(&f, "Interrupted delivery");
    StoryService::new(&f.ctx())
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    StoryService::new(&f.ctx())
        .set_awaiting(&id, "repair")
        .unwrap();
    let mut delivery = deliveries(&f).remove(0);
    delivery.status = DeliveryStatus::Attempting;
    f.store()
        .write(|tx| tx.update_block_delivery(&delivery, DeliveryStatus::Pending))
        .unwrap();

    let outcome = {
        let _fault = arm(
            FaultPoint::BeforeCommit,
            FaultAction::Fail("interrupted".into()),
        );
        storyhook::daemon::block_delivery::recover(f.store(), f.env())
    };
    let error = outcome.expect_err(
        "an interrupted delivery is repaired inside a write transaction, which the armed \
         fault must interrupt — an Ok here means recovery no longer writes at all",
    );
    assert!(
        error.to_string().contains("interrupted"),
        "the failure must be the injected one: {error}"
    );
    assert_eq!(
        deliveries(&f)[0].status,
        DeliveryStatus::Attempting,
        "a repair that failed at its commit must roll back whole"
    );

    storyhook::daemon::block_delivery::recover(f.store(), f.env()).unwrap();
    assert_eq!(deliveries(&f)[0].status, DeliveryStatus::Uncertain);
}
