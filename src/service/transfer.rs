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

use crate::domain::remote::{OwnedOrigin, RemoteUrl};
use crate::domain::{
    ImportStory, Member, Priority, StateDef, StoryEvent, SuperState, TypeDef, fold_story,
    normalize_labels, relation_edges,
};
use crate::error::AppError;
use crate::output::StoryView;
use serde_json::value::RawValue;

use crate::store::{
    EventSeq, ExpectedSeq, LinkSource, NewProject, ProjectId, RawEvent, ReadOps, Store, StoreError,
    StoredEvent, StoredPayload, StoryNo, StoryQuery, WriteOps, partition_known,
};

use super::project::{
    DEFAULT_PREFIX, ProjectPointer, Registration, read_pointer, register_origin, unique_slug,
    write_pointer,
};
use super::state_set::write_states_repairing;
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
    /// The settings a user wrote, absent when they wrote none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<ExportedSettings>,
    /// The project's registered git origins, empty when it has none.
    ///
    /// Unlike `github.sync` (see [`ExportedSettings`]), this is a full carry:
    /// there is no partial-registration hazard a missing counterpart table can
    /// create, and the alternative — recovering an origin from a checkout's own
    /// `git config --get remote.origin.url` — is not universally available. A
    /// project whose only remote is a bare repository with no working checkout
    /// anywhere, or whose checkout has since been deleted, has no other record
    /// of it; a rollback dropping this silently is the loss, not a symmetry
    /// with `project_paths`, which a checkout can always re-derive by walking
    /// its directories.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remotes: Vec<ExportedRemote>,
    /// Every story, open ones first.
    pub stories: Vec<ExportedStory>,
}

/// A git origin an export document carries — the wire form of
/// [`ProjectRemoteRecord`](crate::store::ProjectRemoteRecord).
///
/// Not that store type directly, for the same reason [`ExportedSettings`] is
/// not `store::ProjectSettings`: `store/types.rs` owns no wire format. Here the
/// two shapes happen to carry the same three fields — there is no column this
/// document must deliberately withhold, the way `ProjectSettings::github_sync`
/// is withheld from [`ExportedSettings`] — but the type stays distinct so a
/// future store-only column does not silently become part of this document's
/// contract by inheriting `Serialize`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportedRemote {
    /// The identity key project selection matches on — see
    /// [`RemoteUrl::key`](crate::domain::remote::RemoteUrl::key).
    pub normalized: String,
    /// The URL exactly as it was registered.
    pub raw: String,
    /// RFC3339 timestamp of the registration.
    pub registered_at: String,
}

impl From<crate::store::ProjectRemoteRecord> for ExportedRemote {
    fn from(record: crate::store::ProjectRemoteRecord) -> Self {
        Self {
            normalized: record.normalized,
            raw: record.raw,
            registered_at: record.registered_at,
        }
    }
}

/// The project settings an export document carries.
///
/// # Why this is not `store::ProjectSettings`
///
/// Two reasons, and the second is the load-bearing one. Deriving `Serialize` on
/// the store row would freeze its *column names* as this document's public
/// contract and enrol every future column into the wire format by default —
/// `store/types.rs` owns no wire format, which is the rule SH-67 settled one
/// story earlier. And the store row has a third field, `github_sync`, that this
/// document deliberately does not carry: a separate type makes that
/// **unrepresentable** rather than a `skip_serializing_if` somebody could
/// remove without noticing what it was holding back.
///
/// The shape is the legacy `project.toml`'s, because the document's job is to be
/// the contract between the two storage layouts and the layout it must survive
/// into spells them this way.
///
/// # Why `github.sync` does not travel
///
/// Not because the document could not hold it — it is a serde struct and could.
/// Because a **partial** carry is worse than none. `load_config` decides whether
/// a project is configured by whether this blob is present, and never looks at
/// the per-story `github_bases` merge-base table, which the legacy leg has
/// nowhere to write: `src/legacy/` and `src/storage.rs` hold no github knowledge
/// at all. Restore the blob without the bases and the next sync's
/// `load_base(..).unwrap_or_else(|| story.clone())` treats *local as base*, so
/// every field the user edited since the last sync reads as unchanged and the
/// stale remote value is filed as an ordinary pull — silently, at exit 0. The
/// blob also carries `github.owner`/`github.repo`, which only the setup wizard
/// re-derives, so a document restored into a fork would push to the original
/// repository.
///
/// What a backup therefore does not carry is named by `story doctor` rather than
/// left to be discovered at the next sync.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportedSettings {
    /// The `[sync]` table, absent when nothing in it is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<ExportedSyncSettings>,
    /// The `[doctor]` table, absent when nothing in it is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doctor: Option<ExportedDoctorSettings>,
}

