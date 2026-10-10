//! Global ownership coordinates one native episode; each subject needs its own
//! live restoration and original-head admission before it can execute again.
use super::native::{self, Fault, Subject};
use super::{HostFaultEvidence, HostRestorationEvidence};
use crate::{
    domain::StoryEvent,
    error::AppError,
    service::{Ctx, VerificationCandidate, attribution::AttributionRecord},
    store::{
        ExpectedSeq, GlobalSeq, HostRecovery, ProjectId, ReadOps, Store, StoreError, StoryNo,
        WriteOps,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    subject: Subject,
    enrolled_at: String,
    fault_receipt: Value,
    restored: Option<Restored>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Restored {
    at: String,
    receipt: Value,
    /// None means native host restoration was proved but unrelated components
    /// still hold this attribution. No broad hold-clearing is permitted.
    released: Option<AttributionRecord>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u8,
    fault: Fault,
    started_at: String,
    updated_at: String,
    members: Vec<Member>,
}

/// A read-only projection, not reconstructed native recovery authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostRecoveryView {
    /// Durable identity shared across projects for exactly one native episode.
    pub id: String,
    /// Current compare-and-swap revision.
    pub revision: i64,
    /// Whether this episode still pauses new host admissions.
    pub active: bool,
    /// Original fault ownership time; retries never replace it.
    pub started_at: String,
    /// Number of original submissions retained by this owner.
    pub submissions: usize,
    /// Number eligible for a fresh gate on their original head.
    pub readmitted: usize,
}

/// The selected project may enroll or restore only its own native subject;
/// the episode owner and admission pause are shared across the entire store.
pub struct HostRecoveryService<'a, S: Store> {
    ctx: &'a Ctx<'a, S>,
}
impl<'a, S: Store> HostRecoveryService<'a, S> {
    /// Bind without querying a broker or changing runtime policy.
    pub fn new(ctx: &'a Ctx<'a, S>) -> Self {
        Self { ctx }
    }

    /// Retain one original submission using fresh, native fault-only authority.
    /// No repair process, test, installation or host mutation is dispatched.
    pub fn enroll(&self, proof: &HostFaultEvidence) -> Result<HostRecoveryView, AppError> {
        if proof.subject().candidate.project != self.ctx.project() {
            return Err(invalid("foreign enrollment project").into());
        }
        let now = self.ctx.now();
        self.ctx.write_stories(|tx| {
            proof.validate(tx)?;
            let subject = proof.subject();
            let key = fingerprint(&subject.request.fault)?;
            let existing = tx.host_recoveries()?.into_iter().find(|r| r.fault_key == key);
            let (mut record, mut state) = match existing {
                Some(record) => { let state = decode(&record)?; (record, state) },
                None => {
                    let state = State { version: 1, fault: subject.request.fault.clone(), started_at: subject.admitted_at.clone(), updated_at: now.clone(), members: Vec::new() };
                    (HostRecovery { id: uuid::Uuid::new_v4().to_string(), fault_key: key, revision: 0, active: true, state: Value::Null }, state)
                }
            };
            if let Some(member) = state.members.iter().find(|m| same_identity(&m.subject, subject)) {
                if !same_subject(&member.subject, subject) { return Err(invalid("original host subject changed before enrollment replay")); }
                proof.validate(tx)?;
                return Ok(view(&record, &state));
            }
            let fresh = state.members.is_empty();
            state.members.push(Member { subject: subject.clone(), enrolled_at: now.clone(), fault_receipt: proof.receipt().clone(), restored: None });
            state.updated_at = now.clone();
            if fresh {
                record.state = encode(&state)?;
                decode(&record)?;
                if !tx.insert_host_recovery(&record)? { return Err(invalid("host fault ownership changed")); }
            } else { save(tx, &mut record, &state)?; }
            comment(tx, self.ctx, subject, &now, format!("HOST RECOVERY {}: retain original generation and head for native pressure episode {}. One store-wide owner waits for fresh native restoration; no author repair or resubmission is assigned.", record.id, state.fault.sequence))?;
            proof.validate(tx)?;
            Ok(view(&record, &state))
        }).map_err(Into::into)
    }

