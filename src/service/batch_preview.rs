//! The batch the verifier would form at a dequeue (SH-830; spec B1, B2, B3).
//!
//! [`select`] is the selection rule of `docs/spec/verification-batching.md`.
//! The head is the story the verifier dequeued. The rest of the queue is swept
//! in queue order, and a story joins only when its trial merge onto the base
//! plus the members already accepted is clean, up to the cap. A story that
//! cannot join is listed with the reason. SH-830 publishes the result as a
//! shadow preview that changes nothing the verifier does; the later batching
//! children build their batch with the same rule.

use super::batch_smoothing::{Classification, admits_all, classify, policy_from_pointer};
use super::trial_merge::{ConflictShape, TrialMerge, TrialMerger};
use crate::domain::conflict_smoothing::SmoothPolicy;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Most conflicted paths one entry keeps; its detail counts the rest.
pub const MAX_RECORDED_PATHS: usize = 20;

/// One story as the selection sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewCandidate {
    /// Project story id, including its prefix.
    pub story_id: String,
    /// Whether the story may be tried at all.
    pub standing: Standing,
}

/// Whether a story may be tried, decided from store facts before any merge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// The story may join at exactly this commit (the head's pushed commit,
    /// which its submission reported).
    Commit(String),
    /// The story may join; this is its leased branch, resolved when the
    /// preview runs.
    Branch(String),
    /// The story cannot join, for a reason known without a merge.
    Ineligible {
        /// Why it cannot join.
        reason: ExclusionReason,
        /// The fact behind the reason, as an operator reads it.
        detail: String,
    },
}

/// Why a queued or held story is not a member of the batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExclusionReason {
    /// Its head does not merge cleanly onto the base alone.
    ConflictWithBase,
    /// Its head merges onto the base alone but not onto the batch so far.
    ConflictWithMember,
    /// The queue itself leaves it out: human-only, awaiting a person, a
    /// pending reset, or a generation that project recovery owns.
    Held,
    /// An open dependency holds it.
    Blocked,
    /// A durable landing attempt for it awaits its outcome.
    LandingPending,
    /// It has no branch the verifier could submit, or its submission is
    /// ambiguous.
    Unsubmitted,
    /// Its head merges cleanly onto the batch, but the batch is full.
    Cap,
    /// Its trial merge could not be run; the detail says why.
    TrialFailed,
}

impl ExclusionReason {
    /// The reason's wire slug.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConflictWithBase => "conflict-with-base",
            Self::ConflictWithMember => "conflict-with-member",
            Self::Held => "held",
            Self::Blocked => "blocked",
            Self::LandingPending => "landing-pending",
            Self::Unsubmitted => "unsubmitted",
            Self::Cap => "cap",
            Self::TrialFailed => "trial-failed",
        }
    }
}

/// How a preview ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreviewOutcome {
    /// The head merges onto the base; `members` is the would-be batch.
    Batch,
    /// The head conflicts with the base, so no batch forms (B3: the head
    /// keeps today's conflict hold).
    HeadConflict,
    /// The preview could not be computed; `detail` says why.
    Unavailable,
}

/// One would-be batch member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewMember {
    /// Project story id, including its prefix.
    pub story_id: String,
    /// The head commit that was trial-merged.
    pub commit: String,
    /// The paths the verifier would smooth to let this member join, last
    /// (SH-834); empty for a member whose merge is clean.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub smoothed: Vec<String>,
}

/// How a conflict with a member reads for smoothing (SH-834, council D1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SmoothingClass {
    /// Every conflicted path passes the deny floor and the text checks,
    /// and every hunk is insertion-only: the verifier could unite it.
    UnionSmoothable,
    /// The same, except that a hunk changes lines both sides share: only
    /// a resolver that writes text could settle it, and none is built.
    AgentCandidate,
}

/// The smoothing measure of one conflict with a member (SH-834 D3): its
/// class, whatever the allowlist says, and whether the base's allowlist
/// admits every conflicted path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmoothingMark {
    /// How the conflict reads.
    pub class: SmoothingClass,
    /// Whether the base's `[batch] smooth` admits every conflicted path.
    pub allowlisted: bool,
}

