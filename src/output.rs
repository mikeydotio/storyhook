use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::{
    CommentMention, CommitReference, Member, Priority, ProgressRollup, StateDef, StoryRelation,
    StorySnapshot, SuperState,
};
use crate::error::AppError;
use crate::store::{EngineAgent, EngineLaneState, EngineRunState, EngineScope, PrLink};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StaleInfo {
    pub last_activity_at: String,
    pub last_activity_type: String,
    pub days_stale: u64,
}

/// Everything that names a story without living in its own comment thread
/// (SH-169): the commits `commit-sync` found, the pull requests `story
/// link-pr` recorded, and the comments *other* stories wrote naming this one
/// (SH-220). Assembled at query time, not folded — `commits` is copied from
/// the story's own [`StorySnapshot::referenced_by_commits`], `prs` comes from
/// a separate store read (`ReadOps::pr_links`), and `comment_mentions` is
/// derived from the project's folded comment threads
/// ([`domain::derive_comment_mentions`](crate::domain::derive_comment_mentions)) —
/// the same split [`StoryView::derived_relationships`] draws between what a
/// story's own event log says and what a cross-story read adds.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ReferencedBy {
    /// Commits `commit-sync` found naming this story, oldest first — folded
    /// from the story's own event log, so present on every view regardless
    /// of `include_derived` (see `service::query::story_views`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<CommitReference>,
    /// Pull requests `story link-pr` recorded against this story, any
    /// status — a project-wide store read, so only populated when
    /// `include_derived` is set (`story show`, not `story list`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prs: Vec<PrLink>,
    /// Comments on *other* stories that named this one, oldest first — a
    /// project-wide scan, so gated on `include_derived` exactly as `prs` is,
    /// and never stored anywhere (SH-220).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comment_mentions: Vec<CommentMention>,
}

impl ReferencedBy {
    /// Whether there is nothing here to show — which is what omits the whole
    /// block from `--json` and from `story show`.
    ///
    /// Every list must be named: a story reachable only by a comment mention
    /// would otherwise serialize an empty `referenced_by` object rather than
    /// no `referenced_by` at all.
    fn is_empty(&self) -> bool {
        self.commits.is_empty() && self.prs.is_empty() && self.comment_mentions.is_empty()
    }

    /// `commits` alone — the shape every caller that skips the project-wide
    /// reads builds (`query::bare_view`, `transfer::import_project`), so as
    /// not to repeat the same field-by-field literal at each one.
    pub fn commits_only(commits: Vec<CommitReference>) -> Self {
        Self {
            commits,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoryView {
    pub story: StorySnapshot,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived_relationships: Vec<StoryRelation>,
    #[serde(default, skip_serializing_if = "ReferencedBy::is_empty")]
    pub referenced_by: ReferencedBy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flagged_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_info: Option<StaleInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<ProgressRollup>,
    /// Where the Web board should place this story's card, when that differs
    /// from `story.state`. One promotion
    /// ([`crate::domain::compute_display_state`]): a story sitting in the
    /// project's default open state that
    /// [`needs intervention`](crate::domain::needs_intervention) — a person
    /// has to clear it, not the backlog simply proceeding (SH-487, narrowing
    /// SH-407's original `!is_ready`) — promotes to `"blocked"`. An epic's
    /// own state is instead projected directly onto `story.state` by
    /// [`crate::domain::apply_computed_epic_states`] (SH-165/SH-446), which
    /// is why this field never carries an epic's promotion. `None` means
    /// "use `story.state`" — the CLI and TUI do exactly that today, so this
    /// field is additive and changes nothing for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_state: Option<String>,
    /// The change-feed position of the event this story's row was folded from
    /// (`stories.head_global_seq`, SH-336) — the exact tiebreak for a recency
    /// ordering, since every storyhook timestamp is RFC3339 at one-second
    /// precision and a burst of writes inside one second is this tracker's
    /// normal workload.
    ///
    /// `None` from a view built without a row read (`query::bare_view`,
    /// `transfer::import_project`) or from an older daemon. A consumer must
    /// fall back to its previous tiebreak when either side of a comparison is
    /// absent, exactly as `display_state`'s `None` means "use `story.state`".
    /// Deliberately not on [`StorySnapshot`] itself: that type is the fold of
    /// a story's events, serialized verbatim into the store's own `snapshot`
    /// column and compared field-by-field against a fresh fold by `story
    /// doctor` — a non-fold field there would report every story as
    /// divergent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_global_seq: Option<crate::store::GlobalSeq>,
}

/// Structured scope in the `story engine … --json` run object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineScopeView {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epic: Option<String>,
}

/// One lane as presented by the engine control surfaces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineLaneView {
    pub index: u32,
    pub state: EngineLaneState,
    pub story: Option<String>,
    pub elapsed_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome_detail: Option<String>,
}

/// One `no-auto` story this run deliberately leaves for a person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineNeedsHumanView {
    pub id: String,
    pub title: String,
}

/// One engine run, shared by all six CLI controls and their JSON envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineRunView {
    pub id: String,
    pub scope: EngineScopeView,
    pub agent: EngineAgent,
    pub state: EngineRunState,
    pub lane_count: u32,
    pub consecutive_hard_stops: u32,
    pub stop_reason: Option<String>,
    pub acknowledged_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub lanes: Vec<EngineLaneView>,
    pub needs_human: Vec<EngineNeedsHumanView>,
}

impl EngineRunView {
    /// Converts the service's durable timestamps into a presentation-relative
    /// elapsed value once, before the response crosses the daemon wire.
    pub fn from_service(view: crate::service::engine::RunView, now: &str) -> Self {
        let now = chrono::DateTime::parse_from_rfc3339(now).ok();
        let scope = match view.run.scope {
            EngineScope::Project => EngineScopeView {
                kind: "project".to_string(),
                epic: None,
            },
            EngineScope::Epic(epic) => EngineScopeView {
                kind: "epic".to_string(),
                epic: Some(epic),
            },
        };
        let lanes = view
            .lanes
            .into_iter()
            .map(|lane| {
                let elapsed_seconds = lane.dispatched_at.as_deref().and_then(|started| {
                    let started = chrono::DateTime::parse_from_rfc3339(started).ok()?;
                    Some(
                        now.as_ref()?
                            .signed_duration_since(started)
                            .num_seconds()
                            .max(0) as u64,
                    )
                });
                EngineLaneView {
                    index: lane.lane_index,
                    state: lane.state,
                    story: lane.story_id,
                    elapsed_seconds,
                    outcome: lane.outcome,
                    outcome_detail: lane.outcome_detail,
                }
            })
            .collect();
        Self {
            id: view.run.id,
            scope,
            agent: view.run.agent,
            state: view.run.state,
            lane_count: view.run.lanes,
            consecutive_hard_stops: view.run.consecutive_hard_stops,
            stop_reason: view.run.stop_reason,
            acknowledged_at: view.run.acknowledged_at,
            created_at: view.run.created_at,
            updated_at: view.run.updated_at,
            lanes,
            needs_human: view
                .skipped_no_auto
                .into_iter()
                .map(|story| EngineNeedsHumanView {
                    id: story.id,
                    title: story.title,
                })
                .collect(),
        }
    }
}

/// What `story project delete` would destroy, counted before anything is.
///
/// The payload of the only [`Response`] that is a *question* rather than an
/// answer. An unforced delete returns it without writing anything; it travels
/// to whichever process has a terminal and becomes a prompt there — or, with
/// `--json` or no terminal, a refusal naming `--force`.
///
/// Typed rather than prose because it has two front-ends. The dashboard builds
/// its own warning and gates its own button from these numbers, and a
/// pre-rendered English sentence would leave it parsing one. That both of them
/// render *this* value is what stops the CLI and the browser from growing two
/// different ideas of what delete does.
///
/// It carried two more fields until SH-117 — the repository files the verb was
/// about to remove, and the ones it had decided to keep. There are none of
/// either now: `delete` touches no filesystem, so there is nothing to promise
/// about one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeletePlan {
    /// The project's slug — what the user must type to confirm.
    pub slug: String,
    /// Its display name.
    pub name: String,
    /// Its story-id prefix, so the warning can say `SH-1…SH-40`.
    pub prefix: String,
    /// How many stories go, deleted and archived ones included.
    pub stories: usize,
    /// How many events go. The irreversible number.
    pub events: usize,
    /// Every checkout the store records, whether or not it still exists.
    ///
    /// Listed because each is a directory that will be left carrying a
    /// `.storyhook.toml` naming a project that no longer exists. Nothing
    /// deletes them; saying so is the whole of what this list is for.
    pub checkouts: Vec<String>,
}

/// What `story delete` would destroy, read before anything is.
///
/// The sibling of [`DeletePlan`], and typed for the same reason: the numbers
/// are the warning, and a pre-rendered English sentence would leave a second
/// front-end parsing prose.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoryDeletePlan {
    /// The story's id — what the user must type to confirm.
    pub id: String,
    /// Its title, so the person confirming can tell it is the right story.
    pub title: String,
    /// How many events go. The irreversible number.
    pub events: usize,
    /// The edges surviving stories still claim into this one, as `(story id,
    /// relation)`. Each is retracted with a real `StoryRelationshipRemoved`
    /// event before the story goes — otherwise the rebuild oracle reports a
    /// divergence that `doctor --fix` can never repair, because the story the
    /// claim names is not there to re-link.
    pub retracted: Vec<(String, String)>,
}

/// What `story unclaim` did, or would do (SH-483).
///
/// One type for both the real release and its `--dry-run` plan: the two
/// answer the identical question and a second struct would only be an
/// opportunity for them to drift.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnclaimOutcome {
    /// The story released, canonicalized.
    pub id: String,
    /// The project's active-role state — the one it was released *from*.
    pub from: String,
    /// Where it landed: the state it was claimed from, or
    /// [`UnclaimFallback`]'s destination when the replay could not answer.
    pub restored_to: String,
    /// Why [`restored_to`](Self::restored_to) is the fallback rather than the
    /// state the story was claimed from. `None` on an ordinary release.
    ///
    /// Never silent: a substituted destination is a wrong answer stored about
    /// where the work came from, so it is reported in the result on every
    /// path and in the default comment on the path that writes one.
    pub fallback: Option<UnclaimFallback>,
}

