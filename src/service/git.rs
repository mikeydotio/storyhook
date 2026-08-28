//! `story commit-sync`: linking git commits to the stories they name.
//!
//! Reads the repository's log through `git` itself, finds story ids anywhere in
//! a commit *message*, and records each one in the story's `referenced_by`
//! (SH-169; the `[git] <short-hash>: <subject>` text is still what a reader
//! sees, just no longer inside `comments` — see `domain::StoryCommitLinked`) —
//! moving it into the project's active state the first time a commit
//! **claims** it, if the project has one and has not turned that off.
//!
//! # A mention is not a claim (SH-124)
//!
//! Every id used to move its story. A trailer meaning "this change relates to
//! that story" — `Refs CAL-21, CAL-28, CAL-29` — was read as "I am working on
//! those three now", and because `story next` and `story list --ready` both
//! exclude the active state, each wrongly-moved story was silently removed from
//! the set a contributor picks work from. The failure is quiet and it
//! accumulates: five stories in one sibling project, one of them moved three
//! times from prose alone.
//!
//! [`scan_story_refs`] now answers with an intent per id, and only a
//! [`Claim`](crate::domain::ReferenceIntent::Claim) may move a story. Linking is
//! unchanged and still universal — the comment trail is the half that was always
//! wanted. The grammar lives on `scan_story_refs`; what matters here is that the
//! run **reports what it declined to move**, because a project whose commits use
//! no claim word would otherwise see auto-transition simply stop, with nothing
//! to tell "off" from "broken".
//!
//! This was never a new promise. `story help commit-sync` has always said it
//! "auto-transitions stories based on commit patterns (e.g. `closes SH-1`)";
//! the keyword half had just never been implemented.
//!
//! # Naming the real cause, not just "no claim word" (SH-178)
//!
//! "reports what it declined to move" used to mean one sentence for every
//! story that linked without moving, whatever the reason: "no claim word".
//! That is right for a mention, and wrong for the other four causes —
//! `sync.auto_transition` being off, the project having no active state (the
//! common shape since SH-125: three required OPEN states means the old
//! two-open-state guess never fires), an earlier commit in the same run
//! already having claimed the story, and a story already moved past the
//! default open state. Each told the same lie: that the user's commit
//! grammar was wrong, when nothing about their commits was. [`NotMovedReason`]
//! now travels with the decision that produced it, from
//! [`GitService::record_commit`] outward, and the report groups by it.
//!
//! # The whole message, not the subject line (SH-58)
//!
//! `--format=%H %s` gave the subject and nothing else, so a commit whose body
//! said `Closes SH-12` was invisible. That is not a corner case: this project
//! mandates Conventional Commits, whose subject line is reserved for a summary
//! under 72 characters, so a story reference belongs in the body — and
//! following the style guide therefore guaranteed the feature did nothing.
//!
//! Widening the scan was blocked for two waves for a reason worth recording.
//! Under `.storyhook/`, a link comment dirtied a version-controlled file, so
//! recording it took another commit, whose message named the same stories, so
//! scanning bodies would have linked *that* commit too, ad infinitum (SH-61).
//! What terminated the loop was the accident that bookkeeping commits kept
//! their story ids out of the subject. After the flip a story lives in the
//! store, a link comment writes nothing into the repository, and the loop has
//! no fuel — which is what
//! `tests/commit_sync_termination.rs` exists to prove rather than assume.
//!
//! # Parsing `%B` needs a record separator (also SH-58)
//!
//! `%B` emits multi-line records, and the parser was line-oriented: splitting
//! each line on its first space turns a body line `Closes SH-12` into
//! hash=`Closes`, subject=`SH-12`. That is worse than the miss it replaces,
//! because the short hash is the idempotency key, so a garbage hash breaks
//! "safe to run repeatedly" as well. [`read_log`] therefore asks for
//! `%H%x1f%B%x1e` and splits on the ASCII record and unit separators — control
//! characters git will not emit from a commit message — so **the hash comes
//! from `%H` and never from message text**.
//!
//! # Idempotency
//!
//! A commit already linked to a story is skipped, so re-running over an
//! overlapping window adds nothing. The check ([`already_linked`]) is a
//! primary-key probe against the structured `story_commit_links` table, not a
//! scan of comment text — see that function's doc comment for why a second
//! probe (the seven-character abbreviation) still matters for a pre-#18 row.
//!
//! # A closed story still takes a link (SH-279)
//!
//! [`GitService::record_commit`] used to resolve every story through
//! `resolve_open_story`, so a merge commit whose body named a story that had
//! already closed — the shape this project's own workflow produces on every
//! PR merge — recorded **nothing**: no [`StoryEvent::StoryCommitLinked`], no
//! `referenced_by` entry, no diagnostic. A merged commit vanishing from the
//! story it implements is a hole in the provenance record `referenced_by`
//! exists to carry, and nothing said so.
//!
//! SH-261 had already drawn the line this needed: a closed story's *state,
//! scope and rollups* cannot change, but an append that touches none of them
//! is permitted through [`super::Intent::Append`]. A commit link is such an append —
//! `StoryCommitLinked` folds into `referenced_by_commits` and `updated_at`
//! alone (`domain::fold_story`) — so `record_commit` now resolves under that
//! intent instead. It still never *moves* a closed story: moving is exactly
//! the edit SH-261 kept refused, and [`NotMovedReason::StoryIsClosed`] names
//! the reason locally rather than leaving it to fall out of `default_open`
//! never matching an archived story's resting state by accident.
//!
//! An id naming no story at all is declined and named in the run report rather
//! than swallowed; see [`DeclinedReason`]. "Naming what was
//! declined is what keeps this fix from having the same shape as the defect
//! it removes" (SH-178, above) applies just as much to a decline as to a
//! non-move.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::{
    StateDef, StoryEvent, active_state, default_open_state, has_children, parse_duration,
    scan_story_refs, short_sha,
};
use crate::error::AppError;
use crate::store::{ExpectedSeq, ProjectId, ReadOps, Store, StoreError, StoryNo, WriteOps};

