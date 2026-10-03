use super::*;
use crate::store::{GateInputs, GateSubmission, GlobalSeq, ProjectId};

const AT: &str = "2026-10-03T00:00:00Z";

fn contrast() -> AttributionRecord {
    let component = FailureComponent {
        id: "case-1".into(),
        check: "queue::preserves_submission".into(),
        signature: "assertion: expected verifying, found in-progress".into(),
        requirement: "uncertain failures retain the submission".into(),
        log: "/tmp/original.log".into(),
        observed_cause: FailureCause::Unknown,
    };
    let mut record = AttributionRecord {
        version: 1,
        id: "diagnosis".into(),
        revision: 0,
        submission: GateSubmission {
            project: ProjectId::new(1),
            story_id: "SH-1".into(),
            generation: Some(GlobalSeq::new(2)),
            submitted_at: Some(AT.into()),
        },
        attempt: "attempt".into(),
        inputs: GateInputs {
            head: Some("1".repeat(40)),
            base: Some("2".repeat(40)),
            tree: Some("3".repeat(40)),
            ..Default::default()
        },
        created_at: AT.into(),
        components: vec![component.clone()],
        plans: vec![ContrastPlan {
            component: component.id.clone(),
            candidate_tree: "3".repeat(40),
            base: "2".repeat(40),
            control_tree: "4".repeat(40),
            detector: "assertion-content".into(),
            relation: DetectorRelation::Unchanged,
            argv: vec!["runner".into(), "--exact".into(), component.check.clone()],
        }],
        probes: vec![],
        assessments: vec![],
        diagnosis_ms: 40,
        held: true,
        retired: None,
    };
    for (i, side) in [
        ProbeSide::Candidate,
        ProbeSide::Control,
        ProbeSide::Control,
        ProbeSide::Candidate,
    ]
    .into_iter()
    .enumerate()
    {
        record.probes.push(DiagnosticProbe {
            id: format!("probe-{i}"),
            plan: 0,
            side,
            started_at: AT.into(),
            completed: Some(ProbeResult {
                tree: if side == ProbeSide::Candidate {
                    "3".repeat(40)
                } else {
                    "4".repeat(40)
                },
                detector: "assertion-content".into(),
                executions: 1,
                outcome: if side == ProbeSide::Candidate {
                    ProbeOutcome::Failed {
                        signature: component.signature.clone(),
                    }
                } else {
                    ProbeOutcome::Passed
                },
                environment: Some(ProbeEnvironment {
                    toolchain: "rust-1.90".into(),
                    fixtures: "fixture-digest".into(),
                    resource_policy: "policy-1".into(),
                    grant: format!("grant-{i}"),
                    supported: true,
                }),
                log: format!("/tmp/probe-{i}.log"),
                execution_id: format!("execution-{i}"),
                cleanup_complete: true,
                milliseconds: 10,
            }),
        });
    }
    record
}

fn cause(record: &AttributionRecord) -> FailureCause {
    classify(record, &record.components[0])
}

#[test]
fn reproducible_contrast_identifies_only_the_selected_candidate_failure() {
    let mut record = contrast();
    assert_eq!(cause(&record), FailureCause::CandidateCaused);
    let mut shared = record.components[0].clone();
    shared.id = "independent-failure".into();
    shared.signature = "unrelated assertion".into();
    record.components.push(shared.clone());
    assert_eq!(classify(&record, &shared), FailureCause::Unknown);
    assert_eq!(cause(&record), FailureCause::CandidateCaused);
}

#[test]
fn repeated_base_failure_is_shared_and_one_green_retry_proves_nothing() {
    let mut record = contrast();
    for probe in &mut record.probes {
        probe.completed.as_mut().unwrap().outcome = ProbeOutcome::Failed {
            signature: record.components[0].signature.clone(),
        };
    }
    assert_eq!(cause(&record), FailureCause::SharedProject);
    record.probes[1].completed.as_mut().unwrap().outcome = ProbeOutcome::Passed;
    assert_eq!(cause(&record), FailureCause::Unknown);
    record.probes.truncate(2);
    assert_eq!(cause(&record), FailureCause::Unknown);
}

