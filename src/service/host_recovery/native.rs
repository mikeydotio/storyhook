//! Only a fresh shipped helper speaking to the authenticated native broker can
//! mint host authority. Retained JSON and journal observations remain evidence.
use crate::{
    env::Environment,
    error::AppError,
    process::Cancellation,
    service::{
        Ctx, VerificationCandidate,
        attribution::{AttributionRecord, FailureCause},
    },
    store::{GateExecution, GlobalSeq, ReadOps, Store, StoreError},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Seek, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Fault {
    pub authority: String,
    pub host: String,
    pub boot: String,
    pub policy: String,
    pub sequence: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub attempt_id: String,
    pub execution_id: String,
    pub generation: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Affected {
    pub lease: String,
    pub binding: Binding,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Window {
    pub start_sequence: u64,
    pub end_sequence: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub operation: String,
    pub nonce: String,
    pub fault: Fault,
    pub window: Window,
    pub affected: Vec<Affected>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Archive {
    path: PathBuf,
    digest: String,
}
impl Archive {
    fn capture(path: &Path) -> Result<(Self, Vec<u8>), StoreError> {
        if !path.is_absolute() {
            return Err(invalid("native journal path is not absolute"));
        }
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| invalid(&format!("native journal unavailable: {e}")))?;
        let metadata = file.metadata().map_err(|e| invalid(&e.to_string()))?;
        if !metadata.is_file() || metadata.len() > 16 * 1024 * 1024 {
            return Err(invalid("native journal is not a bounded regular file"));
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| invalid(&e.to_string()))?;
        if bytes.len() as u64 != metadata.len() {
            return Err(invalid("native journal changed during capture"));
        }
        Ok((
            Self {
                path: path.into(),
                digest: format!("{:x}", Sha256::digest(&bytes)),
            },
            bytes,
        ))
    }
    pub(super) fn verify(&self) -> Result<(), StoreError> {
        if Self::capture(&self.path)?.0 != *self {
            return Err(invalid("native host journal custody changed"));
        }
        Ok(())
    }
}

/// Persisted subject facts are evidence; they cannot reconstruct either capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Subject {
    pub candidate: VerificationCandidate,
    pub attribution: AttributionRecord,
    pub execution: GateExecution,
    pub component: String,
    pub control: i64,
    pub labels: Option<GlobalSeq>,
    pub archive: Archive,
    pub request: Request,
    pub repository_common: PathBuf,
}

struct Live {
    subject: Subject,
    reply: Value,
    deadline: Instant,
    cancellation: Cancellation,
}

/// Native causal fault enrollment only. It cannot release any held submission.
/// ```compile_fail
/// use storyhook::service::host_recovery::HostFaultEvidence;
/// let _: HostFaultEvidence = serde_json::from_str("{}").unwrap();
/// ```
pub struct HostFaultEvidence {
    live: Live,
}
/// Fresh native hysteresis/settlement proof, separate from fault enrollment.
/// ```compile_fail
/// use storyhook::service::host_recovery::HostRestorationEvidence;
/// let _: HostRestorationEvidence = serde_json::from_str("{}").unwrap();
/// ```
pub struct HostRestorationEvidence {
    live: Live,
}

impl Live {
    fn fresh(&self) -> Result<(), StoreError> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(invalid("native host proof expired or cancelled"));
        }
        freshness(
            &self.reply,
            monotonic_ms()?,
            self.subject.request.operation == "restoration-proof",
        )
    }
    fn validate(&self, tx: &impl ReadOps) -> Result<(), StoreError> {
        self.fresh()?;
        current(tx, &self.subject)?;
        self.subject.archive.verify()?;
        self.fresh()
    }
}
impl HostFaultEvidence {
    pub(super) fn subject(&self) -> &Subject {
        &self.live.subject
    }
    pub(super) fn receipt(&self) -> &Value {
        &self.live.reply
    }
    pub(super) fn validate(&self, tx: &impl ReadOps) -> Result<(), StoreError> {
        self.live.validate(tx)
    }
}
impl HostRestorationEvidence {
    pub(super) fn subject(&self) -> &Subject {
        &self.live.subject
    }
    pub(super) fn receipt(&self) -> &Value {
        &self.live.reply
    }
    pub(super) fn validate(&self, tx: &impl ReadOps) -> Result<(), StoreError> {
        self.live.validate(tx)
    }
    pub(super) fn validate_after_release(&self, tx: &impl ReadOps) -> Result<(), StoreError> {
        self.live.fresh()?;
        authority(tx, &self.live.subject)?;
        self.live.subject.archive.verify()?;
        self.live.fresh()
    }
}

