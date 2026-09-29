//! The durable record of one verification batch (SH-831; spec B4, B5, B10).
//!
//! A batch is several submitted stories gated together on one merge-gate
//! branch. Its record is history plus the evidence a restarted verifier needs:
//! a batch that was still live when its verifier stopped is abandoned at the
//! next worker start. The record grants no authority over its member stories
//! (each stays an ordinary `verifying` story with its own generation), so it
//! is not an ownership-fence owner.

use super::{GlobalSeq, ProjectId, StoreError, StoryNo};
use crate::domain::gate_verdict::GateVerdict;
use serde::{Deserialize, Serialize};

/// Hex digits in a batch id: enough that two verifiers never pick the same
/// branch name, short enough to read in a branch or a title.
const BATCH_ID_HEX: usize = 12;

/// The branch prefix every batch branch lives under on origin.
pub const BATCH_BRANCH_PREFIX: &str = "storyhook/verify-batch/";

/// A batch's identity: 12 lowercase hex digits, also its branch name's leaf.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BatchId(String);

impl BatchId {
    /// A new random batch id.
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string()[..BATCH_ID_HEX].to_owned())
    }

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The batch's branch on origin: `storyhook/verify-batch/<id>`.
    #[must_use]
    pub fn branch(&self) -> String {
        format!("{BATCH_BRANCH_PREFIX}{}", self.0)
    }
}

impl TryFrom<String> for BatchId {
    type Error = StoreError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if text.len() == BATCH_ID_HEX
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(text))
        } else {
            Err(StoreError::Validation(format!(
                "a batch id is {BATCH_ID_HEX} lowercase hex digits, not {text:?}"
            )))
        }
    }
}

impl From<BatchId> for String {
    fn from(id: BatchId) -> Self {
        id.0
    }
}

impl std::fmt::Display for BatchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a batch is in its life. The first three are live; the last two end it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BatchPhase {
    /// Members are locked and submitted, and their merge commits are made.
    Assembled,
    /// The batch branch is on origin and the batch pull request is open.
    Submitted,
    /// The batch pull request's merge tree is being gated.
    Gating,
    /// The gate ended; landing is not built yet, so every member went back
    /// to the single-story queue.
    Released,
    /// The batch ended before a verdict could count: a member changed, the
    /// verifier stopped or failed, or its verifier restarted.
    Abandoned,
}

impl BatchPhase {
    /// Whether a batch in this phase may still move on.
    #[must_use]
    pub fn is_live(self) -> bool {
        matches!(self, Self::Assembled | Self::Submitted | Self::Gating)
    }

    /// Whether a batch may move from this phase to `next`: forward through
    /// the live phases, or to an end; never out of an end.
    #[must_use]
    pub fn may_become(self, next: Self) -> bool {
        match (self, next) {
            (current, next) if current == next => true,
            (Self::Assembled, Self::Submitted) | (Self::Submitted, Self::Gating) => true,
            (current, Self::Released | Self::Abandoned) => current.is_live(),
            _ => false,
        }
    }

    /// The phase's wire slug.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assembled => "assembled",
            Self::Submitted => "submitted",
            Self::Gating => "gating",
            Self::Released => "released",
            Self::Abandoned => "abandoned",
        }
    }
}

/// One story in a batch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchMember {
    /// The story's number in its project.
    pub story: StoryNo,
    /// The story's public id, including its prefix.
    pub story_id: String,
    /// The exact transition into `verifying` the batch took.
    pub generation: GlobalSeq,
    /// The commit merged into the batch: the head its submission pushed.
    pub head_commit: String,
    /// The story's own pull request, which the batch pull request links.
    pub pull_request: String,
    /// Queue order within the batch, from 0 (the head).
    pub position: u32,
}

/// Why a story the preview selected did not become a member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BatchExclusionReason {
    /// Another operation holds the story's workspace lock.
    WorkspaceBusy,
    /// Its submission was refused, with something its agent must fix.
    SubmissionRefused,
    /// Its submission failed independently of its code.
    SubmissionFailed,
    /// Its generation changed before its submission could be recorded.
    Superseded,
    /// It links a pull request other than the one its submission found.
    PullRequestMismatch,
    /// Its pushed head is not the commit the preview merged.
    HeadMoved,
    /// Its pull request targets another base branch.
    BaseMismatch,
}

/// A selected story that did not become a member, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchExclusion {
    /// The story's public id.
    pub story_id: String,
    /// Why it is not a member.
    pub reason: BatchExclusionReason,
    /// The fact behind the reason, as an operator reads it.
    pub detail: String,
}

/// The batch pull request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchPullRequest {
    /// Its URL.
    pub url: String,
    /// Its number.
    pub number: u64,
}

