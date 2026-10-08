//! Shared recovery must carry the same native custody as a causal return.
use super::*;
mod recovery;

fn settled(
    evidence: &Evidence,
    f: &Fixture,
    mixed: bool,
) -> (SettledRustComparison, AttributionRecord) {
    settled_named(
        evidence,
        f,
        mixed,
        &evidence.candidate,
        "attempt",
        "attribution",
    )
}

fn settled_named(
    evidence: &Evidence,
    f: &Fixture,
    mixed: bool,
    candidate: &VerificationCandidate,
    attempt: &str,
    attribution: &str,
) -> (SettledRustComparison, AttributionRecord) {
    let mut native = f.comparison();
    for (index, side) in [
        ProbeSide::Candidate,
        ProbeSide::Control,
        ProbeSide::Control,
        ProbeSide::Candidate,
    ]
    .into_iter()
    .enumerate()
    {
        let id = if attempt == "attempt" {
            format!("probe-{index}")
        } else {
            format!("{attempt}-probe-{index}")
        };
        let journal = f.directory.path().join(format!("{id}.journal"));
        fs::write(&journal, "").unwrap();
        let output = f.directory.path().join(&id);
        let result = native
            .execute(
                side,
                &NativeProbeBinding {
                    project: "fixture",
                    attempt,
                    execution: &id,
                    generation: candidate.verifying_generation.unwrap().get(),
                    request: &id,
                    journal: &journal,
                    output: &output,
                    termination_grace: Duration::from_secs(1),
                },
            )
            .unwrap();
        assert_eq!(result.executions, 1);
        assert!(result.cleanup_complete);
    }
    let original = f
        .directory
        .path()
        .join(format!("{attempt}-original-gate.log"));
    let mut log = b"     Running tests/contract.rs (target/contract)\n".to_vec();
    log.extend(fs::read(Path::new(&native.observations[0].1.log).join("run.stdout")).unwrap());
    fs::write(&original, log).unwrap();
    let mut record =
        evidence.retain_named(&native, &original, mixed, candidate, attempt, attribution);
    let settled = native.settle().unwrap();
    record.diagnosis_ms = record.diagnosis_ms.max(settled.milliseconds());
    record.settlement = Some(DiagnosticSettlement {
        completed_at: record.created_at.clone(),
        milliseconds: settled.milliseconds(),
        detail: "native shared comparison settled".into(),
        cleanup_complete: true,
    });
    evidence.save(&mut record);
    (settled, record)
}

#[test]
fn native_shared_failure_grants_recovery_but_never_a_causal_return() {
    let mut f = Fixture::new(false);
    // The base already fails with 41. An equivalent Rust expression preserves
    // that exact failure while staying inside the native source intervention.
    f.base = f.git(&["rev-parse", "HEAD"]);
    f.write("src/lib.rs", "pub fn answer() -> u32 { 40 + 1 }\n");
    f.git(&["add", "src/lib.rs"]);
    f.git(&["commit", "-qm", "equivalent failing candidate"]);
    let evidence = Evidence::new();
    let (settled, record) = settled(&evidence, &f, true);
    assert_eq!(
        classify(&record, &record.components[0]),
        FailureCause::SharedProject
    );
    assert!(
        evidence
            .store
            .read(|tx| settled.prove(tx, &evidence.candidate, &record.id, "original"))
            .is_err()
    );
    let proof = evidence
        .store
        .read(|tx| settled.prove_shared(tx, &evidence.candidate, &record.id, "original"))
        .unwrap();
    assert!(
        evidence
            .store
            .read(|tx| proof.validate(tx, &evidence.candidate))
            .unwrap()
    );
    // Store permission can be revoked after proof minting, without changing
    // the result bytes. A recovery transaction must still refuse the proof.
    let aborted: Result<(), crate::store::StoreError> = evidence.store.write(|tx| {
        tx.put_verification_enabled(evidence.candidate.project, false)?;
        assert!(!proof.validate(tx, &evidence.candidate)?);
        Err(crate::store::StoreError::Validation(
            "rollback test revocation".into(),
        ))
    });
    assert!(aborted.is_err());
    assert!(
        evidence
            .store
            .read(|tx| proof.validate(tx, &evidence.candidate))
            .unwrap()
    );
    let before = evidence
        .store
        .read(|tx| tx.attributions(evidence.candidate.project))
        .unwrap();
    assert_eq!(
        before.iter().find(|r| r.id == record.id).unwrap(),
        &record,
        "proof minting must not retire mixed or shared holds"
    );
    let raw = f.directory.path().join("probe-0/run.stdout");
    fs::write(&raw, "substituted diagnostic output").unwrap();
    assert!(
        evidence
            .store
            .read(|tx| proof.validate(tx, &evidence.candidate))
            .is_err(),
        "recovery authority accepted changed retained raw evidence"
    );
}

#[test]
fn native_candidate_failure_cannot_grant_shared_recovery_authority() {
    let f = Fixture::new(false);
    let evidence = Evidence::new();
    let (settled, record) = settled(&evidence, &f, true);
    assert_eq!(
        classify(&record, &record.components[0]),
        FailureCause::CandidateCaused
    );
    assert!(
        evidence
            .store
            .read(|tx| settled.prove(tx, &evidence.candidate, &record.id, "original"))
            .is_ok()
    );
    assert!(
        evidence
            .store
            .read(|tx| settled.prove_shared(tx, &evidence.candidate, &record.id, "original"))
            .is_err(),
        "candidate-only regression acquired a shared repair owner"
    );
}