/// Derive the complete gate binding and original causal inputs from the store,
/// then query only the live canonical native broker using shipped helper bytes.
#[allow(clippy::too_many_arguments)]
pub fn observe_fault<S: Store>(
    ctx: &Ctx<'_, S>,
    candidate: &VerificationCandidate,
    attribution: &str,
    execution: &str,
    component: &str,
    deadline: Instant,
    cancellation: Cancellation,
) -> Result<HostFaultEvidence, AppError> {
    let live = observe(
        ctx,
        candidate,
        attribution,
        execution,
        component,
        false,
        deadline,
        cancellation,
    )?;
    Ok(HostFaultEvidence { live })
}

/// Re-fetch native restoration for the same retained original failure. A saved
/// receipt, later green sample, or restarted daemon cannot call this capability's constructor.
#[allow(clippy::too_many_arguments)]
pub fn observe_restoration<S: Store>(
    ctx: &Ctx<'_, S>,
    candidate: &VerificationCandidate,
    attribution: &str,
    execution: &str,
    component: &str,
    deadline: Instant,
    cancellation: Cancellation,
) -> Result<HostRestorationEvidence, AppError> {
    let live = observe(
        ctx,
        candidate,
        attribution,
        execution,
        component,
        true,
        deadline,
        cancellation,
    )?;
    Ok(HostRestorationEvidence { live })
}

#[allow(clippy::too_many_arguments)]
fn observe<S: Store>(
    ctx: &Ctx<'_, S>,
    candidate: &VerificationCandidate,
    attribution: &str,
    execution: &str,
    component: &str,
    restored: bool,
    deadline: Instant,
    cancellation: Cancellation,
) -> Result<Live, AppError> {
    if candidate.project != ctx.project() {
        return Err(invalid("foreign host subject project").into());
    }
    let mut subject = ctx
        .store()
        .read(|tx| capture(tx, candidate, attribution, execution, component))?;
    subject.request.operation = if restored {
        "restoration-proof"
    } else {
        "fault-proof"
    }
    .into();
    subject.request.nonce = uuid::Uuid::new_v4().simple().to_string();
    subject.repository_common = common(ctx.env(), &candidate.checkout, deadline, &cancellation)?;
    let reply = query(ctx.env(), &subject.request, deadline, &cancellation)?;
    validate_reply(&subject, &reply, monotonic_ms()?, restored)?;
    let live = Live {
        subject,
        reply,
        deadline,
        cancellation,
    };
    ctx.store().read(|tx| live.validate(tx))?;
    Ok(live)
}

fn capture(
    tx: &impl ReadOps,
    candidate: &VerificationCandidate,
    attribution: &str,
    execution: &str,
    component: &str,
) -> Result<Subject, StoreError> {
    let record = tx
        .attributions(candidate.project)?
        .into_iter()
        .find(|a| a.id == attribution)
        .ok_or_else(|| invalid("original host attribution missing"))?;
    record.validate()?;
    let attempt = tx
        .gate_attempts(candidate.project)?
        .into_iter()
        .find(|a| a.id == record.attempt && a.submission == record.submission)
        .ok_or_else(|| invalid("original host attempt missing"))?;
    let gate = attempt
        .executions
        .iter()
        .find(|e| e.id == execution)
        .cloned()
        .ok_or_else(|| invalid("original host execution missing"))?;
    if attempt.finished_at.is_none()
        || attempt.elapsed.estimated
        || gate.finished_at.is_none()
        || gate.estimated
        || !gate.purpose.is_gate()
        || !gate.journal_bound
        || gate.verdict.as_deref() != Some("infrastructure-failure")
        || !gate.diagnostics.is_empty()
        || gate.inputs != record.inputs
        || !gate.submissions.contains(&record.submission)
        || !record
            .inputs
            .head
            .as_deref()
            .is_some_and(crate::service::project_fault::is_pinned_oid)
        || !record.held
        || record.retired.is_some()
        || record.has_unsettled_diagnosis()
        || !record
            .components
            .iter()
            .any(|c| c.id == component && c.observed_cause == FailureCause::HostExternal)
        || record.submission.generation != candidate.verifying_generation
        || record.submission.story_id != candidate.story_id
    {
        return Err(invalid(
            "host enrollment lacks exact completed original failure, head, generation or cleanup",
        ));
    }
    let (archive, raw) = Archive::capture(Path::new(&gate.journal_path))?;
    let request = derive_request(
        &attempt.id,
        &gate,
        candidate.verifying_generation.map(GlobalSeq::get),
        &raw,
    )?;
    let story = record
        .submission
        .story_number()
        .ok_or_else(|| invalid("invalid host subject story"))?;
    let subject = Subject {
        candidate: candidate.clone(),
        attribution: record,
        execution: gate,
        component: component.into(),
        control: tx.verification_control_revision(candidate.project)?,
        labels: crate::service::project_recovery::recovery_label_revision(
            tx,
            candidate.project,
            story,
        )?,
        archive,
        request,
        repository_common: PathBuf::new(),
    };
    if attempt.control_revision != Some(subject.control) {
        return Err(invalid("host subject control epoch changed"));
    }
    current(tx, &subject)?;
    Ok(subject)
}

