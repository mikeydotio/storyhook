//! Moving a project's data in and out: `export`, `import`, `import-project`
//! and the batch half of `decompose`.
//!
//! # Two importers, and they are not the same thing
//!
//! `story import` takes a list of *descriptions* — titles, priorities, labels —
//! and creates new stories from them, allocating ids as it goes. `story
//! import-project` takes an [export document](ProjectExport) and materializes
//! the project it describes, ids and event histories included. The first is a
//! bulk `story new`; the second is a restore.
//!
//! # Why the batch importer does not call [`StoryService::create`]
//!
//! [`StoryService::create`](super::StoryService::create) is `story new`, and
//! `story new` is stricter than `story import` has ever been: an unparseable
//! priority is a rejection there and is silently dropped here. Reproducing that
//! leniency is the point — the port's governing rule is byte-compatibility, and
//! a script that has been feeding storyhook `"priority": "urgent"` for a year
//! must keep getting the same stories out. The event *batch* the two produce is
//! identical field for field and in the same order; only the validation
//! differs, and [`import_events`] says so where a reader will see it.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::{
    ImportStory, Member, Priority, StateDef, StoryEvent, SuperState, TypeDef, fold_story,
    relation_edges,
};
use crate::error::AppError;
use crate::output::StoryView;
use crate::store::{
    EventSeq, ExpectedSeq, NewProject, ProjectId, ReadOps, Store, StoreError, StoryNo, StoryQuery,
    WriteOps, partition_known,
};

use super::project::{DEFAULT_PREFIX, path_kind, unique_slug};
use super::{Clock, Ctx, append_and_fold, project_prefix};

/// A whole project, as `story export` writes it and `story import-project`
/// reads it.
///
/// The **rollback envelope**, and that is why it is a type of its own rather
/// than a serialization of whatever the store happens to hold. `docs/rearch/
/// flip-checklist.md` names exactly what it does and does not carry, and the
/// two-way door out of the rearchitecture is `store -> export -> this document
/// -> a legacy tree`. `tests/migrate_round_trip.rs` runs that loop and
/// compares the read models story by story.
///
/// It lived in `src/storage.rs` until the legacy path was deleted. It never
/// belonged there: the document is the *contract between* the two storage
/// layouts, so the layer that is going away is the wrong owner for it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProjectExport {
    /// The legacy project file's schema version — see [`EXPORT_SCHEMA`].
    pub schema: u32,
    /// The story-id prefix, absent when the project uses the default.
    pub prefix: Option<String>,
    /// The configured states, in order.
    pub states: Vec<StateDef>,
    /// The configured story types, in order.
    #[serde(default)]
    pub types: Vec<TypeDef>,
    /// The project's members.
    pub members: Vec<Member>,
    /// Every story, open ones first.
    pub stories: Vec<ExportedStory>,
}

/// One story inside a [`ProjectExport`]: its whole event history, not a
/// snapshot.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExportedStory {
    /// The story id, prefix included.
    pub id: String,
    /// Every event, oldest first.
    ///
    /// Decoded events only: an event kind this binary does not understand is
    /// dropped on export, which is the one lossy edge of the round trip and is
    /// recorded as such in the flip checklist.
    pub events: Vec<StoryEvent>,
    /// Whether the story lives in the legacy archive rather than as an open
    /// log.
    pub archived: bool,
}

/// The `schema` an export document declares.
///
/// One, and a constant rather than a stored column: it is the version of the
/// *legacy project file* the document was born from, and a store-backed project
/// has no such file. Freezing it here keeps `story export` byte-identical
/// across the flip, which is what the golden corpus pins.
const EXPORT_SCHEMA: u32 = 1;

/// What one `story import` or `story decompose` call produced.
#[derive(Debug)]
pub struct ImportBatch {
    /// The stories created, in the order they were described.
    pub views: Vec<StoryView>,
    /// Human-readable relationship lines (`SH-10 child-of SH-9`), which
    /// `decompose` renders as its summary and `import` discards.
    pub relationship_lines: Vec<String>,
}