/// Why `story unclaim` could not restore the state a story was claimed from
/// (SH-483).
///
/// Three cases, all real, and each one names what it could not use rather
/// than merely admitting it substituted something.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnclaimFallback {
    /// The story was created directly in the active state — `story new
    /// --state in-progress` — so there is no earlier state to go back to.
    NoPriorState,
    /// The state it was claimed from has since been removed from the
    /// project's vocabulary (`story state remove`).
    PriorStateRemoved(String),
    /// The state it was claimed from is no longer an OPEN state, so
    /// restoring the story to it would close the story.
    PriorStateClosed(String),
}

impl UnclaimFallback {
    /// The stable slug a `--json` caller branches on.
    ///
    /// A code rather than the sentence below, for the reason SH-372 settled
    /// one field over: a consumer that has to pattern-match prose is a
    /// consumer that breaks when the prose is improved.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoPriorState => "no-prior-state",
            Self::PriorStateRemoved(_) => "prior-state-removed",
            Self::PriorStateClosed(_) => "prior-state-closed",
        }
    }

    /// The clause the human rendering and the default comment both end in.
    ///
    /// `active` is the state being released, which only the first case needs
    /// to name — and it needs to name it because "created directly in it" is
    /// meaningless without it.
    #[must_use]
    pub fn explain(&self, active: &str) -> String {
        match self {
            Self::NoPriorState => {
                format!(
                    "this story was created in {active}, so there is no earlier state to restore"
                )
            }
            Self::PriorStateRemoved(slug) => {
                format!(
                    "the state it was claimed from ({slug}) is no longer defined by this project"
                )
            }
            Self::PriorStateClosed(slug) => {
                format!("the state it was claimed from ({slug}) is no longer open")
            }
        }
    }
}

/// What a bulk "Archive" on a CLOSED-superstate column would hide, read
/// before anything is (SH-43).
///
/// The dry-run half of the two-phase preview/commit contract the council
/// mandated: every surface (web, CLI, TUI) confirms off the exact same
/// `ids` this plan names, then passes them back to commit, rather than each
/// surface recomputing "what's in this column right now" independently
/// between the two calls — which could hide a story nobody confirmed if
/// another change landed the story into this state in between.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HideStatePlan {
    /// The state slug the button was pressed on — what the person confirming
    /// sees named back to them.
    pub state: String,
    /// Every story currently in `state` and not already hidden, in the same
    /// order `hide_state` will archive them.
    pub ids: Vec<String>,
}

/// What `story project set-prefix` would rewrite, read before anything is.
///
/// Unlike [`DeletePlan`] and [`StoryDeletePlan`] this is not a plan to destroy
/// anything — every story, event and relationship survives. What is
/// irreversible is the *prefix itself*: every id a person or a script has
/// already written down under `old_prefix` stops resolving the moment this
/// runs, which is a different kind of one-way door and still one this gate
/// belongs in front of.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetPrefixPlan {
    /// The project's slug, so the person confirming can tell it is the right
    /// project — the prefix itself is about to stop being a reliable way to
    /// name it.
    pub slug: String,
    /// Its display name.
    pub name: String,
    /// The prefix every existing id was minted under.
    pub old_prefix: String,
    /// The prefix every id renders under from this point on — what the user
    /// must type to confirm.
    pub new_prefix: String,
    /// How many stories will be refolded so their rendered `id` reflects
    /// `new_prefix`. Every story in the project, deleted and archived ones
    /// included — `id` is a read-model artifact of all of them, not just the
    /// open ones.
    pub stories: usize,
    /// How many relationships will each be rewritten by one retracting and
    /// one re-asserting event, since a relationship's `other_id` is folded
    /// verbatim from the event that set it and does not re-derive from the
    /// project's current prefix the way a story's own `id` does.
    pub relationships: usize,
}

/// What a destructive command is about to do, in the shape its own kind of
/// destruction needs.
///
/// The payload of [`Response::ConfirmationRequired`], and the reason there is
/// one prompt rather than one per verb. Everything the gate does — refuse under
/// `--json`, refuse with no terminal, ask for a typed token, name `--force` —
/// is identical whatever is being destroyed; only the warning and the token
/// differ. Two copies of that logic is two prompts that drift apart, and the
/// one that drifts is the one used least.
///
/// Internally tagged, so the document says which kind it is rather than
/// leaving a reader to infer it from which fields are present. The tag is
/// additive: a delete plan carries every field it still has.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "confirm", rename_all = "kebab-case")]
pub enum ConfirmationPlan {
    /// `story project delete` — a project and everything recorded against it.
    Delete(DeletePlan),
    /// `story delete` — one story and everything recorded against it.
    DeleteStory(StoryDeletePlan),
    /// `story project set-prefix` — every id this project has ever minted.
    SetPrefix(SetPrefixPlan),
    /// A bulk "Archive" of every story in a CLOSED-superstate column
    /// (SH-43).
    HideState(HideStatePlan),
}

impl ConfirmationPlan {
    /// What the user must type, exactly, to go through with this.
    ///
    /// Also what the refusal names, so a caller reading a warning about `X`
    /// is reading the same `X` they would have had to type.
    #[must_use]
    pub fn token(&self) -> &str {
        match self {
            Self::Delete(plan) => &plan.slug,
            Self::DeleteStory(plan) => &plan.id,
            Self::SetPrefix(plan) => &plan.new_prefix,
            Self::HideState(plan) => &plan.state,
        }
    }

    /// The one-sentence fragment naming *what* this would do, for the
    /// terminal prompt that precedes the full plan.
    ///
    /// Factored out because [`Self::Delete`] and [`Self::DeleteStory`] are both
    /// permanent deletions, [`Self::SetPrefix`] destroys nothing — it makes
    /// every id already written down under the old prefix stop resolving —
    /// while bulk archive is reversible. A prompt that called one by another's
    /// headline would describe an act that never happens.
    #[must_use]
    pub fn headline(&self) -> String {
        match self {
            Self::Delete(_) | Self::DeleteStory(_) => {
                format!("this would permanently delete `{}`", self.token())
            }
            Self::SetPrefix(plan) => format!(
                "this would rename every `{}` id in `{}` to `{}`",
                plan.old_prefix, plan.slug, plan.new_prefix
            ),
            Self::HideState(plan) => format!(
                "this would archive {} stor{} currently in `{}`",
                plan.ids.len(),
                if plan.ids.len() == 1 { "y" } else { "ies" },
                plan.state
            ),
        }
    }

    /// Whether confirming this plan means typing [`Self::token`] back
    /// verbatim, or a plain `y`/`yes` is enough.
    ///
    /// [`Self::Delete`], [`Self::DeleteStory`] and [`Self::SetPrefix`] are each a
    /// one-way door, so the gate matches: prove the token was read by typing
    /// it. [`Self::HideState`] is reversible with `story unhide`, story by
    /// story, so one keystroke is the right weight.
    #[must_use]
    pub fn requires_typed_confirmation(&self) -> bool {
        !matches!(self, Self::HideState(_))
    }

