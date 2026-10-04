//! SH-882: skipped tests cannot become certification, including after restart.

use serde_json::json;
use storyhook::domain::landing::VerifiedSubmission;
use storyhook::domain::landing::{LandingAuthority, SkippedPolicy, SkippedSubmission};

#[test]
fn legacy_certification_round_trips_without_changing_payload_bytes() {
    let legacy = VerifiedSubmission {
        head: "a".repeat(40),
        tree: "b".repeat(40),
        gate: "cargo test".into(),
    };
    let bytes = serde_json::to_string(&legacy).unwrap();
    let authority: LandingAuthority = serde_json::from_str(&bytes).unwrap();
    authority.validate().unwrap();
    assert_eq!(authority.certified(), Some(&legacy));
    assert_eq!(serde_json::to_string(&authority).unwrap(), bytes);
}

#[test]
fn skipped_authority_requires_exact_objects_and_an_admission_identity() {
    let valid = SkippedSubmission {
        mode: SkippedPolicy::VerificationSkipped,
        head: "a".repeat(40),
        tree: "b".repeat(64),
        attempt: "owning-attempt".into(),
    };
    valid.validate().unwrap();
    let authority = LandingAuthority::Skipped(valid.clone());
    let value = serde_json::to_value(&authority).unwrap();
    assert_eq!(
        serde_json::from_value::<LandingAuthority>(value.clone()).unwrap(),
        authority
    );
    assert!(serde_json::from_value::<VerifiedSubmission>(value).is_err());
    assert!(authority.certified().is_none());
    for invalid in ["", " ", "short", &"x".repeat(40)] {
        let mut head = valid.clone();
        head.head = invalid.into();
        assert!(head.validate().is_err());
        let mut tree = valid.clone();
        tree.tree = invalid.into();
        assert!(tree.validate().is_err());
    }
    for attempt in ["", " ", "one\ntwo", "../owner", "owner;command"] {
        let mut invalid = valid.clone();
        invalid.attempt = attempt.into();
        assert!(invalid.validate().is_err());
    }
    for invalid in [
        json!({"mode":"unknown", "head":valid.head, "tree":valid.tree, "attempt":valid.attempt}),
        json!({"mode":"verification-skipped", "head":valid.head, "tree":valid.tree}),
    ] {
        assert!(serde_json::from_value::<LandingAuthority>(invalid).is_err());
    }
}

#[test]
fn a_skipped_or_ambiguous_payload_cannot_decode_as_certification() {
    for extra in [
        json!({"mode":"verification-skipped", "attempt":"owner"}),
        json!({"mode":"unknown"}),
    ] {
        let mut value = json!({"head":"a".repeat(40), "tree":"b".repeat(40), "gate":"true"});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(serde_json::from_value::<VerifiedSubmission>(value).is_err());
    }
}