/// Import and export over one project in one store.
pub struct TransferService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> TransferService<'ctx, S> {
    /// A transfer service bound to `ctx`.
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// The project's whole contents as an export document.
    ///
    /// Ordering reproduces the legacy path exactly: open stories first, then
    /// archived ones, each group sorted by story id *as text*. The legacy
    /// exporter got that order from a directory listing and from `ORDER BY id`
    /// in the archive database respectively; both are lexicographic, so
    /// `SH-10` precedes `SH-2`. It is reproduced rather than corrected because
    /// this document is compared byte for byte against the pre-rearchitecture
    /// one.
    pub fn export(&self) -> Result<ProjectExport, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            let prefix = project_prefix(tx, project)?;
            let mut open = Vec::new();
            let mut archived = Vec::new();
            for row in tx.stories(project, &StoryQuery::all())? {
                let story_no = StoryNo::parse_id(&prefix, &row.snapshot.id)
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                let stored = tx.events_for(project, story_no)?;
                let (events, _unknown) = partition_known(story_no, &stored);
                let exported = ExportedStory {
                    id: row.snapshot.id.clone(),
                    events,
                    archived: row.archived,
                };
                if row.archived {
                    archived.push(exported);
                } else {
                    open.push(exported);
                }
            }
            open.sort_by(|a, b| a.id.cmp(&b.id));
            archived.sort_by(|a, b| a.id.cmp(&b.id));
            open.append(&mut archived);

            Ok(ProjectExport {
                schema: EXPORT_SCHEMA,
                prefix: exported_prefix(&prefix),
                states: tx.states(project)?,
                types: tx.types(project)?,
                members: tx.members(project)?,
                stories: open,
            })
        })?)
    }

    /// Creates one story per description, then resolves the batch's
    /// relationships.
    ///
    /// Two passes, because a description may relate to another description by
    /// index and therefore to a story that does not exist yet. Every story
    /// type named anywhere in the batch is validated *before* the first story
    /// is created, so a typo in the last description does not leave the first
    /// nine behind.
    pub fn import(&self, stories: &[ImportStory]) -> Result<ImportBatch, AppError> {
        if stories.is_empty() {
            return Ok(ImportBatch {
                views: Vec::new(),
                relationship_lines: Vec::new(),
            });
        }
        let now = self.ctx.now();
        let project = self.ctx.project();

        let (created_ids, relationship_lines) = self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let ordered = tx.states(project)?;
            let states = slug_map(&ordered);
            require_known_types(&*tx, project, stories)?;

            let mut created_ids: Vec<String> = Vec::new();
            for story in stories {
                let events = import_events(&*tx, project, &ordered, story, &now)?;
                let story_no = tx.allocate_story_no(project)?;
                let snapshot = append_and_fold(
                    tx,
                    project,
                    story_no,
                    &prefix,
                    &states,
                    ExpectedSeq::Exact(EventSeq::ZERO),
                    &events,
                )?;
                created_ids.push(snapshot.id);
            }

            let batch = Batch {
                project,
                prefix: &prefix,
                states: &states,
                now: &now,
            };
            let lines = link_batch(tx, &batch, stories, &created_ids)?;
            Ok((created_ids, lines))
        })?;

        let views = self.ctx.store().read(|tx| {
            let mut views = Vec::new();
            for id in &created_ids {
                let story_no = StoryNo::parse_id(&project_prefix(tx, project)?, id)
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                let row = tx
                    .story(project, story_no)?
                    .ok_or_else(|| StoreError::NotFound(format!("story `{id}` not found")))?;
                // Deliberately the bare view the legacy importer answered with:
                // no derived relationships, no warnings, no progress rollup.
                views.push(StoryView {
                    story: row.snapshot,
                    derived_relationships: Vec::new(),
                    warnings: Vec::new(),
                    flagged_reasons: Vec::new(),
                    stale_info: None,
                    progress: None,
                });
            }
            Ok(views)
        })?;

        Ok(ImportBatch {
            views,
            relationship_lines,
        })
    }
}