    /// The yes/no prompt for a reversible plan. Kept with the plan so a new
    /// non-typed variant cannot inherit words describing a different action.
    #[must_use]
    pub fn confirmation_question(&self) -> &str {
        match self {
            Self::HideState(_) => "Archive these stories? [y/N] ",
            Self::Delete(_) | Self::DeleteStory(_) | Self::SetPrefix(_) => {
                "Confirm this operation? [y/N] "
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SummaryView {
    pub total_open: usize,
    pub total_closed: usize,
    pub by_state: Vec<(String, usize)>,
    pub by_priority: Vec<(String, usize)>,
    pub by_type: Vec<(String, usize)>,
    pub blocked_count: usize,
    pub flagged_count: usize,
    pub ready_count: usize,
    pub ready_stories: Vec<StoryView>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReportData {
    pub summary: SummaryView,
    pub stories: Vec<StoryView>,
    pub ready_ids: Vec<String>,
    pub blocked_ids: Vec<String>,
    /// The ids `story next --count N` would hand out, in the exact order it
    /// would hand them out. The first is claimable now; each later id becomes
    /// claimable after every preceding id virtually completes, with each
    /// available frontier ordered by own priority, parent epic priority, then
    /// story number (all ascending)
    /// ([`crate::domain::ready_order`]), over leaf stories only
    /// ([`crate::domain::has_children`] excludes an epic). Unlike
    /// [`Self::ready_ids`] (the unsorted, immediately claimable set driving
    /// ready/blocked board badges), this is what the web dashboard's "Next" board
    /// sort and List "Order" column need: the browser cannot reach `story
    /// next` itself (`/api/v1/invoke` is loopback- and master-token-gated),
    /// so the server computes the queue once and ships the order (SH-407,
    /// SH-450).
    pub next_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critical_path: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_chain: Option<BlockedChainView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_groups: Option<Vec<Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview: Option<GraphOverview>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockedChainView {
    pub source: String,
    pub blocked: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphOverview {
    pub total_open: usize,
    pub total_edges: usize,
    pub roots: Vec<String>,
    pub leaves: Vec<String>,
}

/// A whole project in one value: its catalog, its people, and its open
/// stories.
///
/// What a client that holds a *model* needs, as opposed to a client that asks
/// a question. The TUI rebuilds its board from one of these after every
/// change; before the seam it made five separate reads and could observe a
/// different instant in each.
///
/// Open stories only, deliberately. That is the set the board renders, and
/// carrying the archive would make the payload grow without bound in exchange
/// for rows nothing displays.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSnapshotView {
    /// The project's slug — what a daemon change event names
    /// (`Change::Project`, `src/daemon/bus.rs`), so a client holding this
    /// snapshot can tell its own project's activity from everyone else's.
    pub slug: String,
    /// The project's story-id prefix.
    pub prefix: String,
    /// The state catalog, in configured order — which is the order a board
    /// puts its columns in, so it is not merely a set.
    pub states: Vec<StateDef>,
    /// The project's members, for resolving an assignee client-side.
    pub members: Vec<Member>,
    /// Every unarchived, non-draft story — what the board renders.
    pub stories: Vec<StorySnapshot>,
    /// Every draft (SH-175), carried separately rather than folded into
    /// [`stories`](Self::stories): the Drafts popover and its count badge are
    /// the only thing that reads this, and the board must never render one of
    /// these as a card. Two lists made that unrepresentable instead of a flag
    /// per story a board-rendering call site could forget to check.
    #[serde(default)]
    pub drafts: Vec<StorySnapshot>,
    /// Each story's `head_global_seq` (SH-336), keyed by id, covering
    /// [`stories`](Self::stories) and [`drafts`](Self::drafts) alike.
    ///
    /// Carried beside the snapshots rather than on them, for the same reason
    /// [`StoryView::head_global_seq`] is not on [`StorySnapshot`]: that type
    /// is the fold of a story's events and nothing else. This is the TUI's
    /// only wire type — it never sees a [`StoryView`] — so this is where its
    /// recency comparator's tiebreak has to arrive. `#[serde(default)]` so an
    /// older daemon answering a newer TUI degrades to today's behaviour
    /// (every story absent from the map) rather than failing to deserialize.
    #[serde(default)]
    pub head_global_seqs: std::collections::BTreeMap<String, crate::store::GlobalSeq>,
}

/// Which project a command resolved to, and where its repo-side work runs.
///
/// **The answer to a question nothing else could ask.** `story project list`
/// enumerates every project as a block of prose; it cannot say which one *you*
/// are, and it is dispatched without a [`Ctx`](crate::service::Ctx) so it cannot
/// honour `--project` either. This is the scoped singular.
///
/// Its consumer is `plugins/story/bin/story.sh`, which needs the slug and
/// the directory **in one round trip** — the slug so it can pin `--project`
/// before it changes directory, and the directory so it can go there. Splitting
/// them across two calls would mean re-resolving after the `cd`, from a working
/// directory that is entitled to answer differently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectView {
    /// The handle `--project` takes.
    pub slug: String,
    /// The display name.
    pub name: String,
    /// The story-id prefix, minted into every id this project will ever have.
    pub prefix: String,
    /// Where this project's repo-side operations execute, if it has a checkout
    /// on this machine.
    ///
    /// **Never `skip_serializing_if`.** A script must be able to tell `null` —
    /// "this project has no checkout here", the ordinary state of six of the
    /// fourteen projects in the author's own store — from the field having been
    /// renamed out from under it. Those two want opposite reactions, and a
    /// missing key cannot distinguish them.
    ///
    /// Reported exactly as recorded, unchecked: whether the directory is still
    /// there is `story doctor`'s question, and answering it here would put a
    /// filesystem probe on the path of a command that is meant to be a lookup.
    pub checkout: Option<PathBuf>,
    /// The origins registered to this project — the only thing project
    /// selection ever consults.
    pub origins: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseView {
    pub phase: String,
    pub title: Option<String>,
    pub total: usize,
    pub done: usize,
    pub in_progress: usize,
    pub todo: usize,
    pub blocked: usize,
    pub story_ids: Vec<String>,
}

/// How a project setting's value is spelled, and therefore how a written one
/// is validated.
///
/// Carried on the wire beside the value so a reader knows how to interpret a
/// string it did not type. Deliberately *not* a serialized `serde_json::Value`
/// per setting: see [`SettingView::value`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingKind {
    /// `true` or `false`.
    Boolean,
    /// A duration such as `14d`, in the form `story commit-sync --since` takes.
    Duration,
    /// A structured document another command owns. Reported as presence only.
    Document,
}

/// Where a project setting's effective value comes from.
///
/// Three-valued rather than an `is_default` flag, because two of the three are
/// otherwise indistinguishable and the difference matters:
/// `sync.auto_transition` unset means `true` is in force, while
/// `doctor.stale_threshold` unset means *nothing* is in force. A boolean
/// reports both as "not set by you" and so lies about the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingSource {
    /// Written on this project. [`SettingView::value`] is what was written.
    Set,
    /// Never written, but the code supplies a default that is in force.
    Default,
    /// Never written, and nothing applies in its absence.
    Unset,
}

/// One project setting, as `story project settings` reports it.
///
/// Everything a renderer needs travels here, including the prose — the
/// description, the owning command, the "nothing reads this yet" note — so
/// that the CLI and any other front end describe a setting the same way, and
/// so that the annotations come from the registry row rather than from a
/// string written at a render site. A hand-written label is one that survives
/// the day the setting starts having an effect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingView {
    /// The dotted name a user types, such as `sync.auto_transition`.
    pub key: String,
    /// How the value is spelled.
    pub kind: SettingKind,
    /// The value in force, or `None` when nothing is.
    ///
    /// A string rather than a typed JSON value, in every kind — the same
    /// bargain `git config --list` makes, with [`kind`](Self::kind) naming how
    /// to read it. A typed value would buy a `jq` consumer one `tonumber` and
    /// would tempt [`SettingKind::Document`] into serializing a document's
    /// shape directly — making the surface a property of whichever feature-
    /// gated module happens to own that document's type, rather than of the
    /// data model alone (`story github-sync`'s own document was the case
    /// that made this concrete, before SH-408 retired it).
    pub value: Option<String>,
    /// Whether [`value`](Self::value) was written, defaulted, or is absent.
    pub source: SettingSource,
    /// What applies when nothing is written, if anything does.
    pub default: Option<String>,
    /// Whether `story project settings set` accepts this key.
    pub settable: bool,
    /// For a key no user may write, the command that owns its value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_by: Option<String>,
    /// One line saying what the setting is for.
    pub description: String,
    /// A caveat that belongs to the key itself — currently, that nothing reads
    /// it yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One line of `story log` — what happened to a story, and what did it
/// (SH-246).
///
/// The **auditable** view of an event, as opposed to
/// [`Response::StoryHistory`]'s raw one: it carries the two provenance columns
/// as separate fields rather than a rendered string, so a `--json` consumer can
/// tell an attested `command` from a self-attested `actor` without parsing
/// prose — which is the whole distinction the feature exists to preserve.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    /// Position within the story, so two events in the same second still have
    /// an unambiguous order.
    pub seq: i64,
    /// The event's own timestamp.
    pub at: String,
    /// The event's kind discriminant, e.g. `StoryStateChanged`.
    pub kind: String,
    /// What changed, in one phrase — `state → in-progress`. `None` for a kind
    /// this binary does not recognise, whose payload it cannot summarise but
    /// whose existence it still reports (SH-54).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The verb the daemon dispatched. `null` for an event written before
    /// SH-246, or replayed by `migrate`/`import-project`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// What the caller declared itself to be. `null` when it declared nothing —
    /// never filled in from [`command`](Self::command), because "said nothing"
    /// and "said `move`" are different facts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
}

/// Everything a command can return, before any rendering decision is made.
///
/// This is the **wire envelope**: `app::run` produces it, and every renderer
/// in this module consumes it. Its serde form is deliberately *not* the
/// `--json` envelope [`render_json`] emits — that one is a presentation
/// format aimed at a human's `jq`, this one is a transport format aimed at
/// another storyhook process. Externally tagged so the variant travels as
/// the key (`{"story": {…}}`), which keeps every payload a plain object
/// rather than a variant-name-and-payload pair.
///
/// The round trip is load-bearing: `render_response` of a `Response` and of
/// that same `Response` after a serialize/deserialize hop must produce
/// identical bytes, in all four `(json, quiet)` combinations. That property
/// is what makes carrying this envelope over HTTP output-preserving *by
/// construction* rather than by inspection, and it is pinned in
/// `tests/wire_envelope.rs`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    Message(String),
    /// A plain-text result plus one or more non-fatal warnings about what it
    /// did **not** do.
    ///
    /// A second variant rather than a field added to [`Message`](Self::Message):
    /// that variant has dozens of call sites answering with nothing to warn
    /// about, and widening it would make every one of them decide what to pass.
    /// The warnings ride in the JSON envelope's own `warnings` field — the same
    /// one [`StoryView::warnings`] already populates — rather than folded into
    /// `message`'s prose, so a scripted `--json` caller reads them as data
    /// instead of having to parse a sentence.
    MessageWithWarnings(String, Vec<String>),
    Story(Box<StoryView>),
    /// `story claim` (SH-476): the story claimed, and the state it was
    /// claimed *from* — a fact only this command produces, since every other
    /// writer already knows the state it started from without being told.
    ///
    /// Renders into the same `.story` key [`Story`](Self::Story) does, plus
    /// one more envelope field (`claimed_from`), so every existing
    /// `.story.story.id` consumer reads a claim exactly like a `next`.
    Claimed(Box<StoryView>, String),
    /// `story unclaim` (SH-483): the story released, and what became of it.
    ///
    /// The mirror of [`Claimed`](Self::Claimed) and rendered the same way —
    /// the same `.story` key, plus envelope fields naming the state it left
    /// and, when the destination was substituted, why.
    Unclaimed(Box<StoryView>, Box<UnclaimOutcome>),
    /// Several stories at once — `story list`, `story next` with a count,
    /// `story import`, `story decompose`, `story epic list`.
    ///
    /// A struct variant since SH-358, not the original `(Vec<StoryView>,
    /// Option<String>)`: a batch creator (`import`, `decompose`) needed a
    /// warnings channel of its own — [`StoryView::warnings`] answers for one
    /// story, and a bulk file has no single story to hang a batch-level
    /// warning on. [`Response::StoryLog`] is the in-repo precedent for naming
    /// fields here rather than leaving a third positional slot for callers to
    /// guess at.
    Stories {
        views: Vec<StoryView>,
        message: Option<String>,
        /// Batch-level warnings — never per-story; those ride on each
        /// [`StoryView::warnings`] instead. `skip_serializing_if` is not used
        /// here the way [`JsonEnvelope::warnings`] uses it on the wire struct:
        /// this is the in-process `Response`, not its serialized envelope, so
        /// an empty vec is just the ordinary "nothing to warn about" case
        /// every other `Response` construction site already passes.
        warnings: Vec<String>,
    },
    /// One Full Auto engine run after a start, read, or control mutation.
    EngineRun(Box<EngineRunView>),
    Summary(Box<SummaryView>),
    Graph(Box<GraphView>),
    Issues(Vec<String>),
    PhaseList(Vec<PhaseView>),
    /// One project's settings — the whole set from `list`, or the single
    /// entry `get`, `set` and `unset` answer with.
    ///
    /// Always a list, even of one, so that all four forms of the verb render
    /// through the same arm and a script does not have to branch on which one
    /// it asked for.
    ProjectSettings(Vec<SettingView>),
    /// Raw JSON output — bypasses normal envelope wrapping.
    /// Used by session-start and similar commands that need exact JSON control.
    RawJson(String),
    /// A whole project, for a client that holds a model rather than asking a
    /// question.
    ///
    /// Rendered as JSON in both forms. There is no human rendering of a
    /// project snapshot that a human would want — `story list` is that
    /// command — and inventing one would be a second, worse `list`.
    ProjectSnapshot(Box<ProjectSnapshotView>),
    /// One story's raw event history, oldest first.
    ///
    /// Rendered as JSON, for the same reason as [`Response::ProjectSnapshot`]:
    /// it is a machine's value. A human wanting a story's history has
    /// `story show`, which renders the *fold* of it.
    StoryHistory(Vec<crate::domain::StoryEvent>),
    /// One story's write history, rendered for a person (SH-246) — the answer
    /// to "what moved this story, and when".
    ///
    /// A sibling of [`StoryHistory`](Self::StoryHistory) rather than a
    /// replacement: that one is the TUI's undo snapshot, a raw log meant to be
    /// handed back verbatim, and it deliberately carries no provenance because
    /// restoring one must not claim the original writer performed the restore.
    StoryLog {
        /// The story these entries belong to, so a rendering can name it.
        id: String,
        /// Its title, for the same reason.
        title: String,
        /// Every event, oldest first.
        entries: Vec<LogEntry>,
    },
    /// A destructive command asking to be confirmed, and saying what it would
    /// destroy.
    ///
    /// The only variant that is not an answer. It is returned *instead of*
    /// doing the work, so receiving one means nothing has been written; the
    /// caller decides whether to ask the same command again with `force` set.
    /// Putting the decision here rather than inside the service is what lets
    /// the prompt render in the process that has a terminal — over the daemon
    /// the service runs somewhere with no way to reach the user at all.
    ConfirmationRequired(Box<ConfirmationPlan>),
    /// Which project this command resolved to, and where its work runs.
    ///
    /// Boxed like [`Story`](Self::Story) and [`Summary`](Self::Summary): a
    /// single object rather than a collection, and the enum's other variants
    /// should not all pay for its size.
    Project(Box<ProjectView>),
}

#[derive(Serialize)]
struct JsonEnvelope<'a> {
    result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    story: Option<&'a StoryView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stories: Option<&'a [StoryView]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<&'a SummaryView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<&'a GraphView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issues: Option<&'a [String]>,
    /// `story doctor`'s findings, as data. Always present (if empty) on
    /// doctor's own healthy envelope, so a `jq '.findings[]'` consumer never
    /// meets `null`; absent everywhere else.
    #[serde(skip_serializing_if = "Option::is_none")]
    findings: Option<&'a [crate::domain::finding::Finding]>,
    /// What `story doctor` has to say that is not damage — the correctly
    /// named successor to `issues`, which carries advice despite its name.
    #[serde(skip_serializing_if = "Option::is_none")]
    advice: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phases: Option<&'a [PhaseView]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<&'a [SettingView]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<&'a ProjectView>,
    /// `story claim` only (SH-476): the state the claimed story came out of,
    /// so a caller that has to undo its own claim — the plugin's dispatch
    /// rollback — knows what to move back to. Absent everywhere else.
    #[serde(skip_serializing_if = "Option::is_none")]
    claimed_from: Option<&'a str>,
    /// `story unclaim` only (SH-483): the active state the story was released
    /// from. The mirror of [`claimed_from`](Self::claimed_from), and needed
    /// for the same reason — where it *landed* is already
    /// `.story.story.state`, and where it came from is nowhere else.
    #[serde(skip_serializing_if = "Option::is_none")]
    unclaimed_from: Option<&'a str>,
    /// `story unclaim` only (SH-483): the [`UnclaimFallback::code`] when the
    /// story could not be restored to the state it was claimed from. Absent
    /// on an ordinary release, which is what makes its presence the signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    restore_fallback: Option<&'a str>,
    #[serde(default, skip_serializing_if = "<[_]>::is_empty")]
    warnings: &'a [String],
    #[serde(default, skip_serializing_if = "<[_]>::is_empty")]
    flagged_reasons: &'a [String],
}

pub fn render_response(response: &Response, json: bool, quiet: bool) -> String {
    // RawJson always outputs directly, regardless of --json or --quiet flags
    if let Response::RawJson(raw) = response {
        return format!("{raw}\n");
    }

    if quiet {
        return String::new();
    }

    if json {
        return render_json(response);
    }

    render_human(response)
}

pub fn render_error(error: &AppError, json: bool) -> String {
    if json {
        if let AppError::StateConflict(expected, actual) = error {
            return format!(
                "{}\n",
                serde_json::json!({
                    "result": "conflict",
                    "error": error.to_string(),
                    "exit_code": error.exit_code(),
                    "expected": expected,
                    "actual": actual,
                })
            );
        }
        // An integrity report carries its findings as data beside the prose
        // (SH-244). `error` is unchanged — it is these findings' own messages
        // joined — so a caller reading it is unaffected, and a caller wanting
        // `field`/`persisted`/`rebuilt` reads them instead of regexing a
        // 1.68MB string for them.
        if let AppError::Integrity(detail) = error {
            return format!(
                "{}\n",
                serde_json::json!({
                    "result": "error",
                    "error": error.to_string(),
                    "exit_code": error.exit_code(),
                    "findings": detail.findings,
                    "advice": detail.advice,
                })
            );
        }
        return format!(
            "{}\n",
            serde_json::json!({
                "result": "error",
                "error": error.to_string(),
                "exit_code": error.exit_code(),
            })
        );
    }

    format!("error: {error}\n")
}

fn render_json(response: &Response) -> String {
    let rendered = match response {
        Response::Message(message) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            claimed_from: None,
            unclaimed_from: None,
            restore_fallback: None,
            message: Some(message),
            story: None,
            stories: None,
            summary: None,
            graph: None,
            issues: None,
            findings: None,
            advice: None,
            phases: None,
            settings: None,
            project: None,
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::MessageWithWarnings(message, warnings) => {
            serde_json::to_string_pretty(&JsonEnvelope {
                result: "ok",
                claimed_from: None,
                unclaimed_from: None,
                restore_fallback: None,
                message: Some(message),
                story: None,
                stories: None,
                summary: None,
                graph: None,
                issues: None,
                findings: None,
                advice: None,
                phases: None,
                settings: None,
                project: None,
                warnings,
                flagged_reasons: &[],
            })
        }
        Response::Story(view) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            claimed_from: None,
            unclaimed_from: None,
            restore_fallback: None,
            message: None,
            story: Some(view.as_ref()),
            stories: None,
            summary: None,
            graph: None,
            issues: None,
            findings: None,
            advice: None,
            phases: None,
            settings: None,
            project: None,
            warnings: &view.warnings,
            flagged_reasons: &view.flagged_reasons,
        }),
        Response::Claimed(view, claimed_from) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            claimed_from: Some(claimed_from.as_str()),
            unclaimed_from: None,
            restore_fallback: None,
            message: None,
            story: Some(view.as_ref()),
            stories: None,
            summary: None,
            graph: None,
            issues: None,
            findings: None,
            advice: None,
            phases: None,
            settings: None,
            project: None,
            warnings: &view.warnings,
            flagged_reasons: &view.flagged_reasons,
        }),
        Response::Unclaimed(view, outcome) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            claimed_from: None,
            unclaimed_from: Some(outcome.from.as_str()),
            restore_fallback: outcome.fallback.as_ref().map(UnclaimFallback::code),
            message: None,
            story: Some(view.as_ref()),
            stories: None,
            summary: None,
            graph: None,
            issues: None,
            findings: None,
            advice: None,
            phases: None,
            settings: None,
            project: None,
            warnings: &view.warnings,
            flagged_reasons: &view.flagged_reasons,
        }),
        Response::Stories {
            views,
            message,
            warnings,
        } => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            claimed_from: None,
            unclaimed_from: None,
            restore_fallback: None,
            message: message.as_deref(),
            story: None,
            stories: Some(views),
            summary: None,
            graph: None,
            issues: None,
            findings: None,
            advice: None,
            phases: None,
            settings: None,
            project: None,
            warnings,
            flagged_reasons: &[],
        }),
        Response::EngineRun(run) => serde_json::to_string_pretty(&serde_json::json!({
            "result": "ok",
            "run": run,
        })),
        Response::Summary(summary) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            claimed_from: None,
            unclaimed_from: None,
            restore_fallback: None,
            message: None,
            story: None,
            stories: None,
            summary: Some(summary.as_ref()),
            graph: None,
            issues: None,
            findings: None,
            advice: None,
            phases: None,
            settings: None,
            project: None,
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::Graph(graph) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            claimed_from: None,
            unclaimed_from: None,
            restore_fallback: None,
            message: None,
            story: None,
            stories: None,
            summary: None,
            graph: Some(graph.as_ref()),
            issues: None,
            findings: None,
            advice: None,
            phases: None,
            settings: None,
            project: None,
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::Issues(issues) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            claimed_from: None,
            unclaimed_from: None,
            restore_fallback: None,
            message: None,
            story: None,
            stories: None,
            summary: None,
            graph: None,
            // `issues` is DEPRECATED and emitted unchanged for one release
            // (SH-244). It has always carried *advice* rather than issues —
            // a healthy `story doctor` is the only thing that produces this
            // variant — so `advice` is the same list under its right name,
            // and nothing in the tree reads `issues` today: the plugin's
            // `_project_integrity` reads `.result` and `.error` only.
            issues: Some(issues),
            // Always present, always empty: this variant *is* the healthy
            // answer, so a consumer can read `.findings` on both outcomes
            // without branching on which one it got.
            findings: Some(&[]),
            advice: Some(issues),
            phases: None,
            settings: None,
            project: None,
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::PhaseList(phase_views) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            claimed_from: None,
            unclaimed_from: None,
            restore_fallback: None,
            message: None,
            story: None,
            stories: None,
            summary: None,
            graph: None,
            issues: None,
            findings: None,
            advice: None,
            phases: Some(phase_views),
            settings: None,
            project: None,
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::ProjectSettings(settings) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            claimed_from: None,
            unclaimed_from: None,
            restore_fallback: None,
            message: None,
            story: None,
            stories: None,
            summary: None,
            graph: None,
            issues: None,
            findings: None,
            advice: None,
            phases: None,
            settings: Some(settings),
            project: None,
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::Project(view) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            claimed_from: None,
            unclaimed_from: None,
            restore_fallback: None,
            message: None,
            story: None,
            stories: None,
            summary: None,
            graph: None,
            issues: None,
            findings: None,
            advice: None,
            phases: None,
            settings: None,
            project: Some(view.as_ref()),
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::RawJson(raw) => {
            // Should not reach here — render_response handles RawJson before calling render_json.
            return format!("{raw}\n");
        }
        Response::ProjectSnapshot(view) => serde_json::to_string_pretty(view.as_ref()),
        Response::StoryHistory(events) => serde_json::to_string_pretty(events),
        // `command` and `actor` stay separate fields rather than the rendered
        // "move (story.sh:dispatch)" a human sees: a script must be able to tell
        // the attested half from the self-attested one without parsing prose.
        Response::StoryLog { id, title, entries } => {
            serde_json::to_string_pretty(&serde_json::json!({
                "result": "ok",
                "id": id,
                "title": title,
                "log": entries,
            }))
        }
        // Not `result: "ok"`: nothing happened. A scripted caller that saw
        // "ok" here would reasonably conclude the project was gone.
        Response::ConfirmationRequired(plan) => serde_json::to_string_pretty(&serde_json::json!({
            "result": "confirmation-required",
            "plan": plan.as_ref(),
        })),
    }
    .expect("response should serialize");

    format!("{rendered}\n")
}