use super::story::state_transition_events;
use super::{Ctx, Intent, append_and_fold, project_prefix};

/// The default scanning window, when `--since` is not given.
const DEFAULT_WINDOW: &str = "7d";

/// The separator between one `git log` record and the next.
///
/// ASCII RS. A commit message cannot contain one — git rejects NUL and this
/// family of control characters never survives an editor — so it cannot be
/// forged from message text, which is the whole point of choosing it over a
/// newline.
const RECORD_SEPARATOR: char = '\u{1e}';

/// The separator between a record's hash and its message. ASCII US; see
/// [`RECORD_SEPARATOR`].
const FIELD_SEPARATOR: char = '\u{1f}';

/// One commit as `git log` reported it.
struct Commit {
    /// The full forty-character hash, from `%H` alone. Nothing in the message
    /// can influence it.
    ///
    /// Full, not abbreviated, because it is what a link record stores: seven
    /// characters are unique in a repository today and not forever, and the
    /// record is permanent. Only the abbreviation is rendered.
    sha: String,
    /// The subject line — the message's first line, and the only part that
    /// reaches the rendered comment.
    subject: String,
    /// The whole message, subject included. This is what is scanned.
    message: String,
}

/// A transition `commit-sync` performed, for its report.
struct Transition {
    story_id: String,
    from: String,
    to: String,
    short_hash: String,
}

/// Why a commit linked a story without moving it (SH-178).
///
/// Before this type existed, every one of these collapsed into one printed
/// sentence — "no claim word" — because [`GitService::record_commit`] threw the
/// reason away and the printer had to guess. That told a user whose project
/// has no active state (the common shape since SH-125: three required OPEN
/// states means the old two-open-state guess never fires) that their commit
/// *grammar* was wrong, when the true cause was a configuration fact. The
/// reason now travels with the decision that produced it.
///
/// Declaration order is the report's group order — see
/// [`GitService::commit_sync`]'s message assembly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum NotMovedReason {
    /// The reference was a mention, not a claim.
    NotAClaim,
    /// `sync.auto_transition` is off for this project.
    AutoTransitionOff,
    /// The project has no state carrying the `active` role, and its open
    /// states don't fit the two-state guess either.
    NoActiveState,
    /// An earlier commit in this run already claimed the story. Never
    /// observed in a rendered report: the story is also in `transitions` by
    /// construction, so [`GitService::commit_sync`]'s cleanup pass always
    /// removes it before the message is built. Kept as a real, named branch
    /// rather than folded into another reason, because the decision that
    /// produces it — the same-run dedup — is a genuine fifth cause, not an
    /// alias for one of the other four.
    AlreadyClaimedThisRun,
    /// The story is closed (SH-279). Unlike the other reasons, this one is a
    /// fact about the story rather than about this run or this commit's
    /// grammar — declared after them so a run-level cause still wins the
    /// declaration-order priority [`eligible_active_state`] documents, and
    /// before [`NotInDefaultState`] because a closed story's resting state is
    /// never the project's default *open* one, so this is the more specific,
    /// more actionable of the two.
    StoryIsClosed,
    /// Structural epics derive state from their children and cannot be claimed
    /// by a commit mention.
    StoryIsEpic,
    /// The story has already moved out of the project's default open state.
    NotInDefaultState,
}

