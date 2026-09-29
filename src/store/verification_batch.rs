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

mod bisection;
pub use bisection::{BatchBisection, BisectionOf, BisectionOutcome, BisectionProbe, ProbeKind};

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

/// Where a batch is in its life. The first four are live; the last three end
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BatchPhase {
    /// Members are locked and submitted, and their merge commits are made.
    Assembled,
    /// The batch branch is on origin and the batch pull request is open.
    Submitted,
    /// The batch pull request's merge tree is being gated.
    Gating,
    /// The gate certified the batch tree and every member holds a landing
    /// intent: the merge is requested, or its outcome is not yet confirmed
    /// (SH-832). Never abandoned: a restart recovers it from the intents.
    Landing,
    /// The batch merge landed with the certified tree and its members are
    /// done (a member a person holds keeps its landing intent).
    Landed,
    /// The gate ended without a landing: a verdict other than certified, a
    /// landing refused before its merge, or a merge that was never
    /// requested. Every member went back to the single-story queue, except
    /// what a bisection of a red batch did with them (SH-833): its culprit
    /// returned, its certified prefix landed through a probe batch. A
    /// certified probe that did not end the search is released too.
    Released,
    /// The batch ended before a verdict could count: a member changed, the
    /// verifier stopped or failed, or its verifier restarted.
    Abandoned,
}

impl BatchPhase {
    /// Whether a batch in this phase may still move on.
    #[must_use]
    pub fn is_live(self) -> bool {
        matches!(
            self,
            Self::Assembled | Self::Submitted | Self::Gating | Self::Landing
        )
    }

    /// Whether a live batch may be abandoned: every live phase except
    /// `landing`, whose outcome belongs to its landing intents (B10). Both
    /// the worker-start check and the abandonment itself read this one
    /// predicate, so a start never opens a write it then finds empty.
    #[must_use]
    pub fn is_abandonable(self) -> bool {
        self.is_live() && self != Self::Landing
    }