    /// Fresh hysteresis and full native custody can end the global pause. Only
    /// the exact proved member may be readmitted; other members keep their hold.
    pub fn restore(
        &self,
        id: &str,
        expected: i64,
        proof: &HostRestorationEvidence,
    ) -> Result<Option<HostRecoveryView>, AppError> {
        if proof.subject().candidate.project != self.ctx.project() {
            return Err(invalid("foreign restoration project").into());
        }
        let now = self.ctx.now();
        self.ctx.write_stories(|tx| {
            proof.validate(tx)?;
            let mut record = tx.host_recoveries()?.into_iter().find(|r| r.id == id).ok_or_else(|| invalid("host owner missing"))?;
            let mut state = decode(&record)?;
            if record.revision != expected { return Ok(None); }
            let index = state.members.iter().position(|m| same_subject(&m.subject, proof.subject())).ok_or_else(|| invalid("restoration differs from the enrolled original subject"))?;
            if state.members[index].restored.is_some() { return Ok(Some(view(&record, &state))); }
            let original = &state.members[index].subject;
            // Do not use another project's native proof as authority for this
            // member, or erase mixed/unknown component holds.
            let released = (original.attribution.components.len() == 1).then(|| released_record(id, original)).transpose()?;
            if let Some(released) = &released {
                if !tx.update_attribution(released, original.attribution.revision)? { return Err(invalid("host attribution changed before release")); }
                comment(tx, self.ctx, original, &now, format!("HOST RECOVERY {id} READMISSION: native pressure restoration is proved for this original submission. The same generation and head now require a fresh current-base central gate; this receipt certifies no source tree."))?;
            }
            state.members[index].restored = Some(Restored { at: now.clone(), receipt: proof.receipt().clone(), released });
            state.updated_at = now.clone();
            record.active = false;
            save(tx, &mut record, &state)?;
            // The attribution changed in this transaction only. Preserve every
            // other fence and recheck live proof age after transaction work.
            proof.validate_after_release(tx)?;
            Ok(Some(view(&record, &state)))
        }).map_err(Into::into)
    }

    /// Show retained evidence without acquiring executable authority.
    pub fn list(&self) -> Result<Vec<HostRecoveryView>, AppError> {
        self.ctx
            .store()
            .read(|tx| {
                tx.host_recoveries()?
                    .iter()
                    .map(|r| decode(r).map(|s| view(r, &s)))
                    .collect()
            })
            .map_err(Into::into)
    }
}

pub(crate) fn blocks_admission(tx: &impl ReadOps) -> Result<bool, StoreError> {
    let mut active = false;
    for record in tx.host_recoveries()? {
        decode(&record)?;
        active |= record.active;
    }
    Ok(active)
}