fn render_human(response: &Response) -> String {
    match response {
        Response::Message(message) => format!("{message}\n"),
        Response::MessageWithWarnings(message, warnings) => {
            let mut body = format!("{message}\n");
            for warning in warnings {
                body.push_str(&format!("warning: {warning}\n"));
            }
            body
        }
        Response::Story(view) => render_story(view),
        Response::Claimed(view, claimed_from) => {
            format!(
                "claimed {} — {claimed_from} -> {}\n{}",
                view.story.id,
                view.story.state,
                render_story(view)
            )
        }
        Response::Unclaimed(view, outcome) => {
            let mut body = format!(
                "unclaimed {} — {} -> {}\n",
                outcome.id, outcome.from, outcome.restored_to
            );
            if let Some(fallback) = &outcome.fallback {
                body.push_str(&format!(
                    "note: restored to {} rather than the state it was claimed from, because {}\n",
                    outcome.restored_to,
                    fallback.explain(&outcome.from)
                ));
            }
            body.push_str(&render_story(view));
            body
        }
        Response::Stories {
            views: stories,
            message: msg,
            warnings,
        } => {
            if stories.is_empty() {
                let mut body = String::new();
                // SH-409: `list --label X` matching only archived stories is
                // exactly the case where this note matters most — it must
                // survive the empty-result early return, not just the
                // populated one below.
                if let Some(msg) = msg {
                    body.push_str(msg);
                    body.push('\n');
                }
                body.push_str("no stories found\n");
                for warning in warnings {
                    body.push_str(&format!("warning: {warning}\n"));
                }
                return body;
            }

            let mut body = String::new();
            if let Some(msg) = msg {
                body.push_str(msg);
                body.push('\n');
            }
            for story in stories {
                let flagged = if story.flagged_reasons.is_empty() {
                    ""
                } else {
                    " [flagged]"
                };
                let priority = if story.story.priority != Priority::None {
                    format!(" ({})", story.story.priority.as_str())
                } else {
                    String::new()
                };
                let type_badge = match story.story.story_type.as_deref() {
                    Some(t) => format!(" [{}]", t),
                    None => " [Default]".to_string(),
                };
                let progress_summary = if let Some(ref p) = story.progress {
                    format!(" ({}/{})", p.children_done, p.children_total)
                } else {
                    String::new()
                };
                let labels = if story.story.labels.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", story.story.labels.join(", "))
                };
                let stale = if let Some(ref info) = story.stale_info {
                    format!(
                        " [stale {}d, last: {}]",
                        info.days_stale, info.last_activity_type
                    )
                } else {
                    String::new()
                };
                // SH-409: `list` excludes archived (hidden) stories by
                // default now, so a story that reaches this render arm
                // carrying `hidden_at` only got here via `--include-archived`
                // / `--all`, or via `story search` (which still shows
                // everything) — either way it needs a badge distinguishing it
                // from a plain closed one, which `story show` already had
                // (rendered as `archived: <timestamp>` there) but `list`
                // never did.
                let archived = if story.story.hidden_at.is_some() {
                    " [archived]"
                } else {
                    ""
                };
                // SH-175: shown inline rather than excluded by default — see
                // the council verdict on SH-175 for why `list` diverges from
                // the web board here.
                let draft = if story.story.draft { " [draft]" } else { "" };
                body.push_str(&format!(
                    "{} [{}]{}{} {}{}{}{}{}{}{}\n",
                    story.story.id,
                    story.story.state,
                    priority,
                    type_badge,
                    story.story.title,
                    progress_summary,
                    labels,
                    archived,
                    draft,
                    flagged,
                    stale
                ));
            }
            // Same `warning: ` shape [`Response::MessageWithWarnings`] and
            // `render_story` already use, so all three read alike (SH-358).
            for warning in warnings {
                body.push_str(&format!("warning: {warning}\n"));
            }
            body
        }
        Response::EngineRun(run) => render_engine_run(run),
        Response::Summary(summary) => render_summary(summary),
        Response::Graph(graph) => render_graph(graph),
        Response::Issues(issues) => {
            if issues.is_empty() {
                return "no integrity issues found\n".to_string();
            }
            let mut body = String::new();
            for issue in issues {
                body.push_str(issue);
                body.push('\n');
            }
            body
        }
        Response::PhaseList(phase_views) => {
            if phase_views.is_empty() {
                return "no phases found\n".to_string();
            }
            let mut body = String::new();
            for pv in phase_views {
                let title_str = pv
                    .title
                    .as_ref()
                    .map(|t| format!(": {t}"))
                    .unwrap_or_default();
                let status = if pv.total == 0 {
                    "(empty)".to_string()
                } else {
                    let mut parts = Vec::new();
                    parts.push(format!("{}/{} done", pv.done, pv.total));
                    if pv.in_progress > 0 {
                        parts.push(format!("{} in-progress", pv.in_progress));
                    }
                    if pv.blocked > 0 {
                        parts.push(format!("{} blocked", pv.blocked));
                    }
                    format!("({})", parts.join(", "))
                };
                body.push_str(&format!(
                    "Phase {}{} -- {} {}\n",
                    pv.phase, title_str, pv.total, status
                ));
            }
            body
        }
        Response::ProjectSettings(settings) => render_project_settings(settings),
        Response::RawJson(raw) => {
            // Should not reach here — render_response handles RawJson before calling render_human.
            format!("{raw}\n")
        }
        // Deliberately the same JSON in both forms: a project snapshot is a
        // machine's value, and a human asking for one is asking the wrong
        // command.
        Response::ProjectSnapshot(view) => {
            format!(
                "{}\n",
                serde_json::to_string_pretty(view.as_ref()).unwrap_or_default()
            )
        }
        Response::StoryHistory(events) => {
            format!(
                "{}\n",
                serde_json::to_string_pretty(events).unwrap_or_default()
            )
        }
        Response::StoryLog { id, title, entries } => render_story_log(id, title, entries),
        Response::Project(view) => render_project(view),
        Response::ConfirmationRequired(plan) => render_confirmation_plan(plan),
    }
}

