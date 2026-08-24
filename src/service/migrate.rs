//! `story migrate`: one legacy `.storyhook` tree into the store.
//!
//! # Three rules, in priority order
//!
//! 1. **Never touch the source.** The tree the operator started with is their
//!    rollback. [`crate::legacy`] cannot write, and this module reads through
//!    it and nothing else.
//! 2. **Never lose anything quietly.** A legacy tree can hold shapes the store
//!    refuses (SH-60). Each one is either *repaired faithfully and reported*,
//!    or it stops the migration with a list naming every instance. There is no
//!    third option — in particular there is no "skip the bad ones".
//! 3. **Refuse a second time.** A migration is not idempotent in the sense of
//!    "runs twice harmlessly"; it is idempotent in the sense of "refuses the
//!    second time and says why". Merging would guess at ids; duplicating would
//!    mint the second copy of a project, which is the exact corruption this
//!    program was written to end.
//!
//! # The two SH-60 families, and the one rule that settles both
//!
//! A **one-sided relation** — `SH-1` claims `parent-of SH-2` while `SH-2`'s
//! history never mentions it — is repairable without inventing anything: the
//! missing half is a missing event, so the migration writes it, stamped with
//! the instant the surviving half was written and inserted in timestamp order.
//! Nothing is dropped, and both histories then fold to the same edge.
//!
//! **Multiple parents** is the other family, and the two are not independent:
//! completing a one-sided `parent-of` is the thing that hands a story a second
//! parent. This repository's own tree is the worked example — ten one-sided
//! relations, and completing them naively produces five stories with two
//! parents each.
//!
//! One rule settles both: **agreement beats assertion**. A parentage recorded
//! by *both* stories outranks one recorded by a single story, so the unilateral
//! claim is retracted rather than completed. When the rule has nothing to weigh
//! — two mutual parents, or several that all disagree — the migration refuses
//! and names them, because at that point storyhook would be inventing an
//! answer. [`settle_parent_conflicts`] holds the whole argument, including why
//! refusing every conflict outright was rejected.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::domain::provenance::Provenance;
use crate::domain::{
    Priority, StateDef, StoryEvent, StorySnapshot, fold_story, inverse_relation,
    validate_state_defs_for_write, validate_type_slug,
};
use crate::error::AppError;
use crate::legacy::{LegacyProject, LegacyStory};
use crate::store::{
    EventSeq, ExpectedSeq, LinkSource, NewProject, ProjectSettings, RawEvent, ReadOps, Store,
    StoreError, StoryNo, StoryQuery, WriteOps, partition_known,
};

use super::project::default_types;
use super::project::{ProjectPointer, pointer_path, read_pointer, unique_slug, write_pointer};
use super::state_set::write_states_repairing;
use super::story::default_story_type;
use super::transfer::restore_default_events;

/// What a repair did to one story's history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepairKind {
    /// The missing half of a relation only the far end's history claimed. An
    /// addition: nothing is dropped, and both ends then agree.
    CompletedInverse,
    /// A parentage only one history asserted, annulled because the same child
    /// has a parent both histories agree on.
    RetractedUnilateralParent {
        /// The story that had more than one parent.
        child: String,
        /// The parentage that was kept, because both ends record it.
        winner: String,
    },
}

/// One SH-60 violation, and the event written to settle it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repair {
    /// The story whose history gains an event.
    pub story: String,
    /// The relation the new event names.
    pub relation: String,
    /// The story at the far end.
    pub other: String,
    /// The timestamp the repair carries — the instant of the claim it
    /// completes or annuls, never the migration's, so a story's `updated_at`
    /// does not jump to the day it was migrated.
    pub at: String,
    /// Which of the two repairs this is.
    pub kind: RepairKind,
}

impl Repair {
    /// The event this repair appends to [`story`](Self::story)'s history.
    fn event(&self) -> StoryEvent {
        match self.kind {
            RepairKind::CompletedInverse => StoryEvent::StoryRelationshipAdded {
                at: self.at.clone(),
                other_id: self.other.clone(),
                relation: self.relation.clone(),
            },
            RepairKind::RetractedUnilateralParent { .. } => StoryEvent::StoryRelationshipRemoved {
                at: self.at.clone(),
                other_id: self.other.clone(),
                relation: self.relation.clone(),
            },
        }
    }
}

impl std::fmt::Display for Repair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            RepairKind::CompletedInverse => write!(
                f,
                "{}: added the missing `{} {}` — only {} recorded that edge (at {})",
                self.story, self.relation, self.other, self.other, self.at
            ),
            RepairKind::RetractedUnilateralParent { child, winner } => write!(
                f,
                "{}: retracted `{} {}` — only {} recorded it, and {child}'s parentage under \
                 {winner} is recorded by both ends (at {})",
                self.story, self.relation, self.other, self.story, self.at
            ),
        }
    }
}

/// An event this binary could not decode, kept verbatim and reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedUnknown {
    /// The story it belongs to.
    pub story: String,
    /// Its `kind` discriminant.
    pub kind: String,
}