impl NotMovedReason {
    /// The report line naming why `story_ids` linked without moving.
    fn report_line(self, story_ids: &[String]) -> String {
        let ids = story_ids.join(", ");
        match self {
            NotMovedReason::NotAClaim => {
                format!("linked without claiming: {ids} (no claim word, so state unchanged)")
            }
            NotMovedReason::AutoTransitionOff => format!(
                "linked without moving: {ids} (sync.auto_transition is off for this project)"
            ),
            NotMovedReason::NoActiveState => format!(
                "linked without moving: {ids} (this project has no active state configured)"
            ),
            NotMovedReason::AlreadyClaimedThisRun => format!(
                "linked without moving: {ids} (an earlier commit in this run already claimed it)"
            ),
            NotMovedReason::StoryIsClosed => {
                format!("linked without moving: {ids} (the story is closed)")
            }
            NotMovedReason::StoryIsEpic => format!(
                "linked without moving: {ids} (the story is an epic; its state is computed from children)"
            ),
            NotMovedReason::NotInDefaultState => format!(
                "linked without moving: {ids} (already out of the project's default open state)"
            ),
        }
    }
}

/// What linking one commit to one already-open, not-yet-linked story did.
enum LinkOutcome {
    /// The story moved, from `from` to `to`.
    Moved { from: String, to: String },
    /// The commit linked the story but did not move it, and why.
    NotMoved(NotMovedReason),
}

/// Why an id could not be linked to a story **at all** (SH-279) — as opposed
/// to [`NotMovedReason`], which explains a link that landed but did not move
/// its story. Both exist for the same reason: a decline that is not named is
/// indistinguishable from a defect, which is what this story was filed about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum DeclinedReason {
    /// No story with this id exists in this project — a typo, a deleted
    /// story, or an id belonging to a different repository entirely.
    NoSuchStory,
}

impl DeclinedReason {
    /// The report line naming why `story_ids` were named but never linked.
    ///
    fn report_line(self, story_ids: &[String]) -> String {
        let ids = story_ids.join(", ");
        match self {
            DeclinedReason::NoSuchStory => {
                format!("named but not linked: {ids} (no such story)")
            }
        }
    }
}

/// What resolving one (commit, story id) pair did.
enum RecordOutcome {
    /// The commit was already linked to this story; nothing changed.
    AlreadyLinked,
    /// The id could not be resolved as an append target, and why.
    Declined(DeclinedReason),
    /// A link was added, and whether the story moved.
    Linked(LinkOutcome),
}

/// Whether `story_id` is eligible to move on this commit, and if not, why.
///
/// Checked in [`NotMovedReason`]'s own declaration order: grammar and project
/// configuration are questions about this run alone, asked before anything
/// about this individual story's own history.
fn eligible_active_state(
    claims: bool,
    auto_transition: bool,
    active: Option<&StateDef>,
    already_claimed_this_run: bool,
) -> Result<&StateDef, NotMovedReason> {
    if !claims {
        return Err(NotMovedReason::NotAClaim);
    }
    if !auto_transition {
        return Err(NotMovedReason::AutoTransitionOff);
    }
    let active = active.ok_or(NotMovedReason::NoActiveState)?;
    if already_claimed_this_run {
        return Err(NotMovedReason::AlreadyClaimedThisRun);
    }
    Ok(active)
}