/// Materializes the project an export document describes, at `root`.
///
/// Not a [`TransferService`] method, and not [`Ctx`]-shaped, for the same
/// reason `story init` is not: the target project may not exist yet. `story
/// import-project` into an empty directory is how the round-trip test restores
/// a backup, so the arm has to be able to create what it is importing into.
///
/// # Divergence from the legacy path, deliberate
///
/// The legacy importer *overwrites*: it rewrites every story's event log and
/// `INSERT OR REPLACE`s every archived row, so importing into a live project
/// silently replaces whatever was there. An append-only store cannot express
/// that and should not want to — a restore that half-overwrites a project is
/// how a tracker loses history. Importing into a project that already has
/// stories is refused here, with a message that names the project.
pub fn import_project<S: Store>(
    store: &S,
    root: &std::path::Path,
    clock: &Clock,
    export: &ProjectExport,
) -> Result<usize, AppError> {
    let now = clock.now();
    let prefix = export
        .prefix
        .clone()
        .unwrap_or_else(|| DEFAULT_PREFIX.to_string());
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    store.write(|tx| {
        let project = match tx.project_by_path(&root)? {
            Some(existing) => {
                if !tx.stories(existing.id, &StoryQuery::all())?.is_empty() {
                    return Err(AppError::Validation(format!(
                        "`{}` already holds stories; import-project restores into an empty \
                         project",
                        existing.slug
                    ))
                    .into());
                }
                tx.touch_project_path(existing.id, &root, path_kind(&root))?;
                existing.id
            }
            None => {
                let name = root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "project".to_string());
                let project = tx.create_project(&NewProject {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    slug: unique_slug(&*tx, &name)?,
                    name,
                    prefix: prefix.clone(),
                    created_at: now.clone(),
                })?;
                tx.touch_project_path(project, &root, path_kind(&root))?;
                project
            }
        };

        tx.put_states(project, &export.states)?;
        if !export.types.is_empty() {
            tx.put_types(project, &export.types)?;
        }
        for member in &export.members {
            tx.put_member(project, member)?;
        }

        let states = slug_map(&tx.states(project)?);
        let mut highest = StoryNo::new(0);
        // Two passes over the stories, because a story's relations name other
        // stories and the read model's foreign keys refuse an edge to a story
        // that does not exist yet. Pass one writes every history; pass two
        // folds them. Both are inside this one transaction, so a document whose
        // relations do not close leaves nothing behind.
        let mut written: Vec<(StoryNo, EventSeq)> = Vec::new();
        for story in &export.stories {
            let story_no = StoryNo::parse_id(&prefix, &story.id).map_err(|_| {
                AppError::Validation(format!(
                    "story `{}` does not belong to a project with prefix `{prefix}`",
                    story.id
                ))
            })?;
            let head = tx.append_events(
                project,
                story_no,
                ExpectedSeq::Exact(EventSeq::ZERO),
                &story.events,
            )?;
            if story_no.get() > highest.get() {
                highest = story_no;
            }
            written.push((story_no, head));
        }
        for (story_no, head) in written {
            let stored = tx.events_for(project, story_no)?;
            let (known, _unknown) = partition_known(story_no, &stored);
            let snapshot = fold_story(&story_no.to_id(&prefix), &known, &states)?;
            tx.put_story(project, &snapshot, head)?;
        }
        tx.reserve_story_no(project, highest)?;
        Ok(())
    })?;

    Ok(export.stories.len())
}

/// The `prefix` field an export document carries.
///
/// `None` for the default prefix, which looks like an omission and is in fact
/// exactly what the legacy exporter emitted: `project.toml` stored the prefix as
/// an *option* that `story init` left unset unless `--prefix` was given, and
/// every reader defaults an absent one to [`DEFAULT_PREFIX`]. Emitting `SH`
/// here would move a byte in a document the golden corpus compares literally.
///
/// The one project this cannot reproduce is one initialized with an explicit
/// `--prefix SH`: the legacy document says `"SH"` and this says nothing. The
/// two import to the same project, because the reader defaults it back.
fn exported_prefix(prefix: &str) -> Option<String> {
    (prefix != DEFAULT_PREFIX).then(|| prefix.to_string())
}

