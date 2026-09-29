//! The shadow batch preview at each dequeue (SH-830).
//!
//! Just before a gate starts, the verifier computes the batch it would form
//! around the story it dequeued ([`crate::service::batch_preview::select`]),
//! shows it on its slot while the gate runs, and records it with the gate's
//! duration and verdict once the gate returns. The preview reads the store
//! and runs trial merges in private object storage; it writes nothing to the
//! store, and every failure (a store read, Git, even a panic) stays inside
//! the preview, so what the verifier verifies, lands and completes is the
//! same with the preview on or off.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::PoisonError;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::{VerificationActuator, VerificationGuard, VerificationOutcome};
use crate::env::Environment;
use crate::error::AppError;
use crate::service::batch_preview::{
    BatchPreview, ExclusionReason, PreviewCandidate, PreviewRequest, Standing, select,
};
use crate::service::{VerificationCandidate, VerificationProblem};
use crate::store::{GlobalSeq, ReadOps, Store};

/// Longest the preview may delay its gate: no trial merge starts later, and
/// each Git command is bounded by what remains. A quarter of the progress
/// interval (`PUBLISH_INTERVAL`, 60 s), so a stuck preview cannot by itself
/// make status report an attempt without progress evidence (decision D12).
pub(super) const PREVIEW_BUDGET: Duration = Duration::from_secs(15);

/// Size at which the record log is rotated to `<slug>.ndjson.1`, which keeps
/// at most about twice this per project.
const RECORD_ROTATE_BYTES: u64 = 4 * 1024 * 1024;

/// What this attempt's submission reported for one generation: the base
/// branch it fetched and the head commit it pushed.
#[derive(Clone, Debug)]
pub(super) struct Published {
    generation: Option<GlobalSeq>,
    base: String,
    head: String,
}

impl Published {
    /// The receipt of `candidate`'s current generation.
    pub(super) fn new(candidate: &VerificationCandidate, base: &str, head: &str) -> Self {
        Self {
            generation: candidate.verifying_generation,
            base: base.to_owned(),
            head: head.to_owned(),
        }
    }
}

/// How the gate that followed a preview ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum GateVerdict {
    Certified,
    TestsFailed,
    Conflict,
    InvalidSubmission,
    ProjectFault,
    InfrastructureFailure,
    Cancelled,
    RepairDeferred,
    CleanupFailed,
    /// The attempt lost its authority (a resubmission or a withdrawal).
    Withdrawn,
    /// An operator stopped the attempt while its gate ran.
    Interrupted,
    /// The verifier could not observe the gate's outcome.
    Error,
}

impl GateVerdict {
    fn of(verified: &Result<Option<VerificationOutcome>, AppError>, cancelled: bool) -> Self {
        match verified {
            Err(_) => Self::Error,
            Ok(None) => Self::Withdrawn,
            Ok(Some(VerificationOutcome::CleanupFailed { .. })) => Self::CleanupFailed,
            // The same test the tick applies before recording an interruption.
            Ok(Some(_)) if cancelled => Self::Interrupted,
            Ok(Some(outcome)) => match outcome {
                VerificationOutcome::Certified { .. } => Self::Certified,
                VerificationOutcome::TestsFailed { .. } => Self::TestsFailed,
                VerificationOutcome::Conflict { .. } => Self::Conflict,
                VerificationOutcome::InvalidSubmission { .. } => Self::InvalidSubmission,
                VerificationOutcome::ProjectFault { .. } => Self::ProjectFault,
                VerificationOutcome::InfrastructureFailure { .. } => Self::InfrastructureFailure,
                VerificationOutcome::Cancelled => Self::Cancelled,
                VerificationOutcome::RepairDeferred { .. } => Self::RepairDeferred,
                VerificationOutcome::CleanupFailed { .. } => Self::CleanupFailed,
            },
        }
    }
}