/// Whether [`select`] may let a smoothable story join (SH-834).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SmoothingMode {
    /// Classify each conflict with a member for the record; admit none.
    /// What the preview does while the verifier forms no batches.
    #[default]
    Measure,
    /// Also admit the first union-smoothable, allowlisted story as the
    /// batch's last member when the batch is below its cap.
    Admit,
}

/// One story that is not a member, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewExclusion {
    /// Project story id, including its prefix.
    pub story_id: String,
    /// Why the story is not a member.
    pub reason: ExclusionReason,
    /// The fact behind the reason, when there is more to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Conflicted paths for a conflict, at most [`MAX_RECORDED_PATHS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// How a conflict with a member reads for smoothing (SH-834); absent
    /// for every other exclusion, and for a conflict that could never be
    /// smoothed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<SmoothingMark>,
}

/// The batch the verifier would form at one dequeue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchPreview {
    /// When the preview was computed (UTC).
    pub computed_at: String,
    /// The story the verifier dequeued: the batch's head.
    pub head: String,
    /// The base branch the head's submission named, such as `dev`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// The commit of `origin/<base_branch>` the trial merges started from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    /// The tree of the head merged onto the base: the tree a single-story
    /// gate certifies when its base is the same, so a record can show whether
    /// the preview's base matched the gate's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_tree: Option<String>,
    /// Batch size cap: the live engine run's lanes, at least 1 (B2).
    pub cap: u32,
    /// Lanes of the project's live engine run, when one is live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_lanes: Option<u32>,
    /// Stories in the project's verification queue at the dequeue, head
    /// included; held stories are not in the queue.
    pub queue_depth: usize,
    /// How the preview ended.
    pub outcome: PreviewOutcome,
    /// Why the preview is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Paths where the head conflicts with the base, at most
    /// [`MAX_RECORDED_PATHS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub head_conflict: Vec<String>,
    /// Would-be members in queue order, head first.
    #[serde(default)]
    pub members: Vec<PreviewMember>,
    /// Every other story considered, in the order considered.
    #[serde(default)]
    pub excluded: Vec<PreviewExclusion>,
    /// The base's `[batch] smooth` allowlist, as written (SH-834).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub smooth: Vec<String>,
    /// Why no conflict was classified or smoothed (SH-834): an unreadable
    /// or invalid allowlist, or a merger that reads no blobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing_unavailable: Option<String>,
}

impl BatchPreview {
    /// A preview that could not be computed, for a reason an operator reads.
    #[must_use]
    pub fn unavailable(request: &PreviewRequest, detail: impl Into<String>) -> Self {
        let mut preview = Self::empty(request);
        preview.outcome = PreviewOutcome::Unavailable;
        preview.detail = Some(detail.into());
        preview
    }

    fn empty(request: &PreviewRequest) -> Self {
        Self {
            computed_at: request.computed_at.clone(),
            head: request.head.story_id.clone(),
            base_branch: request.base_branch.clone(),
            base_commit: None,
            head_tree: None,
            cap: request.cap.max(1),
            live_lanes: request.live_lanes,
            queue_depth: request.queue_depth,
            outcome: PreviewOutcome::Batch,
            detail: None,
            head_conflict: Vec::new(),
            members: Vec::new(),
            excluded: Vec::new(),
            smooth: Vec::new(),
            smoothing_unavailable: None,
        }
    }

    /// The preview without conflicted paths: what status carries on every
    /// read, while the per-dequeue record keeps the paths.
    #[must_use]
    pub fn compact(&self) -> Self {
        let mut compact = self.clone();
        compact.head_conflict.clear();
        for excluded in &mut compact.excluded {
            excluded.paths.clear();
        }
        compact
    }

    /// One line as an operator reads it, for example
    /// `SH-1 + SH-2 (cap 3, 4 queued); excluded SH-4 conflict-with-member`.
    #[must_use]
    pub fn describe(&self) -> String {
        match self.outcome {
            PreviewOutcome::Unavailable => format!(
                "unavailable: {}",
                self.detail.as_deref().unwrap_or("no reason recorded")
            ),
            PreviewOutcome::HeadConflict => format!(
                "{} conflicts with its base; no batch forms ({} queued)",
                self.head, self.queue_depth
            ),
            PreviewOutcome::Batch => {
                let members: Vec<String> = self
                    .members
                    .iter()
                    .map(|m| {
                        if m.smoothed.is_empty() {
                            m.story_id.clone()
                        } else {
                            format!("{} (smoothed)", m.story_id)
                        }
                    })
                    .collect();
                let mut text = format!(
                    "{} (cap {}, {} queued)",
                    members.join(" + "),
                    self.cap,
                    self.queue_depth
                );
                if !self.excluded.is_empty() {
                    let excluded: Vec<String> = self
                        .excluded
                        .iter()
                        .map(|e| format!("{} {}", e.story_id, e.reason.as_str()))
                        .collect();
                    text.push_str(&format!("; excluded {}", excluded.join(", ")));
                }
                text
            }
        }
    }
}