fn render_engine_run(run: &EngineRunView) -> String {
    let scope = run
        .scope
        .epic
        .as_deref()
        .map_or_else(|| run.scope.kind.clone(), |epic| format!("epic {epic}"));
    let mut body = format!(
        "run: {}\nscope: {}\nstate: {}\nagent: {}\nlanes: {}\nconsecutive hard stops: {}\n",
        run.id,
        scope,
        run.state.as_str(),
        run.agent.as_str(),
        run.lane_count,
        run.consecutive_hard_stops
    );
    if let Some(reason) = &run.stop_reason {
        body.push_str(&format!("stop reason: {reason}\n"));
        body.push_str(&format!(
            "acknowledged: {}\n",
            run.acknowledged_at.as_deref().unwrap_or("no")
        ));
    }
    body.push_str("\nlane  state        story       elapsed\n");
    for lane in &run.lanes {
        body.push_str(&format!(
            "{:<5} {:<12} {:<11} {}\n",
            lane.index + 1,
            lane.state.as_str(),
            lane.story.as_deref().unwrap_or("-"),
            lane.elapsed_seconds
                .map(format_elapsed)
                .unwrap_or_else(|| "-".to_string())
        ));
    }
    if !run.needs_human.is_empty() {
        body.push_str("\nneeds a human (no-auto):\n");
        for story in &run.needs_human {
            body.push_str(&format!("  {} — {}\n", story.id, story.title));
        }
    }
    body
}