/// What a migration did, or — under `--dry-run` — what it would do.
#[derive(Clone, Debug)]
pub struct MigrationReport {
    /// The legacy tree that was read.
    pub source: PathBuf,
    /// The story-id prefix the project keeps.
    pub prefix: String,
    /// Nothing was written.
    pub dry_run: bool,
    /// Stories imported.
    pub stories: usize,
    /// Events imported, repairs included.
    pub events: usize,
    /// How many of those stories are archived.
    pub archived: usize,
    /// How many are soft-deleted.
    pub deleted: usize,
    /// Configured states, types and members carried over.
    pub states: usize,
    /// Configured story types carried over.
    pub types: usize,
    /// Members carried over.
    pub members: usize,
    /// The next story number `story new` will mint.
    pub next_story_no: u64,
    /// Every one-sided relation that was repaired.
    pub repairs: Vec<Repair>,
    /// Legacy histories extended with current required metadata events.
    pub metadata_repairs: Vec<MetadataRepair>,
    /// Every event kind this binary does not understand, retained anyway.
    pub unknown_events: Vec<RetainedUnknown>,
    /// The settings this migration found in the tree, as lines describing
    /// each — most carried into the store, one (github-sync's own
    /// configuration, since SH-408) named but deliberately not.
    ///
    /// An **inventory**, not a warning. It read as a warning until SH-133,
    /// because the export envelope had nowhere for these and the report was the
    /// only place an operator would learn a rollback would drop them. The
    /// envelope carries them now; what the line still does is tell an operator
    /// what the tree turned out to be configured with — or, since SH-408,
    /// what it held that this migration left behind and why.
    pub settings: Vec<String>,
}

/// One required-metadata event appended to a legacy story history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataRepair {
    /// The story whose history gains the event.
    pub story: String,
    /// The field and value supplied by the destination project's defaults.
    pub kind: MetadataRepairKind,
}

/// Which required field a legacy history lacked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataRepairKind {
    /// The project's first configured story type.
    Type { story_type: String },
    /// The lowest assignable priority.
    PriorityLow,
}

impl MetadataRepair {
    /// The current event this compatibility repair appends.
    #[must_use]
    pub fn event(&self, at: &str) -> StoryEvent {
        match &self.kind {
            MetadataRepairKind::Type { story_type } => StoryEvent::StoryTypeSet {
                at: at.to_string(),
                story_type: story_type.clone(),
            },
            MetadataRepairKind::PriorityLow => StoryEvent::StoryPrioritySet {
                at: at.to_string(),
                priority: Priority::Low,
            },
        }
    }
}

impl MigrationReport {
    /// The human rendering `story migrate` prints.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let verb = if self.dry_run {
            "would import"
        } else {
            "imported"
        };
        out.push_str(&format!(
            "{verb} {} stories ({} archived, {} soft-deleted) and {} events from {}\n",
            self.stories,
            self.archived,
            self.deleted,
            self.events,
            self.source.display()
        ));
        out.push_str(&format!(
            "  prefix {}, {} states, {} types, {} members, next story {}-{}\n",
            self.prefix, self.states, self.types, self.members, self.prefix, self.next_story_no
        ));
        for setting in &self.settings {
            out.push_str(&format!("  setting: {setting}\n"));
        }

        if self.repairs.is_empty() {
            out.push_str("  repairs: none — every relation is claimed by both ends\n");
        } else {
            let completed = self
                .repairs
                .iter()
                .filter(|repair| repair.kind == RepairKind::CompletedInverse)
                .count();
            out.push_str(&format!(
                "  repairs (SH-60): {completed} one-sided relation{} completed, {} unilateral \
                 parent claim{} retracted\n",
                if completed == 1 { "" } else { "s" },
                self.repairs.len() - completed,
                if self.repairs.len() - completed == 1 {
                    ""
                } else {
                    "s"
                }
            ));
            for repair in &self.repairs {
                out.push_str(&format!("    {repair}\n"));
            }
        }

        if !self.metadata_repairs.is_empty() {
            let types = self
                .metadata_repairs
                .iter()
                .filter(|repair| matches!(repair.kind, MetadataRepairKind::Type { .. }))
                .count();
            let priorities = self.metadata_repairs.len() - types;
            out.push_str(&format!(
                "  required metadata: {types} type default{}, {priorities} low-priority default{}\n",
                if types == 1 { "" } else { "s" },
                if priorities == 1 { "" } else { "s" }
            ));
        }

        if !self.unknown_events.is_empty() {
            out.push_str(&format!(
                "  retained {} event{} of kinds this build does not understand, verbatim:\n",
                self.unknown_events.len(),
                if self.unknown_events.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ));
            for unknown in &self.unknown_events {
                out.push_str(&format!("    {} {}\n", unknown.story, unknown.kind));
            }
        }

        if self.dry_run {
            out.push_str(
                "\nnothing was written. Run without --dry-run to migrate, and leave the \
                 `.storyhook` directory in place until you are satisfied — it is the rollback.\n",
            );
        }
        out
    }
}