/// Everything [`select`] needs besides the merger.
#[derive(Clone, Debug)]
pub struct PreviewRequest {
    /// When the preview is computed (UTC).
    pub computed_at: String,
    /// The story the verifier dequeued.
    pub head: PreviewCandidate,
    /// Every other story to consider: the queue in queue order, then the
    /// stories the queue holds out.
    pub rest: Vec<PreviewCandidate>,
    /// The base branch the head's submission named; `None` when this attempt
    /// submitted nothing.
    pub base_branch: Option<String>,
    /// Batch size cap; values below 1 count as 1.
    pub cap: u32,
    /// Lanes of the project's live engine run, when one is live.
    pub live_lanes: Option<u32>,
    /// Stories in the verification queue at the dequeue, head included.
    pub queue_depth: usize,
    /// No trial merge starts at or after this instant; a story it stops is
    /// excluded as [`ExclusionReason::TrialFailed`].
    pub deadline: Instant,
    /// Whether a smoothable story may join (SH-834).
    pub smoothing: SmoothingMode,
}

/// Computes the batch the verifier would form (B1 to B3).
///
/// The head merges onto `origin/<base_branch>` first; a conflict there ends
/// the preview as [`PreviewOutcome::HeadConflict`]. Each other story is then
/// merged onto the batch so far: clean joins while the batch is below the cap,
/// clean at the cap is excluded as [`ExclusionReason::Cap`] (still tried, so
/// the measurement shows what the cap costs), and a conflict is tried against
/// the base alone to tell a base conflict from a member conflict. Failures
/// are reported inside the preview, never returned.
pub fn select(request: PreviewRequest, merger: &mut dyn TrialMerger) -> BatchPreview {
    let Some(branch) = request.base_branch.clone() else {
        return BatchPreview::unavailable(
            &request,
            "no submission in this attempt named the base branch",
        );
    };
    let base = match merger.resolve(&format!("refs/remotes/origin/{branch}")) {
        Ok(Some(base)) => base,
        Ok(None) => {
            return BatchPreview::unavailable(&request, format!("origin/{branch} names no commit"));
        }
        Err(error) => {
            return BatchPreview::unavailable(
                &request,
                format!("resolving origin/{branch}: {error}"),
            );
        }
    };
    let head_commit = match standing_commit(&request.head, merger) {
        Ok(commit) => commit,
        Err(exclusion) => {
            return BatchPreview::unavailable(
                &request,
                format!(
                    "the head {} cannot be tried ({}): {}",
                    request.head.story_id,
                    exclusion.reason.as_str(),
                    exclusion.detail.unwrap_or_default()
                ),
            );
        }
    };
    let mut preview = BatchPreview::empty(&request);
    preview.base_commit = Some(base.clone());
    let merged = merger
        .merge(&base, &head_commit)
        .and_then(|merge| match merge {
            TrialMerge::Clean { tree } => merger
                .commit(&base, &head_commit, &tree)
                .map(|commit| Ok((tree, commit))),
            TrialMerge::Conflict { paths, .. } => Ok(Err(paths)),
        });
    let mut batch = match merged {
        Ok(Ok((tree, commit))) => {
            preview.head_tree = Some(tree);
            commit
        }
        Ok(Err(paths)) => {
            preview.outcome = PreviewOutcome::HeadConflict;
            preview.head_conflict = recorded(&paths);
            preview.members.push(PreviewMember {
                story_id: request.head.story_id.clone(),
                commit: head_commit,
                smoothed: Vec::new(),
            });
            return preview;
        }
        Err(error) => {
            let mut preview = BatchPreview::unavailable(
                &request,
                format!("trial merge of the head failed: {error}"),
            );
            preview.base_commit = Some(base);
            return preview;
        }
    };
    preview.members.push(PreviewMember {
        story_id: request.head.story_id.clone(),
        commit: head_commit,
        smoothed: Vec::new(),
    });
    let policy = smoothing_policy(&mut preview, merger, &base);
    let cap = preview.cap as usize;
    // Stories that conflict with a member, as (their exclusion, commit):
    // the only stories smoothing may admit (SH-834 D5).
    let mut member_conflicts: Vec<(usize, String)> = Vec::new();
    for candidate in &request.rest {
        let commit = match standing_commit(candidate, merger) {
            Ok(commit) => commit,
            Err(exclusion) => {
                preview.excluded.push(exclusion);
                continue;
            }
        };
        if Instant::now() >= request.deadline {
            preview.excluded.push(exclusion(
                candidate,
                ExclusionReason::TrialFailed,
                "the preview deadline passed before this trial merge",
            ));
            continue;
        }
        match merger.merge(&batch, &commit) {
            Ok(TrialMerge::Clean { tree }) if preview.members.len() < cap => {
                match merger.commit(&batch, &commit, &tree) {
                    Ok(merged) => {
                        preview.members.push(PreviewMember {
                            story_id: candidate.story_id.clone(),
                            commit,
                            smoothed: Vec::new(),
                        });
                        batch = merged;
                    }
                    Err(error) => preview.excluded.push(exclusion(
                        candidate,
                        ExclusionReason::TrialFailed,
                        format!("recording its merge onto the batch failed: {error}"),
                    )),
                }
            }
            Ok(TrialMerge::Clean { .. }) => preview.excluded.push(exclusion(
                candidate,
                ExclusionReason::Cap,
                format!("merges cleanly onto the batch, which is full at {cap}"),
            )),
            Ok(TrialMerge::Conflict { paths, shape }) => {
                preview.excluded.push(match merger.merge(&base, &commit) {
                    Ok(TrialMerge::Clean { .. }) => {
                        member_conflicts.push((preview.excluded.len(), commit.clone()));
                        let mut entry =
                            conflict(candidate, ExclusionReason::ConflictWithMember, &paths);
                        entry.smoothing = policy
                            .as_ref()
                            .and_then(|policy| mark(&shape, policy, merger, request.deadline));
                        entry
                    }
                    Ok(TrialMerge::Conflict { paths, .. }) => {
                        conflict(candidate, ExclusionReason::ConflictWithBase, &paths)
                    }
                    Err(error) => exclusion(
                        candidate,
                        ExclusionReason::TrialFailed,
                        format!("trial merge onto the base failed: {error}"),
                    ),
                });
            }
            Err(error) => preview.excluded.push(exclusion(
                candidate,
                ExclusionReason::TrialFailed,
                format!("trial merge onto the batch failed: {error}"),
            )),
        }
    }
    if request.smoothing == SmoothingMode::Admit
        && preview.members.len() < cap
        && let Some(policy) = policy.filter(|policy| !policy.is_empty())
    {
        admit_smoothable(
            &mut preview,
            merger,
            &batch,
            &policy,
            &member_conflicts,
            request.deadline,
        );
    }
    preview
}