pub(super) fn current(tx: &impl ReadOps, subject: &Subject) -> Result<(), StoreError> {
    authority(tx, subject)?;
    if !tx
        .attributions(subject.candidate.project)?
        .contains(&subject.attribution)
    {
        return Err(invalid("original host attribution changed"));
    }
    Ok(())
}

pub(super) fn authority(tx: &impl ReadOps, subject: &Subject) -> Result<(), StoreError> {
    let c = &subject.candidate;
    let story = subject
        .attribution
        .submission
        .story_number()
        .ok_or_else(|| invalid("host subject story unavailable"))?;
    let row = tx
        .story(c.project, story)?
        .ok_or_else(|| invalid("host subject disappeared"))?;
    let attempts = tx.gate_attempts(c.project)?;
    if !tx.verification_enabled(c.project)?
        || !crate::service::automations::permits_generation(tx, c.project, c.verifying_generation)?
        || crate::domain::is_reserved(&row.snapshot)
        || row.awaiting.is_some()
        || !crate::service::verification::candidate_is_current(tx, &row, c)?
        || !crate::service::verification::submission_is_current(tx, &row, c)?
        || crate::service::project_recovery::recovery_resource_hold(tx, c.project, story)?
        || tx.verification_control_revision(c.project)? != subject.control
        || crate::service::project_recovery::recovery_label_revision(tx, c.project, story)?
            != subject.labels
        || !attempts
            .iter()
            .rev()
            .find(|a| a.submission.matches_story(c.project, &c.story_id))
            .is_some_and(|a| {
                a.id == subject.attribution.attempt
                    && a.submission == subject.attribution.submission
                    && a.executions.contains(&subject.execution)
                    && a.control_revision == Some(subject.control)
                    && a.finished_at.is_some()
                    && !a.elapsed.estimated
            })
    {
        return Err(invalid(
            "host subject authority, original evidence or resource custody changed",
        ));
    }
    Ok(())
}