fn format_elapsed(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// `story project show`, for a person.
///
/// Not the same text as the JSON, unlike [`Response::ProjectSnapshot`] — a
/// snapshot is a machine's value and a human asking for one is asking the wrong
/// command, where this one answers a question a person genuinely asks: *which
/// project am I in, and where does its work run?*
///
/// The checkout line is [`checkout_line`], shared with `story project list`, so
/// the two cannot come to describe the same fact differently.
fn render_project(view: &ProjectView) -> String {
    let mut out = format!(
        "{} — {} ({})\n  checkout  {}\n",
        view.slug,
        view.name,
        view.prefix,
        checkout_line(view.checkout.as_deref())
    );
    for origin in &view.origins {
        out.push_str(&format!("  origin    {origin}\n"));
    }
    out
}

/// How a checkout is described, present or absent.
///
/// **One function, two call sites** — `story project show` and `story project
/// list` — because "no checkout on this machine" is a phrase a user learns
/// once, and two spellings of it would read as two different states.
#[must_use]
pub fn checkout_line(checkout: Option<&std::path::Path>) -> String {
    checkout.map_or_else(
        || NO_CHECKOUT.to_string(),
        |path| path.display().to_string(),
    )
}

/// What a project with no checkout on this machine is called, everywhere.
pub const NO_CHECKOUT: &str = "no checkout on this machine";

/// The warning a destructive command prints before it asks.
///
/// One entry point, so the CLI prompt, the CLI refusal and any other front-end
/// are reading the same words about the same act.
#[must_use]
pub fn render_confirmation_plan(plan: &ConfirmationPlan) -> String {
    match plan {
        ConfirmationPlan::Delete(plan) => render_delete_plan(plan),
        ConfirmationPlan::DeleteStory(plan) => render_story_delete_plan(plan),
        ConfirmationPlan::SetPrefix(plan) => render_set_prefix_plan(plan),
        ConfirmationPlan::HideState(plan) => render_hide_state_plan(plan),
    }
}

/// The warning the bulk column "Archive" action prints before it hides.
///
/// Lists every id by name — the same list `hide_state` will be called with
/// to commit — so the person confirming (or a script reading `--json`) is
/// looking at exactly what is about to disappear from the board, not a bare
/// count.
#[must_use]
pub fn render_hide_state_plan(plan: &HideStatePlan) -> String {
    let mut body = format!(
        "`{}` — {} stor{} will be archived:\n",
        plan.state,
        plan.ids.len(),
        if plan.ids.len() == 1 { "y" } else { "ies" },
    );
    for id in &plan.ids {
        body.push_str(&format!("  {id}\n"));
    }
    body
}

/// The warning `story project set-prefix` prints before it asks.
///
/// Ordered like [`render_delete_plan`] and [`render_story_delete_plan`]: what this
/// is, what changes and by how much, and only then the question. Unlike
/// either of those this is not a body count — nothing is destroyed — so the
/// closing line says what actually cannot be undone: every id already
/// quoted anywhere under the old prefix.
#[must_use]
pub fn render_set_prefix_plan(plan: &SetPrefixPlan) -> String {
    let mut body = String::new();
    body.push_str(&format!(
        "{} — {} ({} → {})\n",
        plan.slug, plan.name, plan.old_prefix, plan.new_prefix
    ));
    body.push_str(&format!(
        "  {} stor{} will be renumbered from `{}-…` to `{}-…`.\n",
        plan.stories,
        if plan.stories == 1 { "y" } else { "ies" },
        plan.old_prefix,
        plan.new_prefix,
    ));
    if plan.relationships > 0 {
        body.push_str(&format!(
            "  {} relationship{} will each be rewritten by one retracting and one \
             re-asserting event.\n",
            plan.relationships,
            if plan.relationships == 1 { "" } else { "s" },
        ));
    }
    body.push_str(
        "  Free-text description and comment bodies are left untouched: any of them that \
         quote an old-prefix id will keep quoting it.\n",
    );
    body.push_str(&format!(
        "\nEvery `{}-…` id already written down — in a commit message, a document, a \
         browser tab — stops resolving. This cannot be undone.\n",
        plan.old_prefix
    ));
    body
}

/// The warning a story deletion prints before it asks.
///
/// Ordered the way [`render_delete_plan`] is, by what a person needs in order
/// to answer: which story this is, what is irreversible about it, what else
/// changes, and only then the question. The retracted claims are here rather
/// than left as a surprise because they are edits to *other* stories' histories
/// — the one part of a deletion that reaches beyond the story being deleted.
#[must_use]
pub fn render_story_delete_plan(plan: &StoryDeletePlan) -> String {
    let mut body = String::new();
    body.push_str(&format!("{} — {}\n", plan.id, plan.title));
    body.push_str(&format!(
        "  {} event{} will be permanently deleted.\n",
        plan.events,
        if plan.events == 1 { "" } else { "s" },
    ));
    for (other, relation) in &plan.retracted {
        body.push_str(&format!("  retract   {other} {relation} {}\n", plan.id));
    }
    body.push_str(&format!(
        "  {} will never be reused as a story id.\n",
        plan.id
    ));
    body.push_str("\nThis cannot be undone.\n");
    body
}

/// `story project settings` in prose.
///
/// Each entry is a value line and its description, because a settings surface
/// is read rarely and by someone deciding whether to change something. Every
/// annotation — the default marker, the owning command, the caveat that
/// nothing reads a value yet — comes from the [`SettingView`] rather than
/// from a condition written here, so the day a setting starts having an effect
/// the label stops appearing without this function being touched.
/// `story log`, for a person (SH-246).
///
/// # The grammar of the last column, which is the point of the command
///
/// * `move` — a bare word is the verb **the daemon derived** from the arm it
///   dispatched. A caller cannot misstate it, because a caller is never asked.
/// * `move (story.sh:dispatch-rollback)` — parentheses mean **self-attested**:
///   the caller declared this about itself in `$STORYHOOK_ACTOR`.
/// * `(unrecorded)` — nothing was captured. Consistent with the rule above
///   rather than an exception to it: the store is admitting it was told
///   nothing, which is why it is parenthesized and why it is deliberately not
///   the `-` this module uses elsewhere for a field that is merely unset. `-`
///   would read as "nobody did this"; these events were written before the
///   columns existed, or replayed by `migrate`.
fn render_story_log(id: &str, title: &str, entries: &[LogEntry]) -> String {
    let mut out = format!("{id} {title}\n");
    if entries.is_empty() {
        // Not reachable through `story log` — a story always has at least a
        // creation event — but a total match beats an assumption about that.
        out.push_str("\nno events\n");
        return out;
    }
    out.push('\n');

    // The `by` column is padded to its own widest entry rather than a constant:
    // most stories are written entirely by undeclared commands, and a fixed
    // column sized for the longest possible actor would leave every ordinary
    // trail full of whitespace.
    let by: Vec<String> = entries.iter().map(by_column).collect();
    let width = by.iter().map(|b| b.chars().count()).max().unwrap_or(0);

    for (entry, by) in entries.iter().zip(&by) {
        let detail = entry.detail.as_deref().unwrap_or(&entry.kind);
        out.push_str(&format!(
            "{}  {:width$}  {}\n",
            entry.at,
            by,
            detail,
            width = width
        ));
    }
    out
}

/// The provenance column for one entry — see [`render_story_log`] for the
/// grammar this implements.
fn by_column(entry: &LogEntry) -> String {
    match (&entry.command, &entry.actor) {
        (Some(command), Some(actor)) => format!("{command} ({actor})"),
        (Some(command), None) => command.clone(),
        // A declared actor with no command cannot arise from the CLI, which
        // always knows its own verb. It is representable, so it renders rather
        // than being asserted away.
        (None, Some(actor)) => format!("({actor})"),
        (None, None) => "(unrecorded)".to_string(),
    }
}

fn render_project_settings(settings: &[SettingView]) -> String {
    if settings.is_empty() {
        return "no settings\n".to_string();
    }

    let mut entries = Vec::with_capacity(settings.len());
    for view in settings {
        let value = view
            .value
            .as_ref()
            .map(|value| format!(" = {value}"))
            .unwrap_or_default();

        // One parenthetical rather than several, so an unset read-only key does
        // not read as `github.sync (unset) (read-only, …)`.
        let mut notes = Vec::new();
        match view.source {
            SettingSource::Set => {}
            SettingSource::Default => notes.push("default".to_string()),
            SettingSource::Unset => notes.push("unset".to_string()),
        }
        if let Some(owner) = &view.managed_by {
            notes.push(format!("read-only, managed by `{owner}`"));
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join("; "))
        };

        let mut entry = format!("{}{value}{notes}\n    {}\n", view.key, view.description);
        if let Some(note) = &view.note {
            entry.push_str(&format!("    Note: {note}.\n"));
        }
        entries.push(entry);
    }
    entries.join("\n")
}

/// The warning a delete prints before it asks.
///
/// Ordered by what a person needs in order to answer: what this is, what is
/// irreversible about it, what will be left behind, and only then the question.
///
/// The checkout list is followed by a sentence saying the files in it are left
/// alone. Without it the list reads exactly as it did when this verb removed
/// them, which is the one misreading that matters here — a person scanning a
/// destruction warning takes a list of paths as a list of casualties.
pub fn render_delete_plan(plan: &DeletePlan) -> String {
    let mut body = String::new();
    body.push_str(&format!("{} — {}\n", plan.slug, plan.name));
    body.push_str(&format!(
        "  {} stor{} and {} event{} will be permanently deleted.\n",
        plan.stories,
        if plan.stories == 1 { "y" } else { "ies" },
        plan.events,
        if plan.events == 1 { "" } else { "s" },
    ));
    for checkout in &plan.checkouts {
        body.push_str(&format!("  checkout  {checkout}\n"));
    }
    if !plan.checkouts.is_empty() {
        body.push_str(
            "  Nothing in those directories is touched; their `.storyhook.toml` is left \
             naming a project that will not exist.\n",
        );
    }
    body.push_str("\nThis cannot be undone.\n");
    body
}

