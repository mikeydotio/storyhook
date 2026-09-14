//! Delivery ownership survives reset races and never transfers to a replacement.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use storyhook::service::reset::{ResetCaller, reset_story};
use storyhook::service::story_reset::StoryResetService;
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{DeliveryStatus, ReadOps, Store, StoryNo, WriteOps};
use storyhook_test_support::ServiceFixture;

fn fixture() -> ServiceFixture {
    let fixture = ServiceFixture::new();
    let repo = fixture.cwd().canonicalize().unwrap();
    let output = storyhook::env::git_env::command(&repo)
        .args(["init", "--initial-branch=main"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(&repo)))
        .unwrap();
    StoryService::new(&fixture.ctx().no_hooks(true))
        .create(&NewStoryInput {
            title: "Keep interruption with its original session".into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap();
    fixture
}

fn reset(fixture: &ServiceFixture, card: bool) -> Result<(), storyhook::error::AppError> {
    let ctx = fixture.ctx().no_hooks(true);
    if card {
        let service = StoryResetService::new(&ctx);
        let owner = service.reserve("SH-1", "SH-1")?;
        service.execute("SH-1", &owner.token, || Ok(()))?;
        Ok(())
    } else {
        reset_story(&ctx, "SH-1", false, &ResetCaller::default())
    }
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "delivery did not reach {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct Release(PathBuf);
impl Drop for Release {
    fn drop(&mut self) {
        std::fs::write(&self.0, "release").expect("release the fixture helper on every exit");
    }
}

#[test]
fn native_and_card_reset_wait_for_an_attempting_helper_before_replacing_its_session() {
    for card in [false, true] {
        let fixture = fixture();
        StoryService::new(&fixture.ctx())
            .set_awaiting("SH-1", "Stop the original session")
            .unwrap();
        let script = fixture.cwd().join("held-interrupt.sh");
        std::fs::write(
            &script,
            r#"touch entered
while [ ! -e released ]; do sleep 0.01; done
printf '{"ok":true,"target":"original-session","display":"original session stopped"}'
"#,
        )
        .unwrap();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                storyhook::daemon::block_delivery::process_one(
                    fixture.store(),
                    fixture.env(),
                    Some(&script),
                )
            });
            let release = Release(fixture.cwd().join("released"));
            wait_for(&fixture.cwd().join("entered"));
            let error = reset(&fixture, card).unwrap_err();
            assert!(
                error.to_string().contains("workspace is busy"),
                "card={card}: {error}"
            );
            fixture
                .store()
                .read(|tx| {
                    assert_eq!(
                        tx.story(fixture.project(), StoryNo::new(1))?.unwrap().state,
                        "in-progress"
                    );
                    assert_eq!(
                        tx.block_deliveries(fixture.project())?[0].status,
                        DeliveryStatus::Attempting
                    );
                    if card {
                        assert!(
                            !tx.story_reset(fixture.project(), StoryNo::new(1))?
                                .unwrap()
                                .completed
                        );
                    }
                    Ok(())
                })
                .unwrap();
            drop(release);
            assert!(worker.join().unwrap().unwrap());
        });
        reset(&fixture, card).unwrap();
        fixture
            .store()
            .read(|tx| {
                assert_eq!(
                    tx.story(fixture.project(), StoryNo::new(1))?.unwrap().state,
                    "todo"
                );
                let delivery = &tx.block_deliveries(fixture.project())?[0];
                assert_eq!(delivery.status, DeliveryStatus::Delivered);
                assert_eq!(delivery.target.as_deref(), Some("original-session"));
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn reset_retires_preclaim_effects_before_a_newly_blocked_replacement() {
    for card in [false, true] {
        let fixture = fixture();
        let ctx = fixture.ctx().no_hooks(true);
        let stories = StoryService::new(&ctx);
        stories.set_awaiting("SH-1", "Old session block").unwrap();
        let original = fixture
            .store()
            .read(|tx| tx.block_deliveries(fixture.project()))
            .unwrap()[0]
            .id;
        reset(&fixture, card).unwrap();
        let retired = fixture
            .store()
            .read(|tx| tx.block_deliveries(fixture.project()))
            .unwrap();
        assert!(
            retired
                .iter()
                .all(|delivery| delivery.status == DeliveryStatus::Superseded)
        );
        stories.clear_awaiting("SH-1").unwrap();
        stories
            .set_state("SH-1", "in-progress", None, None, None)
            .unwrap();
        stories
            .set_awaiting("SH-1", "Replacement session block")
            .unwrap();
        let script = fixture.cwd().join("new-interrupt.sh");
        std::fs::write(
            &script,
            r#"printf call >> replacement-calls
printf '{"ok":true,"target":"replacement-session","display":"replacement session stopped"}'
"#,
        )
        .unwrap();
        assert!(
            storyhook::daemon::block_delivery::process_one(
                fixture.store(),
                fixture.env(),
                Some(&script)
            )
            .unwrap()
        );
        assert!(
            !storyhook::daemon::block_delivery::process_one(
                fixture.store(),
                fixture.env(),
                Some(&script)
            )
            .unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(fixture.cwd().join("replacement-calls")).unwrap(),
            "call"
        );
        fixture
            .store()
            .read(|tx| {
                let deliveries = tx.block_deliveries(fixture.project())?;
                assert_eq!(
                    deliveries.iter().find(|d| d.id == original).unwrap().status,
                    DeliveryStatus::Superseded
                );
                let newest = deliveries.last().unwrap();
                assert!(newest.id > original);
                assert_eq!(newest.status, DeliveryStatus::Delivered);
                assert_eq!(newest.target.as_deref(), Some("replacement-session"));
                Ok(())
            })
            .unwrap();
    }
}