/// A legacy tree checked against the store's rules, ready to be written.
///
/// Building one is the whole of the decision-making: after
/// [`MigrationPlan::build`] returns `Ok`, the tree is known to be
/// representable, every repair has been decided, and [`MigrationPlan::apply`]
/// is a transcription.
///
/// # What travels, and what a rollback therefore does not carry
///
/// A [`ProjectExport`](crate::service::transfer::ProjectExport) holds states,
/// types, members and stories, and the project's settings since SH-133. A
/// *tree* also holds `project.toml`'s `created_at` and `next-id`, which ride
/// alongside the envelope rather than inside it and are what a rollback still
/// does not restore, along with `projects.uuid` and the registered origins.
/// It may also hold `github-sync.toml` and its merge bases (SH-189) — the
/// story↔GitHub-Issues sync engine's own configuration, retired since SH-408
/// and consequently reported, never carried; see [`MigrationPlan::report`]'s
/// own "nothing is lost quietly" note.
///
/// Settings used to be on that list, and the reason given was that widening the
/// document would move bytes in a format the golden corpus compared literally.
/// That reason was never true: `golden-export.json` is *parsed* into a
/// `ProjectExport` and compared field by field, so a field absent when unset
/// moves nothing. What was true is that the gap was benign while `story migrate`
/// was the only writer of those columns — a value in the store had come *from* a
/// tree, so the tree it would roll back to already held it. SH-129 shipped
/// `story project settings` and the gap started losing decisions somebody made.
#[derive(Debug)]
pub struct MigrationPlan {
    project: LegacyProject,
    stories: Vec<PlannedStory>,
    repairs: Vec<Repair>,
    metadata_repairs: Vec<MetadataRepair>,
    highest: StoryNo,
}

/// One story as it will be written: its number, and its whole event log with
/// any repair already spliced in.
#[derive(Debug)]
struct PlannedStory {
    id: String,
    story_no: StoryNo,
    events: Vec<RawEvent>,
    snapshot: StorySnapshot,
    archived: bool,
}

impl MigrationPlan {
    /// Checks a legacy tree against the store's rules.
    ///
    /// Every violation is collected before any is reported: an operator fixing
    /// a tree wants the list, not the first item on it.
    pub fn build(project: LegacyProject) -> Result<Self, AppError> {
        let prefix = project.effective_prefix().to_string();
        let mut refusals = Refusals::default();

        if let Err(error) = validate_state_defs_for_write(&project.states) {
            refusals.push(format!(
                "`{}` holds a state set storyhook would refuse to write: {error}",
                project.root.join(".storyhook/states.toml").display()
            ));
        }

        // SH-183: `story migrate` already refuses a document for a bad STATE
        // slug (above), so leaving TYPE slugs unchecked at this one call site
        // made the same command disagree with itself about the same shape of
        // problem. Named per offending slug rather than short-circuiting on
        // the first, matching this function's own rule that every violation
        // is collected before any is reported.
        for story_type in &project.types {
            if let Err(error) = validate_type_slug(&story_type.slug) {
                refusals.push(format!(
                    "`{}` holds a type slug storyhook would refuse to write: {error}",
                    project.root.join(".storyhook/types.toml").display()
                ));
            }
        }

        let numbers = story_numbers(&project, &prefix, &mut refusals);
        let snapshots = fold_stories(&project, &mut refusals);
        // The relation checks read folded snapshots, so they are only
        // meaningful once every story folded. Reporting "SH-3 has two parents"
        // about a tree whose SH-3 does not parse would be noise on top of the
        // real problem.
        let repairs = if refusals.is_empty() {
            plan_repairs(&project, &snapshots, &mut refusals)
        } else {
            Vec::new()
        };
        refusals.into_result(&project.root)?;

        let mut stories = Vec::new();
        let default_type = project
            .types
            .first()
            .cloned()
            .or_else(|| default_types().into_iter().next())
            .ok_or_else(|| {
                AppError::Validation("project has no configured story types".to_string())
            })?
            .slug;
        let mut metadata_repairs = Vec::new();
        // `max`, not `count`: the counter is ahead of the highest id whenever a
        // number was burned (a rejected `--state`, a story deleted before the
        // archive existed), and a `next` derived from a count would re-mint one.
        let mut highest = StoryNo::new(
            i64::try_from(project.next_id.saturating_sub(1))
                .unwrap_or(i64::MAX)
                .max(1),
        );
        for story in &project.stories {
            let story_no = numbers[&story.id];
            if story_no.get() > highest.get() {
                highest = story_no;
            }
            let snapshot = snapshots[&story.id].clone();
            if snapshot.story_type.is_none() {
                metadata_repairs.push(MetadataRepair {
                    story: story.id.clone(),
                    kind: MetadataRepairKind::Type {
                        story_type: default_type.clone(),
                    },
                });
            }
            if snapshot.priority == Priority::None || !snapshot.priority_assessed {
                metadata_repairs.push(MetadataRepair {
                    story: story.id.clone(),
                    kind: MetadataRepairKind::PriorityLow,
                });
            }
            stories.push(PlannedStory {
                events: raw_events(story, &repairs),
                id: story.id.clone(),
                story_no,
                archived: story.archived,
                snapshot,
            });
        }

        Ok(Self {
            project,
            stories,
            repairs,
            metadata_repairs,
            highest,
        })
    }