pub(super) fn subject_restored(tx: &impl ReadOps, subject: &Subject) -> Result<bool, StoreError> {
    for record in tx.host_recoveries()? {
        let state = decode(&record)?;
        if state
            .members
            .iter()
            .any(|member| same_identity(&member.subject, subject) && member.restored.is_some())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Durable pin remains mandatory after the host owner becomes inactive.
pub(crate) fn expected_head(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    generation: Option<GlobalSeq>,
) -> Result<Option<String>, StoreError> {
    let mut head = None;
    for record in tx.host_recoveries()? {
        let state = decode(&record)?;
        for member in &state.members {
            if member.subject.candidate.project != project
                || member.subject.attribution.submission.story_number() != Some(story)
                || member.subject.candidate.verifying_generation != generation
                || member
                    .restored
                    .as_ref()
                    .and_then(|r| r.released.as_ref())
                    .is_none()
            {
                continue;
            }
            let pinned = member
                .subject
                .attribution
                .inputs
                .head
                .as_ref()
                .ok_or_else(|| invalid("readmission has no original head"))?;
            if head.as_ref().is_some_and(|previous| previous != pinned) {
                return Err(invalid("host owners disagree about original head"));
            }
            head = Some(pinned.clone());
        }
    }
    Ok(head)
}

pub(crate) fn check_input(
    tx: &impl ReadOps,
    candidate: &VerificationCandidate,
    head: &str,
) -> Result<(), StoreError> {
    if blocks_admission(tx)? {
        return Err(invalid("an active native host fault pauses admission"));
    }
    for record in tx.host_recoveries()? {
        let state = decode(&record)?;
        for member in &state.members {
            if member.subject.candidate.project != candidate.project
                || member.subject.candidate.story_id != candidate.story_id
                || member.subject.candidate.verifying_generation != candidate.verifying_generation
            {
                continue;
            }
            let Some(released) = member.restored.as_ref().and_then(|r| r.released.as_ref()) else {
                return Err(invalid("this original host subject is still held"));
            };
            if member.subject.candidate != *candidate
                || member.subject.attribution.inputs.head.as_deref() != Some(head)
            {
                return Err(invalid(
                    "retained host submission head or identity changed before its fresh gate",
                ));
            }
            native::authority(tx, &member.subject)?;
            member.subject.archive.verify()?;
            if !tx.attributions(candidate.project)?.contains(released) {
                return Err(invalid("host release receipt changed"));
            }
        }
    }
    Ok(())
}

fn same_identity(a: &Subject, b: &Subject) -> bool {
    a.candidate.project == b.candidate.project
        && a.attribution.id == b.attribution.id
        && a.execution.id == b.execution.id
        && a.component == b.component
}
fn same_subject(a: &Subject, b: &Subject) -> bool {
    let mut a = a.clone();
    let mut b = b.clone();
    // These correlate one fresh query, not a different immutable submission.
    a.request.nonce.clear();
    b.request.nonce.clear();
    a.request.operation.clear();
    b.request.operation.clear();
    a == b
}
fn fingerprint(fault: &Fault) -> Result<String, StoreError> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(fault).map_err(|e| invalid(&e.to_string()))?)
    ))
}
fn released_record(id: &str, subject: &Subject) -> Result<AttributionRecord, StoreError> {
    let mut record = subject.attribution.clone();
    record.revision = record
        .revision
        .checked_add(1)
        .ok_or_else(|| invalid("host attribution revision overflow"))?;
    record.held = false;
    record.retired = Some(format!(
        "native host recovery {id} restored component {}",
        subject.component
    ));
    Ok(record)
}
fn view(record: &HostRecovery, state: &State) -> HostRecoveryView {
    HostRecoveryView {
        id: record.id.clone(),
        revision: record.revision,
        active: record.active,
        started_at: state.started_at.clone(),
        submissions: state.members.len(),
        readmitted: state
            .members
            .iter()
            .filter(|m| {
                m.restored
                    .as_ref()
                    .and_then(|r| r.released.as_ref())
                    .is_some()
            })
            .count(),
    }
}

/// Identity of a validated pressure episode, not restoration authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFaultStatus {
    /// Diagnostic kind; only native pressure episodes are supported here.
    pub kind: String,
    /// Digest binding authority, host, boot, policy and native fault sequence.
    pub key: String,
    /// Exact causal episode sequence, not a count of retries or attempts.
    pub sequence: u64,
}

/// Status evidence never grants native restoration or gate certification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRecoveryStatus {
    /// Validated fault identity, unavailable for corrupt or legacy status payloads.
    #[serde(default)]
    pub fault: Option<HostFaultStatus>,
    /// Selected-project original generations and their retained admission history.
    #[serde(default)]
    pub retained_submissions: Vec<crate::service::gate_cost::view::RetainedSubmissionStatus>,
    /// Native episode owner identity.
    pub id: String,
    /// Waiting, restored, or invalid retained evidence.
    pub phase: String,
    /// Original admission, including all recovery pauses.
    pub started_at: Option<String>,
    /// Elapsed wall time from original admission; no retry resets it.
    pub elapsed_milliseconds: Option<u64>,
    /// Original submissions in the selected project, not other projects' data.
    pub submissions: Vec<String>,
    /// Whether this owner currently pauses admission across the store.
    pub pauses_admission: bool,
    /// Specific remaining proof or diagnostic obligation.
    pub next_action: String,
}