fn derive_request(
    attempt: &str,
    gate: &GateExecution,
    generation: Option<i64>,
    raw: &[u8],
) -> Result<Request, StoreError> {
    let generation = generation
        .filter(|g| *g > 0)
        .ok_or_else(|| invalid("host subject lacks generation"))?;
    let binding = Binding {
        attempt_id: attempt.into(),
        execution_id: gate.id.clone(),
        generation,
    };
    let text = std::str::from_utf8(raw).map_err(|_| invalid("native host journal is not UTF-8"))?;
    let lines: Vec<Value> = text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .map_err(|_| invalid("native host journal is malformed"))?;
    let first = lines
        .first()
        .ok_or_else(|| invalid("native host journal empty"))?;
    if first["kind"] != "run"
        || first["attempt_id"] != attempt
        || first["execution_id"] != gate.id
        || first["generation"] != generation
        || !lines.iter().any(|r| {
            r["kind"] == "admission"
                && r["entry"] == "verifier-gate"
                && r["cause"] == "pressure"
                && r["retryable"] == true
                && r["reason"] == "severe pressure"
        })
    {
        return Err(invalid(
            "native journal does not bind the gate's typed pressure withdrawal",
        ));
    }
    for event in &gate.resource_events {
        if !lines.iter().any(|r| {
            r["kind"] == "resource"
                && r["attempt_id"] == attempt
                && r["execution_id"] == gate.id
                && r["generation"] == generation
                && r["observation"] == *event
        }) {
            return Err(invalid(
                "retained host event is absent from raw native journal",
            ));
        }
    }
    let causes: Vec<_> = gate
        .resource_events
        .iter()
        .filter(|e| {
            e["event"] == "cancel"
                && e["reason"] == "severe pressure"
                && e["pressure_fault_sequence"].as_u64().is_some()
        })
        .collect();
    let first = causes
        .first()
        .ok_or_else(|| invalid("typed gate pressure cause lacks native cancellation locator"))?;
    let fault = Fault {
        authority: string(first, "authority")?,
        host: string(first, "host")?,
        boot: string(first, "boot")?,
        policy: string(first, "policy")?,
        sequence: number(first, "pressure_fault_sequence")?,
    };
    if causes.iter().any(|e| {
        e["authority"] != fault.authority
            || e["host"] != fault.host
            || e["boot"] != fault.boot
            || e["policy"] != fault.policy
            || e["pressure_fault_sequence"] != fault.sequence
    }) {
        return Err(invalid(
            "gate spans distinct host faults; explicit coordination required",
        ));
    }
    let binding_value = serde_json::to_value(&binding).map_err(|e| invalid(&e.to_string()))?;
    let mut roots: BTreeMap<String, (u64, Option<u64>, bool)> = BTreeMap::new();
    for e in &gate.resource_events {
        if !e["parent"].is_null()
            || e["binding"] != binding_value
            || e["authority"] != fault.authority
            || e["host"] != fault.host
            || e["boot"] != fault.boot
            || e["policy"] != fault.policy
        {
            continue;
        }
        let name = string(e, "lease")?;
        let sequence = number(e, "sequence")?;
        if e["event"] == "request" {
            roots.insert(name, (sequence, None, false));
            continue;
        }
        if let Some(root) = roots.get_mut(&name) {
            if e["event"] == "release" || e["event"] == "cancel" {
                root.1 = Some(sequence);
            }
            root.2 |= e["pressure_fault_sequence"] == fault.sequence;
        }
    }
    let mut affected = Vec::new();
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    for (lease, (start, end, linked)) in roots {
        let end =
            end.ok_or_else(|| invalid("same-binding root lacks terminal native observation"))?;
        if linked || start <= fault.sequence && fault.sequence <= end {
            affected.push(Affected {
                lease,
                binding: binding.clone(),
            });
            starts.push(start);
            ends.push(end);
        }
    }
    if affected.is_empty() {
        return Err(invalid(
            "native pressure gate has no complete affected root set",
        ));
    }
    Ok(Request {
        operation: "fault-proof".into(),
        nonce: String::new(),
        fault,
        window: Window {
            start_sequence: *starts.iter().min().unwrap(),
            end_sequence: *ends.iter().max().unwrap(),
        },
        affected,
    })
}

fn validate_reply(
    subject: &Subject,
    reply: &Value,
    now: u64,
    restored: bool,
) -> Result<(), StoreError> {
    let request = &subject.request;
    let expected_kind = if restored {
        "host-pressure-restoration"
    } else {
        "host-pressure-fault"
    };
    if reply["version"] != 1
        || reply["kind"] != expected_kind
        || reply["nonce"] != request.nonce
        || reply["fault"] != serde_json::to_value(&request.fault).unwrap()
        || reply["window"] != serde_json::to_value(&request.window).unwrap()
        || reply["affected"] != serde_json::to_value(&request.affected).unwrap()
        || reply["broker"]["boot"] != request.fault.boot
        || number(&reply["broker"], "pid")? == 0
        || string(&reply["broker"], "start")?.is_empty()
    {
        return Err(invalid(
            "live native host reply differs from exact request identity",
        ));
    }
    let settled = reply["settled"]
        .as_array()
        .ok_or_else(|| invalid("native host reply lacks terminal subjects"))?;
    if settled.len() != request.affected.len() {
        return Err(invalid("native host affected set differs"));
    }
    let mut linked = false;
    let mut seen = BTreeSet::new();
    for row in settled {
        let lease = string(row, "lease")?;
        let expected = request
            .affected
            .iter()
            .find(|a| a.lease == lease)
            .ok_or_else(|| invalid("native host reply adds another lease"))?;
        if !seen.insert(lease)
            || row["binding"] != serde_json::to_value(&expected.binding).unwrap()
            || row["project"].as_str() != subject.repository_common.to_str()
            || !matches!(row["work"].as_str(), Some("test" | "repair"))
            || !matches!(row["state"].as_str(), Some("released" | "cancelled"))
        {
            return Err(invalid(
                "native host terminal project, work or binding differs",
            ));
        }
        for link in row["pressure_links"]
            .as_array()
            .ok_or_else(|| invalid("native host reply lacks causal links"))?
        {
            if link["pressure_fault_sequence"] == request.fault.sequence
                && link["event"] == "cancel"
                && link["reason"] == "severe pressure"
            {
                let sequence = number(link, "sequence")?;
                linked |= subject.execution.resource_events.iter().any(|e| {
                    e["sequence"] == sequence
                        && e["lease"] == row["lease"]
                        && e["event"] == link["event"]
                        && e["pressure_fault_sequence"] == request.fault.sequence
                });
            }
        }
    }
    if !linked {
        return Err(invalid(
            "temporal pressure overlap is not the gate's native causal withdrawal",
        ));
    }
    freshness(reply, now, restored)
}