/// The `[sync]` table of an [`ExportedSettings`].
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportedSyncSettings {
    /// Whether `commit-sync` moves stories automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_transition: Option<bool>,
}

/// The `[doctor]` table of an [`ExportedSettings`].
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportedDoctorSettings {
    /// How old a story may be before `story doctor` calls it stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_threshold: Option<String>,
}

impl ExportedSettings {
    /// The carried settings of a project that has some, and `None` for one that
    /// has none.
    ///
    /// **"Nothing is set" has exactly one encoding, and that is the point.** An
    /// emitted empty table and an absent one are the same fact, and if the two
    /// exporters disagreed about which to write, `a_round_trip_survives_a_second
    /// _lap` would byte-compare `{}` against nothing and fail. Every producer
    /// goes through here so neither can drift.
    #[must_use]
    pub fn new(auto_transition: Option<bool>, stale_threshold: Option<String>) -> Option<Self> {
        let sync = auto_transition.map(|auto_transition| ExportedSyncSettings {
            auto_transition: Some(auto_transition),
        });
        let doctor = stale_threshold.map(|stale_threshold| ExportedDoctorSettings {
            stale_threshold: Some(stale_threshold),
        });
        (sync.is_some() || doctor.is_some()).then_some(Self { sync, doctor })
    }

    /// `sync.auto_transition`, however deeply the document nests it.
    #[must_use]
    pub fn auto_transition(&self) -> Option<bool> {
        self.sync.as_ref().and_then(|sync| sync.auto_transition)
    }

    /// `doctor.stale_threshold`, however deeply the document nests it.
    #[must_use]
    pub fn stale_threshold(&self) -> Option<&str> {
        self.doctor
            .as_ref()
            .and_then(|doctor| doctor.stale_threshold.as_deref())
    }
}

/// One story inside a [`ProjectExport`]: its whole event history, not a
/// snapshot.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExportedStory {
    /// The story id, prefix included.
    pub id: String,
    /// Every event, oldest first — including the ones this binary cannot
    /// decode.
    pub events: Vec<ExportedEvent>,
    /// Whether the story lives in the legacy archive rather than as an open
    /// log.
    pub archived: bool,
}

/// One event inside an [`ExportedStory`]: decoded when this binary understands
/// it, verbatim when it does not.
///
/// # Why this is not `Vec<StoryEvent>` any more
///
/// It was, and an event kind this build could not decode was dropped on the way
/// out — silently, so an export taken by an older binary and restored later lost
/// whatever a newer one had written (SH-67). The store has never done that:
/// events are rows with an opaque payload and [`StoredPayload::Unknown`] retains
/// them (SH-54). Export was the one place the care stopped.
///
/// # The wire form is one bare event object either way
///
/// A `Known` event serializes exactly as a [`StoryEvent`] always did, so a
/// project holding no unknown kinds produces the same bytes it produced before
/// SH-67 — which is what `golden-export.json` compares literally. An `Unknown`
/// event serializes as its own stored payload text, byte for byte, key order
/// included: re-serializing a parsed value would normalize the key order, and
/// `src/legacy/events.rs` already settled that key order is part of *verbatim*.
///
/// Reading is the mirror and is deliberately **lax**, matching
/// [`crate::store::StoredPayload`]'s own rule rather than the stricter one
/// `crate::legacy::parse_event` applies to a legacy tree: whatever decodes as a
/// `StoryEvent` is `Known`, and anything else is kept as bytes. `story export`
/// must never fail on account of what an event contains — it is the documented
/// backup and rollback step 2, it has no `--force`, and a refusal would turn one
/// undecodable row into a project that cannot be backed up at all. The signal
/// that an event was not understood belongs to `story doctor`, which reports it
/// from the store itself.
///
/// The one thing a document may not hold is an event object with no string
/// `kind` or `at`: those two are what the store denormalizes into columns, and
/// an event storyhook cannot even index by kind and time is corrupt rather than
/// merely unrecognised. That refusal carries the document's line and column,
/// which is what a hand-edited file needs.
#[derive(Clone, Debug)]
pub enum ExportedEvent {
    /// A kind this binary understands, decoded.
    Known(StoryEvent),
    /// A kind this binary does not understand, kept exactly as written.
    Unknown(RawEvent),
}