    /// What this plan would produce, without writing anything.
    #[must_use]
    pub fn report(&self, dry_run: bool) -> MigrationReport {
        let mut settings = Vec::new();
        if let Some(value) = self.project.sync_auto_transition {
            settings.push(format!("sync.auto_transition = {value}"));
        }
        if let Some(value) = &self.project.doctor_stale_threshold {
            settings.push(format!("doctor.stale_threshold = \"{value}\""));
        }
        // github-sync's own configuration is neither read nor carried — the
        // engine that would have interpreted it is retired (SH-408) — but a
        // tree that still holds it is named anyway, by path, rather than
        // silently left behind: this file's own rule that nothing is lost
        // quietly applies to what a migration declines to carry as much as
        // to what it carries.
        if self.project.github_sync_file_present || self.project.github_bases_count > 0 {
            let bases = self.project.github_bases_count;
            let plural = if bases == 1 { "" } else { "s" };
            settings.push(format!(
                "{} and {bases} merge base{plural} found under {} — NOT migrated: storyhook no \
                 longer syncs stories to GitHub issues (SH-408). Left exactly where they are.",
                self.project
                    .root
                    .join(".storyhook/github-sync.toml")
                    .display(),
                self.project
                    .root
                    .join(".storyhook/github-sync/bases")
                    .display(),
            ));
        }

        MigrationReport {
            source: self.project.root.clone(),
            prefix: self.project.effective_prefix().to_string(),
            dry_run,
            stories: self.stories.len(),
            events: self.stories.iter().map(|s| s.events.len()).sum::<usize>()
                + self.metadata_repairs.len(),
            archived: self.stories.iter().filter(|s| s.archived).count(),
            deleted: self.stories.iter().filter(|s| s.snapshot.deleted).count(),
            states: self.project.states.len(),
            types: self.project.types.len(),
            members: self.project.members.len(),
            next_story_no: u64::try_from(self.highest.get()).unwrap_or(0) + 1,
            repairs: self.repairs.clone(),
            metadata_repairs: self.metadata_repairs.clone(),
            unknown_events: self
                .project
                .unknown_events()
                .map(|(story, event)| RetainedUnknown {
                    story: story.id.clone(),
                    kind: event.kind.clone(),
                })
                .collect(),
            settings,
        }
    }

    /// Writes the plan into `store` as a project rooted at `dest`.
    ///
    /// One transaction: a migration that half-lands is a project nobody can
    /// trust and nobody can re-run. The pointer file is written *after* the
    /// commit, so a failed migration leaves a checkout with no claim on a
    /// project that does not exist.
    pub fn apply<S: Store>(
        &self,
        store: &S,
        dest: &Path,
        at: &str,
    ) -> Result<MigrationReport, AppError> {
        let prefix = self.project.effective_prefix().to_string();
        let root = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());
        let uuid = uuid::Uuid::new_v4().to_string();

        store.write(|tx| {
            refuse_second_migration(&*tx, &root)?;

            let name = root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "project".to_string());
            let project = tx.create_project(&NewProject {
                uuid: uuid.clone(),
                slug: unique_slug(&*tx, &name)?,
                name,
                prefix: prefix.clone(),
                // The legacy project's own birthday, not this instant. A
                // migration moves a project; it does not create one.
                created_at: self.project.created_at.clone(),
            })?;
            // The tree being migrated *is* this project's checkout, and saying
            // so is what lets `refuse_second_migration` recognize it again
            // after the pointer file has been deleted.
            super::project::adopt_checkout(tx, project, &root)?;

            // Repairing, not refusing: a legacy tree predates the required
            // floor, and refusing to migrate one because of a state it never
            // had would make old data unmovable rather than conforming.
            write_states_repairing(tx, project, &self.project.states)?;
            if !self.project.types.is_empty() {
                tx.put_types(project, &self.project.types)?;
            } else {
                tx.put_types(project, &default_types())?;
            }
            let default_type = default_story_type(&*tx, project)?;
            for member in &self.project.members {
                tx.put_member(project, member)?;
            }
            // A fresh row, not a read-modify-write: this transaction created
            // the project a few lines above, so there is nothing of anyone
            // else's to preserve. `transfer::import_project` needs the other
            // shape because it can be restoring into a project that already
            // exists.
            //
            // github-sync's own configuration has no column to land in any
            // more (SH-408) — see `report`'s own naming of the file it was
            // read from, which is where an operator learns it was found and
            // left behind rather than silently dropped.
            tx.put_settings(
                project,
                &ProjectSettings {
                    sync_auto_transition: self.project.sync_auto_transition,
                    doctor_stale_threshold: self.project.doctor_stale_threshold.clone(),
                },
            )?;

            // Two passes, because the read model's foreign keys refuse an edge
            // to a story that has no row yet. Pass one writes every history;
            // pass two folds them. Both inside this transaction, so a tree that
            // fails halfway leaves nothing behind.
            let mut heads = Vec::with_capacity(self.stories.len());
            for story in &self.stories {
                let raw_head = tx.append_raw_events(
                    project,
                    story.story_no,
                    ExpectedSeq::Exact(EventSeq::ZERO),
                    &story.events,
                    // A legacy tree's history, written before kind #18 existed:
                    // its `[git]` comments are `commit-sync`'s own link records
                    // and must project, or the first sync after a migration
                    // re-links the repository's whole log.
                    LinkSource::Replayed,
                    // Unrecorded, not `migrate` (SH-246). These events describe
                    // things that happened in a `.storyhook` tree months ago;
                    // `migrate` copied them, it did not perform them. Stamping
                    // this command on them would make every migrated story read
                    // as though one `story migrate` run had moved, commented on
                    // and closed it — a trail that is confidently wrong, which
                    // is worse than one that admits it was told nothing.
                    &Provenance::unrecorded(),
                )?;
                let defaults = restore_default_events(&story.snapshot, &default_type, at);
                let head = if defaults.is_empty() {
                    raw_head
                } else {
                    tx.append_events(
                        project,
                        story.story_no,
                        ExpectedSeq::Exact(raw_head),
                        &defaults,
                        &Provenance::unrecorded(),
                    )?
                };
                heads.push(head);
            }
            for (story, head) in self.stories.iter().zip(heads) {
                let stored = tx.events_for(project, story.story_no)?;
                let (known, _unknown) = partition_known(story.story_no, &stored);
                let snapshot = fold_story(&story.id, &known, &state_map(&self.project.states))?;
                tx.put_story(project, &snapshot, head)?;
            }
            tx.reserve_story_no(project, self.highest)?;
            Ok(())
        })?;

        write_pointer(&root, &ProjectPointer::new(uuid, prefix))?;
        Ok(self.report(false))
    }
}