/// How the batch's gate ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchGate {
    /// The gate's verdict.
    pub verdict: GateVerdict,
    /// The merge tree the gate judged, when it judged one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    /// The gate's own words about the result.
    pub detail: String,
    /// How long the gate ran.
    pub seconds: u64,
}

/// One verification batch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationBatch {
    /// The batch's identity.
    pub id: BatchId,
    /// The project whose queue formed it.
    pub project: ProjectId,
    /// That project's slug when the batch formed.
    pub project_slug: String,
    /// The head story: the one the verifier dequeued.
    pub head: String,
    /// The base branch, origin's default, such as `dev`.
    pub base_branch: String,
    /// The commit of `origin/<base_branch>` the batch branch starts from.
    pub base_commit: String,
    /// `storyhook/verify-batch/<id>`.
    pub branch: String,
    /// The last merge commit of the batch branch.
    pub tip: String,
    /// The batch pull request, once it is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<BatchPullRequest>,
    /// Where the batch is in its life.
    pub phase: BatchPhase,
    /// Members in queue order, head first; always two or more.
    pub members: Vec<BatchMember>,
    /// Selected stories that did not become members.
    #[serde(default)]
    pub excluded: Vec<BatchExclusion>,
    /// How its gate ended, once it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<BatchGate>,
    /// Why the batch ended as it did, as an operator reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Whether its pull request is closed and its branch deleted on origin.
    pub retired: bool,
    /// Compare-and-swap revision, starting at zero.
    pub revision: i64,
    /// When the batch was recorded (UTC).
    pub created_at: String,
    /// When the record last changed (UTC).
    pub updated_at: String,
}

impl VerificationBatch {
    /// Refuses a record that breaks the batch's invariants: two or more
    /// members in positions 0.., head first, a branch named for the id, and
    /// full object ids.
    pub fn validate(&self) -> Result<(), StoreError> {
        let refuse = |why: &str| {
            Err(StoreError::Validation(format!(
                "verification batch {}: {why}",
                self.id
            )))
        };
        if self.members.len() < 2 {
            return refuse("a batch has two or more members");
        }
        if self.members[0].story_id != self.head {
            return refuse("the head is the first member");
        }
        if self
            .members
            .iter()
            .enumerate()
            .any(|(index, member)| member.position as usize != index)
        {
            return refuse("member positions run 0, 1, 2... in queue order");
        }
        let mut stories: Vec<StoryNo> = self.members.iter().map(|member| member.story).collect();
        stories.sort();
        stories.dedup();
        if stories.len() != self.members.len() {
            return refuse("a story is a member once");
        }
        if self.branch != self.id.branch() {
            return refuse("the branch is named for the batch id");
        }
        let pinned =
            |oid: &str| matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit());
        if !pinned(&self.base_commit)
            || !pinned(&self.tip)
            || self
                .members
                .iter()
                .any(|member| !pinned(&member.head_commit))
        {
            return refuse("base, tip and member heads are full object ids");
        }
        if self.revision < 0 {
            return refuse("the revision is never negative");
        }
        Ok(())
    }

    /// The next revision of this record at `now`, in `phase`. Refuses a move
    /// the phase order does not allow.
    pub fn advance(&self, phase: BatchPhase, now: &str) -> Result<Self, StoreError> {
        if !self.phase.may_become(phase) {
            return Err(StoreError::Validation(format!(
                "verification batch {} cannot move from {} to {}",
                self.id,
                self.phase.as_str(),
                phase.as_str()
            )));
        }
        let mut next = self.clone();
        next.phase = phase;
        next.revision = self.revision + 1;
        next.updated_at = now.to_owned();
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_move_forward_or_end_and_never_leave_an_end() {
        use BatchPhase::*;
        let all = [Assembled, Submitted, Gating, Released, Abandoned];
        for from in all {
            for to in all {
                let expected = from == to
                    || matches!((from, to), (Assembled, Submitted) | (Submitted, Gating))
                    || (from.is_live() && matches!(to, Released | Abandoned));
                assert_eq!(from.may_become(to), expected, "{from:?} -> {to:?}");
            }
        }
    }

    #[test]
    fn batch_ids_are_twelve_lowercase_hex_digits_and_name_their_branch() {
        let id = BatchId::generate();
        assert_eq!(id.as_str().len(), 12);
        assert_eq!(id.branch(), format!("storyhook/verify-batch/{id}"));
        assert_eq!(BatchId::try_from(id.to_string()).unwrap(), id);
        for bad in [
            "",
            "ABCDEF012345",
            "abcdef01234",
            "abcdef0123456",
            "abcdef01234g",
            "../../main1",
        ] {
            assert!(BatchId::try_from(bad.to_string()).is_err(), "{bad}");
        }
        let wire = serde_json::to_value(&id).unwrap();
        assert_eq!(wire, id.as_str());
        assert!(serde_json::from_value::<BatchId>(serde_json::json!("NOT-A-BATCH!")).is_err());
    }
}