/// Uses the enclosing snapshot's observation time for visible live elapsed rows.
/// Empty, invalid-only and filtered collections do not require a valid `now`.
pub(crate) fn status_snapshot(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
    now: &str,
) -> Result<Vec<HostRecoveryStatus>, StoreError> {
    let mut result = Vec::new();
    let metadata = tx
        .project(project)?
        .ok_or_else(|| invalid("status project missing"))?;
    let attempts = tx.gate_attempts(project);
    for record in tx.host_recoveries()? {
        let state = match decode(&record).and_then(|state| {
            if state.members.iter().any(|member| {
                let candidate = &member.subject.candidate;
                let submission = &member.subject.attribution.submission;
                !submission.matches_story(candidate.project, &candidate.story_id)
                    || submission.generation != candidate.verifying_generation
            }) {
                return Err(StoreError::Corrupt(
                    "host status subject project, story or generation differs from retained attribution".into(),
                ));
            }
            Ok(state)
        }) {
            Ok(state) => state,
            Err(error) => {
                result.push(HostRecoveryStatus { fault: None, retained_submissions: Vec::new(), id: record.id, phase: "invalid".into(), started_at: None, elapsed_milliseconds: None, submissions: Vec::new(), pauses_admission: true, next_action: format!("Native host owner evidence is invalid; admission remains closed until custody is reconciled: {error}") });
                continue;
            }
        };
        let mut submissions = Vec::new();
        let mut retained_submissions: Vec<
            crate::service::gate_cost::view::RetainedSubmissionStatus,
        > = Vec::new();
        for member in state
            .members
            .iter()
            .filter(|m| m.subject.candidate.project == project)
        {
            let candidate = &member.subject.candidate;
            let story = member
                .subject
                .attribution
                .submission
                .story_number()
                .ok_or_else(|| invalid("status subject story missing"))?;
            if tx
                .story(project, story)?
                .is_some_and(|row| row.state == "verifying")
                && crate::service::verification::verifying_entry(tx, project, story)?
                    .map(|(_, generation)| generation)
                    == candidate.verifying_generation
            {
                if !submissions.contains(&candidate.story_id) {
                    submissions.push(candidate.story_id.clone());
                }
                let original = &member.subject.attribution.submission;
                if !retained_submissions
                    .iter()
                    .any(|s| s.submission.same_generation(original))
                {
                    retained_submissions.push(
                        crate::service::gate_cost::view::RetainedSubmissionStatus::from_history(
                            original,
                            &metadata.slug,
                            &metadata.prefix,
                            &attempts,
                        ),
                    );
                }
            }
        }
        if !record.active && submissions.is_empty() {
            continue;
        }
        let started = chrono::DateTime::parse_from_rfc3339(&state.started_at)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let observed = chrono::DateTime::parse_from_rfc3339(now)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        result.push(HostRecoveryStatus { fault: Some(HostFaultStatus { kind: "native-host-pressure".into(), key: record.fault_key.clone(), sequence: state.fault.sequence }), retained_submissions, id: record.id, phase: if record.active { "waiting-native-restoration" } else { "restored-requires-fresh-gate" }.into(), elapsed_milliseconds: Some((observed.with_timezone(&chrono::Utc)-started.with_timezone(&chrono::Utc)).num_milliseconds().max(0) as u64), started_at: Some(state.started_at), submissions, pauses_admission: record.active, next_action: if record.active { "Wait for fresh native pressure restoration, completed hysteresis and all affected execution custody; JSON status or a later green sample cannot release this hold." } else { "Each retained subject still needs its own fresh restoration proof and an exact-original-head central gate. Manual stops and independent holds remain authoritative." }.into() });
    }
    Ok(result)
}
fn decode(record: &HostRecovery) -> Result<State, StoreError> {
    let state: State = serde_json::from_value(record.state.clone())
        .map_err(|e| StoreError::Corrupt(format!("host owner {}: {e}", record.id)))?;
    let corrupt = || {
        StoreError::Corrupt(format!(
            "host owner {} has inconsistent native episode or subject custody",
            record.id
        ))
    };
    if state.version != 1
        || state.members.is_empty()
        || fingerprint(&state.fault)? != record.fault_key
        || record.revision < 0
        || uuid::Uuid::parse_str(&record.id).is_err()
        || record.active && state.members.iter().any(|m| m.restored.is_some())
        || !record.active && !state.members.iter().any(|m| m.restored.is_some())
    {
        return Err(corrupt());
    }
    for at in [&state.started_at, &state.updated_at].into_iter().chain(
        state
            .members
            .iter()
            .flat_map(|m| std::iter::once(&m.enrolled_at).chain(m.restored.iter().map(|r| &r.at))),
    ) {
        chrono::DateTime::parse_from_rfc3339(at).map_err(|_| corrupt())?;
    }
    let mut identities = std::collections::BTreeSet::new();
    for member in &state.members {
        let s = &member.subject;
        s.attribution.validate()?;
        chrono::DateTime::parse_from_rfc3339(&s.admitted_at).map_err(|_| corrupt())?;
        if s.request.fault != state.fault
            || !s.attribution.held
            || s.attribution.retired.is_some()
            || !s
                .attribution
                .inputs
                .head
                .as_deref()
                .is_some_and(crate::service::project_fault::is_pinned_oid)
            || !identities.insert((
                s.candidate.project,
                s.attribution.id.clone(),
                s.execution.id.clone(),
                s.component.clone(),
            ))
        {
            return Err(corrupt());
        }
        if let Some(released) = member.restored.as_ref().and_then(|r| r.released.as_ref())
            && (s.attribution.components.len() != 1 || *released != released_record(&record.id, s)?)
        {
            return Err(corrupt());
        }
    }
    Ok(state)
}
fn encode(state: &State) -> Result<Value, StoreError> {
    serde_json::to_value(state).map_err(|e| invalid(&e.to_string()))
}
fn save(
    tx: &mut impl WriteOps,
    record: &mut HostRecovery,
    state: &State,
) -> Result<(), StoreError> {
    let expected = record.revision;
    record.revision = expected
        .checked_add(1)
        .ok_or_else(|| invalid("host owner revision overflow"))?;
    record.state = encode(state)?;
    decode(record)?;
    if !tx.update_host_recovery(record, expected)? {
        return Err(invalid("host ownership changed during update"));
    }
    Ok(())
}
fn comment<S: Store>(
    tx: &mut impl WriteOps,
    ctx: &Ctx<'_, S>,
    subject: &Subject,
    now: &str,
    text: String,
) -> Result<(), StoreError> {
    let project = subject.candidate.project;
    let story = subject
        .attribution
        .submission
        .story_number()
        .ok_or_else(|| invalid("host subject story missing"))?;
    let row = tx
        .story(project, story)?
        .ok_or_else(|| invalid("host subject disappeared"))?;
    crate::service::append_and_fold(
        tx,
        project,
        story,
        &crate::service::project_prefix(tx, project)?,
        &tx.state_map(project)?,
        ExpectedSeq::Exact(row.head_seq),
        &[StoryEvent::StoryCommentAdded {
            at: now.into(),
            text,
        }],
        ctx.provenance(),
    )?;
    Ok(())
}
fn invalid(detail: &str) -> StoreError {
    StoreError::Validation(format!("host recovery: {detail}"))
}
