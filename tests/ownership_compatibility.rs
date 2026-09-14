//! Every resource owner uses the same transaction boundary, including raw store callers.
use storyhook::domain::{StoryCleanupLease, TmuxCleanupTarget};
use storyhook::service::landing::{LandingAdmission, VerifiedSubmission};
use storyhook::service::{NewStoryInput, PrLinkService, StoryService, VerificationQueue};
use storyhook::store::{
    EngineReset, LandingIntent, ReadOps, Store, StoreError, StoryNo, StoryReset, WriteOps,
};
use storyhook_test_support::ServiceFixture;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Native,
    Card,
    Engine,
    Landing,
}
const KINDS: [Kind; 4] = [Kind::Native, Kind::Card, Kind::Engine, Kind::Landing];
struct Owners {
    native: String,
    card: StoryReset,
    engine: EngineReset,
    landing: LandingIntent,
}

fn setup(f: &ServiceFixture) -> Owners {
    f.link_origin("https://github.com/acme/widgets");
    let ctx = f.ctx();
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "One resource owner".into(),
            ..Default::default()
        })
        .unwrap();
    PrLinkService::new(&ctx)
        .link(&story.id, "https://github.com/acme/widgets/pull/1", true)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&story.id, "verifying", None, None, None)
        .unwrap();
    let queue = VerificationQueue::new(f.store());
    let candidate = queue.next().unwrap().unwrap();
    let LandingAdmission::Admitted(landing) = queue
        .begin_landing(
            &ctx,
            &candidate,
            &VerifiedSubmission {
                head: "a".repeat(40),
                tree: "b".repeat(40),
                gate: "true".into(),
            },
        )
        .unwrap()
    else {
        panic!("expected landing admission")
    };
    f.store()
        .write(|tx| tx.remove_landing_intent(&landing))
        .unwrap();
    let conn = rusqlite::Connection::open(f.store().path()).unwrap();
    conn.execute("INSERT INTO engine_runs(id,project_slug,scope_kind,lanes,agent,state,created_at,updated_at) VALUES ('owner-run',?1,'project',1,'codex','running','then','then')",[&candidate.project_slug]).unwrap();
    conn.execute("INSERT INTO engine_lanes(run_id,lane_index,state,story_id,last_observed_at) VALUES ('owner-run',0,'working',?1,'then')",[&story.id]).unwrap();
    let no = StoryNo::new(1);
    Owners {
        native:r#"{"operation":"native-token","lease":null,"force":false,"previous_awaiting":null,"detail":"retained"}"#.into(),
        card:StoryReset {project:f.project(),story:no,story_id:story.id.clone(),token:"card-token".into(),original_state:"verifying".into(),lanes:Vec::new(),resources:None,paths:Vec::new(),completed:false,failure:None},
        engine:EngineReset {project:f.project(),story:no,run_id:"owner-run".into(),lane_index:0,token:"engine-token".into(),restore_to:"todo".into(),failure:None,lease:StoryCleanupLease {version:1,project_slug:candidate.project_slug,story_id:story.id,repository_path:f.cwd().into(),worktree_path:f.cwd().join("work"),branch:"feature".into(),tmux:TmuxCleanupTarget {socket_path:f.cwd().join("socket")}}},
        landing,
    }
}

fn reserve(tx: &mut impl WriteOps, owners: &Owners, kind: Kind) -> Result<(), StoreError> {
    match kind {
        Kind::Native => {
            tx.put_legacy_story_reset(owners.card.project, owners.card.story, Some(&owners.native))
        }
        Kind::Card => tx.put_story_reset(&owners.card),
        Kind::Engine => tx.put_engine_reset(&owners.engine),
        Kind::Landing => tx.insert_landing_intent(&owners.landing),
    }
}
fn release(tx: &mut impl WriteOps, owners: &Owners, kind: Kind) -> Result<(), StoreError> {
    match kind {
        Kind::Native => tx.put_legacy_story_reset(owners.card.project, owners.card.story, None),
        Kind::Card => {
            let mut done = owners.card.clone();
            done.completed = true;
            tx.put_story_reset(&done)
        }
        Kind::Engine => tx.remove_engine_reset(&owners.engine),
        Kind::Landing => tx.remove_landing_intent(&owners.landing).map(|_| ()),
    }
}
fn present(tx: &impl ReadOps, owners: &Owners, kind: Kind) -> Result<bool, StoreError> {
    let (project, story) = (owners.card.project, owners.card.story);
    Ok(match kind {
        Kind::Native => tx.story_resets(project)?.contains_key(&story),
        Kind::Card => tx
            .story_reset(project, story)?
            .is_some_and(|r| !r.completed),
        Kind::Engine => tx.engine_reset(project, story)?.is_some(),
        Kind::Landing => tx.landing_intents()?.contains(&owners.landing),
    })
}