fn render_story(view: &StoryView) -> String {
    let story = &view.story;
    let assignee = story.assignee.as_deref().unwrap_or("-");
    let mut body = String::new();
    body.push_str(&format!("{} {}\n", story.id, story.title));
    body.push_str(&format!(
        "state: {} ({})\n",
        story.state,
        story.superstate.as_str()
    ));
    // SH-175: a draft is never shown as a badge on the board, but `story
    // show` is the one place a reader is looking at exactly this story, so
    // it earns an unconditional line the way `flagged` gets one below.
    body.push_str(&format!(
        "draft: {}\n",
        if story.draft { "yes" } else { "no" }
    ));
    body.push_str(&format!("assignee: {assignee}\n"));
    // The parenthetical only appears on the legacy `none` representation when
    // no priority event exists (SH-359). Current creation always emits low,
    // but old logs and exports remain readable.
    let assessment = if story.priority == Priority::None && !story.priority_assessed {
        " (not assessed)"
    } else {
        ""
    };
    body.push_str(&format!(
        "priority: {}{assessment}\n",
        story.priority.as_str()
    ));
    let type_display = story.story_type.as_deref().unwrap_or("Default");
    body.push_str(&format!("type: {type_display}\n"));
    if story.labels.is_empty() {
        body.push_str("labels: -\n");
    } else {
        body.push_str(&format!("labels: {}\n", story.labels.join(", ")));
    }
    if let Some(description) = &story.description {
        body.push_str(&format!("description: {description}\n"));
    }
    if let Some(awaiting) = &story.awaiting {
        body.push_str(&format!("awaiting: {awaiting}\n"));
    }

    if let Some(closed_at) = &story.closed_at {
        body.push_str(&format!("closed_at: {closed_at}\n"));
    }

    // "archived", not "hidden": the internal fact is named `hidden` to stay
    // clear of the store's unrelated, load-bearing `archived` derivation
    // (`closed_at`-is-set), but the word a user reads for this feature is
    // "Archive" everywhere — see `StoryEvent::StoryHidden`'s doc comment.
    if let Some(hidden_at) = &story.hidden_at {
        body.push_str(&format!("archived: {hidden_at}\n"));
    }

    if view.flagged_reasons.is_empty() {
        body.push_str("flagged: no\n");
    } else {
        body.push_str("flagged: yes\n");
        for reason in &view.flagged_reasons {
            body.push_str(&format!("flagged_reason: {reason}\n"));
        }
    }

    if !story.relationships.is_empty() {
        body.push_str("relationships:\n");
        for relation in &story.relationships {
            body.push_str(&format!("- {} {}\n", relation.relation, relation.other_id));
        }
    }

    if !view.derived_relationships.is_empty() {
        body.push_str("derived_relationships:\n");
        for relation in &view.derived_relationships {
            body.push_str(&format!("- {} {}\n", relation.relation, relation.other_id));
        }
    }

    if let Some(ref progress) = view.progress {
        let pct = (progress.children_done as f64 / progress.children_total as f64 * 100.0) as u64;
        body.push_str(&format!(
            "progress: {}/{} children done ({}%)\n",
            progress.children_done, progress.children_total, pct
        ));
    }

    if !story.comments.is_empty() {
        body.push_str("comments:\n");
        for comment in &story.comments {
            body.push_str(&format!("- {} {}\n", comment.at, comment.text));
        }
    }

    // SH-315: empty-gated like every other optional section above, so a
    // story with no attachments — every story in the golden fixture, today —
    // renders exactly as it did before this field existed.
    if !story.attachments.is_empty() {
        body.push_str("attachments:\n");
        for attachment in &story.attachments {
            body.push_str(&format!(
                "- {} {} ({}, {} bytes)\n",
                attachment.id, attachment.name, attachment.media_type, attachment.byte_len
            ));
        }
    }

    if !view.referenced_by.is_empty() {
        body.push_str("referenced_by:\n");
        for commit in &view.referenced_by.commits {
            body.push_str(&format!(
                "- {} {}\n",
                commit.at,
                crate::domain::git_link_comment(&commit.sha, &commit.subject)
            ));
        }
        for pr in &view.referenced_by.prs {
            body.push_str(&format!(
                "- {} [pr] {} ({})\n",
                pr.linked_at, pr.url, pr.status
            ));
        }
        for mention in &view.referenced_by.comment_mentions {
            body.push_str(&format!(
                "- {} [comment] {}: {}\n",
                mention.at, mention.other_id, mention.snippet
            ));
        }
    }

    // Last, and in the same `warning: ` shape [`Response::MessageWithWarnings`]
    // uses, so the two renderings of a non-fatal warning read alike.
    //
    // This is the half of the field that was missing (SH-354).
    // [`StoryView::warnings`] has always been serialized into the JSON envelope
    // and was rendered nowhere, so a warning parked there would have reached a
    // `--json` caller and no human — which is exactly the shape of the defect
    // SH-354 was filed about, one layer up. Nothing populated the field before
    // that story, so this line changes no existing output; `tests/golden_cli.rs`
    // is the check on that claim.
    for warning in &view.warnings {
        body.push_str(&format!("warning: {warning}\n"));
    }

    body
}

fn render_summary(summary: &SummaryView) -> String {
    let mut body = String::new();
    let total = summary.total_open + summary.total_closed;
    body.push_str(&format!(
        "stories: {} ({} open, {} closed)\n",
        total, summary.total_open, summary.total_closed
    ));

    if !summary.by_state.is_empty() {
        body.push_str("by state:\n");
        for (state, count) in &summary.by_state {
            body.push_str(&format!("  {state}: {count}\n"));
        }
    }

    if summary.by_priority.iter().any(|(_, c)| *c > 0) {
        body.push_str("by priority:\n");
        for (priority, count) in &summary.by_priority {
            if *count > 0 {
                body.push_str(&format!("  {priority}: {count}\n"));
            }
        }
    }

    if !summary.by_type.is_empty() {
        body.push_str("by type:\n");
        for (type_name, count) in &summary.by_type {
            body.push_str(&format!("  {type_name}: {count}\n"));
        }
    }

    body.push_str(&format!("blocked: {}\n", summary.blocked_count));
    body.push_str(&format!("flagged: {}\n", summary.flagged_count));
    body.push_str(&format!("ready: {}\n", summary.ready_count));

    if !summary.ready_stories.is_empty() {
        body.push_str("ready stories:\n");
        for view in &summary.ready_stories {
            let priority = if view.story.priority != Priority::None {
                format!(" ({})", view.story.priority.as_str())
            } else {
                String::new()
            };
            body.push_str(&format!(
                "  {} [{}]{} {}\n",
                view.story.id, view.story.state, priority, view.story.title
            ));
        }
    }

    body
}