/// One line of the per-dequeue record log.
#[derive(Serialize)]
struct PreviewRecord<'a> {
    attempt_id: &'a str,
    story_id: &'a str,
    generation: Option<GlobalSeq>,
    finished_at: String,
    gate_seconds: u64,
    verdict: GateVerdict,
    /// The merge tree the gate judged, when it judged one: equal to
    /// `preview.head_tree` exactly when the preview and the gate used the
    /// same base (decision D14).
    #[serde(skip_serializing_if = "Option::is_none")]
    gate_tree: Option<&'a str>,
    preview: &'a BatchPreview,
}

/// Where the verifier records one project's previews: one JSON line per
/// gate that followed a preview (decision D7).
#[must_use]
pub fn batch_preview_log(env: &Environment, project_slug: &str) -> PathBuf {
    env.daemon_state_dir()
        .join("verification-batch-preview")
        .join(format!("{project_slug}.ndjson"))
}

/// Computes the preview for the story `owner` is about to gate and shows it
/// on the owner's slot. `None` when the actuator does not preview.
pub(super) fn compute<S: Store, A: VerificationActuator>(
    store: &S,
    env: &Environment,
    actuator: &A,
    candidate: &VerificationCandidate,
    published: Option<&Published>,
    owner: &VerificationGuard,
) -> Option<BatchPreview> {
    let deadline = Instant::now() + PREVIEW_BUDGET;
    let repository = candidate.cleanup_lease.as_ref().map_or_else(
        || candidate.checkout.clone(),
        |lease| lease.repository_path.clone(),
    );
    let merger = actuator.trial_merges(&repository, deadline, &owner.cancellation)?;
    let computed_at = env.now();
    let preview = catch_unwind(AssertUnwindSafe(|| {
        build(
            store,
            candidate,
            published,
            &repository,
            computed_at.clone(),
            deadline,
            merger,
        )
    }))
    .unwrap_or_else(|_| {
        unavailable(
            candidate,
            &computed_at,
            "the preview panicked; the verifier went on",
        )
    });
    owner.show_preview(Some(preview.compact()));
    Some(preview)
}

/// Takes the preview off the owner's slot and records it with the gate that
/// followed. A record that cannot be written is journaled and dropped.
pub(super) fn finish(
    env: &Environment,
    owner: &VerificationGuard,
    candidate: &VerificationCandidate,
    preview: Option<BatchPreview>,
    gate_started: Instant,
    verified: &Result<Option<VerificationOutcome>, AppError>,
) {
    let Some(preview) = preview else {
        return;
    };
    owner.show_preview(None);
    let gate_tree = match verified {
        Ok(Some(
            VerificationOutcome::Certified { tree, .. }
            | VerificationOutcome::TestsFailed { tree, .. },
        )) => Some(tree.as_str()),
        _ => None,
    };
    let record = PreviewRecord {
        attempt_id: &owner.active.attempt_id,
        story_id: &candidate.story_id,
        generation: candidate.verifying_generation,
        finished_at: env.now(),
        gate_seconds: gate_started.elapsed().as_secs(),
        verdict: GateVerdict::of(verified, owner.is_cancelled()),
        gate_tree,
        preview: &preview,
    };
    if let Err(error) = append(&batch_preview_log(env, &candidate.project_slug), &record) {
        crate::daemon::activity::emit(
            "ERROR",
            "verifier",
            "event",
            &format!("project={} {}", candidate.project_slug, candidate.story_id),
            &format!("batch preview record not written: {error}"),
        );
    }
}

impl VerificationGuard {
    /// Shows `preview` on this owner's slot, or clears it. Takes the registry
    /// lock alone, and only while the slot is still this attempt's (the
    /// SH-768 identity rule), so a late call never reaches a later attempt.
    pub(super) fn show_preview(&self, preview: Option<BatchPreview>) {
        let mut slots = self
            .registry
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = slots
            .get_mut(&self.active.project)
            .filter(|slot| slot.active == self.active)
        {
            slot.preview = preview;
        }
    }
}