/// Git integration over one project in one store.
pub struct GitService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> GitService<'ctx, S> {
    /// A git service bound to `ctx`.
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// Scans the repository's recent commits and records the stories they name.
    ///
    /// `since` is a duration such as `2h`, `1d` or `1w`; absent, the window is
    /// seven days.
    ///
    /// Each story that a commit names is updated in **one transaction**: its
    /// link and, when this is the first commit to mention it, its move into
    /// the active state land together or not at all. The legacy path wrote them
    /// as two separate appends, so an interruption between them left a story
    /// linked but not moved.
    ///
    /// Fires no event hooks, which is what the legacy path did — a `commit-sync`
    /// over a week of history would otherwise fire a burst of `comment` and
    /// `state_change` hooks for work that happened days ago.
    pub fn commit_sync(&self, since: Option<&str>) -> Result<String, AppError> {
        require_git_repository(self.ctx.cwd())?;
        let window = since.unwrap_or(DEFAULT_WINDOW);
        let duration = parse_duration(window).ok_or_else(|| {
            AppError::Validation(format!("invalid duration `{window}` (use e.g. 2h, 1d, 1w)"))
        })?;
        let cutoff =
            (chrono::Utc::now() - duration).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let commits = read_log(self.ctx.cwd(), &cutoff)?;

        let project = self.ctx.project();
        // The receipt (SH-316), written before anything is decided about what
        // the log contained and **unconditionally** — not gated on commits
        // found, links made or stories touched. A project whose commits name no
        // story is the ordinary case, and a receipt that only advanced when a
        // link happened would leave every such project looking permanently
        // unscanned. What it attests is exactly this: a scan of this project's
        // commits ran here, at this instant. See migration 0014's header for
        // what may not be inferred from it.
        let scanned_at = self.ctx.now();
        self.ctx
            .store()
            .write(|tx| tx.record_commit_scan(project, &scanned_at))?;

        let (prefix, active, default_open, auto_transition) = self.ctx.store().read(|tx| {
            let states = tx.states(project)?;
            Ok((
                project_prefix(tx, project)?,
                active_state(&states),
                default_open_state(&states),
                tx.settings(project)?.sync_auto_transition.unwrap_or(true),
            ))
        })?;

        let mut commits_linked = 0usize;
        let mut stories_touched: BTreeSet<String> = BTreeSet::new();
        let mut transitions: Vec<Transition> = Vec::new();
        let mut linked_only: BTreeMap<String, NotMovedReason> = BTreeMap::new();
        // Every id `scan_story_refs` found that never became a link at all —
        // no such story (SH-279). Distinct from
        // `linked_only`: those stories link every time and only fail to move.
        let mut declined: BTreeMap<String, DeclinedReason> = BTreeMap::new();

        for commit in &commits {
            // The whole message, subject included — the comment still carries
            // the subject alone, because a multi-paragraph body in `story show`
            // would be unreadable.
            for reference in scan_story_refs(&prefix, &commit.message) {
                let claims = reference.claims();
                let story_id = reference.id;
                // Only a claim is eligible to move a story, and only the first
                // one in this run (SH-124). `eligible_active_state` names which
                // of those is false when this reference is not eligible, so the
                // report can say which — rather than, as before SH-178, folding
                // every one of them into "no claim word".
                let eligibility = eligible_active_state(
                    claims,
                    auto_transition,
                    active.as_ref(),
                    transitions.iter().any(|t| t.story_id == story_id),
                );
                let outcome = self.record_commit(
                    &prefix,
                    &story_id,
                    commit,
                    eligibility,
                    default_open.as_ref(),
                )?;
                match outcome {
                    RecordOutcome::AlreadyLinked => {}
                    // First cause wins, same as `linked_only` below: a story's
                    // decline reason cannot change mid-run (nothing in a sync
                    // deletes or creates stories), so this only ever guards
                    // against re-recording the identical reason twice.
                    RecordOutcome::Declined(reason) => {
                        declined.entry(story_id).or_insert(reason);
                    }
                    RecordOutcome::Linked(link_outcome) => {
                        commits_linked += 1;
                        stories_touched.insert(story_id.clone());
                        match link_outcome {
                            LinkOutcome::Moved { from, to } => transitions.push(Transition {
                                story_id,
                                from,
                                to,
                                short_hash: short_sha(&commit.sha).to_string(),
                            }),
                            // First cause wins: once a story has a recorded
                            // reason, a later commit's differently-caused
                            // non-move (there rarely is one) does not
                            // overwrite it.
                            LinkOutcome::NotMoved(reason) => {
                                linked_only.entry(story_id).or_insert(reason);
                            }
                        }
                    }
                }
            }
        }

        // A story that a later commit claimed is not "linked only", however
        // many earlier commits merely mentioned it.
        for transition in &transitions {
            linked_only.remove(&transition.story_id);
        }

        let mut message = format!(
            "scanned {} commits, linked {} commits to {} stories",
            commits.len(),
            commits_linked,
            stories_touched.len()
        );
        for transition in &transitions {
            message.push_str(&format!(
                "\n{}: {} \u{2192} {} (referenced in {})",
                transition.story_id, transition.from, transition.to, transition.short_hash
            ));
        }
        // Naming what was declined is what keeps this fix from having the same
        // shape as the defect it removes. Grouped by the actual cause (SH-178)
        // rather than asserted as one cause for all of them — a project with no
        // active state is told that, which is actionable, rather than being
        // told its commit grammar is wrong, which is not.
        let mut by_reason: BTreeMap<NotMovedReason, Vec<String>> = BTreeMap::new();
        for (story_id, reason) in &linked_only {
            by_reason.entry(*reason).or_default().push(story_id.clone());
        }
        for (reason, story_ids) in &by_reason {
            message.push_str(&format!("\n{}", reason.report_line(story_ids)));
        }
        // Declined references, the same way and after the not-moved ones —
        // "linked but stuck" and "never linked" are different enough causes
        // that a reader should not have to guess which list an id is in.
        let mut declined_by_reason: BTreeMap<DeclinedReason, Vec<String>> = BTreeMap::new();
        for (story_id, reason) in &declined {
            declined_by_reason
                .entry(*reason)
                .or_default()
                .push(story_id.clone());
        }
        for (reason, story_ids) in &declined_by_reason {
            message.push_str(&format!("\n{}", reason.report_line(story_ids)));
        }
        Ok(message)
    }

    /// Records one commit against one story, in one transaction.
    ///
    /// [`RecordOutcome::AlreadyLinked`] means there was nothing to do: this
    /// commit is already linked to this story. [`RecordOutcome::Declined`]
    /// means the id could not be resolved as an append target at all — not a
    /// story of this project — and names which.
    /// [`RecordOutcome::Linked`] means a link was added: `NotMoved(_)` names
    /// why the story did not move — a closed story links every time and
    /// never moves, same as any other reason — and `Moved { .. }` means it
    /// did.
    fn record_commit(
        &self,
        prefix: &str,
        story_id: &str,
        commit: &Commit,
        active: Result<&StateDef, NotMovedReason>,
        default_open: Option<&StateDef>,
    ) -> Result<RecordOutcome, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            // `Intent::Append`, not `resolve_open_story` (SH-279): a link
            // reaches only `referenced_by_commits` and `updated_at`, which is
            // the SH-261 argument for permitting an append against a closed
            // story. `NotFound` (no such story in this project) and
            // `Validation` is a real failure; anything other than NotFound is a
            // real store error and must propagate rather than read as a
            // silent skip.
            let (story_no, row) = match Intent::Append.resolve(&*tx, project, prefix, story_id) {
                Ok(resolved) => resolved,
                Err(AppError::NotFound(_)) => {
                    return Ok(RecordOutcome::Declined(DeclinedReason::NoSuchStory));
                }
                Err(err) => return Err(err.into()),
            };
            if already_linked(&*tx, project, story_no, &commit.sha)? {
                return Ok(RecordOutcome::AlreadyLinked);
            }

            let states = tx.state_map(project)?;
            let mut events = vec![StoryEvent::StoryCommitLinked {
                at: now.clone(),
                sha: commit.sha.clone(),
                subject: commit.subject.clone(),
            }];
            // A story moves on the *first* commit that mentions it and only out
            // of the project's default open state — a story someone has already
            // moved on to review is not dragged back. `default_open` is always
            // `Some` for a conforming project (`todo` is a REQUIRED_STATES OPEN
            // state, SH-125), but the match stays total rather than assuming it.
            //
            // A run-level refusal (`Err(reason)` — no claim word, sync off, no
            // active state, already claimed this run) wins over the story's own
            // closed-ness, per `NotMovedReason`'s declaration order: those are
            // questions about this run, asked first. `row.archived` is checked
            // next and explicitly, rather than left to fall out of `default_open`
            // never matching an archived story's resting state by accident — a
            // closed story never moves, and this is why, named rather than
            // inferred.
            let outcome = match (active, default_open) {
                (Err(reason), _) => LinkOutcome::NotMoved(reason),
                (Ok(_), _) if row.archived => LinkOutcome::NotMoved(NotMovedReason::StoryIsClosed),
                (Ok(_), _) if has_children(&row.snapshot) => {
                    LinkOutcome::NotMoved(NotMovedReason::StoryIsEpic)
                }
                (Ok(active), Some(default_open)) if row.snapshot.state == default_open.slug => {
                    events.extend(state_transition_events(
                        active,
                        row.snapshot.awaiting.is_some(),
                        &now,
                        Vec::new(),
                    ));
                    LinkOutcome::Moved {
                        from: row.snapshot.state.clone(),
                        to: active.slug.clone(),
                    }
                }
                (Ok(_), _) => LinkOutcome::NotMoved(NotMovedReason::NotInDefaultState),
            };

            append_and_fold(
                tx,
                project,
                story_no,
                prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &events,
                self.ctx.provenance(),
            )?;
            Ok(RecordOutcome::Linked(outcome))
        })?)
    }
}