/// Refuses a checkout that has already been migrated.
///
/// Two questions, because there are two ways to have been migrated and either
/// alone can be true: the checkout may carry a pointer file naming a project
/// the store holds, or a project may already record this directory as its
/// checkout. Both are asked inside the write transaction so that two
/// `story migrate` calls racing on one directory cannot both pass.
///
/// # Why the second question is not the resolution index coming back
///
/// It used to be `project_by_path`, and SH-119 deleted that with the index it
/// read. `checkout_path` is a different thing wearing a similar shape: nothing
/// *resolves* by it, so the epic's invariant — the filesystem is never required
/// to answer which project this is — is untouched. This asks a narrower
/// question, of one directory, in one command that runs once per repository:
/// have I already moved this tree?
///
/// Without it, deleting `.storyhook.toml` and re-running `story migrate` mints
/// a silent second copy of every story in the tree. That is the defect class
/// this project treats as catastrophic, and a scan of the catalog is a cheap
/// price for closing it in a command nobody runs twice on purpose.
fn refuse_second_migration(tx: &impl ReadOps, root: &Path) -> Result<(), StoreError> {
    if let Some(pointer) = read_pointer(root).map_err(StoreError::from)?
        && let Some(existing) = tx.project_by_uuid(&pointer.uuid)?
    {
        return Err(AppError::Validation(format!(
            "`{}` already points at project `{}` — this checkout has been migrated. Migrating \
             again would mint a second copy of it; there is nothing to merge into.",
            pointer_path(root).display(),
            existing.slug
        ))
        .into());
    }
    if let Some(existing) = project_checked_out_at(tx, root)? {
        let stories = tx.stories(existing.id, &StoryQuery::all())?.len();
        return Err(AppError::Validation(format!(
            "the store already holds project `{}` at `{}` ({stories} stories). Migrating again \
             would mint a second copy of it; there is nothing to merge into.",
            existing.slug,
            root.display()
        ))
        .into());
    }
    Ok(())
}

/// The project that records `root` as its checkout, if one does.
///
/// A scan rather than a lookup, deliberately: `checkout_path` carries no index
/// and must not gain one — migration 0007's header explains why two projects
/// are allowed to share a directory — and this is asked once, by one command,
/// against a catalog of tens of rows.
fn project_checked_out_at(
    tx: &impl ReadOps,
    root: &Path,
) -> Result<Option<crate::store::ProjectRecord>, StoreError> {
    for project in tx.projects()? {
        if tx.checkout_path(project.id)?.as_deref() == Some(root) {
            return Ok(Some(project));
        }
    }
    Ok(None)
}

/// Refuses to run from a linked git worktree.
///
/// A worktree's `.storyhook` is a *copy* of the main checkout's, diverged from
/// it since the branch was made — that divergence is SH-46, the defect the
/// store exists to end. Migrating from one mints a project holding that
/// checkout's version of the truth, and the main checkout then migrates a
/// second one with the same prefix and overlapping story numbers. One command,
/// two trackers, and no way to tell which is which afterwards.
///
/// Detected by asking git rather than by looking for a `.git` *file*: a
/// worktree's `--git-common-dir` points into the main checkout's repository,
/// and `--git-dir` points at its own private directory inside it. Outside a
/// repository entirely there is nothing to refuse — a `.storyhook` tree in a
/// plain directory is migrated on its own terms.
pub fn refuse_in_linked_worktree(root: &Path) -> Result<(), AppError> {
    let ask = |arg: &str| -> Option<PathBuf> {
        let output = crate::env::git_env::command(root)
            .args(["rev-parse", arg])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if raw.is_empty() {
            return None;
        }
        let path = root.join(raw);
        Some(path.canonicalize().unwrap_or(path))
    };

    let (Some(git_dir), Some(common)) = (ask("--git-dir"), ask("--git-common-dir")) else {
        return Ok(());
    };
    if git_dir == common {
        return Ok(());
    }
    let main = common
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| common.clone());
    Err(AppError::Validation(format!(
        "`{}` is a linked git worktree; migrate from the main checkout at `{}` instead.\n\nA \
         worktree's `.storyhook` directory is a copy that has been diverging since the branch \
         was made. Migrating it would create a second project with the same prefix and \
         overlapping story numbers — which is the corruption the store exists to end (SH-46).",
        root.display(),
        main.display()
    )))
}

