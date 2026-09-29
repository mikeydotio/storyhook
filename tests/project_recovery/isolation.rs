//! One invalid recovery stops only the stories it names (SH-848).
//!
//! The fixture strands a decided recovery exactly as `story delete` did
//! before SH-848: the claim into SH-1 is retracted and SH-1 is purged under
//! the recovery, so its exact event references no longer resolve. Readers
//! that ask about another story must not fail; readers for a story the
//! record names, and the diagnostics that read every record, stay loud.
use super::*;
use storyhook::service::RelationService;
use storyhook::service::project_recovery::RecoveryView;

/// A decided recovery whose subject SH-1 was purged; SH-2 is its repair.
fn stranded(f: &ServiceFixture) -> RecoveryView {
    let view = resume::decided(f);
    RelationService::new(&f.ctx())
        .relate("SH-2", "blocks", "SH-1", true)
        .unwrap();
    f.store()
        .write(|tx| tx.purge_story(f.project(), StoryNo::new(1)).map(|_| ()))
        .unwrap();
    assert!(
        ProjectRecoveryService::new(&f.ctx())
            .show(&view.record.id)
            .is_err(),
        "the fixture must leave the recovery invalid"
    );
    view
}

#[test]
fn an_unrelated_story_still_enters_the_verification_queue() {
    let f = fixture();
    let view = stranded(&f);
    let candidate = submitted(&f, "unrelated submission");
    assert_eq!(candidate.story_id, "SH-3");
    assert!(
        VerificationQueue::new(f.store())
            .ordered_for(f.project())
            .unwrap()
            .iter()
            .any(|c| c.story_id == "SH-3")
    );
    // Repair show still reads every reference and still says so.
    assert!(
        ProjectRecoveryService::new(&f.ctx())
            .show(&view.record.id)
            .is_err()
    );
}