impl ExportedEvent {
    /// One stored event as it travels in a document.
    ///
    /// The store has already classified it; this does not re-litigate the
    /// question, because a second classifier is how the two come to disagree.
    #[must_use]
    pub fn from_stored(event: &StoredEvent) -> Self {
        match &event.payload {
            StoredPayload::Known(decoded) => Self::Known(decoded.clone()),
            StoredPayload::Unknown { kind, json } => Self::Unknown(RawEvent {
                kind: kind.clone(),
                at: event.at.clone(),
                payload: json.clone(),
            }),
        }
    }

    /// The decoded event, or `None` when this binary does not understand it.
    ///
    /// The fold consumes only these: [`crate::domain::fold_story`] is defined
    /// over `StoryEvent`, and an unknown event contributes nothing to a
    /// snapshot by construction.
    #[must_use]
    pub fn known(&self) -> Option<&StoryEvent> {
        match self {
            Self::Known(event) => Some(event),
            Self::Unknown(_) => None,
        }
    }

    /// The event as the store's raw triple, ready for `append_raw_events`.
    fn to_raw(&self) -> Result<RawEvent, StoreError> {
        match self {
            Self::Known(event) => RawEvent::from_event(event),
            Self::Unknown(raw) => Ok(raw.clone()),
        }
    }
}

impl serde::Serialize for ExportedEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Known(event) => event.serialize(serializer),
            // `RawValue` rather than a re-serialized `Value`, so the payload's
            // own bytes reach the document unchanged.
            Self::Unknown(raw) => RawValue::from_string(raw.payload.clone())
                .map_err(|error| {
                    serde::ser::Error::custom(format!(
                        "the stored payload of a `{}` event is not JSON and cannot be written \
                         into an export document ({error}). The store holds a torn payload; \
                         `story doctor` reports it.",
                        raw.kind
                    ))
                })?
                .serialize(serializer),
        }
    }
}

impl<'de> serde::Deserialize<'de> for ExportedEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        // Two passes over one element, not two elements: `RawValue` keeps the
        // original text so that a payload this binary cannot decode survives
        // with its bytes intact. `#[serde(untagged)]` cannot do this — it
        // buffers through serde's private `Content`, which cannot produce a
        // `RawValue` — which is why these impls are written out.
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        let text = raw.get();
        if let Ok(event) = serde_json::from_str::<StoryEvent>(text) {
            return Ok(Self::Known(event));
        }

        let fields: serde_json::Map<String, serde_json::Value> = serde_json::from_str(text)
            .map_err(|error| D::Error::custom(format!("an event must be an object: {error}")))?;
        let field = |name: &str| -> Result<String, D::Error> {
            fields
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    D::Error::custom(format!(
                        "an event this build cannot decode must still carry a string `{name}`, \
                         which is how the store indexes one it does not understand"
                    ))
                })
        };
        Ok(Self::Unknown(RawEvent {
            kind: field("kind")?,
            at: field("at")?,
            payload: text.to_string(),
        }))
    }
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
                // Every event, decoded or not. `partition_known` stood here and
                // its second half was discarded, which is what SH-67 was.
                let events = stored.iter().map(ExportedEvent::from_stored).collect();
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

            let stored = tx.settings(project)?;
            Ok(ProjectExport {
                schema: EXPORT_SCHEMA,
                prefix: exported_prefix(&prefix),
                states: tx.states(project)?,
                types: tx.types(project)?,
                members: tx.members(project)?,
                // `github_sync` is deliberately not among them — see
                // [`ExportedSettings`], which says why a partial carry is worse
                // than none.
                settings: ExportedSettings::new(
                    stored.sync_auto_transition,
                    stored.doctor_stale_threshold,
                ),
                remotes: tx
                    .project_remotes(project)?
                    .into_iter()
                    .map(ExportedRemote::from)
                    .collect(),
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

/// What [`import_project`] did, beyond the story count the caller already
/// knew to expect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportOutcome {
    /// How many stories the document carried.
    pub stories: usize,
    /// Every remote the document named that this restore did **not**
    /// register, because another project already holds it.
    pub skipped_remotes: Vec<SkippedRemote>,
}

/// One origin [`import_project`] left unregistered, and who already holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedRemote {
    /// The URL exactly as the document carried it.
    pub url: String,
    /// The slug of the project that already holds it.
    pub holder: String,
}

