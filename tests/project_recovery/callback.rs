use super::*;
use storyhook::cli::{Invocation, VerifierAction, parse_invocation};
use storyhook::daemon::verification::VerificationActivity;
use storyhook::invoke::dispatch;
use storyhook::output::render_response;
use storyhook::service::project_recovery::{RepairInput, RepairScope};

fn command(activity: &VerificationActivity, candidate: &VerificationCandidate) -> Invocation {
    Invocation::Verifier {
        action: VerifierAction::RepairAdmit {
            story_id: candidate.story_id.clone(),
            attempt_id: activity.active_for(candidate.project).unwrap().attempt_id,
            generation: candidate.verifying_generation.unwrap().get(),
            input: RepairInput {
                base: "a".repeat(40),
                head: "b".repeat(40),
                head_tree: "e".repeat(40),
                tree: "f".repeat(40),
            },
        },
    }
}

#[test]
fn private_callback_round_trips_into_one_durable_admission() {
    let f = fixture();
    let view = decision::ready(&f);
    let activity = VerificationActivity::new();
    let ctx = f.ctx().with_verification_activity(Some(&activity));
    ProjectRecoveryService::new(&ctx)
        .decide(
            &view.record.id,
            &decision::input(&view, RepairScope::SameStory),
        )
        .unwrap();
    StoryService::new(&ctx)
        .set_state("SH-1", "verifying", None, None, None)
        .unwrap();
    let candidate = VerificationQueue::new(f.store()).next().unwrap().unwrap();
    let _guard = activity.acquire(&candidate, ctx.now());
    let invocation = command(&activity, &candidate);
    let wire = serde_json::to_vec(&invocation).unwrap();
    let response = dispatch(&ctx, serde_json::from_slice(&wire).unwrap()).unwrap();
    let result: serde_json::Value =
        serde_json::from_str(&render_response(&response, true, false)).unwrap();
    assert_eq!(result["result"], "proceed");
    assert_eq!(result["recovery_id"], view.record.id);
    dispatch(&ctx, invocation).unwrap();
    assert_eq!(
        ProjectRecoveryService::new(&ctx)
            .show(&view.record.id)
            .unwrap()
            .state
            .attempts
            .len(),
        1
    );
}

#[test]
fn private_callback_requires_exact_live_owner_and_original_reservation() {
    let f = fixture();
    let candidate = submitted(&f, "callback authority");
    let activity = VerificationActivity::new();
    let ctx = f.ctx().with_verification_activity(Some(&activity));
    let guard = activity.acquire(&candidate, ctx.now());
    let valid = command(&activity, &candidate);
    for field in ["story_id", "attempt_id", "generation"] {
        let mut wire = serde_json::to_value(&valid).unwrap();
        wire["Verifier"]["action"]["RepairAdmit"][field] = if field == "generation" {
            serde_json::json!(1)
        } else {
            serde_json::json!("wrong")
        };
        let wrong = serde_json::from_value(wire).unwrap();
        assert!(dispatch(&ctx, wrong).is_err(), "must reject {field}");
    }
    dispatch(&ctx, valid.clone()).unwrap();
    StoryService::new(&ctx)
        .set_labels("SH-1", &["human-only".into()], &[])
        .unwrap();
    StoryService::new(&ctx)
        .set_labels("SH-1", &[], &["human-only".into()])
        .unwrap();
    assert!(
        dispatch(&ctx, valid.clone()).is_err(),
        "callback cannot inherit authority after reservation"
    );
    drop(guard);
    assert!(dispatch(&ctx, valid.clone()).is_err());
    assert!(
        dispatch(&f.ctx(), valid).is_err(),
        "no daemon runtime is not idle ownership"
    );
}

#[test]
fn private_callback_parser_requires_full_pins_and_positive_generation() {
    let good: Vec<String> = [
        "verifier",
        "repair-admit",
        "SH-1",
        "attempt",
        "12",
        &"a".repeat(40),
        &"b".repeat(40),
        &"c".repeat(40),
        &"d".repeat(40),
    ]
    .map(str::to_owned)
    .into();
    assert!(parse_invocation(&good).is_ok());
    for (index, bad) in [
        (4, "0"),
        (4, "-1"),
        (4, "bad"),
        (5, "main"),
        (6, "HEAD"),
        (7, ""),
        (8, "tree"),
    ] {
        let mut args = good.clone();
        args[index] = bad.into();
        assert!(parse_invocation(&args).is_err());
    }
    let mut extra = good.clone();
    extra.push("extra".into());
    assert!(parse_invocation(&extra).is_err());
    assert!(parse_invocation(&good[..8]).is_err());
}

#[test]
fn private_callback_cannot_continue_a_cancelled_owner() {
    use storyhook::service::verification_control::VerificationAction;
    let f = fixture();
    let candidate = submitted(&f, "cancelled callback");
    let activity = VerificationActivity::new();
    let ctx = f.ctx().with_verification_activity(Some(&activity));
    let _guard = activity.acquire(&candidate, ctx.now());
    let request = command(&activity, &candidate);
    activity
        .control(f.store(), f.project(), VerificationAction::Stop)
        .unwrap();
    assert!(dispatch(&ctx, request).is_err());
}
