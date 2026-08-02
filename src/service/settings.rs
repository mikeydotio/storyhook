//! A project's settings — the `project_settings` row, reachable (SH-129).
//!
//! Not to be confused with two neighbours that share the word "config":
//!
//! - [`ConfigService`](super::config::ConfigService) is a project's *catalog* —
//!   its states, its story types, its members. Different table, different
//!   lifecycle, different verbs.
//! - `.storyhook.toml`'s `[plugin]` and `[hooks]` tables are *repository*
//!   configuration: decisions about one checkout, versioned with the branch and
//!   carried by a clone. They stay in the file.
//!
//! What is here is the handful of per-project values the store holds and the
//! product reads.
//!
//! # The registry is the design
//!
//! [`REGISTRY`] is one static array, and it is the single source of truth for
//! *both* halves of this module: the renderer reads it to say whether a key is
//! writable and who owns it, and the dispatcher reads it to decide whether a
//! write is accepted at all. There is deliberately no match on key names beside
//! it — a `settable` flag sitting next to a hand-written match arm is drift
//! waiting to happen, and the drift is silent in the direction that matters
//! (the flag says "read-only" while the arm accepts the write).
//!
//! Adding a setting is therefore a registry row plus one arm in each of the two
//! exhaustive matches on [`SettingField`], which the compiler demands.
//! `tests/service_settings.rs` iterates the registry and asserts that every row
//! and its dispatch agree.
//!
//! # The invariant that is easy to get wrong
//!
//! [`WriteOps::put_settings`] rewrites every column unconditionally, by design
//! — the alternative is the read-modify-write through a serialized document
//! that cost SH-49 a whole field. So a write here must read the existing row,
//! change one field, and put it back **inside one write transaction**. Building
//! a fresh `ProjectSettings` compiles, reads naturally, and silently destroys a
//! configured github-sync document; that pattern is safe in
//! [`migrate`](super::migrate) only because it runs against a row that does not
//! yet exist.

use crate::domain::parse_duration;
use crate::error::AppError;
use crate::output::{SettingKind, SettingSource, SettingView};
use crate::store::{ProjectSettings, ReadOps, Store, WriteOps};

use super::Ctx;

/// Which column of [`ProjectSettings`] a key names.
///
/// Separate from [`SettingKind`], which says how a value is *spelled*: two keys
/// could one day share a kind, but no two ever share a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingField {
    SyncAutoTransition,
    DoctorStaleThreshold,
    GithubSync,
}

/// One row of the settings registry: everything both the renderer and the
/// dispatcher know about a key.
#[derive(Clone, Copy, Debug)]
pub struct SettingSpec {
    key: &'static str,
    field: SettingField,
    kind: SettingKind,
    description: &'static str,
    default: Option<&'static str>,
    managed_by: Option<&'static str>,
    note: Option<&'static str>,
}

impl SettingSpec {
    /// The dotted name a user types.
    #[must_use]
    pub const fn key(&self) -> &'static str {
        self.key
    }

    /// How this key's value is spelled.
    #[must_use]
    pub const fn kind(&self) -> SettingKind {
        self.kind
    }

    /// The command that owns this key's value, for a key no user may write.
    #[must_use]
    pub const fn managed_by(&self) -> Option<&'static str> {
        self.managed_by
    }

    /// Whether `story project settings set` accepts this key.
    ///
    /// Derived from [`managed_by`](Self::managed_by) rather than stored beside
    /// it: a flag and an owner that disagree is exactly the drift this registry
    /// exists to prevent, and two fields can disagree while one cannot.
    #[must_use]
    pub const fn settable(&self) -> bool {
        self.managed_by.is_none()
    }
}