/// The base's smoothing allowlist, or `None` with the reason recorded on
/// `preview` when it cannot be read (SH-834). Read from the base only: a
/// member must not widen what its own batch may smooth.
fn smoothing_policy(
    preview: &mut BatchPreview,
    merger: &mut dyn TrialMerger,
    base: &str,
) -> Option<SmoothPolicy> {
    let read = match merger.blobs() {
        None => Err("this trial merger reads no blobs".to_owned()),
        Some(blobs) => blobs
            .file(base, super::batch_smoothing::POINTER)
            .map_err(|error| format!("reading the base's .storyhook.toml failed: {error}"))
            .and_then(|raw| policy_from_pointer(raw.as_deref())),
    };
    match read {
        Ok(policy) => {
            preview.smooth = policy.entries().into_iter().map(str::to_owned).collect();
            Some(policy)
        }
        Err(why) => {
            preview.smoothing_unavailable = Some(why);
            None
        }
    }
}

/// The smoothing measure of one conflict with a member, or `None` for a
/// conflict that could never be smoothed or one the deadline leaves no
/// time to read.
fn mark(
    shape: &ConflictShape,
    policy: &SmoothPolicy,
    merger: &mut dyn TrialMerger,
    deadline: Instant,
) -> Option<SmoothingMark> {
    if Instant::now() >= deadline {
        return None;
    }
    match classify(shape, merger.blobs()?) {
        Classification::UnionSmoothable(files) => Some(SmoothingMark {
            class: SmoothingClass::UnionSmoothable,
            allowlisted: admits_all(policy, &files),
        }),
        Classification::AgentCandidate(_) => Some(SmoothingMark {
            class: SmoothingClass::AgentCandidate,
            allowlisted: shape.paths().iter().all(|path| policy.admits(path)),
        }),
        Classification::NotSmoothable(_) => None,
    }
}