/// Whether this commit is already linked to this story.
///
/// **Two probes, and the second is not redundant.** A record this binary writes
/// carries the full forty-character hash, so that is the first question. Rows
/// backfilled from *pre*-#18 link comments carry the seven-character
/// abbreviation, because `[git] <short>: <subject>` is all such a comment ever
/// preserved — and those rows are not merely historical: every unmigrated
/// `.storyhook` tree is full of them, and `story migrate` projects them the day
/// it reads one. Asking only about the full hash would re-link every commit a
/// migrated project already knew about.
///
/// Both are indexed primary-key probes, so this replaces a scan of every event
/// on the story with two lookups — and, unlike that scan, it cannot be
/// satisfied by a user typing a comment that starts `[git] `.
fn already_linked(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    sha: &str,
) -> Result<bool, StoreError> {
    Ok(tx.commit_linked(project, story, sha)?
        || tx.commit_linked(project, story, short_sha(sha))?)
}

/// Arms this project's receipt, or reports what git says about the commits
/// since it (SH-316).
///
/// The two halves are one function because they are one rule with one
/// precondition — a project has a receipt, or it is given one — and splitting
/// them would let a caller do the second without ever having done the first.
///
/// A project with **no receipt** is armed to `now` and answers `None`. That
/// arming is the whole reason a hook which never fired *once* is detectable:
/// without it the column stays NULL forever, nothing ever advances it, and the
/// most important failure this exists to catch would be the one case it could
/// never report. A receipt that is already set is **never** re-armed — see
/// [`WriteOps::arm_commit_scan`](crate::store::WriteOps::arm_commit_scan) — and
/// that asymmetry is load-bearing: overwriting a stale receipt would erase the
/// evidence the re-run exists to surface.
///
/// Nothing here decides whether a hook is installed, and no caller may report it
/// as though it had.
pub fn commit_scan_notice<S: Store>(
    store: &S,
    project: ProjectId,
    root: &std::path::Path,
    now: &str,
) -> Result<Option<String>, AppError> {
    let Some(scanned_at) = store.read(|tx| tx.commit_scan_at(project))? else {
        store.write(|tx| tx.arm_commit_scan(project, now))?;
        return Ok(None);
    };
    Ok(unscanned_commits(root, &scanned_at, now))
}