/// Every story's number, with a refusal for each id the project cannot hold.
fn story_numbers(
    project: &LegacyProject,
    prefix: &str,
    refusals: &mut Refusals,
) -> BTreeMap<String, StoryNo> {
    let mut numbers = BTreeMap::new();
    // Keyed by number rather than by id, so `SH-07` and `SH-7` are caught as
    // the collision they are: the store holds one row per *number*.
    let mut seen: BTreeMap<i64, &LegacyStory> = BTreeMap::new();
    for story in &project.stories {
        match StoryNo::parse_id(prefix, &story.id) {
            Ok(story_no) => {
                if let Some(first) = seen.insert(story_no.get(), story) {
                    // Two records for one story: an open log and an archive
                    // row, which is the SH-20 split-brain shape a half-finished
                    // archive leaves. The store has one row per story and
                    // cannot hold both, and choosing between them is a guess
                    // about which write landed last.
                    refusals.push(format!(
                        "story `{}` exists twice — in {} and in {}. One of the two is a leftover \
                         from a half-completed archive; delete the wrong one and run this again.",
                        story.id,
                        describe_source(first),
                        describe_source(story)
                    ));
                }
                numbers.insert(story.id.clone(), story_no);
            }
            Err(_) => refusals.push(format!(
                "story `{}` does not belong to a project with prefix `{prefix}`. Every story in \
                 one project shares its prefix; a stray file here has to be moved out by hand.",
                story.id
            )),
        }
    }
    numbers
}

/// Where one story's record lives, for a message about a duplicate.
fn describe_source(story: &LegacyStory) -> String {
    if story.archived {
        "the archive database".to_string()
    } else {
        format!("`{}`", story.source.display())
    }
}

/// Folds every story, collecting a refusal per story that will not fold.
fn fold_stories(
    project: &LegacyProject,
    refusals: &mut Refusals,
) -> BTreeMap<String, StorySnapshot> {
    let states = state_map(&project.states);
    let mut folded = BTreeMap::new();
    for story in &project.stories {
        let known: Vec<StoryEvent> = story
            .events
            .iter()
            .filter_map(|event| event.decoded.clone())
            .collect();
        match fold_story(&story.id, &known, &states) {
            Ok(snapshot) => {
                folded.insert(story.id.clone(), snapshot);
            }
            Err(error) => refusals.push(undefined_state_hint(project, story, &error)),
        }
    }
    folded
}

/// The refusal for a story that will not fold.
///
/// An undefined state gets its own sentence because it is the one failure with
/// an obvious, lossless fix — and because it is *replicated*, not repaired.
/// `fold_story` has always rejected it, so the legacy tool could not open this
/// project either; inventing the missing state would mean guessing whether the
/// story is open or closed, and that guess is the difference between a done
/// story and a live one.
fn undefined_state_hint(project: &LegacyProject, story: &LegacyStory, error: &AppError) -> String {
    let message = error.to_string();
    if !message.contains("undefined state") {
        return format!("story `{}` cannot be folded: {message}", story.id);
    }
    format!(
        "{message}. Add it back to `{}` with the OPEN/CLOSED mapping it had, then run this \
         again — storyhook will not guess whether a story in it is finished.",
        project.root.join(".storyhook/states.toml").display()
    )
}

/// Decides every repair, and refuses every violation no rule can settle.
///
/// Two passes, in this order and for a reason. Parent conflicts are settled
/// *first*, because the second pass would otherwise fabricate the very event
/// that creates one: completing a one-sided `parent-of` is what hands a story
/// its second parent. Anything the first pass annuls is then excluded from the
/// second.
fn plan_repairs(
    project: &LegacyProject,
    snapshots: &BTreeMap<String, StorySnapshot>,
    refusals: &mut Refusals,
) -> Vec<Repair> {
    // Claims as each story's own history states them.
    let claims: BTreeMap<&str, BTreeSet<(&str, &str)>> = snapshots
        .iter()
        .map(|(id, snapshot)| {
            (
                id.as_str(),
                snapshot
                    .relationships
                    .iter()
                    .map(|edge| (edge.relation.as_str(), edge.other_id.as_str()))
                    .collect(),
            )
        })
        .collect();

    let mut repairs = settle_parent_conflicts(project, snapshots, &claims, refusals);
    // The annulled edges, by value: a retraction means the second pass must
    // *not* fabricate the mirror of the claim it just withdrew.
    let annulled: BTreeSet<(String, String)> = repairs
        .iter()
        .map(|repair| (repair.story.clone(), repair.other.clone()))
        .collect();

    for (id, snapshot) in snapshots {
        for edge in &snapshot.relationships {
            let other = edge.other_id.as_str();
            if annulled.contains(&(id.clone(), other.to_string())) {
                continue;
            }
            if other == id {
                refusals.push(format!(
                    "story `{id}` relates to itself (`{}`); the store cannot hold an edge with \
                     one end. Remove it with `story unrelate {id} {} {id}`.",
                    edge.relation, edge.relation
                ));
                continue;
            }
            let Some(inverse) = inverse_relation(&edge.relation) else {
                refusals.push(format!(
                    "story `{id}` claims relation `{}`, which storyhook has no inverse for and \
                     the store's relation vocabulary does not contain.",
                    edge.relation
                ));
                continue;
            };
            let Some(far) = claims.get(other) else {
                refusals.push(format!(
                    "story `{id}` claims `{} {other}`, and `{other}` does not exist in this tree. \
                     The store refuses an edge with a missing end; remove it with `story \
                     unrelate {id} {} {other}`.",
                    edge.relation, edge.relation
                ));
                continue;
            };
            if !far.contains(&(inverse, id.as_str())) {
                repairs.push(Repair {
                    story: other.to_string(),
                    relation: inverse.to_string(),
                    other: id.clone(),
                    at: claim_instant(project, snapshots, id, &edge.relation, other),
                    kind: RepairKind::CompletedInverse,
                });
            }
        }
    }

    repairs
        .sort_by(|a, b| (&a.story, &a.relation, &a.other).cmp(&(&b.story, &b.relation, &b.other)));
    repairs
}