#[test]
fn all_ordered_pairs_refuse_conflict_and_preserve_owner_until_explicit_completion() {
    for first in KINDS {
        for second in KINDS {
            if first == second {
                continue;
            }
            let f = ServiceFixture::new();
            let owners = setup(&f);
            f.store().write(|tx| reserve(tx, &owners, first)).unwrap();
            assert!(
                f.store().write(|tx| reserve(tx, &owners, second)).is_err(),
                "{first:?} then {second:?}"
            );
            assert!(f.store().read(|tx| present(tx, &owners, first)).unwrap());
            assert!(!f.store().read(|tx| present(tx, &owners, second)).unwrap());
            f.store().write(|tx| release(tx, &owners, first)).unwrap();
            f.store().write(|tx| reserve(tx, &owners, second)).unwrap();
        }
    }
}

#[test]
fn racing_distinct_connections_admit_exactly_one_owner_for_each_pair() {
    for (index, first) in KINDS.iter().copied().enumerate() {
        for second in KINDS[index + 1..].iter().copied() {
            let f = ServiceFixture::new();
            let owners = setup(&f);
            let barrier = std::sync::Barrier::new(2);
            let answers = std::thread::scope(|scope| {
                let handles = [first, second]
                    .into_iter()
                    .map(|kind| {
                        let owners = &owners;
                        let barrier = &barrier;
                        let path = f.store().path();
                        scope.spawn(move || {
                            let store = storyhook::store::SqliteStore::open(path).unwrap();
                            barrier.wait();
                            store.write(|tx| reserve(tx, owners, kind)).is_ok()
                        })
                    })
                    .collect::<Vec<_>>();
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .collect::<Vec<_>>()
            });
            assert_eq!(
                answers.iter().filter(|ok| **ok).count(),
                1,
                "{first:?} vs {second:?}: {answers:?}"
            );
        }
    }
}

#[test]
fn unresolved_owners_survive_purge_project_deletion_and_checkout_transfer() {
    for kind in KINDS {
        let f = ServiceFixture::new();
        let owners = setup(&f);
        f.store().write(|tx| reserve(tx, &owners, kind)).unwrap();
        assert!(
            f.store()
                .write(|tx| tx.purge_story(f.project(), StoryNo::new(1)))
                .is_err(),
            "purge {kind:?}"
        );
        assert!(
            f.store()
                .write(|tx| tx.delete_project(f.project()))
                .is_err(),
            "delete {kind:?}"
        );
        assert!(
            f.store()
                .write(|tx| tx.set_checkout_path(f.project(), Some(&f.cwd().join("different"))))
                .is_err(),
            "transfer {kind:?}"
        );
        assert!(f.store().read(|tx| present(tx, &owners, kind)).unwrap());
    }
}

#[test]
fn attempting_delivery_coexists_with_each_resource_owner_and_releases_independently() {
    use storyhook::store::{BlockAction, DeliveryStatus};
    for kind in KINDS {
        for reserve_first in [false, true] {
            let f = ServiceFixture::new();
            let owners = setup(&f);
            if reserve_first {
                f.store().write(|tx| reserve(tx, &owners, kind)).unwrap();
            }
            f.store()
                .write(|tx| {
                    tx.enqueue_block_delivery(
                        f.project(),
                        owners.card.story,
                        BlockAction::Interrupt,
                    )
                })
                .unwrap();
            let mut delivery = f
                .store()
                .read(|tx| Ok(tx.block_deliveries(f.project())?.remove(0)))
                .unwrap();
            delivery.status = DeliveryStatus::Attempting;
            delivery.target = Some("existing-worker".into());
            assert!(
                f.store()
                    .write(|tx| tx.update_block_delivery(&delivery, DeliveryStatus::Pending))
                    .unwrap()
            );
            if !reserve_first {
                f.store().write(|tx| reserve(tx, &owners, kind)).unwrap();
            }
            assert!(f.store().read(|tx| present(tx, &owners, kind)).unwrap());
            if reserve_first {
                f.store().write(|tx| release(tx, &owners, kind)).unwrap();
            } else {
                delivery.status = DeliveryStatus::Uncertain;
                assert!(
                    f.store()
                        .write(|tx| tx.update_block_delivery(&delivery, DeliveryStatus::Attempting))
                        .unwrap()
                );
            }
            assert!(
                f.store()
                    .write(
                        |tx| tx.set_checkout_path(f.project(), Some(&f.cwd().join("still-pinned")))
                    )
                    .is_err(),
                "{kind:?} retains the other owner"
            );
            if reserve_first {
                delivery.status = DeliveryStatus::Delivered;
                assert!(
                    f.store()
                        .write(|tx| tx.update_block_delivery(&delivery, DeliveryStatus::Attempting))
                        .unwrap()
                );
            } else {
                f.store().write(|tx| release(tx, &owners, kind)).unwrap();
            }
            f.store()
                .write(|tx| tx.set_checkout_path(f.project(), Some(&f.cwd().join("released"))))
                .unwrap();
        }
    }
}