/// Every setting `story project settings` knows, in the order it lists them.
///
/// Dotted names rather than column names, matching what `story migrate`'s
/// report already prints. Column names would freeze the schema as public
/// contract; these can outlive a rename.
static REGISTRY: [SettingSpec; 3] = [
    SettingSpec {
        key: "sync.auto_transition",
        field: SettingField::SyncAutoTransition,
        kind: SettingKind::Boolean,
        description: "Whether `story commit-sync` moves a story a commit mentions \
                      into the active state.",
        // Not a convention: `GitService::commit_sync` reads the column with
        // `.unwrap_or(true)`, so an unwritten value genuinely is `true`.
        default: Some("true"),
        managed_by: None,
        note: None,
    },
    SettingSpec {
        key: "doctor.stale_threshold",
        field: SettingField::DoctorStaleThreshold,
        kind: SettingKind::Duration,
        description: "How long a story may sit untouched before it counts as stale.",
        default: None,
        managed_by: None,
        // Writable anyway, and deliberately: `story migrate` already imports
        // this value, so refusing the write here would leave a migrated project
        // holding a threshold its owner could never change while a project born
        // after the flip could never acquire one — SH-129's own defect,
        // reproduced for one column out of three. The inertness is answered by
        // saying so, and this note is the only thing standing between a user
        // and the reasonable belief that writing it changed something.
        //
        // Deleting this line is the completion criterion for whichever story
        // makes `story doctor` read the value.
        note: Some("no command reads this yet"),
    },
    SettingSpec {
        key: "github.sync",
        field: SettingField::GithubSync,
        kind: SettingKind::Document,
        description: "The github-sync document: etags and story-to-issue mappings.",
        default: None,
        // Listed and gettable, never writable. It holds etags and mappings that
        // must agree with `github_bases`, and under `--no-default-features` the
        // type describing its shape does not exist — so `set` could not
        // validate what it accepted.
        managed_by: Some("story github-sync"),
        note: None,
    },
];

/// Every setting `story project settings` knows, in listing order.
#[must_use]
pub fn registry() -> &'static [SettingSpec] {
    &REGISTRY
}

/// What a [`SettingKind::Document`] reports instead of its contents.
///
/// Opaque presence, never a parsed shape: the surface must be a property of the
/// data model rather than of which cargo features the binary was built with.
const DOCUMENT_PRESENT: &str = "configured";

/// Reading and writing one project's settings.
pub struct SettingsService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> SettingsService<'ctx, S> {
    /// A settings service bound to `ctx`.
    #[must_use]
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// Every setting, with its effective value and where that value came from.
    pub fn list(&self) -> Result<Vec<SettingView>, AppError> {
        let row = self.row()?;
        Ok(REGISTRY.iter().map(|spec| view(spec, &row)).collect())
    }

    /// One setting.
    ///
    /// An unknown key is a [`AppError::Usage`], not a `NotFound`: the key set is
    /// compile-time grammar rather than a store entity, and `NotFound` would
    /// conflate "no such key" with "that key is not set" — the distinction the
    /// three-valued source exists to preserve.
    pub fn get(&self, key: &str) -> Result<SettingView, AppError> {
        let spec = lookup(key)?;
        Ok(view(spec, &self.row()?))
    }

    /// Writes one setting, and answers with the setting as it now reads.
    pub fn set(&self, key: &str, value: &str) -> Result<SettingView, AppError> {
        let spec = writable(key)?;
        let parsed = parse(spec, value)?;
        self.mutate(spec, Some(parsed))
    }

    /// Clears one setting, returning it to its default or to nothing.
    ///
    /// Clearing a key that was never written is a no-op rather than an error:
    /// the store ends in the state the caller asked for either way.
    pub fn unset(&self, key: &str) -> Result<SettingView, AppError> {
        let spec = writable(key)?;
        self.mutate(spec, None)
    }

    /// The project's settings row.
    fn row(&self) -> Result<ProjectSettings, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| tx.settings(project))?)
    }

    /// Read the whole row, change one field, write it back — in one
    /// transaction.
    ///
    /// The shape is the point. `put_settings` rewrites every column, so a write
    /// assembled from anything less than the current row drops whatever it did
    /// not know to carry; and reading in a separate transaction from the write
    /// would let a concurrent `story github-sync` land between the two.
    fn mutate(&self, spec: &SettingSpec, value: Option<String>) -> Result<SettingView, AppError> {
        let project = self.ctx.project();
        let row = self.ctx.store().write(|tx| {
            let mut settings = tx.settings(project)?;
            apply(spec.field, &mut settings, value.as_deref());
            tx.put_settings(project, &settings)?;
            Ok(settings)
        })?;
        Ok(view(spec, &row))
    }
}

/// The registry row for `key`, or a refusal naming every key there is.
fn lookup(key: &str) -> Result<&'static SettingSpec, AppError> {
    REGISTRY.iter().find(|spec| spec.key == key).ok_or_else(|| {
        let known: Vec<&str> = REGISTRY.iter().map(|spec| spec.key).collect();
        AppError::Usage(format!(
            "unknown setting `{key}`. Known settings: {}.\n\nRun `story project settings list` \
             to see them with their current values.",
            known.join(", ")
        ))
    })
}