fn build<S: Store>(
    store: &S,
    candidate: &VerificationCandidate,
    published: Option<&Published>,
    repository: &Path,
    computed_at: String,
    deadline: Instant,
    merger: Result<Box<dyn crate::service::trial_merge::TrialMerger>, AppError>,
) -> BatchPreview {
    let read = store.read(|tx| {
        Ok((
            crate::service::verification::ordered_candidates_for(tx, candidate.project)?,
            crate::service::verification::held_verifying_for(tx, candidate.project)?,
            tx.live_engine_runs()?
                .into_iter()
                .find(|run| run.project_slug == candidate.project_slug)
                .map(|run| run.lanes),
        ))
    });
    let (ordered, held, live_lanes) = match read {
        Ok(read) => read,
        Err(error) => {
            return unavailable(
                candidate,
                &computed_at,
                &format!("reading the queue failed: {error}"),
            );
        }
    };
    let mut merger = match merger {
        Ok(merger) => merger,
        Err(error) => {
            return unavailable(
                candidate,
                &computed_at,
                &format!("opening private object storage failed: {error}"),
            );
        }
    };
    // Only the receipt of the generation now gating names its base and head.
    let receipt = published.filter(|receipt| receipt.generation == candidate.verifying_generation);
    let head = PreviewCandidate {
        story_id: candidate.story_id.clone(),
        standing: receipt.map_or_else(
            || standing(candidate, repository),
            |receipt| Standing::Commit(receipt.head.clone()),
        ),
    };
    let rest = ordered
        .iter()
        .filter(|queued| queued.story_id != candidate.story_id)
        .map(|queued| PreviewCandidate {
            story_id: queued.story_id.clone(),
            standing: standing(queued, repository),
        })
        .chain(held.into_iter().map(|(story_id, why)| PreviewCandidate {
            story_id,
            standing: Standing::Ineligible {
                reason: ExclusionReason::Held,
                detail: why,
            },
        }))
        .collect();
    select(
        PreviewRequest {
            computed_at,
            head,
            rest,
            base_branch: receipt.map(|receipt| receipt.base.clone()),
            cap: live_lanes.unwrap_or(1),
            live_lanes,
            queue_depth: ordered.len(),
            deadline,
        },
        merger.as_mut(),
    )
}

/// Whether a queued story may be tried, from store facts alone. Mirrors
/// `submission_due`: a lease plus a linked PR, or a lease with its PR still
/// to be opened, is a story the verifier could submit.
fn standing(candidate: &VerificationCandidate, repository: &Path) -> Standing {
    let ineligible = |reason, detail: String| Standing::Ineligible { reason, detail };
    if !candidate.blocked_by.is_empty() {
        return ineligible(
            ExclusionReason::Blocked,
            format!("blocked by {}", candidate.blocked_by.join(", ")),
        );
    }
    if candidate.landing_pending {
        return ineligible(
            ExclusionReason::LandingPending,
            "a landing attempt awaits its outcome".into(),
        );
    }
    match (&candidate.cleanup_lease, &candidate.pull_request) {
        (None, _) => ineligible(
            ExclusionReason::Unsubmitted,
            "no leased branch the verifier could push".into(),
        ),
        (Some(_), Err(problem)) if !matches!(problem, VerificationProblem::MissingPullRequest) => {
            ineligible(ExclusionReason::Unsubmitted, problem.message())
        }
        (Some(lease), _) if lease.repository_path != repository => ineligible(
            ExclusionReason::Unsubmitted,
            format!(
                "its branch is in another repository, {}",
                lease.repository_path.display()
            ),
        ),
        (Some(lease), _) => Standing::Branch(lease.branch.clone()),
    }
}

fn unavailable(candidate: &VerificationCandidate, computed_at: &str, detail: &str) -> BatchPreview {
    BatchPreview::unavailable(
        &PreviewRequest {
            computed_at: computed_at.to_owned(),
            head: PreviewCandidate {
                story_id: candidate.story_id.clone(),
                standing: Standing::Branch(String::new()),
            },
            rest: Vec::new(),
            base_branch: None,
            cap: 1,
            live_lanes: None,
            queue_depth: 0,
            deadline: Instant::now(),
        },
        detail,
    )
}

fn append(path: &Path, record: &PreviewRecord<'_>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() >= RECORD_ROTATE_BYTES) {
        std::fs::rename(path, path.with_extension("ndjson.1"))?;
    }
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    // Only this project's single worker appends, one whole line per write.
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?
        .write_all(&line)
}
