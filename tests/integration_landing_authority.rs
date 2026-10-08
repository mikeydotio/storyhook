//! SH-871: a managed certificate cannot fall back to ordinary PR authority.
use serde::Deserialize;
use serde_json::json;
use storyhook::daemon::verification::{
    LandingOutcome, ShellVerificationActuator, VerificationActuator,
};
use storyhook::domain::landing::{
    IntegrationAuthority, IntegrationLanding, LandingAuthority, SkippedPolicy, SkippedSubmission,
    VerifiedSubmission,
};
use storyhook::service::landing::LandingAdmission;
use storyhook::service::{NewStoryInput, PrLinkService, StoryService, VerificationQueue};
use storyhook::store::{LandingIntent, ReadOps, Store, WriteOps};
use storyhook_test_support::ServiceFixture;

const ORIGINAL_PR: &str = "https://github.com/acme/widgets/pull/1";
const MANAGED_PR: &str = "https://github.com/acme/widgets/pull/2";

fn certificate() -> VerifiedSubmission {
    VerifiedSubmission {
        head: "a".repeat(40),
        tree: "b".repeat(40),
        gate: "make test".into(),
    }
}

fn authority() -> LandingAuthority {
    LandingAuthority::Integration(IntegrationAuthority {
        integration: IntegrationLanding {
            version: 1,
            owner: "d49b082f-d032-40be-b885-5b8cbe6dd099".into(),
            epoch: 3,
            attempt: "a5e4b5a3-587d-4226-911e-5c845235676c".into(),
            pull_request: MANAGED_PR.into(),
            original_head: "c".repeat(40),
            pinned_base: "d".repeat(40),
            base: "e".repeat(40),
            certification: certificate(),
        },
    })
}

// Exact pre-integration decoder shape. Both legacy payloads are strict.
#[derive(Deserialize)]
#[serde(untagged)]
enum LegacyAuthority {
    Certified(VerifiedSubmission),
    Skipped(SkippedSubmission),
}

#[test]
fn sh871_managed_landing_envelope_is_rejected_by_legacy_authority() {
    let value = authority();
    value.validate().unwrap();
    let bytes = serde_json::to_string(&value).unwrap();
    assert!(serde_json::from_str::<LegacyAuthority>(&bytes).is_err());
    assert_eq!(
        serde_json::from_str::<LandingAuthority>(&bytes).unwrap(),
        value
    );
    assert_eq!(value.head(), certificate().head);
    assert_eq!(value.tree(), certificate().tree);
    assert_eq!(value.certified(), Some(&certificate()));
    assert_eq!(value.integration().unwrap().pull_request, MANAGED_PR);
}

#[test]
fn sh871_managed_format_preserves_exact_legacy_payloads() {
    let certified = certificate();
    let skipped = SkippedSubmission {
        mode: SkippedPolicy::VerificationSkipped,
        head: "a".repeat(40),
        tree: "b".repeat(40),
        attempt: "legacy-attempt".into(),
    };
    for bytes in [
        serde_json::to_string(&certified).unwrap(),
        serde_json::to_string(&skipped).unwrap(),
    ] {
        let current: LandingAuthority = serde_json::from_str(&bytes).unwrap();
        assert!(current.integration().is_none());
        assert_eq!(serde_json::to_string(&current).unwrap(), bytes);
        match serde_json::from_str::<LegacyAuthority>(&bytes).unwrap() {
            LegacyAuthority::Certified(value) => assert_eq!(value, certified),
            LegacyAuthority::Skipped(value) => assert_eq!(value, skipped),
        }
    }
}

#[test]
fn sh871_managed_envelope_refuses_ambiguity_and_invalid_binding_shapes() {
    let value = serde_json::to_value(authority()).unwrap();
    for (field, bad) in [
        ("version", json!(2)),
        ("epoch", json!(0)),
        ("owner", json!("../owner")),
        ("attempt", json!("attempt;command")),
        ("pull_request", json!("not a pull request")),
        ("original_head", json!("short")),
        ("pinned_base", json!("x".repeat(40))),
        ("base", json!("")),
    ] {
        let mut invalid = value.clone();
        invalid["integration"][field] = bad;
        let parsed: LandingAuthority = serde_json::from_value(invalid).unwrap();
        assert!(parsed.validate().is_err(), "accepted {field}");
    }
    for level in ["outer", "binding", "certificate"] {
        let mut invalid = value.clone();
        let target = match level {
            "outer" => &mut invalid,
            "binding" => &mut invalid["integration"],
            _ => &mut invalid["integration"]["certification"],
        };
        target["unexpected"] = json!(true);
        assert!(serde_json::from_value::<LandingAuthority>(invalid).is_err());
    }
    let mut ambiguous = value;
    ambiguous.as_object_mut().unwrap().extend(
        serde_json::to_value(certificate())
            .unwrap()
            .as_object()
            .unwrap()
            .clone(),
    );
    assert!(serde_json::from_value::<LandingAuthority>(ambiguous).is_err());
}