/// Materializes the project an export document describes, at `root`.
///
/// Not a [`TransferService`] method, and not [`Ctx`]-shaped, for the same
/// reason `story project new` is not: the target project may not exist yet. `story
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
///
/// # What identifies the directory afterwards
///
/// The committed [pointer file](crate::service::project::ProjectPointer), written
/// by this function, exactly as `story migrate` writes one for the tree it moves.
/// An export document carries no uuid — it is a document about stories, not about
/// which project row they came from — so the pointer names the one minted here.
///
/// Without it an imported checkout would be identified by nothing at all once
/// SH-119 deleted the recorded-path index: `story list` in the directory the
/// restore landed in would refuse, and a second `import-project` there would mint
/// a *second* project rather than meeting the refusal above.
///
/// # A remote already held elsewhere does not fail the restore
///
/// A story's whole history is the payload this transaction exists to protect
/// atomically; a registered origin is one of six auxiliary categories riding
/// alongside it, and the store's unique index on `project_remotes.normalized`
/// makes "another project already holds this URL" an ordinary outcome rather
/// than a corruption signal (SH-115's migration header). Failing a
/// several-hundred-story restore over one stale or reclaimed origin would make
/// the recovery path itself unreliable in a case it must anticipate — so a
/// collision is skipped, not fatal, and named in [`ImportOutcome`] instead of
/// silently dropped: for a project whose only remote is a bare repository with
/// no working checkout anywhere, this may be the only surviving record of it
/// (SH-138).
pub fn import_project<S: Store>(
    store: &S,
    root: &std::path::Path,
    clock: &Clock,
    export: &ProjectExport,
) -> Result<ImportOutcome, AppError> {
    let now = clock.now();
    let prefix = export
        .prefix
        .clone()
        .unwrap_or_else(|| DEFAULT_PREFIX.to_string());
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    // Read before the transaction opens, and written after it commits: a
    // filesystem touch inside `BEGIN IMMEDIATE` holds the store's only write
    // lock for as long as the disk takes.
    let existing_pointer = read_pointer(&root)?;

    let (uuid, skipped_remotes) = store.write(|tx| {
        let existing = match &existing_pointer {
            Some(pointer) => tx.project_by_uuid(&pointer.uuid)?,
            None => None,
        };
        let (project, uuid) = match existing {
            Some(existing) => {
                if !tx.stories(existing.id, &StoryQuery::all())?.is_empty() {
                    return Err(AppError::Validation(format!(
                        "`{}` already holds stories; import-project restores into an empty \
                         project",
                        existing.slug
                    ))
                    .into());
                }
                super::project::adopt_checkout(tx, existing.id, &root)?;
                (existing.id, existing.uuid)
            }
            None => {
                let name = root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "project".to_string());
                let uuid = uuid::Uuid::new_v4().to_string();
                let project = tx.create_project(&NewProject {
                    uuid: uuid.clone(),
                    slug: unique_slug(&*tx, &name)?,
                    name,
                    prefix: prefix.clone(),
                    created_at: now.clone(),
                })?;
                super::project::adopt_checkout(tx, project, &root)?;
                (project, uuid)
            }
        };

        // Repairing, and this is also where an unvalidated write used to be:
        // an export document's catalog reached the store untouched, so a
        // hand-edited document could install any state set at all.
        write_states_repairing(tx, project, &export.states)?;
        if !export.types.is_empty() {
            tx.put_types(project, &export.types)?;
        }
        for member in &export.members {
            tx.put_member(project, member)?;
        }
        apply_settings(tx, project, export.settings.as_ref())?;

        let mut skipped_remotes = Vec::new();
        for remote in &export.remotes {
            let url = RemoteUrl::normalize(&remote.raw).map_err(|error| {
                AppError::Validation(format!("remote `{}` does not parse: {error}", remote.raw))
            })?;
            match register_origin(
                tx,
                project,
                &OwnedOrigin::explicit(url),
                &remote.registered_at,
            )? {
                Registration::Recorded => {}
                Registration::HeldBy(holder) => skipped_remotes.push(SkippedRemote {
                    url: remote.raw.clone(),
                    holder,
                }),
            }
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
            // One raw append per story, not one per decodable run: a restore
            // has to write events this build cannot decode, and splitting the
            // history at each of them would be the same call made three times.
            // `LinkSource::Live` is what `append_events` passed here before
            // SH-67 and is therefore this path's behaviour unchanged — whether
            // it *should* be `Replayed` is SH-70, and is a decision this commit
            // deliberately does not make.
            let raw = story
                .events
                .iter()
                .map(ExportedEvent::to_raw)
                .collect::<Result<Vec<_>, _>>()?;
            let head = tx.append_raw_events(
                project,
                story_no,
                ExpectedSeq::Exact(EventSeq::ZERO),
                &raw,
                LinkSource::Live,
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
        Ok((uuid, skipped_remotes))
    })?;

    // Never overwritten, for the reason `story project new` never overwrites
    // one: the file is the user's the moment it carries a `[plugin]` or
    // `[hooks]` table, and the identity it already names is the one just
    // imported into.
    if existing_pointer.is_none() {
        write_pointer(&root, &ProjectPointer::new(uuid, prefix.clone()))?;
    }

    Ok(ImportOutcome {
        stories: export.stories.len(),
        skipped_remotes,
    })
}

/// The `prefix` field an export document carries.
///
/// `None` for the default prefix, which looks like an omission and is in fact
/// exactly what the legacy exporter emitted: `project.toml` stored the prefix as
/// an *option* that `story project new` left unset unless `--prefix` was given, and
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
/// Applies a document's settings to the project it is being restored into,
/// leaving every column the document does not carry alone.
///
/// **Read, modify, write — not a fresh row.** [`WriteOps::put_settings`] writes
/// every column, and `import-project` can be restoring *into a project that
/// already exists*: that branch adopts a checkout whose row may already hold a
/// configured `github_sync`, and handing `put_settings` a value built only from
/// the document would blank it. That is the SH-49 shape — a read-modify-write
/// round trip through a value that does not know about a field destroying the
/// field — and it is the reason `store::ProjectSettings` is columns rather than a
/// blob in the first place.
///
/// A document with no settings writes nothing at all, rather than writing
/// emptiness: restoring a backup taken before this existed must not clear the
/// settings of the project it lands in.
fn apply_settings(
    tx: &mut impl WriteOps,
    project: ProjectId,
    settings: Option<&ExportedSettings>,
) -> Result<(), StoreError> {
    let Some(settings) = settings else {
        return Ok(());
    };
    let mut row = tx.settings(project)?;
    if let Some(auto_transition) = settings.auto_transition() {
        row.sync_auto_transition = Some(auto_transition);
    }
    if let Some(threshold) = settings.stale_threshold() {
        row.doctor_stale_threshold = Some(threshold.to_string());
    }
    tx.put_settings(project, &row)
}

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
        let normalized = normalize_labels(labels);
        if !normalized.is_empty() {
            events.push(StoryEvent::StoryLabelsSet {
                at: now.to_string(),
                labels: normalized,
            });
        }
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