/// Admits the first story that conflicts with a member, merged again onto
/// the final batch, whose conflict is union-smoothable and whose every
/// conflicted path `policy` admits: it becomes the last member, so a batch
/// holds at most one resolution and every shorter prefix of its merge chain
/// holds none (SH-834 D5). Clean members were all taken first; none is ever
/// displaced.
fn admit_smoothable(
    preview: &mut BatchPreview,
    merger: &mut dyn TrialMerger,
    batch: &str,
    policy: &SmoothPolicy,
    member_conflicts: &[(usize, String)],
    deadline: Instant,
) {
    for (index, commit) in member_conflicts {
        if Instant::now() >= deadline {
            return;
        }
        let Ok(TrialMerge::Conflict { shape, .. }) = merger.merge(batch, commit) else {
            continue;
        };
        let Some(blobs) = merger.blobs() else {
            return;
        };
        let Classification::UnionSmoothable(files) = classify(&shape, blobs) else {
            continue;
        };
        if !admits_all(policy, &files) {
            continue;
        }
        let entry = preview.excluded.remove(*index);
        preview.members.push(PreviewMember {
            story_id: entry.story_id,
            commit: commit.clone(),
            smoothed: files.into_iter().map(|file| file.path).collect(),
        });
        return;
    }
}

/// The commit a story would be tried at, or why it cannot be tried.
fn standing_commit(
    candidate: &PreviewCandidate,
    merger: &mut dyn TrialMerger,
) -> Result<String, PreviewExclusion> {
    match &candidate.standing {
        Standing::Commit(commit) => Ok(commit.clone()),
        Standing::Ineligible { reason, detail } => {
            Err(exclusion(candidate, *reason, detail.clone()))
        }
        Standing::Branch(branch) => match merger.resolve(&format!("refs/heads/{branch}")) {
            Ok(Some(commit)) => Ok(commit),
            Ok(None) => Err(exclusion(
                candidate,
                ExclusionReason::Unsubmitted,
                format!("its leased branch {branch} names no commit"),
            )),
            Err(error) => Err(exclusion(
                candidate,
                ExclusionReason::TrialFailed,
                format!("resolving its branch {branch}: {error}"),
            )),
        },
    }
}

fn exclusion(
    candidate: &PreviewCandidate,
    reason: ExclusionReason,
    detail: impl Into<String>,
) -> PreviewExclusion {
    PreviewExclusion {
        story_id: candidate.story_id.clone(),
        reason,
        detail: Some(detail.into()),
        paths: Vec::new(),
        smoothing: None,
    }
}

fn conflict(
    candidate: &PreviewCandidate,
    reason: ExclusionReason,
    paths: &[String],
) -> PreviewExclusion {
    PreviewExclusion {
        story_id: candidate.story_id.clone(),
        reason,
        detail: (paths.len() > MAX_RECORDED_PATHS).then(|| {
            format!(
                "{} conflicted paths; the first {MAX_RECORDED_PATHS} are listed",
                paths.len()
            )
        }),
        paths: recorded(paths),
        smoothing: None,
    }
}