/// A slug-keyed view of an ordered state list.
fn slug_map(states: &[StateDef]) -> BTreeMap<String, StateDef> {
    states
        .iter()
        .map(|state| (state.slug.clone(), state.clone()))
        .collect()
}

/// Rejects the whole batch if any description names a type the project does not
/// define.
///
/// Reported as one message listing every unknown type and every available one,
/// which is what the legacy importer said and what a caller fixing a generated
/// batch needs.
fn require_known_types(
    tx: &impl ReadOps,
    project: ProjectId,
    stories: &[ImportStory],
) -> Result<(), AppError> {
    let known: BTreeMap<String, TypeDef> = tx
        .types(project)?
        .into_iter()
        .map(|t| (t.slug.clone(), t))
        .collect();
    let invalid: BTreeSet<&str> = stories
        .iter()
        .filter_map(|story| story.story_type.as_deref())
        .filter(|slug| !known.contains_key(*slug))
        .collect();
    if invalid.is_empty() {
        return Ok(());
    }
    Err(AppError::Validation(format!(
        "unknown types: {}. Available types: {}",
        invalid.into_iter().collect::<Vec<_>>().join(", "),
        known.keys().cloned().collect::<Vec<_>>().join(", ")
    )))
}

/// The events one imported description writes.
///
/// The same batch, in the same order, as
/// [`creation_events`](super::story) builds for `story new` — with two
/// deliberate differences, both of them leniencies the legacy importer has
/// always had: an unparseable priority is **dropped** rather than rejected, and
/// the story type has already been validated for the whole batch.
fn import_events(
    tx: &impl ReadOps,
    project: ProjectId,
    states: &[StateDef],
    story: &ImportStory,
    now: &str,
) -> Result<Vec<StoryEvent>, AppError> {
    let state_slug = match &story.state {
        Some(slug) => {
            let open = states
                .iter()
                .any(|state| &state.slug == slug && state.super_state == SuperState::Open);
            if !open {
                return Err(AppError::Validation(format!(
                    "'{slug}' is not a valid OPEN state. Available OPEN states: {}",
                    states
                        .iter()
                        .filter(|state| state.super_state == SuperState::Open)
                        .map(|state| state.slug.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            slug.clone()
        }
        None => states
            .iter()
            .find(|state| state.super_state == SuperState::Open)
            .map(|state| state.slug.clone())
            .ok_or_else(|| {
                AppError::Validation("project has no OPEN-mapped default state".to_string())
            })?,
    };

    let mut events = vec![StoryEvent::StoryCreated {
        at: now.to_string(),
        title: story.title.clone(),
        state: state_slug,
    }];
    if let Some(priority) = story.priority.as_deref().and_then(Priority::parse) {
        events.push(StoryEvent::StoryPrioritySet {
            at: now.to_string(),
            priority,
        });
    }
    if let Some(labels) = &story.labels
        && !labels.is_empty()
    {
        let mut sorted = labels.clone();
        sorted.sort();
        sorted.dedup();
        events.push(StoryEvent::StoryLabelsSet {
            at: now.to_string(),
            labels: sorted,
        });
    }
    if let Some(lookup) = &story.assignee {
        events.push(StoryEvent::StoryAssigned {
            at: now.to_string(),
            member_id: find_member(tx, project, lookup)?.id,
        });
    }
    if let Some(description) = &story.description
        && !description.trim().is_empty()
    {
        events.push(StoryEvent::StoryDescriptionSet {
            at: now.to_string(),
            description: description.clone(),
        });
    }
    if let Some(story_type) = &story.story_type {
        events.push(StoryEvent::StoryTypeSet {
            at: now.to_string(),
            story_type: story_type.clone(),
        });
    }
    Ok(events)
}

/// The fixed part of one import batch: which project, its prefix, its states,
/// and the instant every event in the batch is stamped with.
///
/// A struct rather than four more parameters — the linking pass otherwise takes
/// eight, which is the point at which an argument list starts silently taking
/// them in the wrong order.
struct Batch<'a> {
    project: ProjectId,
    prefix: &'a str,
    states: &'a BTreeMap<String, StateDef>,
    now: &'a str,
}

/// The second pass: every relation a batch's descriptions asked for.
///
/// Both ends are appended to when both are open, which is what the legacy
/// importer did and what keeps the two histories agreeing. A relation naming a
/// story outside the batch that does not exist is refused by the read model's
/// foreign key rather than silently stored as half an edge — the shape that is
/// SH-60.
fn link_batch(
    tx: &mut impl WriteOps,
    batch: &Batch<'_>,
    stories: &[ImportStory],
    created_ids: &[String],
) -> Result<Vec<String>, AppError> {
    let mut lines = Vec::new();
    for (index, story) in stories.iter().enumerate() {
        let Some(relationships) = &story.relationships else {
            continue;
        };
        let a_id = &created_ids[index];
        for relationship in relationships {
            let b_id = match (relationship.ref_index, &relationship.other_id) {
                (Some(ref_index), _) => created_ids.get(ref_index).cloned().ok_or_else(|| {
                    AppError::Validation(format!(
                        "ref_index {ref_index} out of bounds for import batch"
                    ))
                })?,
                (None, Some(other)) => other.clone(),
                (None, None) => {
                    return Err(AppError::Validation(
                        "relationship must have ref_index or other_id".to_string(),
                    ));
                }
            };
            if a_id == &b_id {
                continue;
            }
            let edges = relation_edges(&relationship.relation).ok_or_else(|| {
                AppError::Validation(format!(
                    "unsupported relationship `{}`",
                    relationship.relation
                ))
            })?;
            for (a_relation, b_relation) in edges {
                append_relation(tx, batch, a_id, &b_id, a_relation)?;
                if is_open(&*tx, batch.project, batch.prefix, &b_id)? {
                    append_relation(tx, batch, &b_id, a_id, b_relation)?;
                }
            }
            lines.push(format!("{a_id} {} {b_id}", relationship.relation));
        }
    }
    Ok(lines)
}

/// Appends one end of one edge.
fn append_relation(
    tx: &mut impl WriteOps,
    batch: &Batch<'_>,
    story_id: &str,
    other_id: &str,
    relation: &str,
) -> Result<(), AppError> {
    let story_no = StoryNo::parse_id(batch.prefix, story_id)
        .map_err(|_| AppError::NotFound(format!("story `{story_id}` not found")))?;
    append_and_fold(
        tx,
        batch.project,
        story_no,
        batch.prefix,
        batch.states,
        ExpectedSeq::Any,
        &[StoryEvent::StoryRelationshipAdded {
            at: batch.now.to_string(),
            other_id: other_id.to_string(),
            relation: relation.to_string(),
        }],
    )?;
    Ok(())
}

/// Whether a story exists in this project and has not been archived.
fn is_open(
    tx: &impl ReadOps,
    project: ProjectId,
    prefix: &str,
    id: &str,
) -> Result<bool, AppError> {
    let Ok(story_no) = StoryNo::parse_id(prefix, id) else {
        return Ok(false);
    };
    Ok(tx
        .story(project, story_no)?
        .is_some_and(|row| !row.archived))
}

/// A member by id or by GitHub handle.
fn find_member(tx: &impl ReadOps, project: ProjectId, lookup: &str) -> Result<Member, AppError> {
    tx.members(project)?
        .into_iter()
        .find(|member| {
            member.id == lookup || member.github.as_deref() == Some(lookup.trim_start_matches('@'))
        })
        .ok_or_else(|| AppError::NotFound(format!("member `{lookup}` not found")))
}