    /// Whether a batch may move from this phase to `next`: forward through
    /// the live phases, or to an end; never out of an end. Only a certified
    /// gate lands, only a landing lands, and a landing is never abandoned.
    #[must_use]
    pub fn may_become(self, next: Self) -> bool {
        match (self, next) {
            (current, next) if current == next => true,
            (Self::Assembled, Self::Submitted)
            | (Self::Submitted, Self::Gating)
            | (Self::Gating, Self::Landing)
            | (Self::Landing, Self::Landed | Self::Released) => true,
            (current, Self::Released | Self::Abandoned) => current.is_abandonable(),
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
            Self::Landing => "landing",
            Self::Landed => "landed",
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
    /// The member's own branch, whose copy on origin is deleted once the
    /// batch landed (SH-832 D8). Absent in records written before SH-832.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The batch merge commit that merged this member: the tip of the
    /// batch's prefix that ends with it (SH-833). Absent in records
    /// written before SH-833, which a bisection cannot use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_commit: Option<String>,
    /// That merge commit's tree: the tree a gate of the prefix judges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_tree: Option<String>,
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
    /// The bisection this batch is a probe of (SH-833); absent for a batch
    /// the queue formed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bisects: Option<BisectionOf>,
    /// The bisection of this red batch (SH-833).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bisection: Option<BatchBisection>,
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
        if self.members.iter().any(|member| {
            member
                .merge_commit
                .as_deref()
                .is_some_and(|oid| !pinned(oid))
                || member.merge_tree.as_deref().is_some_and(|oid| !pinned(oid))
        }) {
            return refuse("member merge commits and trees are full object ids");
        }
        if let Some(last) = self.members.last().and_then(|m| m.merge_commit.as_deref())
            && last != self.tip
        {
            return refuse("the last member's merge commit is the tip");
        }
        if let Some(of) = &self.bisects {
            if of.prefix < 2 || of.prefix as usize != self.members.len() {
                return refuse("a bisection probe has exactly its prefix of two or more members");
            }
            if of.parent == self.id {
                return refuse("a bisection probe is not its own parent");
            }
        }
        if let Some(bisection) = &self.bisection
            && bisection
                .probes
                .iter()
                .any(|probe| probe.prefix == 0 || probe.prefix as usize >= self.members.len())
        {
            return refuse("a bisection probes prefixes shorter than the batch");
        }
        Ok(())
    }

    /// Whether the verifier must settle this record when it did not end it
    /// itself: a live record it can abandon, or a bisection that never
    /// recorded how it ended. One predicate for the read that decides to
    /// write and for the write, so a worker start never opens an empty
    /// write (SH-693).
    #[must_use]
    pub fn needs_finalization(&self) -> bool {
        self.phase.is_abandonable()
            || self
                .bisection
                .as_ref()
                .is_some_and(BatchBisection::is_unfinished)
    }

    /// The members' merge commits and trees in order, when the record has
    /// them all (written by SH-833 or later): what a bisection gates.
    #[must_use]
    pub fn merge_chain(&self) -> Option<Vec<(String, String)>> {
        self.members
            .iter()
            .map(|member| Some((member.merge_commit.clone()?, member.merge_tree.clone()?)))
            .collect()
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

    fn oid(digit: char) -> String {
        digit.to_string().repeat(40)
    }

    /// A three-member batch whose members carry their merge chain.
    fn chained() -> VerificationBatch {
        let id = BatchId::try_from("0123456789ab".to_string()).unwrap();
        let members: Vec<BatchMember> = (0..3u32)
            .map(|position| BatchMember {
                story: StoryNo::new(i64::from(position) + 1),
                story_id: format!("SH-{}", position + 1),
                generation: GlobalSeq::new(10 + i64::from(position)),
                head_commit: oid(char::from_digit(position + 1, 10).unwrap()),
                pull_request: format!("https://github.com/acme/widgets/pull/{}", position + 1),
                position,
                branch: None,
                merge_commit: Some(oid(char::from_digit(position + 4, 10).unwrap())),
                merge_tree: Some(oid(['a', 'b', 'c'][position as usize])),
            })
            .collect();
        VerificationBatch {
            branch: id.branch(),
            id,
            project: ProjectId::new(1),
            project_slug: "widgets".into(),
            head: "SH-1".into(),
            base_branch: "dev".into(),
            base_commit: oid('0'),
            tip: oid('6'),
            pull_request: None,
            phase: BatchPhase::Assembled,
            members,
            excluded: Vec::new(),
            gate: None,
            detail: None,
            bisects: None,
            bisection: None,
            retired: false,
            revision: 0,
            created_at: "2026-09-29T00:00:00Z".into(),
            updated_at: "2026-09-29T00:00:00Z".into(),
        }
    }

    fn probe(prefix: u32) -> BisectionProbe {
        BisectionProbe {
            prefix,
            kind: ProbeKind::Search,
            batch: None,
            tree: oid('a'),
            verdict: GateVerdict::Certified,
            log: None,
            detail: "passed".into(),
            seconds: 1,
        }
    }

    #[test]
    fn a_record_written_before_sh_833_still_reads_and_has_no_merge_chain() {
        let mut wire = serde_json::to_value(chained()).unwrap();
        for member in wire["members"].as_array_mut().unwrap() {
            let member = member.as_object_mut().unwrap();
            member.remove("merge_commit");
            member.remove("merge_tree");
        }
        let old: VerificationBatch = serde_json::from_value(wire).unwrap();
        assert!(old.validate().is_ok());
        assert_eq!(old.merge_chain(), None);
        assert_eq!((&old.bisects, &old.bisection), (&None, &None));
        let text = serde_json::to_string(&old).unwrap();
        for absent in ["merge_commit", "merge_tree", "bisects", "bisection"] {
            assert!(
                !text.contains(absent),
                "{absent} is not written when absent: {text}"
            );
        }
    }

    #[test]
    fn the_merge_chain_lists_every_prefix_tip_and_tree_in_order() {
        let batch = chained();
        assert!(batch.validate().is_ok());
        assert_eq!(
            batch.merge_chain().unwrap(),
            vec![
                (oid('4'), oid('a')),
                (oid('5'), oid('b')),
                (oid('6'), oid('c'))
            ]
        );
        let mut partial = batch;
        partial.members[1].merge_tree = None;
        assert_eq!(partial.merge_chain(), None);
    }

    #[test]
    fn merge_data_and_bisection_links_are_validated() {
        let refused = |change: &dyn Fn(&mut VerificationBatch), why: &str| {
            let mut batch = chained();
            change(&mut batch);
            let error = batch.validate().unwrap_err().to_string();
            assert!(error.contains(why), "{error}");
        };
        refused(
            &|batch| batch.members[0].merge_commit = Some("abc".into()),
            "full object ids",
        );
        refused(
            &|batch| batch.members[1].merge_tree = Some("xyz".into()),
            "full object ids",
        );
        refused(&|batch| batch.tip = oid('9'), "merge commit is the tip");
        let parent = BatchId::try_from("fedcba987654".to_string()).unwrap();
        refused(
            &|batch| {
                batch.bisects = Some(BisectionOf {
                    parent: parent.clone(),
                    prefix: 2,
                });
            },
            "exactly its prefix",
        );
        refused(
            &|batch| {
                batch.bisects = Some(BisectionOf {
                    parent: batch.id.clone(),
                    prefix: 3,
                });
            },
            "its own parent",
        );
        for bad in [0, 3, 4] {
            refused(
                &|batch| {
                    batch.bisection = Some(BatchBisection {
                        probes: vec![probe(bad)],
                        outcome: None,
                    });
                },
                "shorter than the batch",
            );
        }
        let mut probe_batch = chained();
        probe_batch.bisects = Some(BisectionOf { parent, prefix: 3 });
        probe_batch.bisection = Some(BatchBisection {
            probes: vec![probe(1), probe(2)],
            outcome: None,
        });
        assert!(probe_batch.validate().is_ok());
    }

    #[test]
    fn a_record_needs_finalization_when_live_or_its_bisection_never_ended() {
        let mut batch = chained();
        assert!(batch.needs_finalization(), "assembled is abandonable");
        batch.phase = BatchPhase::Landing;
        assert!(
            !batch.needs_finalization(),
            "a landing belongs to its intents"
        );
        batch.phase = BatchPhase::Released;
        assert!(!batch.needs_finalization());
        batch.bisection = Some(BatchBisection::default());
        assert!(batch.needs_finalization(), "a bisection with no outcome");
        batch.bisection = Some(BatchBisection {
            probes: Vec::new(),
            outcome: Some(BisectionOutcome::Inconclusive {
                detail: "the base moved".into(),
            }),
        });
        assert!(!batch.needs_finalization());
    }

    #[test]
    fn bisection_outcomes_have_a_kind_tag_on_the_wire() {
        let culprit = BisectionOutcome::Culprit {
            story_id: "SH-2".into(),
            position: 2,
            tree: oid('b'),
            log: "/tmp/gate.log".into(),
            certified: 1,
            detail: "returned".into(),
        };
        let wire = serde_json::to_value(&culprit).unwrap();
        assert_eq!(wire["kind"], "culprit");
        assert_eq!(
            serde_json::from_value::<BisectionOutcome>(wire).unwrap(),
            culprit
        );
        let interrupted = serde_json::to_value(BisectionOutcome::Interrupted {
            detail: "restart".into(),
        })
        .unwrap();
        assert_eq!(interrupted["kind"], "interrupted");
        assert_eq!(serde_json::to_value(ProbeKind::Landing).unwrap(), "landing");
    }

    #[test]
    fn phases_move_forward_or_end_and_never_leave_an_end() {
        use BatchPhase::*;
        let all = [
            Assembled, Submitted, Gating, Landing, Landed, Released, Abandoned,
        ];
        for from in all {
            for to in all {
                let expected = from == to
                    || matches!(
                        (from, to),
                        (Assembled, Submitted)
                            | (Submitted, Gating)
                            | (Gating, Landing)
                            | (Landing, Landed | Released)
                    )
                    || (matches!(from, Assembled | Submitted | Gating)
                        && matches!(to, Released | Abandoned));
                assert_eq!(from.may_become(to), expected, "{from:?} -> {to:?}");
            }
        }
        assert!(Landing.is_live() && !Landing.is_abandonable());
        assert!(!Landed.is_live());
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