fn recorded(paths: &[String]) -> Vec<String> {
    paths.iter().take(MAX_RECORDED_PATHS).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use std::collections::{BTreeMap, VecDeque};

    /// A merger that answers `resolve` from a table and `merge` and `commit`
    /// from scripts, in call order: the failure paths real Git cannot be made
    /// to produce on demand.
    #[derive(Default)]
    struct Scripted {
        refs: BTreeMap<String, Result<Option<String>, String>>,
        merges: VecDeque<Result<TrialMerge, String>>,
        commits: VecDeque<Result<String, String>>,
    }

    impl TrialMerger for Scripted {
        fn resolve(&mut self, rev: &str) -> Result<Option<String>, AppError> {
            self.refs
                .get(rev)
                .cloned()
                .unwrap_or(Ok(None))
                .map_err(AppError::Storage)
        }
        fn merge(&mut self, _onto: &str, _head: &str) -> Result<TrialMerge, AppError> {
            self.merges
                .pop_front()
                .expect("an unscripted merge")
                .map_err(AppError::Storage)
        }
        fn commit(&mut self, _onto: &str, _head: &str, _tree: &str) -> Result<String, AppError> {
            self.commits
                .pop_front()
                .expect("an unscripted commit")
                .map_err(AppError::Storage)
        }
    }

    fn oid(digit: char) -> String {
        digit.to_string().repeat(40)
    }

    fn scripted(stories: &[&str]) -> Scripted {
        let mut merger = Scripted::default();
        merger
            .refs
            .insert("refs/remotes/origin/dev".into(), Ok(Some(oid('0'))));
        for (index, story) in stories.iter().enumerate() {
            merger.refs.insert(
                format!("refs/heads/worktree-{story}"),
                Ok(Some(oid(char::from_digit(index as u32 + 1, 10).unwrap()))),
            );
        }
        merger
    }

    fn request(rest: &[&str]) -> PreviewRequest {
        let candidate = |story: &str| PreviewCandidate {
            story_id: story.into(),
            standing: Standing::Branch(format!("worktree-{story}")),
        };
        PreviewRequest {
            computed_at: "2026-01-01T00:00:00Z".into(),
            head: candidate("SH-1"),
            rest: rest.iter().map(|story| candidate(story)).collect(),
            base_branch: Some("dev".into()),
            cap: 5,
            live_lanes: None,
            queue_depth: rest.len() + 1,
            deadline: Instant::now() + std::time::Duration::from_secs(600),
            smoothing: SmoothingMode::Admit,
        }
    }

    fn clean(digit: char) -> Result<TrialMerge, String> {
        Ok(TrialMerge::Clean { tree: oid(digit) })
    }

    fn conflicted(paths: Vec<String>) -> Result<TrialMerge, String> {
        Ok(TrialMerge::Conflict {
            paths,
            shape: crate::service::trial_merge::ConflictShape::default(),
        })
    }

    #[test]
    fn a_failed_trial_merge_is_reported_in_the_preview_and_the_sweep_goes_on() {
        let mut merger = scripted(&["SH-1", "SH-2", "SH-3"]);
        merger.merges = VecDeque::from([clean('a'), Err("git died".into()), clean('b')]);
        merger.commits = VecDeque::from([Ok(oid('c')), Ok(oid('d'))]);

        let preview = select(request(&["SH-2", "SH-3"]), &mut merger);

        assert_eq!(preview.outcome, PreviewOutcome::Batch);
        let members: Vec<_> = preview
            .members
            .iter()
            .map(|m| m.story_id.as_str())
            .collect();
        assert_eq!(members, ["SH-1", "SH-3"]);
        assert_eq!(preview.excluded.len(), 1);
        assert_eq!(preview.excluded[0].reason, ExclusionReason::TrialFailed);
        assert!(
            preview.excluded[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("git died")
        );
        assert_eq!(preview.head_tree, Some(oid('a')));
    }

    #[test]
    fn a_member_whose_merge_cannot_be_recorded_is_excluded_as_trial_failed() {
        let mut merger = scripted(&["SH-1", "SH-2"]);
        merger.merges = VecDeque::from([clean('a'), clean('b')]);
        merger.commits = VecDeque::from([Ok(oid('c')), Err("no space".into())]);

        let preview = select(request(&["SH-2"]), &mut merger);

        assert_eq!(preview.members.len(), 1);
        assert_eq!(preview.excluded[0].reason, ExclusionReason::TrialFailed);
    }

    #[test]
    fn a_head_that_cannot_be_merged_or_recorded_makes_the_preview_unavailable() {
        for (merges, commits) in [
            (vec![Err("git died".to_string())], vec![]),
            (vec![clean('a')], vec![Err("no space".to_string())]),
        ] {
            let mut merger = scripted(&["SH-1"]);
            merger.merges = merges.into();
            merger.commits = commits.into();
            let preview = select(request(&[]), &mut merger);
            assert_eq!(preview.outcome, PreviewOutcome::Unavailable, "{preview:?}");
            assert_eq!(preview.base_commit, Some(oid('0')));
            assert!(preview.members.is_empty());
        }
    }

    #[test]
    fn a_base_that_cannot_be_read_or_a_head_without_a_branch_is_unavailable() {
        let mut merger = scripted(&["SH-1"]);
        merger.refs.insert(
            "refs/remotes/origin/dev".into(),
            Err("repository gone".into()),
        );
        let preview = select(request(&[]), &mut merger);
        assert_eq!(preview.outcome, PreviewOutcome::Unavailable);
        assert!(preview.detail.unwrap().contains("repository gone"));

        let mut merger = scripted(&[]);
        let preview = select(request(&[]), &mut merger);
        assert_eq!(preview.outcome, PreviewOutcome::Unavailable);
        assert!(
            preview
                .detail
                .unwrap()
                .contains("the head SH-1 cannot be tried")
        );
    }

    #[test]
    fn more_conflicted_paths_than_the_record_keeps_are_counted() {
        let paths: Vec<String> = (0..25).map(|index| format!("src/{index}.rs")).collect();
        let mut merger = scripted(&["SH-1", "SH-2"]);
        merger.merges = VecDeque::from([clean('a'), conflicted(paths.clone()), conflicted(paths)]);
        merger.commits = VecDeque::from([Ok(oid('c'))]);

        let preview = select(request(&["SH-2"]), &mut merger);

        let entry = &preview.excluded[0];
        assert_eq!(entry.reason, ExclusionReason::ConflictWithBase);
        assert_eq!(entry.paths.len(), MAX_RECORDED_PATHS);
        assert_eq!(
            entry.detail.as_deref(),
            Some("25 conflicted paths; the first 20 are listed")
        );
        let compact = preview.compact();
        assert!(compact.excluded[0].paths.is_empty());
        assert_eq!(compact.excluded[0].detail, entry.detail);
    }

    #[test]
    fn describe_reads_each_outcome() {
        let mut merger = scripted(&["SH-1", "SH-2"]);
        merger.merges = VecDeque::from([clean('a'), conflicted(vec!["a".into()]), clean('b')]);
        merger.commits = VecDeque::from([Ok(oid('c'))]);
        let preview = select(request(&["SH-2"]), &mut merger);
        assert_eq!(
            preview.describe(),
            "SH-1 (cap 5, 2 queued); excluded SH-2 conflict-with-member"
        );

        let mut merger = scripted(&["SH-1"]);
        merger.merges = VecDeque::from([conflicted(vec!["a".into()])]);
        let preview = select(request(&[]), &mut merger);
        assert_eq!(
            preview.describe(),
            "SH-1 conflicts with its base; no batch forms (1 queued)"
        );

        let mut unsubmitted = request(&[]);
        unsubmitted.base_branch = None;
        assert_eq!(
            select(unsubmitted, &mut scripted(&["SH-1"])).describe(),
            "unavailable: no submission in this attempt named the base branch"
        );
    }

    #[test]
    fn the_wire_form_uses_kebab_case_slugs_and_omits_what_is_absent() {
        let mut merger = scripted(&["SH-1", "SH-2"]);
        merger.merges = VecDeque::from([clean('a'), clean('b')]);
        merger.commits = VecDeque::from([Ok(oid('c'))]);
        let mut capped = request(&["SH-2"]);
        capped.cap = 1;
        let preview = select(capped, &mut merger);
        let value = serde_json::to_value(&preview).unwrap();
        assert_eq!(value["outcome"], "batch");
        assert_eq!(value["excluded"][0]["reason"], "cap");
        assert!(value.get("live_lanes").is_none(), "{value}");
        assert!(value.get("head_conflict").is_none(), "{value}");
        assert!(value["excluded"][0].get("paths").is_none(), "{value}");
        let decoded: BatchPreview = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, preview);
        for reason in [
            ExclusionReason::ConflictWithBase,
            ExclusionReason::ConflictWithMember,
            ExclusionReason::Held,
            ExclusionReason::Blocked,
            ExclusionReason::LandingPending,
            ExclusionReason::Unsubmitted,
            ExclusionReason::Cap,
            ExclusionReason::TrialFailed,
        ] {
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                reason.as_str(),
                "as_str and the wire slug are one name"
            );
        }
    }
}