/// The registry row for `key`, refusing a key this command does not own.
fn writable(key: &str) -> Result<&'static SettingSpec, AppError> {
    let spec = lookup(key)?;
    match spec.managed_by {
        // A usage error rather than a validation one: the key set is grammar,
        // and no value the user could have typed would have been accepted.
        Some(owner) => Err(AppError::Usage(format!(
            "`{key}` is read-only here: its value is maintained by `{owner}`.\n\nNothing has been \
             written. Run `story project settings list` for the settings you can change."
        ))),
        None => Ok(spec),
    }
}

/// Validates a written value and returns it in the form the column stores.
fn parse(spec: &SettingSpec, raw: &str) -> Result<String, AppError> {
    let value = raw.trim();
    match spec.kind {
        SettingKind::Boolean => match value.to_ascii_lowercase().as_str() {
            "true" => Ok("true".to_string()),
            "false" => Ok("false".to_string()),
            _ => Err(AppError::Validation(format!(
                "`{}` takes `true` or `false`, not `{raw}`.",
                spec.key
            ))),
        },
        SettingKind::Duration => {
            // `parse_duration` reads the count as an `i64`, so it answers
            // `Some` for `0d` and for `-14d`. Neither is a threshold, and a
            // stored one would be a value no command could act on sensibly.
            let parsed = parse_duration(value).filter(|d| *d > chrono::Duration::zero());
            if parsed.is_none() {
                return Err(AppError::Validation(format!(
                    "`{}` takes a positive duration such as `14d`, `2w`, `36h` or `90m`, \
                     not `{raw}`.",
                    spec.key
                )));
            }
            Ok(value.to_string())
        }
        // Unreachable through `set`, which consults `writable` first. Written
        // as a refusal rather than an `unreachable!` because the registry is
        // data: a later row that is both a document and settable must fail
        // loudly here rather than store an unvalidated blob.
        SettingKind::Document => Err(AppError::Validation(format!(
            "`{}` holds a structured document that cannot be written from the command line.",
            spec.key
        ))),
    }
}

/// Writes `value` into the column `field` names, leaving every other column
/// alone.
///
/// A match on the field rather than on the key, so that a new setting is a
/// compiler error here until it is handled rather than a silent no-op.
fn apply(field: SettingField, settings: &mut ProjectSettings, value: Option<&str>) {
    match field {
        SettingField::SyncAutoTransition => {
            // Total because `parse` canonicalizes a boolean to exactly `true`
            // or `false` before it gets here — the one coupling between the two
            // functions, and pinned by the case-folding test in
            // `tests/service_settings.rs`.
            settings.sync_auto_transition = value.map(|value| value == "true");
        }
        SettingField::DoctorStaleThreshold => {
            settings.doctor_stale_threshold = value.map(str::to_string);
        }
        SettingField::GithubSync => {
            // Unreachable today: `writable` refuses this key for both `set` and
            // `unset`. Written as a clear rather than as a panic because it is
            // the correct behaviour if the registry row ever becomes settable —
            // clearing a document *is* unsetting it, and `set` would still be
            // caught by `parse`, which has no string form for a document.
            settings.github_sync = None;
        }
    }
}

/// What the column currently holds, in the string form the surface reports.
fn stored(field: SettingField, settings: &ProjectSettings) -> Option<String> {
    match field {
        SettingField::SyncAutoTransition => settings.sync_auto_transition.map(|on| on.to_string()),
        SettingField::DoctorStaleThreshold => settings.doctor_stale_threshold.clone(),
        // Presence, never the document: see `DOCUMENT_PRESENT`.
        SettingField::GithubSync => settings
            .github_sync
            .as_ref()
            .map(|_| DOCUMENT_PRESENT.to_string()),
    }
}

/// Projects one stored value through its registry row.
fn view(spec: &SettingSpec, settings: &ProjectSettings) -> SettingView {
    let (value, source) = match stored(spec.field, settings) {
        Some(value) => (Some(value), SettingSource::Set),
        None => match spec.default {
            Some(default) => (Some(default.to_string()), SettingSource::Default),
            None => (None, SettingSource::Unset),
        },
    };

    SettingView {
        key: spec.key.to_string(),
        kind: spec.kind,
        value,
        source,
        default: spec.default.map(str::to_string),
        settable: spec.settable(),
        managed_by: spec.managed_by.map(str::to_string),
        description: spec.description.to_string(),
        note: spec.note.map(str::to_string),
    }
}