fn freshness(reply: &Value, now: u64, restored: bool) -> Result<(), StoreError> {
    let checked = number(reply, "checked_at")?;
    let stale = number(&reply["timing"], "stale_ms")?;
    let start = if restored {
        number(&reply["sample"], "at")?
    } else {
        checked
    };
    if stale == 0 || start > checked || checked > now || now - start > stale {
        return Err(invalid("native host proof is stale at consumption"));
    }
    Ok(())
}
fn monotonic_ms() -> Result<u64, StoreError> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } != 0
        || value.tv_sec < 0
        || value.tv_nsec < 0
    {
        return Err(invalid("native monotonic clock unavailable"));
    }
    (value.tv_sec as u64)
        .checked_mul(1000)
        .and_then(|v| v.checked_add(value.tv_nsec as u64 / 1_000_000))
        .ok_or_else(|| invalid("native monotonic clock overflow"))
}
fn common(
    env: &Environment,
    checkout: &Path,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<PathBuf, AppError> {
    let mut command = crate::env::git_env::command(checkout);
    command.args(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let output = crate::process::run_captured_private_until(
        command,
        deadline.min(Instant::now() + env.subprocess_bound(Duration::from_secs(30))),
        &|| cancellation.is_cancelled(),
    )
    .map_err(|e| invalid(&e.detail()))?;
    if !output.status.success() || output.stdout_truncated {
        return Err(invalid("native repository common directory unavailable").into());
    }
    Path::new(
        std::str::from_utf8(&output.stdout)
            .map_err(|_| invalid("repository path is not UTF-8"))?
            .trim(),
    )
    .canonicalize()
    .map_err(Into::into)
}
fn query(
    env: &Environment,
    request: &Request,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<Value, AppError> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Err(invalid("native host query cancelled or expired").into());
    }
    let bundle = crate::daemon::verifier_bundle::materialize(env)?;
    let mut input = tempfile::tempfile()?;
    input.write_all(&serde_json::to_vec(request).map_err(|e| invalid(&e.to_string()))?)?;
    input.rewind()?;
    let mut command = Command::new("bash");
    command
        .arg(bundle.join("python-runtime.sh"))
        .arg("--")
        .arg(bundle.join("python-bin/python3"))
        .arg("-B")
        .arg(bundle.join("host-restoration.py"))
        .current_dir(&bundle)
        .env_clear();
    for name in ["PATH", "HOME"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let output = crate::process::run_captured_private_input_until(
        command,
        Some(input),
        deadline.min(Instant::now() + env.subprocess_bound(Duration::from_secs(30))),
        &|| cancellation.is_cancelled(),
    )
    .map_err(|e| invalid(&e.detail()))?;
    if !output.status.success() || output.stdout_truncated || !output.stderr.is_empty() {
        return Err(
            invalid("native host broker proof refused or helper did not settle cleanly").into(),
        );
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| invalid(&format!("native host reply malformed: {e}")).into())
}
fn string(value: &Value, key: &str) -> Result<String, StoreError> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .map(str::to_owned)
        .ok_or_else(|| invalid(&format!("invalid native host {key}")))
}
fn number(value: &Value, key: &str) -> Result<u64, StoreError> {
    value[key]
        .as_u64()
        .ok_or_else(|| invalid(&format!("invalid native host {key}")))
}
fn invalid(detail: &str) -> StoreError {
    StoreError::Validation(format!("host recovery: {detail}"))
}

#[cfg(test)]
mod tests;