#[test]
fn incomplete_or_incomparable_evidence_never_assigns_author_responsibility() {
    let mutations: &[fn(&mut AttributionRecord)] = &[
        |r| {
            r.probes.pop();
        },
        |r| r.probes[1].completed = None,
        |r| r.probes[0].completed.as_mut().unwrap().executions = 0,
        |r| r.probes[0].completed.as_mut().unwrap().executions = 2,
        |r| r.probes[0].completed.as_mut().unwrap().environment = None,
        |r| {
            r.probes[0]
                .completed
                .as_mut()
                .unwrap()
                .environment
                .as_mut()
                .unwrap()
                .supported = false
        },
        |r| {
            r.probes[0]
                .completed
                .as_mut()
                .unwrap()
                .environment
                .as_mut()
                .unwrap()
                .grant
                .clear()
        },
        |r| {
            r.probes[0]
                .completed
                .as_mut()
                .unwrap()
                .environment
                .as_mut()
                .unwrap()
                .toolchain = "other".into()
        },
        |r| {
            r.probes[0]
                .completed
                .as_mut()
                .unwrap()
                .environment
                .as_mut()
                .unwrap()
                .fixtures = "other".into()
        },
        |r| {
            r.probes[0]
                .completed
                .as_mut()
                .unwrap()
                .environment
                .as_mut()
                .unwrap()
                .resource_policy = "other".into()
        },
        |r| r.probes[0].completed.as_mut().unwrap().tree = "wrong-tree".into(),
        |r| r.probes[0].completed.as_mut().unwrap().detector = "disabled-assertion".into(),
        |r| r.probes[0].completed.as_mut().unwrap().log.clear(),
        |r| r.probes[0].completed.as_mut().unwrap().execution_id.clear(),
        |r| r.probes[0].completed.as_mut().unwrap().cleanup_complete = false,
        |r| {
            r.probes[0].completed.as_mut().unwrap().outcome = ProbeOutcome::Failed {
                signature: "different assertion".into(),
            }
        },
        |r| {
            r.probes[0].completed.as_mut().unwrap().outcome = ProbeOutcome::Unavailable {
                detail: "missing tool".into(),
            }
        },
        |r| r.probes[0].side = ProbeSide::Control,
        |r| r.probes[0].id = r.probes[1].id.clone(),
        |r| r.probes[0].completed.as_mut().unwrap().execution_id = "execution-1".into(),
        |r| r.plans[0].candidate_tree = "other-tree".into(),
        |r| r.plans[0].base = "other-base".into(),
        |r| r.plans[0].argv.clear(),
        |r| r.plans[0].detector.clear(),
        |r| r.inputs.head = None,
        |r| r.inputs.base = None,
        |r| r.inputs.tree = None,
        |r| r.submission.generation = None,
        |r| r.held = false,
        |r| r.retired = Some("superseded".into()),
    ];
    for (index, change) in mutations.iter().enumerate() {
        let mut record = contrast();
        change(&mut record);
        assert_eq!(
            cause(&record),
            FailureCause::Unknown,
            "invalid evidence mutation {index}"
        );
    }
}

#[test]
fn changed_detectors_need_a_retained_control_patch() {
    for relation in [
        DetectorRelation::Transplant {
            patch: "retained-transplant-digest".into(),
        },
        DetectorRelation::Ablation {
            patch: "retained-ablation-digest".into(),
        },
    ] {
        let mut record = contrast();
        record.plans[0].relation = relation;
        assert_eq!(cause(&record), FailureCause::CandidateCaused);
    }
    for relation in [
        DetectorRelation::Transplant {
            patch: String::new(),
        },
        DetectorRelation::Ablation {
            patch: String::new(),
        },
    ] {
        let mut record = contrast();
        record.plans[0].relation = relation;
        assert_eq!(cause(&record), FailureCause::Unknown);
    }
}

#[test]
fn assertions_and_extra_inconsistent_probes_cannot_override_evidence() {
    let mut record = contrast();
    record.probes.clear();
    record.components[0].observed_cause = FailureCause::CandidateCaused;
    assert_eq!(cause(&record), FailureCause::Unknown);
    record = contrast();
    let mut extra = record.probes[0].clone();
    extra.id = "extra".into();
    extra.completed.as_mut().unwrap().execution_id = "extra-execution".into();
    extra.completed.as_mut().unwrap().outcome = ProbeOutcome::Passed;
    record.probes.push(extra);
    assert_eq!(cause(&record), FailureCause::Unknown);
}

#[test]
fn historical_categories_are_retained_as_observations_with_their_uncertainty() {
    let audit: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/verification-causes.json"
    ))
    .unwrap();
    let cases = audit["returns"].as_array().unwrap();
    assert_eq!(cases.len(), 30);
    for case in cases {
        assert!(case["confidence"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(
            case.get("cause").is_none(),
            "a historical category is not a causal finding"
        );
    }
    assert!(
        cases[0]["confidence"]
            .as_str()
            .unwrap()
            .contains("Probable")
    );
    assert!(
        cases[1]["confidence"]
            .as_str()
            .unwrap()
            .contains("Probable")
    );
    let mut record = contrast();
    record.probes.clear();
    record.components[0].signature = cases[5]["observation"].as_str().unwrap().into();
    assert_eq!(
        cause(&record),
        FailureCause::Unknown,
        "even a confirmed historical repair needs this submission's evidence"
    );
}