/// What git says about commits this store has no record of scanning (SH-316) —
/// or `None`, which is the answer in every case but one.
///
/// `scanned_at` is the receipt: the instant a `commit-sync` last ran for this
/// project. This asks git for the commits dated after it, and says so. It is
/// deliberately **not** a hook check, and nothing here can tell whether a hook
/// exists: the receipt records a scan, git records commits, and the only claim
/// available from those two facts is that some commits have not been scanned.
///
/// # Silence is the default, and every uncertainty resolves to it
///
/// `None` — print nothing at all — for a repository git will not answer about
/// (no git, no repository, no commits: [`git_env::output`] collapses all three,
/// which is harmless here precisely because they share one verdict), and for a
/// log that comes back empty. That is what keeps a record which disagrees with
/// the disk from ever becoming a claim.
///
/// # The `--until` is not decoration — it answers the dissent
///
/// The bound is `scanned_at < committed <= now`, and the upper half is what
/// makes the advisory **self-clearing**. A committer date in the future
/// post-dates every receipt this store will write for as long as the clock takes
/// to catch up, so without `--until` a single skewed commit would keep the
/// warning alive *after* the user ran the remedy — an advisory escapable only by
/// learning to ignore advisories, which is SH-306's own defect class, and the
/// ground on which SH-316's council dissented from this comparator. With it, the
/// remedy sets the receipt to now and nothing dated at-or-before now can remain
/// unscanned, so the next run is silent by construction.
///
/// The cost is named rather than hidden: a future-dated commit is invisible to
/// this detector. So is a backdated one, and for the same reason — the remedy
/// itself scans by committer date, so a detector that reported what the remedy
/// cannot fix would be reporting an unrunnable errand.
fn unscanned_commits(root: &std::path::Path, scanned_at: &str, now: &str) -> Option<String> {
    let log = crate::env::git_env::output(
        root,
        &[
            "log",
            "--format=%cI",
            &format!("--since={}", first_unscanned_second(scanned_at)),
            &format!("--until={now}"),
        ],
    )?;
    let count = log.lines().filter(|line| !line.is_empty()).count();
    if count == 0 {
        return None;
    }

    let commits = if count == 1 {
        "1 commit is".to_string()
    } else {
        format!("{count} commits are")
    };
    // The window has to reach back to the receipt, or the command storyhook
    // prints does not cover the gap it just reported. Rounded up for the same
    // reason, and rounding up is free: `commit-sync` is idempotent.
    let window = window_reaching(scanned_at, now);
    Some(format!(
        "{commits} dated after this store last scanned this project's commits \
         ({scanned_at}). A post-commit hook git no longer runs looks like this; \
         so does a repository whose commits were scanned from another checkout, \
         or by another store. Run `story commit-sync --since {window}` to scan \
         them now."
    ))
}