/// Which stories claim to be a story's parent, and from which side.
#[derive(Default)]
struct ParentClaim {
    /// The child's own history says `child-of`.
    by_child: bool,
    /// The parent's own history says `parent-of`.
    by_parent: bool,
}

impl ParentClaim {
    fn mutual(&self) -> bool {
        self.by_child && self.by_parent
    }
}

/// Settles every story that more than one story claims as a child.
///
/// # The rule: agreement beats assertion
///
/// The store allows one parent per story, and a legacy tree can hold several
/// because nothing ever refused the write. Resolving that needs a rule, and
/// there is exactly one that is *evidence-based* rather than arbitrary: a
/// parentage **both** histories record outranks one that only a single history
/// asserts. The unilateral claim is annulled with a `StoryRelationshipRemoved`
/// event stamped at the instant of the claim it retracts, so the fold reads
/// "asserted and never mutual" rather than "true until the migration".
///
/// Nothing is deleted. The original `StoryRelationshipAdded` is imported
/// verbatim like every other event, the retraction sits beside it in the log,
/// and the report names every one. What changes is the *read model*: an epic
/// loses children it claimed alone.
///
/// # What is refused instead
///
/// When the rule cannot decide — two parents that both agree, or several that
/// all disagree — the migration stops and names them. There is no evidence to
/// weigh, and picking by id order or by timestamp would be storyhook inventing
/// an answer to a question only the operator can settle.
///
/// The alternative to all of this was refusing every multi-parent tree
/// outright. It was rejected once the remedy was written down: every story
/// involved in this repository's own five conflicts is *archived*, and
/// `story unrelate` resolves open stories only — so the advice would have been
/// "reopen five closed stories, unrelate, re-close", which rewrites more
/// history than the retraction does.
fn settle_parent_conflicts(
    project: &LegacyProject,
    snapshots: &BTreeMap<String, StorySnapshot>,
    claims: &BTreeMap<&str, BTreeSet<(&str, &str)>>,
    refusals: &mut Refusals,
) -> Vec<Repair> {
    let mut parents: BTreeMap<&str, BTreeMap<&str, ParentClaim>> = BTreeMap::new();
    for (id, snapshot) in snapshots {
        for edge in &snapshot.relationships {
            match edge.relation.as_str() {
                "child-of" => {
                    parents
                        .entry(id)
                        .or_default()
                        .entry(&edge.other_id)
                        .or_default()
                        .by_child = true;
                }
                "parent-of" => {
                    parents
                        .entry(&edge.other_id)
                        .or_default()
                        .entry(id)
                        .or_default()
                        .by_parent = true;
                }
                _ => {}
            }
        }
    }

    let mut repairs = Vec::new();
    for (child, found) in &parents {
        if found.len() < 2 {
            continue;
        }
        // A parent named by a story that is not in this tree is a dangling
        // edge, which the second pass refuses on its own terms. Counting it
        // here as well would report one broken edge as two problems.
        let present: Vec<(&&str, &ParentClaim)> = found
            .iter()
            .filter(|(parent, _)| claims.contains_key(**parent))
            .collect();
        if present.len() < 2 {
            continue;
        }

        let mutual: Vec<&str> = present
            .iter()
            .filter(|(_, claim)| claim.mutual())
            .map(|(parent, _)| **parent)
            .collect();
        if mutual.len() != 1 {
            refusals.push(describe_undecidable_parents(child, &present, &mutual));
            continue;
        }

        let winner = mutual[0];
        for (parent, claim) in &present {
            let parent = **parent;
            if parent == winner {
                continue;
            }
            // Exactly one side asserts this parentage; that side is the one
            // whose history gains the retraction.
            let (story, relation, other) = if claim.by_child {
                (child.to_string(), "child-of", parent)
            } else {
                (parent.to_string(), "parent-of", *child)
            };
            repairs.push(Repair {
                at: claim_instant(project, snapshots, &story, relation, other),
                story,
                relation: relation.to_string(),
                other: other.to_string(),
                kind: RepairKind::RetractedUnilateralParent {
                    child: (*child).to_string(),
                    winner: winner.to_string(),
                },
            });
        }
    }
    repairs
}