fn render_graph(graph: &GraphView) -> String {
    let mut body = String::new();

    if let Some(ref overview) = graph.overview {
        body.push_str(&format!("open stories: {}\n", overview.total_open));
        body.push_str(&format!("dependency edges: {}\n", overview.total_edges));
        if !overview.roots.is_empty() {
            body.push_str(&format!(
                "roots (no predecessors): {}\n",
                overview.roots.join(", ")
            ));
        }
        if !overview.leaves.is_empty() {
            body.push_str(&format!(
                "leaves (no successors): {}\n",
                overview.leaves.join(", ")
            ));
        }
    }

    if let Some(ref path) = graph.critical_path {
        if path.is_empty() {
            body.push_str("critical path: (none)\n");
        } else {
            body.push_str(&format!("critical path ({} stories):\n", path.len()));
            body.push_str(&format!("  {}\n", path.join(" -> ")));
        }
    }

    if let Some(ref chain) = graph.blocked_chain {
        if chain.blocked.is_empty() {
            body.push_str(&format!("nothing is blocked by {}\n", chain.source));
        } else {
            body.push_str(&format!(
                "blocked by {} ({} stories):\n",
                chain.source,
                chain.blocked.len()
            ));
            for id in &chain.blocked {
                body.push_str(&format!("  {id}\n"));
            }
        }
    }

    if let Some(ref groups) = graph.parallel_groups {
        body.push_str(&format!("parallel groups: {}\n", groups.len()));
        for (i, group) in groups.iter().enumerate() {
            body.push_str(&format!("  group {}: {}\n", i + 1, group.join(", ")));
        }
    }

    body
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn render_html_report(
    summary: &SummaryView,
    stories: &[StoryView],
    is_ready_fn: &dyn Fn(&str) -> bool,
    is_blocked_fn: &dyn Fn(&str) -> bool,
) -> String {
    let total = summary.total_open + summary.total_closed;

    let state_colors = [
        "#3b82f6", "#10b981", "#f59e0b", "#ef4444", "#8b5cf6", "#ec4899", "#06b6d4", "#84cc16",
        "#f97316", "#6366f1",
    ];

    let state_bar = build_state_bar(summary, total, &state_colors);
    let state_legend = build_state_legend(summary, total, &state_colors);
    let priority_html = build_priority_section(summary);
    let type_html = build_type_section(summary);
    let table_rows = build_table_rows(stories, is_ready_fn, is_blocked_fn);

    let stories_table = if stories.is_empty() {
        String::from("<p class=\"empty\">No stories in this project.</p>")
    } else {
        format!(
            "<table>\n<thead><tr><th>ID</th><th>Title</th><th>State</th><th>Priority</th><th>Labels</th><th>Assignee</th><th>Updated</th></tr></thead>\n<tbody>\n{}</tbody>\n</table>",
            table_rows
        )
    };

    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Storyhook Report</title>
<style>
:root {{
  --bg: #ffffff;
  --fg: #1a1a2e;
  --bg-card: #f8f9fa;
  --border: #e2e8f0;
  --muted: #94a3b8;
  --row-blocked-bg: #fef2f2;
  --row-ready-bg: #f0fdf4;
  --row-blocked-border: #fca5a5;
  --row-ready-border: #86efac;
  --table-header-bg: #f1f5f9;
  --table-hover: #f8fafc;
}}
@media (prefers-color-scheme: dark) {{
  :root {{
    --bg: #0f172a;
    --fg: #e2e8f0;
    --bg-card: #1e293b;
    --border: #334155;
    --muted: #64748b;
    --row-blocked-bg: #450a0a;
    --row-ready-bg: #052e16;
    --row-blocked-border: #991b1b;
    --row-ready-border: #166534;
    --table-header-bg: #1e293b;
    --table-hover: #1e293b;
  }}
}}
* {{ margin:0; padding:0; box-sizing:border-box; }}
body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; background:var(--bg); color:var(--fg); line-height:1.6; padding:2rem; max-width:1200px; margin:0 auto; }}
h1 {{ font-size:1.5rem; font-weight:700; margin-bottom:0.25rem; }}
.subtitle {{ color:var(--muted); font-size:0.875rem; margin-bottom:1.5rem; }}
.stats {{ display:flex; gap:1rem; flex-wrap:wrap; margin-bottom:1.5rem; }}
.stat-card {{ background:var(--bg-card); border:1px solid var(--border); border-radius:0.5rem; padding:1rem 1.25rem; min-width:120px; }}
.stat-value {{ font-size:1.5rem; font-weight:700; }}
.stat-label {{ font-size:0.75rem; color:var(--muted); text-transform:uppercase; letter-spacing:0.05em; }}
.section {{ margin-bottom:1.5rem; }}
.section-title {{ font-size:0.875rem; font-weight:600; text-transform:uppercase; letter-spacing:0.05em; color:var(--muted); margin-bottom:0.5rem; }}
.bar-chart {{ display:flex; height:1.5rem; border-radius:0.375rem; overflow:hidden; margin-bottom:0.5rem; }}
.bar-segment {{ min-width:2px; transition:width 0.3s; }}
.legend {{ display:flex; flex-wrap:wrap; gap:0.75rem; font-size:0.8125rem; }}
.legend-item {{ display:inline-flex; align-items:center; gap:0.25rem; }}
.legend-dot {{ width:0.625rem; height:0.625rem; border-radius:50%; display:inline-block; }}
.priorities {{ display:flex; gap:0.5rem; flex-wrap:wrap; }}
.priority-badge {{ font-size:0.75rem; padding:0.125rem 0.5rem; border-radius:9999px; font-weight:500; }}
.priority-critical {{ background:#fef2f2; color:#dc2626; border:1px solid #fca5a5; }}
.priority-high {{ background:#fff7ed; color:#ea580c; border:1px solid #fdba74; }}
.priority-medium {{ background:#fefce8; color:#ca8a04; border:1px solid #fde047; }}
.priority-low {{ background:#f0fdf4; color:#16a34a; border:1px solid #86efac; }}
.priority-none {{ background:var(--bg-card); color:var(--muted); border:1px solid var(--border); }}
@media (prefers-color-scheme: dark) {{
  .priority-critical {{ background:#450a0a; color:#f87171; border-color:#991b1b; }}
  .priority-high {{ background:#431407; color:#fb923c; border-color:#9a3412; }}
  .priority-medium {{ background:#422006; color:#facc15; border-color:#854d0e; }}
  .priority-low {{ background:#052e16; color:#4ade80; border-color:#166534; }}
}}
table {{ width:100%; border-collapse:collapse; font-size:0.875rem; }}
thead th {{ text-align:left; padding:0.625rem 0.75rem; background:var(--table-header-bg); border-bottom:2px solid var(--border); font-weight:600; font-size:0.75rem; text-transform:uppercase; letter-spacing:0.05em; color:var(--muted); }}
tbody td {{ padding:0.5rem 0.75rem; border-bottom:1px solid var(--border); vertical-align:top; }}
tbody tr:hover {{ background:var(--table-hover); }}
.row-blocked {{ background:var(--row-blocked-bg); border-left:3px solid var(--row-blocked-border); }}
.row-ready {{ background:var(--row-ready-bg); border-left:3px solid var(--row-ready-border); }}
.col-id {{ font-family:ui-monospace,SFMono-Regular,monospace; white-space:nowrap; font-size:0.8125rem; }}
.col-date {{ white-space:nowrap; color:var(--muted); font-size:0.8125rem; }}
.label {{ display:inline-block; font-size:0.6875rem; padding:0.0625rem 0.375rem; border-radius:9999px; background:var(--bg-card); border:1px solid var(--border); margin-right:0.25rem; }}
.muted {{ color:var(--muted); }}
.empty {{ text-align:center; padding:2rem; color:var(--muted); }}
</style>
</head>
<body>
<h1>Storyhook Report</h1>
<p class="subtitle">Generated {generated_at} &middot; {total} stories</p>

<div class="stats">
<div class="stat-card"><div class="stat-value">{total}</div><div class="stat-label">Total</div></div>
<div class="stat-card"><div class="stat-value">{open}</div><div class="stat-label">Open</div></div>
<div class="stat-card"><div class="stat-value">{closed}</div><div class="stat-label">Closed</div></div>
<div class="stat-card"><div class="stat-value">{blocked}</div><div class="stat-label">Blocked</div></div>
<div class="stat-card"><div class="stat-value">{ready}</div><div class="stat-label">Ready</div></div>
</div>

<div class="section">
<div class="section-title">State Distribution</div>
<div class="bar-chart">{state_bar}</div>
<div class="legend">{state_legend}</div>
</div>

<div class="section">
<div class="section-title">Priority Breakdown</div>
<div class="priorities">{priority_html}</div>
</div>

<div class="section">
<div class="section-title">Type Breakdown</div>
<div class="priorities">{type_html}</div>
</div>

<div class="section">
<div class="section-title">Stories</div>
{stories_table}
</div>

</body>
</html>
"##,
        generated_at = html_escape(&chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string()),
        total = total,
        open = summary.total_open,
        closed = summary.total_closed,
        blocked = summary.blocked_count,
        ready = summary.ready_count,
        state_bar = state_bar,
        state_legend = state_legend,
        priority_html = priority_html,
        type_html = type_html,
        stories_table = stories_table,
    )
}

fn build_state_bar(summary: &SummaryView, total: usize, colors: &[&str]) -> String {
    let mut html = String::new();
    if total > 0 {
        for (i, (state, count)) in summary.by_state.iter().enumerate() {
            let pct = (*count as f64 / total as f64) * 100.0;
            if pct > 0.0 {
                let color = colors[i % colors.len()];
                html.push_str(&format!(
                    "<div class=\"bar-segment\" style=\"width:{pct:.1}%;background:{color}\" title=\"{}: {} ({pct:.0}%)\"></div>",
                    html_escape(state), count
                ));
            }
        }
    }
    html
}

fn build_state_legend(summary: &SummaryView, total: usize, colors: &[&str]) -> String {
    let mut html = String::new();
    for (i, (state, count)) in summary.by_state.iter().enumerate() {
        let color = colors[i % colors.len()];
        let pct = if total > 0 {
            (*count as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        html.push_str(&format!(
            "<span class=\"legend-item\"><span class=\"legend-dot\" style=\"background:{color}\"></span>{} {} ({pct:.0}%)</span>",
            html_escape(state), count
        ));
    }
    html
}

fn build_priority_section(summary: &SummaryView) -> String {
    let mut html = String::new();
    for (priority, count) in &summary.by_priority {
        if *count > 0 {
            let cls = match priority.as_str() {
                "critical" => "priority-critical",
                "high" => "priority-high",
                "medium" => "priority-medium",
                "low" => "priority-low",
                _ => "priority-none",
            };
            html.push_str(&format!(
                "<span class=\"priority-badge {cls}\">{}: {count}</span>",
                html_escape(priority)
            ));
        }
    }
    if html.is_empty() {
        html.push_str("<span class=\"muted\">No priorities set</span>");
    }
    html
}

fn build_type_section(summary: &SummaryView) -> String {
    let mut html = String::new();
    for (type_name, count) in &summary.by_type {
        html.push_str(&format!(
            "<span class=\"priority-badge priority-none\">{}: {count}</span>",
            html_escape(type_name)
        ));
    }
    if html.is_empty() {
        html.push_str("<span class=\"muted\">No types set</span>");
    }
    html
}

fn build_table_rows(
    stories: &[StoryView],
    is_ready_fn: &dyn Fn(&str) -> bool,
    is_blocked_fn: &dyn Fn(&str) -> bool,
) -> String {
    let mut sorted: Vec<&StoryView> = stories.iter().collect();
    sorted.sort_by(|a, b| {
        a.story
            .priority
            .cmp(&b.story.priority)
            .then_with(|| a.story.state.cmp(&b.story.state))
            .then_with(|| a.story.title.cmp(&b.story.title))
    });

    let mut html = String::new();
    for view in &sorted {
        let s = &view.story;
        let row_class = if s.superstate == SuperState::Open && is_blocked_fn(&s.id) {
            " class=\"row-blocked\""
        } else if is_ready_fn(&s.id) {
            " class=\"row-ready\""
        } else {
            ""
        };

        let priority_cls = match s.priority {
            Priority::Critical => "priority-critical",
            Priority::High => "priority-high",
            Priority::Medium => "priority-medium",
            Priority::Low => "priority-low",
            Priority::None => "priority-none",
        };

        let labels_html = if s.labels.is_empty() {
            String::from("<span class=\"muted\">-</span>")
        } else {
            s.labels
                .iter()
                .map(|l| format!("<span class=\"label\">{}</span>", html_escape(l)))
                .collect::<Vec<_>>()
                .join(" ")
        };

        let assignee = s
            .assignee
            .as_deref()
            .map(html_escape)
            .unwrap_or_else(|| String::from("<span class=\"muted\">-</span>"));

        let updated = &s.updated_at;
        let updated_display = if updated.len() >= 10 {
            html_escape(&updated[..10])
        } else {
            html_escape(updated)
        };

        html.push_str(&format!(
            "<tr{row_class}><td class=\"col-id\">{}</td><td>{}</td><td>{}</td><td><span class=\"priority-badge {priority_cls}\">{}</span></td><td>{labels_html}</td><td>{assignee}</td><td class=\"col-date\">{updated_display}</td></tr>\n",
            html_escape(&s.id),
            html_escape(&s.title),
            html_escape(&s.state),
            html_escape(s.priority.as_str()),
        ));
    }
    html
}