/// The first second a commit could be in and genuinely not have been scanned:
/// one after the receipt.
///
/// **Measured on git 2.50.1: `--since` is inclusive.** `git log
/// --since=<T>` returns a commit whose committer date is exactly `T`. Both
/// quantities here have one-second resolution, so passing the receipt unshifted
/// reports every commit made in the same second as the scan — and the scan
/// itself covers that second, since `commit-sync` cuts off at `now` minus its
/// window and is inclusive in the same way. Those commits were scanned. Worse
/// than the wrong count, they are unclearable: running the remedy re-stamps the
/// receipt at some second, and any commit sharing it stays "unscanned" forever,
/// which is exactly the survives-its-own-remedy failure the council dissented
/// about.
///
/// Falls back to the receipt verbatim if it will not parse, which can only
/// over-report — and a receipt this store did not write is not a shape that
/// exists.
fn first_unscanned_second(scanned_at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(scanned_at)
        .ok()
        .and_then(|at| at.checked_add_signed(chrono::Duration::seconds(1)))
        .map_or_else(
            || scanned_at.to_string(),
            |at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
}

/// A `--since` argument that certainly reaches back to `scanned_at`.
///
/// Rounded **up**, and coarse on purpose: this is a duration a human is about to
/// retype, so `19d` is worth more than an exact `18d7h13m`, and scanning further
/// back than needed costs nothing (link idempotency is a database constraint
/// since W6). Falls back to the widest of the three units when either instant is
/// unparseable, because a window that is too wide still works and a window that
/// is too narrow silently misses the commits this sentence just promised to
/// cover.
fn window_reaching(scanned_at: &str, now: &str) -> String {
    let span = chrono::DateTime::parse_from_rfc3339(now)
        .ok()
        .zip(chrono::DateTime::parse_from_rfc3339(scanned_at).ok())
        .map(|(now, then)| now.signed_duration_since(then));
    let Some(span) = span else {
        return "1w".to_string();
    };
    let minutes = span.num_seconds().div_euclid(60) + 1;
    match minutes {
        i64::MIN..=60 => format!("{minutes}m"),
        61..=2880 => format!("{}h", minutes.div_euclid(60) + 1),
        _ => format!("{}d", minutes.div_euclid(1440) + 1),
    }
}

/// Fails unless `root` is inside a git repository.
///
/// Reported as [`AppError::Validation`] with the same sentence the legacy path
/// used: `commit-sync` outside a repository is a usage mistake, not a storage
/// failure, and scripts test for it.
fn require_git_repository(root: &std::path::Path) -> Result<(), AppError> {
    let probed = crate::env::git_env::command(root)
        .args(["rev-parse", "--git-dir"])
        .output();
    match probed {
        Ok(output) if output.status.success() => Ok(()),
        _ => Err(AppError::Validation("not a git repository".to_string())),
    }
}

/// The commits reachable from HEAD that are newer than `cutoff`.
///
/// One record per commit, `<full hash>\x1f<whole message>\x1e`, parsed by
/// separator rather than by line. See this module's header for why the format
/// is not `%H %B`: a line-oriented parse of a multi-line message reads body
/// text as a commit hash, and the hash is the idempotency key.
fn read_log(root: &std::path::Path, cutoff: &str) -> Result<Vec<Commit>, AppError> {
    let format = format!("--format=%H{FIELD_SEPARATOR}%B{RECORD_SEPARATOR}");
    let output = crate::env::git_env::command(root)
        .args(["log", &format, &format!("--since={cutoff}")])
        .output()
        .map_err(|error| AppError::Storage(format!("failed to run git: {error}")))?;
    if !output.status.success() {
        return Err(AppError::Storage(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(parse_log(&String::from_utf8_lossy(&output.stdout)))
}

/// Splits `git log`'s separator-delimited output into commits.
///
/// Separated from [`read_log`] so the parse — the part SH-58 got wrong — can be
/// tested against fabricated output, including the shapes a real repository
/// makes it awkward to produce.
fn parse_log(text: &str) -> Vec<Commit> {
    let mut commits = Vec::new();
    for record in text.split(RECORD_SEPARATOR) {
        // git puts a newline between one record's terminator and the next
        // record's hash. Leading whitespace is therefore separator noise, and
        // the trailing newline `%B` always appends is not part of the message.
        let record = record.trim_start_matches(['\n', '\r']);
        if record.is_empty() {
            continue;
        }
        // A record with no field separator is not something git can produce.
        // Skipping it is deliberate: the alternative is inventing a hash, and a
        // wrong hash is the idempotency bug this parse exists to avoid.
        let Some((hash, message)) = record.split_once(FIELD_SEPARATOR) else {
            continue;
        };
        let message = message.trim_end_matches(['\n', '\r']);
        commits.push(Commit {
            sha: hash.to_string(),
            // A commit with an empty message has no first line. It is still
            // *scanned* — the count has always included it — but names nothing.
            subject: message.lines().next().unwrap_or("").to_string(),
            message: message.to_string(),
        });
    }
    commits
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one `git log` record in the format [`read_log`] asks for.
    fn record(hash: &str, message: &str) -> String {
        format!("{hash}{FIELD_SEPARATOR}{message}\n{RECORD_SEPARATOR}")
    }

    #[test]
    fn a_record_yields_its_hash_subject_and_whole_message() {
        let commits = parse_log(&record(
            "0123456789abcdef0123456789abcdef01234567",
            "feat: a thing\n\nCloses SH-1",
        ));
        assert_eq!(commits.len(), 1);
        assert_eq!(short_sha(&commits[0].sha), "0123456");
        assert_eq!(commits[0].subject, "feat: a thing");
        assert_eq!(commits[0].message, "feat: a thing\n\nCloses SH-1");
    }

    /// The SH-58 trap: a body line that looks like the *old* format's output.
    ///
    /// Under `--format=%H %s` parsed per line, `deadbeef SH-1` became a commit
    /// with hash `deadbeef`. The separator format cannot express that, and this
    /// is what says so.
    #[test]
    fn body_text_shaped_like_a_log_line_is_message_not_hash() {
        let commits = parse_log(&record(
            "0123456789abcdef0123456789abcdef01234567",
            "fix: something\n\ndeadbeef SH-1 was the old parse's idea of a commit",
        ));
        assert_eq!(commits.len(), 1, "one commit, not two");
        assert_eq!(short_sha(&commits[0].sha), "0123456");
    }

    #[test]
    fn records_are_separated_by_the_record_separator_not_by_newlines() {
        let text = format!(
            "{}{}",
            record("aaaaaaaaaaaaaaaa", "one\n\nline two\nline three"),
            record("bbbbbbbbbbbbbbbb", "two")
        );
        let commits = parse_log(&text);
        assert_eq!(commits.len(), 2);
        assert_eq!(short_sha(&commits[0].sha), "aaaaaaa");
        assert_eq!(short_sha(&commits[1].sha), "bbbbbbb");
        assert_eq!(commits[1].subject, "two");
    }

    #[test]
    fn an_empty_log_yields_no_commits() {
        assert!(parse_log("").is_empty());
        assert!(parse_log("\n").is_empty());
    }

    /// A commit with an empty message is still counted, and names nothing.
    #[test]
    fn an_empty_message_is_a_commit_with_an_empty_subject() {
        let commits = parse_log(&record("cccccccccccccccc", ""));
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "");
        assert_eq!(commits[0].message, "");
    }

    /// Nothing git produces, so nothing is invented for it: a record with no
    /// field separator is skipped rather than given a fabricated hash.
    #[test]
    fn a_record_without_a_field_separator_is_skipped() {
        let text = format!("no-separator-here\n{RECORD_SEPARATOR}");
        assert!(parse_log(&text).is_empty());
    }

    // -------------------------------------------------------------------
    // eligible_active_state (SH-178) — every branch, exhaustively
    // -------------------------------------------------------------------

    fn sample_active_state() -> crate::domain::StateDef {
        crate::domain::StateDef {
            slug: "in-progress".to_string(),
            super_state: crate::domain::SuperState::Open,
            role: Some(crate::domain::STATE_ROLE_ACTIVE.to_string()),
            description: None,
        }
    }

    #[test]
    fn a_mention_is_never_eligible_regardless_of_everything_else() {
        let state = sample_active_state();
        assert_eq!(
            eligible_active_state(false, true, Some(&state), false),
            Err(NotMovedReason::NotAClaim)
        );
        // Grammar is checked first: even a project with the setting off and no
        // active state configured is still reported as "not a claim", because
        // that is the one true thing about this reference.
        assert_eq!(
            eligible_active_state(false, false, None, true),
            Err(NotMovedReason::NotAClaim)
        );
    }

    #[test]
    fn a_claim_with_auto_transition_off_is_not_eligible() {
        let state = sample_active_state();
        assert_eq!(
            eligible_active_state(true, false, Some(&state), false),
            Err(NotMovedReason::AutoTransitionOff)
        );
    }

    #[test]
    fn a_claim_with_no_active_state_is_not_eligible() {
        assert_eq!(
            eligible_active_state(true, true, None, false),
            Err(NotMovedReason::NoActiveState)
        );
    }

    #[test]
    fn a_claim_already_used_by_an_earlier_commit_this_run_is_not_eligible() {
        let state = sample_active_state();
        assert_eq!(
            eligible_active_state(true, true, Some(&state), true),
            Err(NotMovedReason::AlreadyClaimedThisRun)
        );
    }

    #[test]
    fn a_claim_with_everything_else_satisfied_is_eligible() {
        let state = sample_active_state();
        assert_eq!(
            eligible_active_state(true, true, Some(&state), false),
            Ok(&state)
        );
    }

    #[test]
    fn every_reason_report_line_names_its_own_cause_not_no_claim_word() {
        let ids = vec!["SH-1".to_string(), "SH-2".to_string()];
        let cases = [
            (
                NotMovedReason::NotAClaim,
                "linked without claiming: SH-1, SH-2 (no claim word, so state unchanged)",
            ),
            (
                NotMovedReason::AutoTransitionOff,
                "linked without moving: SH-1, SH-2 (sync.auto_transition is off for this project)",
            ),
            (
                NotMovedReason::NoActiveState,
                "linked without moving: SH-1, SH-2 (this project has no active state configured)",
            ),
            (
                NotMovedReason::AlreadyClaimedThisRun,
                "linked without moving: SH-1, SH-2 (an earlier commit in this run already claimed it)",
            ),
            (
                NotMovedReason::StoryIsClosed,
                "linked without moving: SH-1, SH-2 (the story is closed)",
            ),
            (
                NotMovedReason::StoryIsEpic,
                "linked without moving: SH-1, SH-2 (the story is an epic; its state is computed from children)",
            ),
            (
                NotMovedReason::NotInDefaultState,
                "linked without moving: SH-1, SH-2 (already out of the project's default open state)",
            ),
        ];
        for (reason, expected) in cases {
            assert_eq!(reason.report_line(&ids), expected);
            if reason != NotMovedReason::NotAClaim {
                assert!(
                    !reason.report_line(&ids).contains("no claim word"),
                    "only a real grammar miss may say so: {reason:?}"
                );
            }
        }
    }

    #[test]
    fn every_declined_reason_report_line_names_its_own_cause() {
        let ids = vec!["SH-1".to_string(), "SH-2".to_string()];
        let cases = [(
            DeclinedReason::NoSuchStory,
            "named but not linked: SH-1, SH-2 (no such story)",
        )];
        for (reason, expected) in cases {
            assert_eq!(reason.report_line(&ids), expected);
        }
    }
}