/// The refusal for a parent conflict the agreement rule cannot settle.
fn describe_undecidable_parents(
    child: &str,
    present: &[(&&str, &ParentClaim)],
    mutual: &[&str],
) -> String {
    let described: Vec<String> = present
        .iter()
        .map(|(parent, claim)| {
            let side = if claim.mutual() {
                "both histories agree"
            } else if claim.by_child {
                "claimed only by the child"
            } else {
                "claimed only by the parent"
            };
            format!("`{parent}` ({side})")
        })
        .collect();
    let why = if mutual.len() > 1 {
        format!(
            "{} of them are agreed by both ends, so there is no weaker claim to annul",
            mutual.len()
        )
    } else {
        "none of them is agreed by both ends, so there is no stronger claim to keep".to_string()
    };
    format!(
        "story `{child}` has {} parents — {} — and the store allows one. {why}. Decide which \
         parentage is real, drop the others, and run this again; storyhook will not choose \
         between claims it has no evidence to rank.",
        present.len(),
        described.join(", ")
    )
}

/// When the surviving half of a one-sided relation was written.
///
/// Read out of the claiming story's *events*, because a folded
/// [`StoryRelation`](crate::domain::StoryRelation) carries no timestamp — the
/// relation set is a fold, and the instant only exists in the event that
/// produced it. The last matching `StoryRelationshipAdded` wins: an edge added,
/// removed and added again appeared, for the purposes of the missing half, the
/// last time.
///
/// The repair carries that original instant rather than the migration's, so the
/// two halves of one edge agree about when it appeared and the repaired story's
/// `updated_at` does not jump to today. Falls back to the claiming story's
/// `updated_at` when no event can be found, which only a hand-edited tree
/// produces.
fn claim_instant(
    project: &LegacyProject,
    snapshots: &BTreeMap<String, StorySnapshot>,
    claimer: &str,
    relation: &str,
    other: &str,
) -> String {
    project
        .stories
        .iter()
        .find(|story| story.id == claimer)
        .and_then(|story| {
            story
                .events
                .iter()
                .rev()
                .filter_map(|event| event.decoded.as_ref())
                .find_map(|event| match event {
                    StoryEvent::StoryRelationshipAdded {
                        at,
                        other_id,
                        relation: found,
                    } if found == relation && other_id == other => Some(at.clone()),
                    _ => None,
                })
        })
        .unwrap_or_else(|| snapshots[claimer].updated_at.clone())
}

/// One story's events as the store will store them, with its repairs spliced in
/// at the instant they belong to.
///
/// Every original event survives, in order, byte for byte. A repair is placed
/// in *timestamp* order rather than appended, and the reason is `updated_at`:
/// `fold_story` takes it from the last event it sees, so an edge stamped 2025
/// appended to a story closed in 2025 would be fine, while the same edge
/// appended after later activity would rewrite when the story last moved. A
/// retraction goes immediately after the claim it annuls — it carries that
/// claim's own timestamp, and the fold then reads "asserted, never mutual"
/// rather than "true until someone ran a migration".
fn raw_events(story: &LegacyStory, repairs: &[Repair]) -> Vec<RawEvent> {
    let mut events: Vec<RawEvent> = story
        .events
        .iter()
        .map(|event| RawEvent {
            kind: event.kind.clone(),
            at: event.at.clone(),
            payload: event.payload.clone(),
        })
        .collect();

    for repair in repairs.iter().filter(|repair| repair.story == story.id) {
        let event = repair.event();
        let raw = RawEvent {
            kind: crate::domain::event_kind(&event).to_string(),
            at: repair.at.clone(),
            payload: serde_json::to_string(&event).expect("a StoryEvent always serializes"),
        };
        // Never before the creation event: a story related before it exists
        // folds to a history no sequence of commands could have written.
        let at = events
            .iter()
            .skip(1)
            .position(|existing| existing.at.as_str() > raw.at.as_str())
            .map_or(events.len(), |index| index + 1);
        events.insert(at, raw);
    }
    events
}

/// A slug-keyed view of an ordered state list.
fn state_map(states: &[StateDef]) -> BTreeMap<String, StateDef> {
    states
        .iter()
        .map(|state| (state.slug.clone(), state.clone()))
        .collect()
}

/// Everything wrong with a tree, reported at once.
///
/// An operator repairing a tracker by hand needs the list. Failing on the first
/// violation turns one migration into one round trip per violation, and this
/// repository's own tree has fifteen.
#[derive(Default)]
struct Refusals(Vec<String>);

impl Refusals {
    fn push(&mut self, refusal: String) {
        self.0.push(refusal);
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn into_result(mut self, root: &Path) -> Result<(), AppError> {
        if self.0.is_empty() {
            return Ok(());
        }
        self.0.sort();
        self.0.dedup();
        let mut message = format!(
            "`{}` holds {} thing{} the store cannot represent, and nothing was imported:\n",
            root.display(),
            self.0.len(),
            if self.0.len() == 1 { "" } else { "s" }
        );
        for refusal in &self.0 {
            message.push_str(&format!("  - {refusal}\n"));
        }
        message.push_str(
            "\nEach one is a decision storyhook will not make on your behalf. Fix them in the \
             legacy project, then run `story migrate` again — nothing has changed on either side.",
        );
        Err(AppError::Integrity(message.into()))
    }
}