fn admitted(f: &ServiceFixture) -> LandingIntent {
    f.github_checkout("https://github.com/acme/widgets");
    let ctx = f.ctx();
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "original submission".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&ctx)
        .link(&id, ORIGINAL_PR, true)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    let queue = VerificationQueue::new(f.store());
    let candidate = queue.next().unwrap().unwrap();
    let LandingAdmission::Admitted(intent) = queue
        .begin_landing(&ctx, &candidate, &certificate())
        .unwrap()
    else {
        panic!("expected original intent");
    };
    intent
}

#[test]
fn sh871_managed_target_preserves_original_link_but_store_requires_exact_owner() {
    let f = ServiceFixture::new();
    let original = admitted(&f);
    let mut managed = original.clone();
    managed.certification = authority();
    assert_eq!(managed.pull_request, ORIGINAL_PR);
    assert_eq!(managed.landing_pull_request(), MANAGED_PR);
    assert_eq!(
        managed.landing_attempt(),
        authority().integration().unwrap().attempt
    );
    let error = f
        .store()
        .write(|tx| {
            tx.remove_landing_intent(&original)?;
            tx.insert_landing_intent(&managed)
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("integration owner missing"), "{error}");
    assert_eq!(
        f.store().read(|tx| tx.landing_intents()).unwrap(),
        [original]
    );
}

#[test]
fn sh871_managed_landing_cannot_complete_original_pr_through_ordinary_controller() {
    let f = ServiceFixture::new();
    let original = admitted(&f);
    let events = f
        .store()
        .read(|tx| tx.events_for(original.project, original.story))
        .unwrap();
    let links = f
        .store()
        .read(|tx| tx.open_pr_links_for_story(original.project, original.story))
        .unwrap();
    let mut managed = original.clone();
    managed.certification = authority();
    let error = VerificationQueue::new(f.store())
        .complete_landing(
            &f.ctx(),
            &managed,
            "managed PR merged, original remains open",
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("dedicated owner controller"), "{error}");
    assert_eq!(
        f.store()
            .read(|tx| tx.events_for(original.project, original.story))
            .unwrap(),
        events
    );
    assert_eq!(
        f.store()
            .read(|tx| tx.open_pr_links_for_story(original.project, original.story))
            .unwrap(),
        links
    );
    assert_eq!(
        f.store().read(|tx| tx.landing_intents()).unwrap(),
        [original]
    );
}

#[test]
fn sh871_managed_landing_cannot_borrow_batch_identity() {
    let f = ServiceFixture::new();
    let original = admitted(&f);
    let mut mixed = original.clone();
    mixed.certification = authority();
    mixed.batch = Some(storyhook::store::BatchLanding {
        id: storyhook::store::BatchId::generate(),
        landing: "batch-attempt".into(),
        pull_request: "https://github.com/acme/widgets/pull/3".into(),
    });
    let error = f
        .store()
        .write(|tx| {
            tx.remove_landing_intent(&original)?;
            tx.insert_landing_intent(&mixed)
        })
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("cannot combine batch and integration"),
        "{error}"
    );
    assert_eq!(
        f.store().read(|tx| tx.landing_intents()).unwrap(),
        [original]
    );
}

#[test]
fn sh871_managed_landing_refuses_native_attempt_and_recovery_before_any_helper() {
    let f = ServiceFixture::new();
    let mut managed = admitted(&f);
    managed.certification = authority();
    let candidate = VerificationQueue::new(f.store())
        .ordered()
        .unwrap()
        .remove(0);
    let actuator = ShellVerificationActuator::new(f.env().clone());
    for outcome in [
        actuator.land(&candidate, &managed),
        actuator.recover_landing(&candidate, &managed),
    ] {
        let LandingOutcome::Uncertain { detail } = outcome else {
            panic!("ordinary native landing did not refuse managed authority");
        };
        assert!(detail.contains("dedicated owner controller"), "{detail}");
    }
}
