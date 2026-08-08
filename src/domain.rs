use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// Git remote URLs, reduced to the one key project identity is decided by.
///
/// Its own file rather than another thousand lines here: the grammar is
/// self-contained, has no dependency on anything else in this module, and is
/// the sort of thing a reader looks for by name.
pub mod remote;

/// The story-id prefix — one validator, one derivation, no second opinion.
///
/// Its own file for the same reason [`remote`] is: it is a self-contained
/// grammar with three callers that must never disagree, and a reader looks for
/// it by name.
pub mod prefix;

/// Credentials the request envelope carries, and the rule that stops them
/// printing themselves.
///
/// Here rather than under `github` because `src/github` is gated on the
/// `github-sync` feature while the envelope carries the field in every build —
/// the same reason [`ConflictSide`](crate::cli::ConflictSide) lives in `cli`.
pub mod secret;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportStory {
    pub title: String,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub relationships: Option<Vec<ImportRelationship>>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub story_type: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportRelationship {
    pub relation: String,
    #[serde(default)]
    pub ref_index: Option<usize>,
    #[serde(default)]
    pub other_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Critical,
    High,
    Medium,
    Low,
    #[default]
    None,
}

impl Priority {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "critical" => Some(Self::Critical),
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SuperState {
    Open,
    Closed,
}

impl SuperState {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_uppercase().as_str() {
            "OPEN" => Some(Self::Open),
            "CLOSED" => Some(Self::Closed),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "OPEN",
            Self::Closed => "CLOSED",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDef {
    pub slug: String,
    #[serde(rename = "super")]
    pub super_state: SuperState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Free text explaining what the state means, surfaced in the UIs.
    ///
    /// Round-tripping this is not optional: `save_states` rewrites the whole
    /// file, so a field the struct doesn't know about is silently destroyed
    /// on the next `story state add` (SH-49).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The only [`StateDef::role`] the tool understands — see
/// [`crate::storage::find_active_state`], which uses it to pick the state a
/// story moves into when work starts.
pub const STATE_ROLE_ACTIVE: &str = "active";

/// A three-way edit of an optional field: leave it as it is, empty it, or
/// give it a value.
///
/// `Option<Option<T>>` says the same thing and is misread at every call site.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FieldEdit<T> {
    #[default]
    Keep,
    Clear,
    Set(T),
}

impl<T> FieldEdit<T> {
    /// Resolves the edit against the field's current value.
    pub fn apply(self, current: Option<T>) -> Option<T> {
        match self {
            FieldEdit::Keep => current,
            FieldEdit::Clear => None,
            FieldEdit::Set(value) => Some(value),
        }
    }

    /// Whether this edit would leave the field untouched.
    pub fn is_keep(&self) -> bool {
        matches!(self, FieldEdit::Keep)
    }
}

/// The parts of a [`StateDef`] that can be edited in place.
///
/// `slug` is deliberately absent: a state's slug is recorded in every
/// `StoryStateChanged` event ever written, so renaming one would orphan
/// history rather than update it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateChanges {
    pub super_state: Option<SuperState>,
    pub role: FieldEdit<String>,
    pub description: FieldEdit<String>,
}

impl StateChanges {
    /// Whether this would change nothing at all — callers reject a no-op edit
    /// rather than rewriting states.toml for it.
    pub fn is_empty(&self) -> bool {
        self.super_state.is_none() && self.role.is_keep() && self.description.is_keep()
    }
}

/// The parts of a [`TypeDef`] that can be edited in place.
///
/// `slug` is deliberately absent, for the same reason as [`StateChanges`]:
/// every `StoryTypeSet` event ever written names the slug it set, so renaming
/// one would orphan history rather than update it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeChanges {
    pub description: FieldEdit<String>,
    pub emoji: FieldEdit<String>,
}

impl TypeChanges {
    /// Whether this would change nothing at all.
    pub fn is_empty(&self) -> bool {
        self.description.is_keep() && self.emoji.is_keep()
    }
}

/// How many stories reference a state, split by where they live.
///
/// Open stories can be migrated elsewhere; archived ones cannot, which is
/// why the two are counted separately rather than summed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateUsage {
    pub open: usize,
    pub archived: usize,
}

impl StateUsage {
    /// Total stories in this state.
    pub fn total(&self) -> usize {
        self.open + self.archived
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeDef {
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A short glyph (typically one emoji) the web dashboard renders next to
    /// stories of this type. `None` for a type nobody has given one to yet —
    /// the dashboard falls back to a generic tag glyph rather than treating
    /// that the same as "untyped".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProgressRollup {
    pub children_done: usize,
    pub children_total: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryComment {
    pub at: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct StoryRelation {
    pub relation: String,
    pub other_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorySnapshot {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub state: String,
    pub superstate: SuperState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default)]
    pub awaiting: Option<String>,
    #[serde(default)]
    pub comments: Vec<StoryComment>,
    #[serde(default)]
    pub relationships: Vec<StoryRelation>,
    #[serde(default)]
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub story_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    /// `true` when the story was removed via `story delete` rather than
    /// closed through the normal state machine. A deleted story is always
    /// folded to `superstate: CLOSED` regardless of its `state` slug.
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
    /// The reason passed to `story delete`, when [`deleted`](Self::deleted)
    /// is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_reason: Option<String>,
    /// When the story was hidden from the primary UI via `story hide`, if it
    /// currently is. `Some` only while [`superstate`](Self::superstate) is
    /// [`SuperState::Closed`] — [`fold_story`] clears it the moment a story
    /// reopens, so "hidden implies closed" holds without a schema CHECK.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_at: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum StoryEvent {
    StoryCreated {
        at: String,
        title: String,
        state: String,
    },
    StoryCommentAdded {
        at: String,
        text: String,
    },
    /// Retracts a comment that was added earlier — the inverse of
    /// [`StoryCommentAdded`](Self::StoryCommentAdded).
    ///
    /// Comments are the one part of a story that only ever accumulates, so
    /// they are the one part an append-only undo cannot express by setting a
    /// field back. The retraction names the comment it annuls by *both* its
    /// instant and its text: storyhook's timestamps have second precision, so
    /// two comments can share an `at`, and an audit log should say what was
    /// withdrawn rather than merely when.
    StoryCommentRetracted {
        at: String,
        comment_at: String,
        text: String,
    },
    StoryAssigned {
        at: String,
        member_id: String,
    },
    /// Clears a story's assignee — the inverse of
    /// [`StoryAssigned`](Self::StoryAssigned) when the story had nobody before.
    ///
    /// Sibling of [`StoryAwaitingCleared`](Self::StoryAwaitingCleared), and
    /// added for the same reason: a field with an event that sets it and none
    /// that clears it cannot be put back without rewriting history.
    StoryAssigneeCleared {
        at: String,
    },
    StoryAwaitingSet {
        at: String,
        awaiting: String,
    },
    StoryAwaitingCleared {
        at: String,
    },
    StoryStateChanged {
        at: String,
        state: String,
    },
    StoryRelationshipAdded {
        at: String,
        other_id: String,
        relation: String,
    },
    StoryRelationshipRemoved {
        at: String,
        other_id: String,
        relation: String,
    },
    StoryPrioritySet {
        at: String,
        priority: Priority,
    },
    StoryTypeSet {
        at: String,
        story_type: String,
    },
    StoryLabelsSet {
        at: String,
        labels: Vec<String>,
    },
    StoryTitleSet {
        at: String,
        title: String,
    },
    StoryDescriptionSet {
        at: String,
        description: String,
    },
    StoryClosedAndArchived {
        at: String,
        state: String,
    },
    StoryDeleted {
        at: String,
        reason: String,
    },
    /// A git commit that names this story, recorded once and only once.
    ///
    /// Storyhook's eighteenth event kind, and the only one that exists for an
    /// *invariant* rather than for a new fact. `commit-sync` used to write its
    /// link as a `StoryCommentAdded` reading `[git] <short>: <subject>`, and
    /// then decide whether it had already done so by scanning every event on
    /// the story for a comment starting with that prefix — O(events) per
    /// commit per story, and defeatable by a user typing a comment that starts
    /// the same way. Here the sha is a field, so `(project, story, sha)` is a
    /// primary key in the store and a second link record is not a state the
    /// database can hold.
    ///
    /// **It renders exactly as the comment it replaces.** `fold_story` turns it
    /// into a `StoryComment` whose text is `[git] <short>: <subject>`, in the
    /// same position in the same stream, so `story show`, the dashboard, an
    /// export and the golden corpus see no difference. That is deliberate: the
    /// change is to how idempotency is *guaranteed*, not to what a user reads.
    ///
    /// `sha` is the full 40-character hash even though only seven of them are
    /// rendered — the record is permanent, and an abbreviation that is unique
    /// in a repository today is not unique in it forever.
    StoryCommitLinked {
        at: String,
        /// The commit's full hash, from `git log --format=%H`.
        sha: String,
        /// The commit's subject line — its first line, and all that is
        /// rendered.
        subject: String,
    },
    /// Hides a CLOSED-superstate story from the primary UI — the "Archive"
    /// action in `story hide` and the dashboard's per-column Archive button
    /// (SH-43).
    ///
    /// Deliberately not named `archived`: that word is already load-bearing —
    /// [`StoryRow::archived`](crate::store::types::StoryRow::archived) is
    /// derived from `closed_at` and tied to it by a schema CHECK, and
    /// `resolve_open_story` uses it as the "this story cannot be edited"
    /// test. `hidden` is an orthogonal, reversible display fact: it only ever
    /// applies on top of an already-closed story, and the fold clears it
    /// automatically the moment the story's superstate resolves back to OPEN
    /// (see [`fold_story`]), so "hidden implies closed" holds without a
    /// schema CHECK against a column a CHECK cannot see (`superstate` is
    /// itself derived, not stored raw on this event).
    StoryHidden {
        at: String,
    },
    /// The inverse of [`StoryHidden`](Self::StoryHidden) — the "Unarchive"
    /// action.
    StoryUnhidden {
        at: String,
    },
}

/// The `kind` tag [`StoryEvent::StoryCommitLinked`] serializes with.
///
/// Named, unlike the other seventeen, because the store keys a *projection* off
/// it — `store::sqlite::write::project_commit_link` reads the `kind` column to
/// decide whether an event carries a sha — and the same literal spelled in two
/// files is a literal that drifts.
pub const KIND_STORY_COMMIT_LINKED: &str = "StoryCommitLinked";

/// How much of a commit hash a link record shows.
///
/// Seven, which is what `commit-sync` has always abbreviated to and therefore
/// what every existing `[git]` comment carries.
const SHORT_SHA: usize = 7;

/// The abbreviated form of `sha`, as it appears in a link record.
#[must_use]
pub fn short_sha(sha: &str) -> &str {
    &sha[..SHORT_SHA.min(sha.len())]
}

/// The comment text a git link record renders as.
///
/// One function, called by [`fold_story`] and by nothing else that formats —
/// the string `[git] <short>: <subject>` is a user-visible contract with a
/// test of its own (`service_git.rs::the_comment_reads_git_short_hash_colon_subject`),
/// and a second copy of it is how such a contract drifts.
#[must_use]
pub fn git_link_comment(sha: &str, subject: &str) -> String {
    format!("[git] {}: {subject}", short_sha(sha))
}

/// The commit hash a *pre*-#18 link record names, if `text` is one.
///
/// Before [`StoryEvent::StoryCommitLinked`] existed, `commit-sync` recorded a
/// link as an ordinary comment reading `[git] <short>: <subject>`. Those events
/// are permanent — they are in every store that has run the old code, and in
/// every `.storyhook` tree `story migrate` has yet to read — so the store
/// projects them into `story_commit_links` alongside the real thing, and this
/// is the rule it uses. Schema migration 2 states the same rule in SQL for rows
/// that were already there; `the_sql_backfill_and_the_rust_parser_agree` is
/// what keeps the two spellings honest.
///
/// Returns the *abbreviation*, because that is all such a comment preserved.
#[must_use]
pub fn git_link_sha(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("[git] ")?;
    let (sha, _) = rest.split_once(':')?;
    // Hex and non-empty. A user comment that opens `[git] rebase: ...` is not a
    // link record, and treating it as one would suppress a real commit.
    (!sha.is_empty() && sha.chars().all(|c| c.is_ascii_hexdigit())).then_some(sha)
}

/// Every `kind` tag [`StoryEvent`] answers to.
///
/// The legacy importer is why this is a list rather than an implicit property
/// of the derive: reading a `.storyhook` event log has to tell *this kind is
/// from a newer storyhook and must be kept verbatim* apart from *this event is
/// corrupt*, and both look identical to `serde_json::from_str::<StoryEvent>` —
/// it returns `Err` either way. Retaining a corrupt `StoryCreated` as an
/// unknown payload would import a story with no title and never say so.
///
/// It cannot drift: `every_known_kind_is_a_variant_and_every_variant_is_known`
/// reads serde's own `unknown variant, expected one of …` list back out of the
/// derive and compares it to this array.
pub const EVENT_KINDS: [&str; 20] = [
    "StoryCreated",
    "StoryCommentAdded",
    "StoryCommentRetracted",
    "StoryAssigned",
    "StoryAssigneeCleared",
    "StoryAwaitingSet",
    "StoryAwaitingCleared",
    "StoryStateChanged",
    "StoryRelationshipAdded",
    "StoryRelationshipRemoved",
    "StoryPrioritySet",
    "StoryTypeSet",
    "StoryLabelsSet",
    "StoryTitleSet",
    "StoryDescriptionSet",
    "StoryClosedAndArchived",
    "StoryDeleted",
    KIND_STORY_COMMIT_LINKED,
    "StoryHidden",
    "StoryUnhidden",
];

/// Whether `kind` is an event this binary can decode.
#[must_use]
pub fn is_known_event_kind(kind: &str) -> bool {
    EVENT_KINDS.contains(&kind)
}

/// The serde `kind` tag `event` serializes with.
///
/// An exhaustive match, so a new variant cannot reach the store's `kind` column
/// without a name being chosen for it here.
#[must_use]
pub fn event_kind(event: &StoryEvent) -> &'static str {
    match event {
        StoryEvent::StoryCreated { .. } => "StoryCreated",
        StoryEvent::StoryCommentAdded { .. } => "StoryCommentAdded",
        StoryEvent::StoryCommentRetracted { .. } => "StoryCommentRetracted",
        StoryEvent::StoryAssigned { .. } => "StoryAssigned",
        StoryEvent::StoryAssigneeCleared { .. } => "StoryAssigneeCleared",
        StoryEvent::StoryAwaitingSet { .. } => "StoryAwaitingSet",
        StoryEvent::StoryAwaitingCleared { .. } => "StoryAwaitingCleared",
        StoryEvent::StoryStateChanged { .. } => "StoryStateChanged",
        StoryEvent::StoryRelationshipAdded { .. } => "StoryRelationshipAdded",
        StoryEvent::StoryRelationshipRemoved { .. } => "StoryRelationshipRemoved",
        StoryEvent::StoryPrioritySet { .. } => "StoryPrioritySet",
        StoryEvent::StoryTypeSet { .. } => "StoryTypeSet",
        StoryEvent::StoryLabelsSet { .. } => "StoryLabelsSet",
        StoryEvent::StoryTitleSet { .. } => "StoryTitleSet",
        StoryEvent::StoryDescriptionSet { .. } => "StoryDescriptionSet",
        StoryEvent::StoryClosedAndArchived { .. } => "StoryClosedAndArchived",
        StoryEvent::StoryDeleted { .. } => "StoryDeleted",
        StoryEvent::StoryCommitLinked { .. } => KIND_STORY_COMMIT_LINKED,
        StoryEvent::StoryHidden { .. } => "StoryHidden",
        StoryEvent::StoryUnhidden { .. } => "StoryUnhidden",
    }
}

/// The write-path guard against a label no reader can ever address again.
///
/// Every producer of [`StoryEvent::StoryLabelsSet`] is expected to normalize
/// its labels through [`normalize_labels`] before appending — this exists for
/// the producer added later that forgets. Because normalization is
/// idempotent, a legitimately-built event can never trip it; it is meant to
/// fail loud rather than let a comma-bearing or blank label back into the
/// store the way SH-145 did.
///
/// Called from [`crate::service::append_and_fold`], the one write path every
/// service funnels through. Two callers bypass it deliberately, because both
/// replay a history rather than admit new input: `append_raw_events` (the
/// legacy round-trip) and project restore from an export document. A bad
/// label arriving through either is left for `story doctor` to find.
pub fn validate_event_for_append(event: &StoryEvent) -> Result<(), AppError> {
    if let StoryEvent::StoryLabelsSet { labels, .. } = event {
        for label in labels {
            if label.trim() != label || label.is_empty() {
                return Err(AppError::Validation(format!(
                    "invalid label `{label}`: a label cannot be blank or carry leading/trailing whitespace"
                )));
            }
            if label.contains(',') {
                return Err(AppError::Validation(format!(
                    "invalid label `{label}`: a label cannot contain a comma — `,` separates labels, it cannot be part of one"
                )));
            }
        }
    }
    Ok(())
}

pub fn last_activity_type(events: &[StoryEvent]) -> &'static str {
    events
        .last()
        .map(|event| match event {
            StoryEvent::StoryCreated { .. } => "created",
            StoryEvent::StoryCommentAdded { .. } => "comment",
            StoryEvent::StoryCommentRetracted { .. } => "comment-retracted",
            StoryEvent::StoryAssigned { .. } => "assigned",
            StoryEvent::StoryAssigneeCleared { .. } => "assignee-cleared",
            StoryEvent::StoryAwaitingSet { .. } => "awaiting-set",
            StoryEvent::StoryAwaitingCleared { .. } => "awaiting-cleared",
            StoryEvent::StoryStateChanged { .. } => "state-change",
            StoryEvent::StoryRelationshipAdded { .. } => "relationship-added",
            StoryEvent::StoryRelationshipRemoved { .. } => "relationship-removed",
            StoryEvent::StoryPrioritySet { .. } => "priority-set",
            StoryEvent::StoryTypeSet { .. } => "type-set",
            StoryEvent::StoryLabelsSet { .. } => "labels-set",
            StoryEvent::StoryTitleSet { .. } => "title-set",
            StoryEvent::StoryDescriptionSet { .. } => "description-set",
            StoryEvent::StoryClosedAndArchived { .. } => "archived",
            StoryEvent::StoryDeleted { .. } => "deleted",
            // `"comment"`, not `"commit-linked"`. A link record *is* a comment
            // to everything that reads a story — see `StoryCommitLinked` — and
            // this string is rendered to users in `story list --stale`. The
            // event kind is a storage detail; changing what a human reads
            // because of it would be a behaviour change smuggled in under a
            // refactor.
            StoryEvent::StoryCommitLinked { .. } => "comment",
            StoryEvent::StoryHidden { .. } => "hidden",
            StoryEvent::StoryUnhidden { .. } => "unhidden",
        })
        .unwrap_or("unknown")
}

/// Read-path validation, applied by `storage::load_states` on **every** read.
///
/// Deliberately minimal: a rule added here can make an existing project
/// unreadable rather than merely uneditable. Rules that only a new state set
/// must satisfy belong in [`validate_state_defs_for_write`].
pub fn validate_state_defs(states: &[StateDef]) -> Result<(), AppError> {
    let has_open = states
        .iter()
        .any(|state| state.super_state == SuperState::Open);
    let has_closed = states
        .iter()
        .any(|state| state.super_state == SuperState::Closed);

    if has_open && has_closed {
        Ok(())
    } else {
        Err(AppError::Validation(
            "state set must include at least one OPEN state and one CLOSED state".to_string(),
        ))
    }
}

/// A state slug must be lowercase alphanumerics in dash-separated words.
///
/// Slugs are addresses, not labels: they are typed as CLI arguments
/// (`story move SH-1 <slug>`) and interpolated into URL path segments
/// (`DELETE /api/repos/<id>/states/<slug>`, split on `/` by the router). A
/// slug containing a space, slash, or capital cannot be addressed by either.
pub fn validate_state_slug(slug: &str) -> Result<(), AppError> {
    let invalid = |reason: &str| {
        Err(AppError::Validation(format!(
            "invalid state slug `{slug}`: {reason} (use lowercase letters, digits, and single dashes, e.g. `in-review`)"
        )))
    };

    if slug.is_empty() {
        return invalid("it is empty");
    }
    if let Some(bad) = slug
        .chars()
        .find(|ch| !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && *ch != '-')
    {
        return invalid(&format!("`{bad}` is not allowed"));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return invalid("it starts or ends with a dash");
    }
    if slug.contains("--") {
        return invalid("it contains a double dash");
    }
    Ok(())
}

/// A story type's slug must be lowercase alphanumerics in dash-separated words.
///
/// The same rule as [`validate_state_slug`], for the same reason: a type slug
/// is an address, not a label. It is typed as a CLI argument (`story new t
/// --type <slug>`, `story type remove <slug>`) and rendered into the
/// dashboard's type picker, so a slug carrying a space, a slash, or a capital
/// cannot reliably be named by the things that have to name it.
///
/// # Why this is a domain invariant rather than a parser rule
///
/// SH-62 taught the CLI to refuse a flag-shaped token, which stopped
/// `story type add --typo` — but only that spelling of it. `story type add --
/// --typo` is a legitimate use of the argument terminator and delivers the same
/// string as data, and `in review` is unaddressable for exactly this reason
/// while not being flag-shaped at all. A parser cannot stand in for a rule
/// about what a slug *is*, so the rule lives here and every surface reaching
/// [`crate::service::ConfigService::add_type`] inherits it.
///
/// Deliberately **not** applied to a slug arriving in an export document or a
/// legacy tree (SH-134's D3): repairing one means renaming it, and every
/// `StoryTypeSet` event names the slug it set, so a rename strands the stories
/// carrying it. Such a slug is reported by `story doctor` instead.
pub fn validate_type_slug(slug: &str) -> Result<(), AppError> {
    let invalid = |reason: &str| {
        Err(AppError::Validation(format!(
            "invalid type slug `{slug}`: {reason} (use lowercase letters, digits, and single dashes, e.g. `feature-request`)"
        )))
    };

    if slug.is_empty() {
        return invalid("it is empty");
    }
    if let Some(bad) = slug
        .chars()
        .find(|ch| !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && *ch != '-')
    {
        return invalid(&format!("`{bad}` is not allowed"));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return invalid("it starts or ends with a dash");
    }
    if slug.contains("--") {
        return invalid("it contains a double dash");
    }
    Ok(())
}

/// The labels a caller's raw values denote.
///
/// Comma is the label delimiter on every surface that *reads* labels back:
/// `story list --label a,b`, `story unlabel <id> a,b`, the GitHub
/// conflict-resolution path that reconstructs a set by splitting on `", "`.
/// This is the one place that delimiter is honored on the way *in*, so that a
/// value like `"web,sse"` handed to any producer of
/// [`StoryEvent::StoryLabelsSet`] becomes the two labels `web` and `sse`
/// rather than one label that none of those readers can ever address again
/// (SH-164 — a comma-bearing label is not malformed data, it is an
/// unreachable one).
///
/// Splits every raw value on `,`, trims whitespace, drops anything left
/// empty, and returns the deduplicated, sorted result. Idempotent by
/// construction: normalizing an already-normalized set changes nothing, which
/// is what lets a caller normalize the *union* of a story's existing labels
/// with new input (as `set_labels` does) without a second pass mattering.
pub fn normalize_labels<I, S>(raw: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let set: BTreeSet<String> = raw
        .into_iter()
        .flat_map(|value| {
            value
                .as_ref()
                .split(',')
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
        })
        .filter(|s| !s.is_empty())
        .collect();
    set.into_iter().collect()
}

/// A type's glyph must be a short, printable, non-blank string.
///
/// This deliberately does not try to prove the value *is* an emoji — that
/// needs Unicode tables this crate does not carry, and would refuse
/// legitimate glyphs like `▲` or `§`. The bound of 12 `char`s is generous: a
/// ZWJ family sequence (👨‍👩‍👧‍👦) is 7 `char`s, a flag tag sequence 7 more.
/// It exists to keep the dashboard's badge from being handed a sentence.
pub fn validate_type_glyph(glyph: &str) -> Result<(), AppError> {
    let trimmed = glyph.trim();
    if trimmed.is_empty() {
        return Err(AppError::Validation(
            "a type's emoji cannot be blank".to_string(),
        ));
    }
    if let Some(bad) = trimmed
        .chars()
        .find(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(AppError::Validation(format!(
            "a type's emoji cannot contain `{}` (found in `{trimmed}`)",
            bad.escape_debug()
        )));
    }
    if trimmed.chars().count() > 12 {
        return Err(AppError::Validation(format!(
            "a type's emoji must be 12 characters or fewer, got `{trimmed}`"
        )));
    }
    Ok(())
}

/// Write-path validation: everything [`validate_state_defs`] requires, plus
/// the rules a state set must satisfy to be *written*.
///
/// Kept separate from the read path so a project carrying a legacy slug
/// still loads — it just reports the offending slug the first time someone
/// edits its states.
pub fn validate_state_defs_for_write(states: &[StateDef]) -> Result<(), AppError> {
    validate_state_defs(states)?;

    let mut seen = BTreeSet::new();
    for state in states {
        validate_state_slug(&state.slug)?;
        if !seen.insert(state.slug.as_str()) {
            return Err(AppError::Validation(format!(
                "state `{}` is defined more than once",
                state.slug
            )));
        }
        if let Some(role) = state.role.as_deref()
            && role != STATE_ROLE_ACTIVE
        {
            return Err(AppError::Validation(format!(
                "state `{}` has unknown role `{role}` (the only role is `{STATE_ROLE_ACTIVE}`)",
                state.slug
            )));
        }
    }

    let active: Vec<&str> = states
        .iter()
        .filter(|state| state.role.as_deref() == Some(STATE_ROLE_ACTIVE))
        .map(|state| state.slug.as_str())
        .collect();
    if active.len() > 1 {
        return Err(AppError::Validation(format!(
            "only one state may have role `{STATE_ROLE_ACTIVE}`, but {} do: {}",
            active.len(),
            active.join(", ")
        )));
    }

    Ok(())
}

/// A state every project must have, and the superstate it must have it in.
///
/// The pair is the unit. A `done` that is OPEN is not what anything downstream
/// means by "done", so an invariant over the slug alone would permit exactly
/// the thing it exists to prevent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequiredState {
    pub slug: &'static str,
    pub super_state: SuperState,
}

/// The state set every project must contain (SH-125).
///
/// A project may define as many further states as it likes, in any order —
/// this is a floor, not the catalog. Deliberately silent about
/// [`STATE_ROLE_ACTIVE`]: the one-active rule in
/// [`validate_state_defs_for_write`] already governs roles, and a project is
/// free to put `active` on a state of its own.
pub static REQUIRED_STATES: [RequiredState; 4] = [
    RequiredState {
        slug: "todo",
        super_state: SuperState::Open,
    },
    RequiredState {
        slug: "in-progress",
        super_state: SuperState::Open,
    },
    RequiredState {
        slug: "blocked",
        super_state: SuperState::Open,
    },
    RequiredState {
        slug: "done",
        super_state: SuperState::Closed,
    },
];

/// The state a deleted story comes to rest in.
///
/// Deletion has to land the story somewhere CLOSED rather than merely stamp its
/// superstate CLOSED, because the two are stored independently and a slug whose
/// superstate contradicts the story's is the SH-130 defect exactly. Completing
/// the derivation here — rather than overriding half of it — is what lets
/// `stories.superstate` become a pure function of the slug and the catalog, and
/// so what lets a composite foreign key express the rule.
///
/// `done` is preferred and is what every conforming project gets:
/// [`REQUIRED_STATES`] makes it CLOSED and unremovable, so the answer needs no
/// ordering information — which matters, because the fold is handed a
/// slug-keyed [`BTreeMap`] that iterates alphabetically and cannot tell which
/// CLOSED state a project configured first.
///
/// The two fallbacks are defensive rather than expected. A catalog reaching the
/// store through `service::state_set` always carries `done`; one that somehow
/// does not still gets a deterministic CLOSED slug, and a catalog with no CLOSED
/// state at all leaves the story where it is, so the fold stays total and the
/// schema — not a panic here — is what refuses the write.
fn resting_state_for_deleted(states: &BTreeMap<String, StateDef>) -> Option<&StateDef> {
    let required_closed = REQUIRED_STATES
        .iter()
        .find(|required| required.super_state == SuperState::Closed)
        .map(|required| required.slug);

    required_closed
        .and_then(|slug| states.get(slug))
        .filter(|def| def.super_state == SuperState::Closed)
        .or_else(|| {
            states
                .values()
                .find(|def| def.super_state == SuperState::Closed)
        })
}

/// Whether `states` satisfies the [`REQUIRED_STATES`] floor.
///
/// Not part of [`validate_state_defs_for_write`], and that separation is
/// load-bearing rather than stylistic: that function runs on the legacy read
/// path (`storage::save_states`) and inside `MigrationPlan::build`, so a floor
/// enforced there would refuse to migrate every legacy tree written before the
/// floor existed — including the baseline corpus that
/// `tests/migrate_round_trip.rs` guards. Writers that must accept old data call
/// [`with_required_states`] instead; only a *user edit* is refused.
pub fn validate_required_states(states: &[StateDef]) -> Result<(), AppError> {
    let mut missing = Vec::new();
    for required in &REQUIRED_STATES {
        match states.iter().find(|state| state.slug == required.slug) {
            None => missing.push(required.slug),
            Some(found) if found.super_state != required.super_state => {
                return Err(AppError::Validation(format!(
                    "state `{}` must be {}, but this project defines it as {}; every project \
                     needs {} (SH-125)",
                    required.slug,
                    required.super_state.as_str(),
                    found.super_state.as_str(),
                    required_state_list()
                )));
            }
            Some(_) => {}
        }
    }

    if missing.is_empty() {
        return Ok(());
    }
    // Worded for two readers: a user whose edit was refused, and `story doctor`
    // reporting a project that was already this way. "would leave it without"
    // is true of the first and false of the second, so neither is said.
    Err(AppError::Validation(format!(
        "every project needs {}; {} {} missing. Run `story doctor --fix` to add {}",
        required_state_list(),
        missing
            .iter()
            .map(|slug| format!("`{slug}`"))
            .collect::<Vec<_>>()
            .join(", "),
        if missing.len() == 1 { "is" } else { "are" },
        if missing.len() == 1 { "it" } else { "them" }
    )))
}

/// `states` with any missing [`REQUIRED_STATES`] added.
///
/// The repair may only **add**. A required slug already present under the wrong
/// superstate is an error rather than something to correct, because the two
/// candidate corrections are both destructive: a second row cannot carry the
/// slug (it is a primary key), and flipping the superstate silently reclassifies
/// every story sitting in that state — the reclassification
/// `ConfigService::update_state` refuses to perform without being told where to
/// migrate the occupants.
///
/// A missing OPEN state is inserted after the last OPEN state, never at the
/// front: position 0 is where new stories are created
/// (`ConfigService`'s callers read the *first* OPEN state), so a repair that
/// landed there would silently change what `story new` does. A missing CLOSED
/// state is appended.
///
/// Idempotent: repairing an already-conforming set returns it unchanged, which
/// is what makes it safe on a path that runs on every import.
pub fn with_required_states(states: &[StateDef]) -> Result<Vec<StateDef>, AppError> {
    let mut repaired = states.to_vec();
    for required in &REQUIRED_STATES {
        if let Some(found) = repaired.iter().find(|state| state.slug == required.slug) {
            if found.super_state != required.super_state {
                return Err(AppError::Validation(format!(
                    "state `{}` must be {}, but this project defines it as {}; storyhook will \
                     add a missing state but will not reclassify the stories in an existing one",
                    required.slug,
                    required.super_state.as_str(),
                    found.super_state.as_str()
                )));
            }
            continue;
        }

        let addition = StateDef {
            slug: required.slug.to_string(),
            super_state: required.super_state.clone(),
            // No role. `active` decides where `commit-sync` moves a claimed
            // story, so awarding one here would change a project's behaviour to
            // satisfy an invariant that says nothing about roles.
            role: None,
            description: None,
        };
        match required.super_state {
            SuperState::Open => {
                let after_last_open = repaired
                    .iter()
                    .rposition(|state| state.super_state == SuperState::Open)
                    .map_or(0, |index| index + 1);
                repaired.insert(after_last_open, addition);
            }
            SuperState::Closed => repaired.push(addition),
        }
    }
    Ok(repaired)
}

/// `` `todo`, `in-progress`, `blocked` and `done` `` — for error messages.
fn required_state_list() -> String {
    let slugs: Vec<String> = REQUIRED_STATES
        .iter()
        .map(|required| format!("`{}`", required.slug))
        .collect();
    match slugs.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => slugs.join(""),
    }
}

pub fn fold_story(
    id: &str,
    events: &[StoryEvent],
    states: &BTreeMap<String, StateDef>,
) -> Result<StorySnapshot, AppError> {
    let mut title = None;
    let mut created_at = None;
    let mut updated_at = None;
    let mut state = None;
    let mut assignee = None;
    let mut awaiting = None;
    let mut priority = Priority::None;
    let mut story_type = None;
    let mut description = None;
    let mut labels = Vec::new();
    let mut comments = Vec::new();
    let mut relationships = BTreeSet::new();
    let mut closed_at = None;
    let mut deleted = false;
    let mut deleted_reason = None;
    let mut hidden_at = None;

    for event in events {
        match event {
            StoryEvent::StoryCreated {
                at,
                title: story_title,
                state: story_state,
            } => {
                title = Some(story_title.clone());
                created_at = Some(at.clone());
                updated_at = Some(at.clone());
                state = Some(story_state.clone());
            }
            StoryEvent::StoryCommentAdded { at, text } => {
                comments.push(StoryComment {
                    at: at.clone(),
                    text: text.clone(),
                });
                updated_at = Some(at.clone());
            }
            // Rendered into the comment stream, in the same position and with
            // the same text the `StoryCommentAdded` it replaced carried. The
            // difference between the two is visible to the store and to
            // nothing else.
            StoryEvent::StoryCommitLinked { at, sha, subject } => {
                comments.push(StoryComment {
                    at: at.clone(),
                    text: git_link_comment(sha, subject),
                });
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryCommentRetracted {
                at,
                comment_at,
                text,
            } => {
                // The most recent match, and a miss is a no-op rather than an
                // error: an event log is replayed, and a replay that could fail
                // on its own history would make a story unreadable rather than
                // merely wrong.
                if let Some(index) = comments
                    .iter()
                    .rposition(|c| &c.at == comment_at && &c.text == text)
                {
                    comments.remove(index);
                }
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryAssigned { at, member_id } => {
                assignee = Some(member_id.clone());
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryAssigneeCleared { at } => {
                assignee = None;
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryAwaitingSet {
                at,
                awaiting: blocked_on,
            } => {
                awaiting = Some(blocked_on.clone());
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryAwaitingCleared { at } => {
                awaiting = None;
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryStateChanged {
                at,
                state: story_state,
            } => {
                state = Some(story_state.clone());
                updated_at = Some(at.clone());
                // Moving into an OPEN state is what *reopening* is, so it
                // retracts the closure markers. Without this the fold is not
                // total: `closed_at` and `deleted` have events that set them
                // and none that clear them, so the only way to reopen a story
                // is to delete history — which is precisely what
                // `unarchive_story` does, and precisely what an append-only
                // event store cannot do.
                //
                // Scoped to states the project actually defines and actually
                // calls open: an unrecognised slug leaves the flags alone, so
                // a story that folds today because `deleted` forces its
                // superstate cannot be made unfoldable by this rule.
                if states
                    .get(story_state)
                    .is_some_and(|def| def.super_state == SuperState::Open)
                {
                    closed_at = None;
                    deleted = false;
                    deleted_reason = None;
                }
            }
            StoryEvent::StoryPrioritySet {
                at,
                priority: new_priority,
            } => {
                priority = new_priority.clone();
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryTypeSet {
                at,
                story_type: new_type,
            } => {
                story_type = Some(new_type.clone());
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryLabelsSet {
                at,
                labels: new_labels,
            } => {
                labels = new_labels.clone();
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryTitleSet {
                at,
                title: new_title,
            } => {
                title = Some(new_title.clone());
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryDescriptionSet {
                at,
                description: new_description,
            } => {
                description = Some(new_description.clone());
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryRelationshipAdded {
                at,
                other_id,
                relation,
            } => {
                relationships.insert(StoryRelation {
                    relation: relation.clone(),
                    other_id: other_id.clone(),
                });
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryRelationshipRemoved {
                at,
                other_id,
                relation,
            } => {
                relationships.remove(&StoryRelation {
                    relation: relation.clone(),
                    other_id: other_id.clone(),
                });
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryClosedAndArchived {
                at,
                state: story_state,
            } => {
                state = Some(story_state.clone());
                updated_at = Some(at.clone());
                // Symmetric with `StoryStateChanged` above, and SH-130 is why.
                //
                // A close marker names the state it closed *into*. If that
                // state is later reclassified OPEN, the story is no longer
                // closed, and stamping `closed_at` anyway produced an archived
                // story reporting an OPEN superstate — the same
                // two-columns-disagreeing defect this story is about, reached
                // through `story state update` rather than through `delete`.
                //
                // Only a state the project *currently* calls OPEN retracts the
                // closure. An unrecognised slug still closes, exactly as
                // before: the fold cannot tell what an undefined state meant,
                // and refusing to close would make a story that folds today
                // stop folding tomorrow.
                if !states
                    .get(story_state)
                    .is_some_and(|def| def.super_state == SuperState::Open)
                {
                    closed_at = Some(at.clone());
                }
            }
            StoryEvent::StoryDeleted { at, reason } => {
                deleted = true;
                deleted_reason = Some(reason.clone());
                updated_at = Some(at.clone());
                if closed_at.is_none() {
                    closed_at = Some(at.clone());
                }
            }
            StoryEvent::StoryHidden { at } => {
                hidden_at = Some(at.clone());
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryUnhidden { at } => {
                hidden_at = None;
                updated_at = Some(at.clone());
            }
        }
    }

    let state = state.ok_or_else(|| AppError::Integrity(format!("story {id} is missing state")))?;
    let title = title.ok_or_else(|| AppError::Integrity(format!("story {id} is missing title")))?;
    let created_at = created_at
        .ok_or_else(|| AppError::Integrity(format!("story {id} is missing created_at")))?;
    let updated_at = updated_at.unwrap_or_else(|| created_at.clone());
    // A deleted story comes to rest in a CLOSED state rather than keeping the
    // slug it happened to be in — SH-130.
    //
    // This arm used to force `superstate = CLOSED` and leave `state` alone,
    // which made the two disagree: SH-20 sat in `todo` while reading CLOSED, so
    // `story list --state todo` returned a closed, deleted story. Half a
    // derivation is what produced the illegal pair; completing it is the fix,
    // and it applies to *historical* logs too — a `StoryDeleted` written years
    // ago folds to the resting state now, which is what keeps the rebuild
    // oracle, `story doctor` and `import` of an old export document all
    // agreeing with the schema.
    //
    // The old comment claimed the override kept a deleted story correct even if
    // its slug were later removed from the catalog. That was true and is now
    // delivered properly rather than asserted: `state_usage` counts only
    // undeleted stories, so `remove_state` never saw them and the slug really
    // was removable — whereas `done` is unremovable under the SH-125 floor.
    let (state, superstate) = if deleted {
        match resting_state_for_deleted(states) {
            Some(resting) => (resting.slug.clone(), SuperState::Closed),
            // No CLOSED state exists to come to rest in. Keep the slug and stay
            // foldable; the schema refuses the write, which is a loud failure
            // rather than a silently illegal row.
            None => (state, SuperState::Closed),
        }
    } else {
        let superstate = states
            .get(&state)
            .ok_or_else(|| {
                AppError::Validation(format!("story {id} references undefined state `{state}`"))
            })?
            .super_state
            .clone();
        (state, superstate)
    };

    // Symmetric with the `deleted`/`closed_at` retraction above, and for the
    // same reason (SH-130's illegal-tuple class, generalized): `states` is
    // one fixed, *current* catalog snapshot applied uniformly across the
    // whole replay, so if the story's resting state has been reclassified
    // OPEN since a `StoryHidden` event was appended, that event's effect must
    // not survive the refold. Applying the rule once here, on the final
    // superstate, covers every path that can produce it — a later
    // `StoryStateChanged` into an OPEN state, or a state reclassified OPEN
    // out from under a story that was validly hidden while it was CLOSED —
    // rather than re-deriving it at each event that can affect superstate.
    if superstate == SuperState::Open {
        hidden_at = None;
    }

    Ok(StorySnapshot {
        id: id.to_string(),
        title,
        created_at,
        updated_at,
        state,
        superstate,
        assignee,
        awaiting,
        priority,
        labels,
        story_type,
        description,
        comments,
        relationships: relationships.into_iter().collect(),
        closed_at,
        deleted,
        deleted_reason,
        hidden_at,
    })
}

pub fn is_relation_input(raw: &str) -> bool {
    relation_edges(raw).is_some()
}

pub fn relation_edges(input: &str) -> Option<Vec<(&'static str, &'static str)>> {
    match input {
        "relates-to" | "related-to" => Some(vec![("relates-to", "relates-to")]),
        "blocks" => Some(vec![("blocks", "blocked-by")]),
        "blocked-by" => Some(vec![("blocked-by", "blocks")]),
        "parent-of" => Some(vec![("parent-of", "child-of")]),
        "child-of" => Some(vec![("child-of", "parent-of")]),
        "duplicate-of" => Some(vec![("duplicate-of", "duplicate-of")]),
        "obviates" => Some(vec![("obviates", "obviated-by")]),
        "obviated-by" => Some(vec![("obviated-by", "obviates")]),
        _ => None,
    }
}

pub fn inverse_relation(relation: &str) -> Option<&'static str> {
    match relation {
        "relates-to" => Some("relates-to"),
        "blocks" => Some("blocked-by"),
        "blocked-by" => Some("blocks"),
        "parent-of" => Some("child-of"),
        "child-of" => Some("parent-of"),
        "duplicate-of" => Some("duplicate-of"),
        "obviates" => Some("obviated-by"),
        "obviated-by" => Some("obviates"),
        _ => None,
    }
}

pub fn is_mutual_relation(relation: &str) -> bool {
    matches!(relation, "relates-to" | "duplicate-of")
}

pub fn compute_integrity_issues(
    stories: &BTreeMap<String, StorySnapshot>,
) -> BTreeMap<String, Vec<String>> {
    let mut issues: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let graph = HierarchyGraph::from_stories(stories);

    for story in stories.values() {
        let parent_count = graph.parents_of(&story.id).len();

        if parent_count > 1 {
            issues
                .entry(story.id.clone())
                .or_default()
                .push("story has multiple parents".to_string());
        }

        for relation in &story.relationships {
            let Some(other_story) = stories.get(&relation.other_id) else {
                issues.entry(story.id.clone()).or_default().push(format!(
                    "dangling relation `{}` to missing story `{}`",
                    relation.relation, relation.other_id
                ));
                continue;
            };

            if let Some(expected_inverse) = inverse_relation(&relation.relation) {
                let has_inverse = other_story.relationships.iter().any(|candidate| {
                    candidate.other_id == story.id && candidate.relation == expected_inverse
                });

                if !has_inverse {
                    let issue = if is_mutual_relation(&relation.relation) {
                        format!(
                            "missing reciprocal relation `{}` on story `{}`",
                            relation.relation, relation.other_id
                        )
                    } else {
                        format!(
                            "missing inverse relation `{}` on story `{}`",
                            expected_inverse, relation.other_id
                        )
                    };
                    issues.entry(story.id.clone()).or_default().push(issue);
                }
            }
        }
    }

    for node in graph.cycle_nodes() {
        issues
            .entry(node)
            .or_default()
            .push("parent/child cycle detected".to_string());
    }

    issues
}

pub fn has_children(story: &StorySnapshot) -> bool {
    story
        .relationships
        .iter()
        .any(|r| r.relation == "parent-of")
}

pub fn compute_progress(
    story: &StorySnapshot,
    all_stories: &BTreeMap<String, StorySnapshot>,
) -> Option<ProgressRollup> {
    let children: Vec<&str> = story
        .relationships
        .iter()
        .filter(|r| r.relation == "parent-of")
        .map(|r| r.other_id.as_str())
        .collect();

    if children.is_empty() {
        return None;
    }

    let children_total = children.len();
    let children_done = children
        .iter()
        .filter(|child_id| {
            all_stories
                .get(**child_id)
                .is_some_and(|s| s.superstate == SuperState::Closed)
        })
        .count();

    Some(ProgressRollup {
        children_done,
        children_total,
    })
}

/// The state a story moves into when a commit first mentions it, and the same
/// answer an epic's display state (below) promotes to.
///
/// The explicit `active` role wins; failing that, a project with exactly two
/// OPEN states is assumed to mean "todo, then the other one". The heuristic is
/// inherited, and it is why `active_state` can answer for a project that has
/// never configured a role.
pub fn active_state(states: &[StateDef]) -> Option<StateDef> {
    if let Some(state) = states
        .iter()
        .find(|state| state.role.as_deref() == Some(STATE_ROLE_ACTIVE))
    {
        return Some(state.clone());
    }
    let open: Vec<&StateDef> = states
        .iter()
        .filter(|state| state.super_state == SuperState::Open)
        .collect();
    (open.len() == 2).then(|| open[1].clone())
}

/// The project's first configured OPEN state.
pub fn default_open_state(states: &[StateDef]) -> Option<StateDef> {
    states
        .iter()
        .find(|state| state.super_state == SuperState::Open)
        .cloned()
}

/// The state at which an epic's Web-board card should be shown, when that
/// differs from its own literal [`StorySnapshot::state`] (SH-165).
///
/// Mirrors the guard `GitService::record_commit` already applies before an
/// auto-transition — the SH-165 council verdict extends it to display
/// promotion rather than inventing a second rule: only a story parked in the
/// project's neutral default open state (what [`default_open_state`]
/// resolves to) is eligible, so a state a human deliberately chose —
/// `blocked`, or any other custom Open state — is never silently overridden.
/// Returns `None` when no override applies, meaning the caller should fall
/// back to the story's own `state`.
pub fn compute_epic_display_state(
    story: &StorySnapshot,
    all_stories: &BTreeMap<String, StorySnapshot>,
    states: &[StateDef],
) -> Option<String> {
    if !has_children(story) {
        return None;
    }
    let default_open = default_open_state(states)?;
    if story.state != default_open.slug {
        return None;
    }
    let active = active_state(states)?;

    let has_active_child = story
        .relationships
        .iter()
        .filter(|r| r.relation == "parent-of")
        .any(|r| {
            all_stories
                .get(r.other_id.as_str())
                .is_some_and(|child| child.state == active.slug)
        });

    has_active_child.then_some(active.slug)
}

pub fn is_ready(story: &StorySnapshot, all_stories: &BTreeMap<String, StorySnapshot>) -> bool {
    if story.superstate != SuperState::Open {
        return false;
    }
    // `"blocked"` is one of the four `REQUIRED_STATES` (SH-125), pinned to
    // `SuperState::Open` in every project by construction, so this is a safe
    // check against a guaranteed reserved slug rather than a fragile string
    // match against project-configurable state names. Without it, a story
    // parked in `blocked` with no `awaiting` and no unmet `blocked-by` edge
    // reported ready — SH-126's council verdict.
    if story.state == "blocked" {
        return false;
    }
    if story.awaiting.is_some() {
        return false;
    }
    if story
        .relationships
        .iter()
        .any(|r| r.relation == "obviated-by")
    {
        return false;
    }
    for relation in &story.relationships {
        if relation.relation == "blocked-by"
            && let Some(other) = all_stories.get(&relation.other_id)
            && other.superstate == SuperState::Open
        {
            return false;
        }
    }
    true
}

/// The order ready work is offered in: `priority ASC, then story number ASC`.
///
/// A **total** order: the pair (priority, number) is unique within a project,
/// because [`story_number`] is. Two stories can never tie on both keys the way
/// the legacy `priority ASC, created_at ASC` comparator let them — `created_at`
/// has one-second precision, so two stories created by the same script, the
/// same decompose run, or the same agent turn shared it, and the stable sort
/// then answered from whatever order they happened to arrive in (SH-63).
///
/// `created_at` is not a fallback key here at all, not even after the number:
/// every write path stamps the story's number and its `created_at` together
/// (`StoryService::create`, `TransferService::import`, github-sync's
/// `create_story`), and `migrate`/`import-project` replay both in step, so
/// `created_at` is nondecreasing in story number in every state the system can
/// produce — keeping it as a second key would agree with this one everywhere
/// reachable and disagree nowhere, which is exactly what makes it dead weight.
///
/// Ends in the id string as a last-resort tiebreak, so the order stays total
/// even for two ids [`story_number`] cannot parse (a hand-imported document
/// predating the id grammar SH-117 introduced), where both sides would
/// otherwise tie at `u64::MAX`.
pub fn ready_order(a: &StorySnapshot, b: &StorySnapshot) -> std::cmp::Ordering {
    a.priority
        .cmp(&b.priority)
        .then_with(|| story_number(&a.id).cmp(&story_number(&b.id)))
        .then_with(|| a.id.cmp(&b.id))
}

/// The number half of a story id (`SH-10` → `10`), or [`u64::MAX`] for an id
/// that has none — so an unparseable id sorts last in [`ready_order`] rather
/// than first.
#[must_use]
pub fn story_number(id: &str) -> u64 {
    id.split('-')
        .nth(1)
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(u64::MAX)
}

pub fn would_create_parent_cycle(
    stories: &BTreeMap<String, StorySnapshot>,
    parent_id: &str,
    child_id: &str,
) -> bool {
    let graph = HierarchyGraph::from_stories(stories);
    graph.would_create_cycle(parent_id, child_id)
}

pub fn derive_family_relationships(
    stories: &BTreeMap<String, StorySnapshot>,
) -> BTreeMap<String, Vec<StoryRelation>> {
    let graph = HierarchyGraph::from_stories(stories);
    let cycle_nodes = graph.cycle_nodes();
    let mut derived = BTreeMap::new();

    for story_id in stories.keys() {
        if cycle_nodes.contains(story_id) {
            derived.insert(story_id.clone(), Vec::new());
            continue;
        }

        let direct_children = graph.children_of(story_id);
        let direct_parents = graph.parents_of(story_id);
        let descendants = graph.transitive_descendants(story_id, &cycle_nodes);
        let ancestors = graph.transitive_ancestors(story_id, &cycle_nodes);
        let mut relationships = Vec::new();

        for descendant in descendants.difference(&direct_children) {
            relationships.push(StoryRelation {
                relation: "ancestor-of".to_string(),
                other_id: descendant.clone(),
            });
        }

        for ancestor in ancestors.difference(&direct_parents) {
            relationships.push(StoryRelation {
                relation: "descendent-of".to_string(),
                other_id: ancestor.clone(),
            });
        }

        derived.insert(story_id.clone(), relationships);
    }

    derived
}

#[derive(Clone, Debug)]
struct HierarchyGraph {
    children_by_parent: BTreeMap<String, BTreeSet<String>>,
    parents_by_child: BTreeMap<String, BTreeSet<String>>,
}

impl HierarchyGraph {
    fn from_stories(stories: &BTreeMap<String, StorySnapshot>) -> Self {
        let mut children_by_parent = stories
            .keys()
            .cloned()
            .map(|story_id| (story_id, BTreeSet::new()))
            .collect::<BTreeMap<_, _>>();
        let mut parents_by_child = stories
            .keys()
            .cloned()
            .map(|story_id| (story_id, BTreeSet::new()))
            .collect::<BTreeMap<_, _>>();

        for story in stories.values() {
            for relation in &story.relationships {
                match relation.relation.as_str() {
                    "parent-of" if stories.contains_key(&relation.other_id) => {
                        children_by_parent
                            .entry(story.id.clone())
                            .or_default()
                            .insert(relation.other_id.clone());
                        parents_by_child
                            .entry(relation.other_id.clone())
                            .or_default()
                            .insert(story.id.clone());
                    }
                    "child-of" if stories.contains_key(&relation.other_id) => {
                        children_by_parent
                            .entry(relation.other_id.clone())
                            .or_default()
                            .insert(story.id.clone());
                        parents_by_child
                            .entry(story.id.clone())
                            .or_default()
                            .insert(relation.other_id.clone());
                    }
                    _ => {}
                }
            }
        }

        Self {
            children_by_parent,
            parents_by_child,
        }
    }

    fn children_of(&self, story_id: &str) -> BTreeSet<String> {
        self.children_by_parent
            .get(story_id)
            .cloned()
            .unwrap_or_default()
    }

    fn parents_of(&self, story_id: &str) -> BTreeSet<String> {
        self.parents_by_child
            .get(story_id)
            .cloned()
            .unwrap_or_default()
    }

    fn would_create_cycle(&self, parent_id: &str, child_id: &str) -> bool {
        if parent_id == child_id {
            return true;
        }

        let mut stack = vec![child_id.to_string()];
        let mut visited = BTreeSet::new();

        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if current == parent_id {
                return true;
            }
            if let Some(children) = self.children_by_parent.get(&current) {
                for child in children {
                    stack.push(child.clone());
                }
            }
        }

        false
    }

    fn cycle_nodes(&self) -> BTreeSet<String> {
        let mut visited = BTreeSet::new();
        let mut visiting = BTreeSet::new();
        let mut stack = Vec::new();
        let mut cycle_nodes = BTreeSet::new();

        for node in self.children_by_parent.keys() {
            self.visit_cycle_nodes(
                node,
                &mut visited,
                &mut visiting,
                &mut stack,
                &mut cycle_nodes,
            );
        }

        cycle_nodes
    }

    fn visit_cycle_nodes(
        &self,
        node: &str,
        visited: &mut BTreeSet<String>,
        visiting: &mut BTreeSet<String>,
        stack: &mut Vec<String>,
        cycle_nodes: &mut BTreeSet<String>,
    ) {
        if visited.contains(node) {
            return;
        }

        visited.insert(node.to_string());
        visiting.insert(node.to_string());
        stack.push(node.to_string());

        if let Some(children) = self.children_by_parent.get(node) {
            for child in children {
                if visiting.contains(child) {
                    for item in stack.iter() {
                        cycle_nodes.insert(item.clone());
                    }
                    cycle_nodes.insert(child.clone());
                    continue;
                }

                self.visit_cycle_nodes(child, visited, visiting, stack, cycle_nodes);
            }
        }

        visiting.remove(node);
        stack.pop();
    }

    fn transitive_descendants(
        &self,
        story_id: &str,
        cycle_nodes: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        self.walk_transitive(story_id, cycle_nodes, true)
    }

    fn transitive_ancestors(
        &self,
        story_id: &str,
        cycle_nodes: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        self.walk_transitive(story_id, cycle_nodes, false)
    }

    fn walk_transitive(
        &self,
        story_id: &str,
        cycle_nodes: &BTreeSet<String>,
        forward: bool,
    ) -> BTreeSet<String> {
        let mut visited = BTreeSet::new();
        let mut stack = if forward {
            self.children_of(story_id).into_iter().collect::<Vec<_>>()
        } else {
            self.parents_of(story_id).into_iter().collect::<Vec<_>>()
        };

        while let Some(current) = stack.pop() {
            if cycle_nodes.contains(&current) || !visited.insert(current.clone()) {
                continue;
            }

            let next = if forward {
                self.children_of(&current)
            } else {
                self.parents_of(&current)
            };
            for item in next {
                stack.push(item);
            }
        }

        visited
    }
}

pub fn parse_duration(input: &str) -> Option<chrono::Duration> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let (num_str, unit) = if let Some(s) = input.strip_suffix('h') {
        (s, 'h')
    } else if let Some(s) = input.strip_suffix('d') {
        (s, 'd')
    } else if let Some(s) = input.strip_suffix('m') {
        (s, 'm')
    } else {
        let s = input.strip_suffix('w')?;
        (s, 'w')
    };
    let num: i64 = num_str.parse().ok()?;
    match unit {
        'm' => chrono::Duration::try_minutes(num),
        'h' => chrono::Duration::try_hours(num),
        'd' => chrono::Duration::try_days(num),
        'w' => chrono::Duration::try_weeks(num),
        _ => None,
    }
}

/// Dependency graph for open stories using follows/starts-after/precedes relationships
#[derive(Clone, Debug)]
pub struct DependencyGraph {
    /// story_id -> set of story_ids that must complete before this one
    predecessors: BTreeMap<String, BTreeSet<String>>,
    /// story_id -> set of story_ids that depend on this one
    successors: BTreeMap<String, BTreeSet<String>>,
    /// All open story IDs in this graph
    nodes: BTreeSet<String>,
}

impl DependencyGraph {
    pub fn from_open_stories(stories: &BTreeMap<String, StorySnapshot>) -> Self {
        let open: BTreeMap<&String, &StorySnapshot> = stories
            .iter()
            .filter(|(_, s)| s.superstate == SuperState::Open)
            .collect();

        let mut predecessors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut successors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut nodes = BTreeSet::new();

        for id in open.keys() {
            nodes.insert((*id).clone());
            predecessors.entry((*id).clone()).or_default();
            successors.entry((*id).clone()).or_default();
        }

        for (id, story) in &open {
            for rel in &story.relationships {
                if !open.contains_key(&rel.other_id) {
                    continue;
                }
                match rel.relation.as_str() {
                    "blocked-by" => {
                        predecessors
                            .entry((*id).clone())
                            .or_default()
                            .insert(rel.other_id.clone());
                        successors
                            .entry(rel.other_id.clone())
                            .or_default()
                            .insert((*id).clone());
                    }
                    "blocks" => {
                        predecessors
                            .entry(rel.other_id.clone())
                            .or_default()
                            .insert((*id).clone());
                        successors
                            .entry((*id).clone())
                            .or_default()
                            .insert(rel.other_id.clone());
                    }
                    _ => {}
                }
            }
        }

        Self {
            predecessors,
            successors,
            nodes,
        }
    }

    /// Longest chain of dependent stories (by count)
    pub fn critical_path(&self) -> Vec<String> {
        let mut longest_from: BTreeMap<String, Vec<String>> = BTreeMap::new();

        // Topological approach: compute longest path from each node using memoization
        fn longest(
            node: &str,
            successors: &BTreeMap<String, BTreeSet<String>>,
            memo: &mut BTreeMap<String, Vec<String>>,
            visiting: &mut BTreeSet<String>,
        ) -> Vec<String> {
            if let Some(cached) = memo.get(node) {
                return cached.clone();
            }
            if !visiting.insert(node.to_string()) {
                // Cycle detected — break it
                return vec![node.to_string()];
            }
            let mut best = Vec::new();
            if let Some(succs) = successors.get(node) {
                for succ in succs {
                    let path = longest(succ, successors, memo, visiting);
                    if path.len() > best.len() {
                        best = path;
                    }
                }
            }
            visiting.remove(node);
            let mut result = vec![node.to_string()];
            result.extend(best);
            memo.insert(node.to_string(), result.clone());
            result
        }

        let mut visiting = BTreeSet::new();
        for node in &self.nodes {
            let path = longest(node, &self.successors, &mut longest_from, &mut visiting);
            longest_from.insert(node.clone(), path);
        }

        longest_from
            .into_values()
            .max_by_key(|p| p.len())
            .unwrap_or_default()
    }

    /// Transitive set of stories blocked (directly or indirectly) by the given story
    pub fn blocked_chain(&self, id: &str) -> BTreeSet<String> {
        let mut visited = BTreeSet::new();
        let mut stack = vec![id.to_string()];
        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(succs) = self.successors.get(&current) {
                for succ in succs {
                    stack.push(succ.clone());
                }
            }
        }
        visited.remove(id);
        visited
    }

    /// Independent clusters of stories with no inter-dependencies
    pub fn parallel_groups(&self) -> Vec<BTreeSet<String>> {
        let mut visited = BTreeSet::new();
        let mut groups = Vec::new();

        for node in &self.nodes {
            if visited.contains(node) {
                continue;
            }
            let mut group = BTreeSet::new();
            let mut stack = vec![node.clone()];
            while let Some(current) = stack.pop() {
                if !group.insert(current.clone()) {
                    continue;
                }
                if let Some(preds) = self.predecessors.get(&current) {
                    for pred in preds {
                        stack.push(pred.clone());
                    }
                }
                if let Some(succs) = self.successors.get(&current) {
                    for succ in succs {
                        stack.push(succ.clone());
                    }
                }
            }
            visited.extend(group.iter().cloned());
            groups.push(group);
        }

        groups
    }
}

/// Why a commit named a story (SH-124).
///
/// **The ordering is the merge rule.** One commit may name a story twice — once
/// in its subject and again in a trailer — and the strongest intent wins, which
/// [`scan_story_refs`] expresses as `max` over this type. Do not reorder the
/// variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReferenceIntent {
    /// The commit named the story and asserted nothing about working on it.
    /// `Refs SH-1`, `see SH-1`, or a bare `SH-1` in prose.
    Mention,
    /// The commit claimed the story: a claim word sits immediately before the
    /// id. `Closes SH-1`, `implements SH-1`.
    Claim,
}

/// One story named by one commit, carrying the intent that naming had.
///
/// # Why this is not a `String` (SH-124)
///
/// The predecessor returned `Vec<String>`, which cannot say "named, but nobody
/// claimed work". `commit_sync` therefore had no way to tell a cross-reference
/// from a claim of ownership, and moved every mentioned story into the active
/// state — silently removing it from `story next` and `story list --ready`,
/// which both exclude in-progress. The defect was expressible only because the
/// return type threw the distinction away; carrying it here makes the wrong
/// behaviour unwritable rather than merely unwritten.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoryReference {
    /// The id exactly as the commit wrote it.
    pub id: String,
    /// Why the commit named it.
    pub intent: ReferenceIntent,
}

impl StoryReference {
    /// Whether this reference claims the story, and so may move it.
    #[must_use]
    pub fn claims(&self) -> bool {
        self.intent == ReferenceIntent::Claim
    }
}

/// A word that may precede a story id, and whether it claims the story.
struct RefWord {
    word: &'static str,
    claims: bool,
}

/// Every word this grammar knows, and what it means.
///
/// **The table is the documentation and the test fixture at once.** The
/// `claims: false` rows are never consulted by the matcher — a bare id is a
/// mention already — they exist so `every_word_in_the_table_behaves_as_the_table_says`
/// pins them in the *negative* direction. A word moved between the two groups
/// changes behaviour and fails that test immediately, rather than drifting.
///
/// `refs` heads the non-claiming rows because `Refs CAL-21, CAL-28, CAL-29` is
/// the trailer that caused SH-124. `story` is there because this repository
/// writes `Story: SH-107` as a real per-commit trailer, and it must never
/// become a claim word through a careless edit.
static REF_WORDS: &[RefWord] = &[
    RefWord {
        word: "close",
        claims: true,
    },
    RefWord {
        word: "closes",
        claims: true,
    },
    RefWord {
        word: "closed",
        claims: true,
    },
    RefWord {
        word: "fix",
        claims: true,
    },
    RefWord {
        word: "fixes",
        claims: true,
    },
    RefWord {
        word: "fixed",
        claims: true,
    },
    RefWord {
        word: "resolve",
        claims: true,
    },
    RefWord {
        word: "resolves",
        claims: true,
    },
    RefWord {
        word: "resolved",
        claims: true,
    },
    RefWord {
        word: "implement",
        claims: true,
    },
    RefWord {
        word: "implements",
        claims: true,
    },
    RefWord {
        word: "implemented",
        claims: true,
    },
    RefWord {
        word: "implementing",
        claims: true,
    },
    RefWord {
        word: "complete",
        claims: true,
    },
    RefWord {
        word: "completes",
        claims: true,
    },
    RefWord {
        word: "completed",
        claims: true,
    },
    RefWord {
        word: "start",
        claims: true,
    },
    RefWord {
        word: "starts",
        claims: true,
    },
    RefWord {
        word: "started",
        claims: true,
    },
    RefWord {
        word: "starting",
        claims: true,
    },
    RefWord {
        word: "wip",
        claims: true,
    },
    RefWord {
        word: "refs",
        claims: false,
    },
    RefWord {
        word: "ref",
        claims: false,
    },
    RefWord {
        word: "references",
        claims: false,
    },
    RefWord {
        word: "see",
        claims: false,
    },
    RefWord {
        word: "related",
        claims: false,
    },
    RefWord {
        word: "part",
        claims: false,
    },
    RefWord {
        word: "mentions",
        claims: false,
    },
    RefWord {
        word: "cc",
        claims: false,
    },
    RefWord {
        word: "re",
        claims: false,
    },
    RefWord {
        word: "tracks",
        claims: false,
    },
    RefWord {
        word: "blocks",
        claims: false,
    },
    RefWord {
        word: "story",
        claims: false,
    },
];

/// Words that cancel a claim when they sit immediately before the claim word.
///
/// **Frozen at five, and frozen means frozen.** This is not a negation parser;
/// natural language has no finite hand-rollable grammar and this does not
/// pretend otherwise. It is a one-way valve: it can only demote a `Claim` to a
/// `Mention`, never manufacture one, so its incompleteness buys *under*-claiming
/// — and under-claiming is the visible failure, because `commit-sync`'s report
/// names what it declined to act on, while over-claiming is the silent
/// accumulating defect SH-124 exists to remove.
///
/// Growing this list is a design change, not a bug fix.
/// `the_negation_list_is_frozen_at_five` fails if it grows.
static NEGATIONS: [&str; 5] = ["not", "no", "never", "without", "unless"];

/// The subject `git revert` writes, verbatim.
///
/// An exact match on git's own generated format rather than an inference about
/// English, which is why this earns a place where a general natural-language
/// rule would not.
const REVERT_SUBJECT_PREFIX: &str = "Revert \"";

/// Whether `word` claims a story it precedes.
fn claim_word(word: &str) -> bool {
    let folded = word.to_ascii_lowercase();
    REF_WORDS
        .iter()
        .find(|entry| entry.word == folded)
        .is_some_and(|entry| entry.claims)
}

/// Whether `word` cancels a claim it precedes.
fn negates(word: &str) -> bool {
    let folded = word.to_ascii_lowercase();
    NEGATIONS.contains(&folded.as_str()) || folded.ends_with("n't")
}

/// Every `{PREFIX}-{DIGITS}` in one line, as `(start, end)` byte offsets.
///
/// Word boundaries as they have always been: the prefix must start the string
/// or follow a non-alphanumeric byte, and the digits stop at the first
/// non-digit. `PUSH-123` is not `SH-123`, and `SH-1SH-2` is one id.
fn ids_in_line(prefix: &str, line: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let prefix_bytes = prefix.as_bytes();
    let bytes = line.as_bytes();
    let prefix_len = prefix_bytes.len();

    let mut i = 0;
    while i + prefix_len < bytes.len() {
        if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        if &bytes[i..i + prefix_len] != prefix_bytes {
            i += 1;
            continue;
        }
        let dash = i + prefix_len;
        if dash >= bytes.len() || bytes[dash] != b'-' {
            i += 1;
            continue;
        }
        let digits_start = dash + 1;
        let mut digits_end = digits_start;
        while digits_end < bytes.len() && bytes[digits_end].is_ascii_digit() {
            digits_end += 1;
        }
        if digits_end == digits_start {
            i += 1;
            continue;
        }
        found.push((i, digits_end));
        i = digits_end;
    }
    found
}

/// Whether the text between two ids joins them into one run.
///
/// A run is ids separated by nothing but whitespace, `,`, `&` and the word
/// `and`, so `Closes SH-1, SH-2 and SH-3` claims three. The first token that is
/// none of those ends the run, which is what leaves `see SH-2` a mention in
/// `Closes SH-1, see SH-2`.
fn joins_a_run(gap: &str) -> bool {
    let stripped: String = gap
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ',' && *c != '&')
        .collect();
    stripped.is_empty() || stripped.eq_ignore_ascii_case("and")
}

/// Which separator sits between a claim word and the id it precedes.
enum Separator {
    /// Whitespace alone. Claims unconditionally.
    Whitespace,
    /// A colon, then whitespace. Claims only when the id run is the whole
    /// remainder of the line — see [`intent_for_run`].
    Colon,
}

/// The claim word immediately before `id_start`, where it starts, and how it is
/// separated from the id.
///
/// `None` when nothing there can claim: no whitespace before the id (which is
/// what makes `fix:SH-1` a mention), or no word before the whitespace.
fn preceding_word(line: &str, id_start: usize) -> Option<(&str, usize, Separator)> {
    let bytes = line.as_bytes();
    let mut at = id_start;

    // An optional single `#`, so `Fixes #SH-1` reads like GitHub.
    if at > 0 && bytes[at - 1] == b'#' {
        at -= 1;
    }

    // Whitespace is mandatory. Its absence is the whole reason `fix:SH-1`
    // cannot claim: a colon with nothing after it is not a trailer.
    let whitespace_end = at;
    while at > 0 && (bytes[at - 1] == b' ' || bytes[at - 1] == b'\t') {
        at -= 1;
    }
    if at == whitespace_end {
        return None;
    }

    let separator = if at > 0 && bytes[at - 1] == b':' {
        at -= 1;
        Separator::Colon
    } else {
        Separator::Whitespace
    };

    let word_end = at;
    while at > 0 && bytes[at - 1].is_ascii_alphanumeric() {
        at -= 1;
    }
    (at < word_end).then(|| (&line[at..word_end], at, separator))
}

/// The word before `word_start`, for the negation lookback.
fn word_before(line: &str, word_start: usize) -> &str {
    let bytes = line.as_bytes();
    let mut at = word_start;
    while at > 0 && (bytes[at - 1] == b' ' || bytes[at - 1] == b'\t') {
        at -= 1;
    }
    let end = at;
    while at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'\'') {
        at -= 1;
    }
    &line[at..end]
}

/// The intent of one run of ids on one line.
///
/// Gates, in order: the claim word, the negation lookback, and — for a colon
/// separator only — the whole-remainder rule. The revert ceiling is applied by
/// the caller, which is the only place that knows the line number.
fn intent_for_run(line: &str, run_start: usize, run_end: usize) -> ReferenceIntent {
    let Some((word, word_start, separator)) = preceding_word(line, run_start) else {
        return ReferenceIntent::Mention;
    };
    if !claim_word(word) {
        return ReferenceIntent::Mention;
    }
    if negates(word_before(line, word_start)) {
        return ReferenceIntent::Mention;
    }

    if let Separator::Colon = separator {
        // `git-interpret-trailers` defines a trailer as `token: value` where the
        // value runs to end of line. Honouring the colon only in that shape is
        // what accepts `Closes: SH-1` while rejecting `fix: SH-12 broken
        // parser`, where the colon is a Conventional Commits *type* rather than
        // a trailer key.
        let rest = line[run_end..].trim();
        if !(rest.is_empty() || rest == "." || rest == ";") {
            return ReferenceIntent::Mention;
        }
    }

    ReferenceIntent::Claim
}

/// Every story a commit message names, and why it named each one (SH-124).
///
/// Ids are unique and keep first-appearance order. One id named twice keeps the
/// strongest intent, merged with `max` over [`ReferenceIntent`], so the caller
/// never sees two entries for one story.
///
/// # The grammar
///
/// An id **claims** its story when, on the same physical line, a claim word
/// immediately precedes it via one of two separators:
///
/// - **whitespace** — `Closes SH-1`. Claims unconditionally, anywhere on the
///   line. Position is deliberately unanchored: `fixed SH-20`, `completed
///   SH-27` and `start SH-41` all appear mid-prose as genuine claims in this
///   repository's own history, and anchoring to line-start would repeat SH-58's
///   mistake at a different position.
/// - **a colon, then whitespace** — `Closes: SH-1`, and only when the id run is
///   the whole remainder of the line.
///
/// Two gates may only *demote* a claim, never create one: a `Revert "…"` subject
/// claims nothing on its first line, and [`NEGATIONS`] before the claim word
/// cancels it.
///
/// Everything else — `Refs SH-1`, `see SH-1`, a bare `SH-1` — is a
/// [`ReferenceIntent::Mention`], which still links.
pub fn scan_story_refs(prefix: &str, message: &str) -> Vec<StoryReference> {
    let mut order: Vec<String> = Vec::new();
    let mut intents: BTreeMap<String, ReferenceIntent> = BTreeMap::new();

    // `git revert` copies only the original subject into line 1, so line 1 is
    // the whole hazard surface. Suppressing the entire message instead would
    // discard a reverter's own hand-written `Closes SH-9 (tracking the revert)`.
    let reverted_subject = message.starts_with(REVERT_SUBJECT_PREFIX);

    for (number, line) in message.lines().enumerate() {
        let occurrences = ids_in_line(prefix, line);
        let mut index = 0;
        while index < occurrences.len() {
            // Extend the run while consecutive ids are joined by nothing but
            // whitespace, `,`, `&` or `and`.
            let mut last = index;
            while last + 1 < occurrences.len()
                && joins_a_run(&line[occurrences[last].1..occurrences[last + 1].0])
            {
                last += 1;
            }

            let intent = if reverted_subject && number == 0 {
                ReferenceIntent::Mention
            } else {
                intent_for_run(line, occurrences[index].0, occurrences[last].1)
            };

            for (start, end) in &occurrences[index..=last] {
                let id = line[*start..*end].to_string();
                match intents.get_mut(&id) {
                    Some(existing) => *existing = (*existing).max(intent),
                    None => {
                        order.push(id.clone());
                        intents.insert(id, intent);
                    }
                }
            }
            index = last + 1;
        }
    }

    order
        .into_iter()
        .map(|id| StoryReference {
            intent: intents[&id],
            id,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        FieldEdit, Priority, REQUIRED_STATES, STATE_ROLE_ACTIVE, StateChanges, StateDef,
        StateUsage, StoryEvent, StoryRelation, StorySnapshot, SuperState, active_state,
        compute_epic_display_state, compute_progress, derive_family_relationships, fold_story,
        has_children, is_ready, last_activity_type, normalize_labels, ready_order, story_number,
        validate_event_for_append, validate_required_states, validate_state_defs,
        validate_state_defs_for_write, validate_state_slug, validate_type_slug,
        with_required_states, would_create_parent_cycle,
    };

    #[test]
    fn requires_open_and_closed_states() {
        let states = vec![StateDef {
            slug: "todo".to_string(),
            super_state: SuperState::Open,
            role: None,
            description: None,
        }];
        let error = validate_state_defs(&states).unwrap_err();
        assert!(error.to_string().contains("OPEN"));
    }

    // --- state slug / write-path validation ---

    fn state(slug: &str, super_state: SuperState, role: Option<&str>) -> StateDef {
        StateDef {
            slug: slug.to_string(),
            super_state,
            role: role.map(str::to_string),
            description: None,
        }
    }

    #[test]
    fn validate_state_slug_accepts_dash_separated_lowercase() {
        for good in ["todo", "in-progress", "wave-2", "x"] {
            validate_state_slug(good).unwrap_or_else(|e| panic!("`{good}` rejected: {e}"));
        }
    }

    #[test]
    fn validate_state_slug_rejects_unaddressable_slugs() {
        // Each of these breaks either the CLI (`story move SH-1 <slug>`) or
        // the web router, which splits paths on `/`.
        for bad in [
            "",
            "In Review",
            "in review",
            "in/review",
            "in_review",
            "-todo",
            "todo-",
            "in--review",
            "tödo",
        ] {
            let error = validate_state_slug(bad).unwrap_err();
            assert!(
                error.to_string().contains("invalid state slug"),
                "`{bad}` should have been rejected"
            );
        }
    }

    #[test]
    fn validate_type_slug_accepts_dash_separated_lowercase() {
        for good in ["bug", "feature-request", "wave-2", "x"] {
            validate_type_slug(good).unwrap_or_else(|e| panic!("`{good}` rejected: {e}"));
        }
    }

    /// The eight shapes SH-134 measured `story type add` accepting, plus the
    /// underscore its state-slug counterpart also refuses.
    ///
    /// Each breaks either the CLI (`story new t --type <slug>`) or a URL path
    /// segment, for the same reason a state slug of the same shape does. The
    /// empty string is listed first because it is the one that got past every
    /// other check: nothing to type, and unremovable once a story carries it.
    #[test]
    fn validate_type_slug_rejects_unaddressable_slugs() {
        for bad in [
            "",
            "in review",
            "a b",
            "Bug",
            "spike/two",
            "in_review",
            "-lead",
            "trailing-",
            "double--dash",
            "café",
            "--typo",
        ] {
            let error = validate_type_slug(bad).unwrap_err();
            assert!(
                error.to_string().contains("invalid type slug"),
                "`{bad}` should have been rejected"
            );
        }
    }

    /// The empty slug earns its own sentence rather than being reported as a
    /// bad character, because it contains no character to name.
    #[test]
    fn an_empty_type_slug_says_so_rather_than_naming_a_character() {
        let error = validate_type_slug("").unwrap_err().to_string();
        assert!(error.contains("it is empty"), "{error}");
    }

    #[test]
    fn normalize_labels_splits_trims_dedups_and_sorts() {
        assert_eq!(
            normalize_labels(["web,sse", " backend ,api", "api"]),
            vec!["api", "backend", "sse", "web"]
        );
    }

    #[test]
    fn normalize_labels_drops_empties() {
        assert_eq!(normalize_labels([",", " , ", "", "solo"]), vec!["solo"]);
        assert!(normalize_labels(Vec::<&str>::new()).is_empty());
    }

    #[test]
    fn normalize_labels_is_idempotent() {
        let once = normalize_labels(["web,sse", "backend"]);
        let twice = normalize_labels(once.clone());
        assert_eq!(once, twice);
    }

    #[test]
    fn validate_event_for_append_rejects_a_comma_in_a_label() {
        let event = StoryEvent::StoryLabelsSet {
            at: "2026-01-01T00:00:00Z".to_string(),
            labels: vec!["web,sse".to_string()],
        };
        let error = validate_event_for_append(&event).unwrap_err();
        assert!(error.to_string().contains("comma"));
    }

    #[test]
    fn validate_event_for_append_rejects_blank_and_untrimmed_labels() {
        for bad in [" web", "web ", "", " "] {
            let event = StoryEvent::StoryLabelsSet {
                at: "2026-01-01T00:00:00Z".to_string(),
                labels: vec![bad.to_string()],
            };
            assert!(
                validate_event_for_append(&event).is_err(),
                "`{bad}` should have been rejected"
            );
        }
    }

    #[test]
    fn validate_event_for_append_accepts_a_normalized_label_set() {
        let event = StoryEvent::StoryLabelsSet {
            at: "2026-01-01T00:00:00Z".to_string(),
            labels: normalize_labels(["web,sse", "backend"]),
        };
        validate_event_for_append(&event).unwrap();
    }

    #[test]
    fn validate_event_for_append_ignores_non_label_events() {
        let event = StoryEvent::StoryTitleSet {
            at: "2026-01-01T00:00:00Z".to_string(),
            title: "has, a comma".to_string(),
        };
        validate_event_for_append(&event).unwrap();
    }

    #[test]
    fn write_validation_rejects_duplicate_slugs() {
        let states = vec![
            state("todo", SuperState::Open, None),
            state("todo", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];
        let error = validate_state_defs_for_write(&states).unwrap_err();
        assert!(error.to_string().contains("defined more than once"));
    }

    #[test]
    fn write_validation_rejects_more_than_one_active_role() {
        let states = vec![
            state("todo", SuperState::Open, Some("active")),
            state("doing", SuperState::Open, Some("active")),
            state("done", SuperState::Closed, None),
        ];
        let error = validate_state_defs_for_write(&states).unwrap_err();
        assert!(error.to_string().contains("only one state may have role"));
    }

    #[test]
    fn write_validation_rejects_unknown_roles() {
        let states = vec![
            state("todo", SuperState::Open, Some("triage")),
            state("done", SuperState::Closed, None),
        ];
        let error = validate_state_defs_for_write(&states).unwrap_err();
        assert!(error.to_string().contains("unknown role `triage`"));
    }

    /// The read path stays permissive on purpose: tightening it would make a
    /// project carrying a legacy slug unloadable rather than uneditable.
    #[test]
    fn read_validation_tolerates_what_the_write_path_rejects() {
        let states = vec![
            state("In Review", SuperState::Open, Some("triage")),
            state("done", SuperState::Closed, None),
        ];
        validate_state_defs(&states).unwrap();
        assert!(validate_state_defs_for_write(&states).is_err());
    }

    // --- the required-state floor (SH-125) ---

    /// Slugs in board order, with `*` marking the `active` role — the shape
    /// every repair assertion below is written against.
    fn board(states: &[StateDef]) -> String {
        states
            .iter()
            .map(|state| {
                format!(
                    "{}{}",
                    state.slug,
                    if state.role.as_deref() == Some(STATE_ROLE_ACTIVE) {
                        "*"
                    } else {
                        ""
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("|")
    }

    #[test]
    fn the_required_floor_is_todo_in_progress_blocked_and_done() {
        let slugs: Vec<&str> = REQUIRED_STATES.iter().map(|r| r.slug).collect();
        assert_eq!(slugs, ["todo", "in-progress", "blocked", "done"]);
        // Every one of them is a slug the CLI and the web router can address.
        for required in &REQUIRED_STATES {
            validate_state_slug(required.slug).expect("a required slug must be addressable");
        }
    }

    #[test]
    fn a_conforming_set_satisfies_the_floor() {
        let states = vec![
            state("todo", SuperState::Open, None),
            state("in-progress", SuperState::Open, Some("active")),
            state("blocked", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];
        validate_required_states(&states).unwrap();
    }

    #[test]
    fn a_missing_required_state_is_named_alongside_the_repair() {
        let states = vec![
            state("todo", SuperState::Open, None),
            state("in-progress", SuperState::Open, Some("active")),
            state("done", SuperState::Closed, None),
        ];
        let error = validate_required_states(&states).unwrap_err().to_string();
        assert!(error.contains("blocked"), "{error}");
        assert!(
            error.contains("story doctor --fix"),
            "the refusal must name the way out: {error}"
        );
    }

    /// A project cannot escape the floor by keeping the slug and changing what
    /// it means.
    #[test]
    fn a_required_state_under_the_wrong_superstate_is_refused() {
        let states = vec![
            state("todo", SuperState::Open, None),
            state("in-progress", SuperState::Open, None),
            state("blocked", SuperState::Open, None),
            state("done", SuperState::Open, None),
            state("shipped", SuperState::Closed, None),
        ];
        let error = validate_required_states(&states).unwrap_err().to_string();
        assert!(error.contains("`done` must be CLOSED"), "{error}");
    }

    #[test]
    fn repairing_adds_only_what_is_missing_and_keeps_the_rest_in_place() {
        let states = vec![
            state("todo", SuperState::Open, None),
            state("in-progress", SuperState::Open, None),
            state("review", SuperState::Open, Some("active")),
            state("done", SuperState::Closed, None),
            state("wont-fix", SuperState::Closed, None),
        ];
        let repaired = with_required_states(&states).unwrap();
        assert_eq!(
            board(&repaired),
            "todo|in-progress|review*|blocked|done|wont-fix"
        );
        validate_required_states(&repaired).unwrap();
    }

    /// The `agentics` case from the live store: two states, so the repair adds
    /// two — and they arrive in the floor's own order rather than reversed.
    #[test]
    fn repairing_a_two_state_project_adds_both_missing_states_in_order() {
        let states = vec![
            state("todo", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];
        let repaired = with_required_states(&states).unwrap();
        assert_eq!(board(&repaired), "todo|in-progress|blocked|done");
        validate_required_states(&repaired).unwrap();
    }

    /// The repair must never take position 0. That slot decides where
    /// `story new` puts a story, so a repair that landed there would change a
    /// project's behaviour while claiming only to add a state.
    #[test]
    fn repairing_never_displaces_the_state_new_stories_open_in() {
        for states in [
            vec![
                state("backlog", SuperState::Open, None),
                state("done", SuperState::Closed, None),
            ],
            // A catalog whose first state is CLOSED — legal, since
            // `reorder_states` accepts any permutation.
            vec![
                state("done", SuperState::Closed, None),
                state("backlog", SuperState::Open, None),
            ],
        ] {
            let first_open = states
                .iter()
                .find(|state| state.super_state == SuperState::Open)
                .expect("a fixture with an OPEN state")
                .slug
                .clone();
            let repaired = with_required_states(&states).unwrap();
            assert_eq!(
                repaired
                    .iter()
                    .find(|state| state.super_state == SuperState::Open)
                    .expect("still has an OPEN state")
                    .slug,
                first_open,
                "the first OPEN state moved: {}",
                board(&repaired)
            );
        }
    }

    /// Idempotency is what makes the repair safe on a path that runs on every
    /// import, not merely tidy.
    #[test]
    fn repairing_twice_changes_nothing_the_second_time() {
        let states = vec![
            state("todo", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];
        let once = with_required_states(&states).unwrap();
        let twice = with_required_states(&once).unwrap();
        assert_eq!(once, twice);
    }

    /// Repair may add; it may not reinterpret. Flipping `done` to CLOSED here
    /// would reclassify every story sitting in it — the reclassification
    /// `update_state` refuses to perform without a migration destination.
    #[test]
    fn repairing_refuses_a_required_slug_under_the_wrong_superstate() {
        let states = vec![
            state("todo", SuperState::Open, None),
            state("in-progress", SuperState::Open, None),
            state("blocked", SuperState::Open, None),
            state("done", SuperState::Open, None),
            state("shipped", SuperState::Closed, None),
        ];
        let error = with_required_states(&states).unwrap_err().to_string();
        assert!(error.contains("will not reclassify"), "{error}");
    }

    /// The repaired set has to survive the *other* validator, or a repair would
    /// simply move the refusal one layer along.
    #[test]
    fn a_repaired_set_still_passes_the_write_path_validator() {
        let states = vec![
            state("todo", SuperState::Open, None),
            state("review", SuperState::Open, Some("active")),
            state("done", SuperState::Closed, None),
        ];
        let repaired = with_required_states(&states).unwrap();
        validate_state_defs_for_write(&repaired).unwrap();
        validate_state_defs(&repaired).unwrap();
    }

    // --- FieldEdit ---

    #[test]
    fn field_edit_keep_clear_and_set() {
        let current = || Some("before".to_string());
        assert_eq!(FieldEdit::Keep.apply(current()), current());
        assert_eq!(FieldEdit::Clear.apply(current()), None);
        assert_eq!(
            FieldEdit::Set("after".to_string()).apply(current()),
            Some("after".to_string())
        );
        assert_eq!(
            FieldEdit::Set("after".to_string()).apply(None),
            Some("after".to_string())
        );
    }

    #[test]
    fn state_changes_default_is_a_no_op() {
        assert!(StateChanges::default().is_empty());
        assert!(
            !StateChanges {
                description: FieldEdit::Clear,
                ..StateChanges::default()
            }
            .is_empty(),
            "clearing a field is a change, not a no-op"
        );
    }

    #[test]
    fn state_usage_totals_both_stores() {
        assert_eq!(
            StateUsage {
                open: 2,
                archived: 3
            }
            .total(),
            5
        );
    }

    #[test]
    fn fold_story_tracks_awaiting_events() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Awaiting".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryAwaitingSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    awaiting: "blocked on API".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.awaiting.as_deref(), Some("blocked on API"));
    }

    #[test]
    fn fold_story_clears_awaiting() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Awaiting".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryAwaitingSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    awaiting: "blocked on API".to_string(),
                },
                StoryEvent::StoryAwaitingCleared {
                    at: "2026-03-13T00:02:00Z".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.awaiting, None);
    }

    #[test]
    fn fold_story_closed_snapshot_has_awaiting_cleared() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Awaiting".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryAwaitingSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    awaiting: "blocked on API".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryAwaitingCleared {
                    at: "2026-03-13T00:02:01Z".to_string(),
                },
                StoryEvent::StoryClosedAndArchived {
                    at: "2026-03-13T00:02:02Z".to_string(),
                    state: "done".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.awaiting, None);
        assert_eq!(story.closed_at.as_deref(), Some("2026-03-13T00:02:02Z"));
    }

    #[test]
    fn fold_story_deleted_comes_to_rest_in_a_closed_state() {
        // Regression test for #18 and then for SH-130.
        //
        // #18 fixed half of this: deleting a story used to leave `state` *and*
        // `superstate` alone, so a story deleted while `todo` stayed OPEN. The
        // fix forced `superstate` to CLOSED and deliberately preserved the
        // slug, which this test then pinned as correct — "a truthful record of
        // what the story was when it was deleted".
        //
        // That is the SH-130 defect. The two columns are stored independently,
        // so preserving one while forcing the other is precisely how a CLOSED
        // story comes to sit in an OPEN state: SH-20 read `todo (CLOSED,
        // deleted)` and `story list --state todo` returned it. The premise is
        // rewritten rather than relaxed — the story now comes to rest in a
        // state whose superstate genuinely is CLOSED, and the record of where
        // it was lives in its event log, which is the thing that cannot lie.
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Deleted while open".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryCommentAdded {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    text: "[deleted] created in error".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    reason: "created in error".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.superstate, SuperState::Closed);
        assert!(story.deleted);
        assert_eq!(story.deleted_reason.as_deref(), Some("created in error"));
        // The slug and the superstate agree, which is the whole invariant.
        assert_eq!(story.state, "done");
        assert_eq!(story.closed_at.as_deref(), Some("2026-03-13T00:01:00Z"));
    }

    #[test]
    fn fold_story_deleted_rests_in_the_required_closed_state_not_merely_a_closed_one() {
        // `done` is chosen by name, not by position, and not by "the first
        // CLOSED state we find". The fold is handed a slug-keyed BTreeMap that
        // iterates alphabetically, so a rule of "first CLOSED state" would pick
        // `abandoned` here — and would pick differently again for a project
        // whose catalog merely differs in spelling. SH-125 guarantees `done`
        // exists and is CLOSED in every project, so naming it needs no ordering
        // information the fold does not have.
        let states: BTreeMap<String, StateDef> = vec![
            StateDef {
                slug: "todo".to_string(),
                super_state: SuperState::Open,
                role: None,
                description: None,
            },
            StateDef {
                slug: "abandoned".to_string(),
                super_state: SuperState::Closed,
                role: None,
                description: None,
            },
            StateDef {
                slug: "done".to_string(),
                super_state: SuperState::Closed,
                role: None,
                description: None,
            },
        ]
        .into_iter()
        .map(|state| (state.slug.clone(), state))
        .collect();

        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Deleted while open".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    reason: "created in error".to_string(),
                },
            ],
            &states,
        )
        .unwrap();

        assert_eq!(story.state, "done");
    }

    #[test]
    fn fold_story_deleted_in_a_catalog_without_done_still_rests_somewhere_closed() {
        // Defensive, not expected: `service::state_set` gives every catalog
        // reaching the store a `done`. A catalog that somehow lacks one still
        // has to fold to a legal pair, or the schema refuses the write and the
        // story becomes unreadable.
        let states: BTreeMap<String, StateDef> = vec![
            StateDef {
                slug: "todo".to_string(),
                super_state: SuperState::Open,
                role: None,
                description: None,
            },
            StateDef {
                slug: "shipped".to_string(),
                super_state: SuperState::Closed,
                role: None,
                description: None,
            },
        ]
        .into_iter()
        .map(|state| (state.slug.clone(), state))
        .collect();

        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Deleted while open".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    reason: "created in error".to_string(),
                },
            ],
            &states,
        )
        .unwrap();

        assert_eq!(story.state, "shipped");
        assert_eq!(story.superstate, SuperState::Closed);
    }

    #[test]
    fn fold_story_deleted_with_no_closed_state_at_all_stays_foldable() {
        // The last fallback. There is nowhere legal to come to rest, so the
        // slug is left alone and the fold still succeeds — a story that cannot
        // be folded is a story that cannot be read, repaired or exported, which
        // is strictly worse than one the schema will refuse to write.
        let states: BTreeMap<String, StateDef> = vec![StateDef {
            slug: "todo".to_string(),
            super_state: SuperState::Open,
            role: None,
            description: None,
        }]
        .into_iter()
        .map(|state| (state.slug.clone(), state))
        .collect();

        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Deleted while open".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    reason: "created in error".to_string(),
                },
            ],
            &states,
        )
        .expect("a story with nowhere to rest must still fold");

        assert_eq!(story.state, "todo");
        assert_eq!(story.superstate, SuperState::Closed);
    }

    #[test]
    fn fold_story_deleted_while_closed_keeps_original_closed_at() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Deleted after closing".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryClosedAndArchived {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    reason: "cleaning up archive".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.superstate, SuperState::Closed);
        assert!(story.deleted);
        // `closed_at` reflects the original close, not the later deletion —
        // `fold_story` only backfills `closed_at` when it was never set.
        assert_eq!(story.closed_at.as_deref(), Some("2026-03-13T00:01:00Z"));
    }

    #[test]
    fn fold_story_reopens_a_closed_story_by_appending_a_move_to_an_open_state() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Closed then reopened".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryClosedAndArchived {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    state: "todo".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.superstate, SuperState::Open);
        assert_eq!(story.state, "todo");
        assert_eq!(story.closed_at, None);
    }

    #[test]
    fn fold_story_undeletes_when_moved_back_into_an_open_state() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Deleted then undeleted".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryCommentAdded {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    text: "[deleted] created in error".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    reason: "created in error".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    state: "todo".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert!(!story.deleted);
        assert_eq!(story.deleted_reason, None);
        assert_eq!(story.closed_at, None);
        assert_eq!(story.superstate, SuperState::Open);
        // The audit trail survives the undelete, exactly as it does when
        // `unarchive_story` rewrites the log: the marker events go, the
        // `[deleted]` comment stays.
        assert_eq!(story.comments.len(), 1);
    }

    #[test]
    fn fold_story_hidden_sets_hidden_at() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Closed then archived".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryClosedAndArchived {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryHidden {
                    at: "2026-03-13T00:02:00Z".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.hidden_at.as_deref(), Some("2026-03-13T00:02:00Z"));
        assert_eq!(story.superstate, SuperState::Closed);
    }

    #[test]
    fn fold_story_unhidden_clears_hidden_at() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Archived then unarchived".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryClosedAndArchived {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryHidden {
                    at: "2026-03-13T00:02:00Z".to_string(),
                },
                StoryEvent::StoryUnhidden {
                    at: "2026-03-13T00:03:00Z".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.hidden_at, None);
    }

    #[test]
    fn fold_story_reopening_clears_hidden_at() {
        // Symmetric with `fold_story_reopens_a_closed_story_by_appending_a_move_to_an_open_state`:
        // reopening a hidden story must not leave it hidden-but-open, the
        // illegal tuple SH-130 fixed for `archived`/`closed_at`/`superstate`.
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Archived then reopened".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryClosedAndArchived {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryHidden {
                    at: "2026-03-13T00:02:00Z".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:03:00Z".to_string(),
                    state: "todo".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.superstate, SuperState::Open);
        assert_eq!(story.hidden_at, None);
    }

    #[test]
    fn fold_story_reclassifying_the_resting_state_open_clears_hidden_at() {
        // The generalized SH-130 case: no new event reopens the story, but the
        // *catalog* passed to the fold reclassifies `done` as OPEN — exactly
        // what a live reclassification does before re-folding every occupant.
        // A `StoryHidden` appended while `done` was still CLOSED must not
        // survive that refold once the story's resting state reads OPEN.
        let mut states = state_map();
        states.get_mut("done").unwrap().super_state = SuperState::Open;

        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "done reclassified OPEN after being archived".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryClosedAndArchived {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryHidden {
                    at: "2026-03-13T00:02:00Z".to_string(),
                },
            ],
            &states,
        )
        .unwrap();

        assert_eq!(story.superstate, SuperState::Open);
        assert_eq!(
            story.hidden_at, None,
            "a story cannot read hidden while its superstate reads OPEN"
        );
    }

    #[test]
    fn fold_story_move_into_a_closed_state_does_not_reopen() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Closed twice".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryClosedAndArchived {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    state: "done".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.superstate, SuperState::Closed);
        assert_eq!(story.closed_at.as_deref(), Some("2026-03-13T00:01:00Z"));
    }

    #[test]
    fn fold_story_move_into_an_undefined_state_leaves_deletion_alone() {
        // A deleted story folds even when its state slug is no longer
        // configured, because `deleted` forces the superstate. Retracting the
        // deletion on an unrecognised slug would turn that into a hard fold
        // failure, so the rule requires a state the project defines *and*
        // calls open.
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Deleted, then moved somewhere unknown".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    reason: "created in error".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    state: "retired".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert!(story.deleted);
        assert_eq!(story.superstate, SuperState::Closed);
    }

    #[test]
    fn is_ready_returns_false_for_deleted_story() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Deleted".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    reason: "created in error".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert!(!is_ready(&story, &BTreeMap::new()));
    }

    /// Regression test for SH-126 (council verdict,
    /// `.council/sh126-blocked-column-membership/DECISION.md`): a story
    /// parked in the required `blocked` state (SH-125) with no `awaiting`
    /// and no unmet `blocked-by` edge used to report `is_ready() == true`,
    /// because `is_ready` never inspected `story.state`. The dashboard's
    /// only affordance for blocking a story — dragging a card into the
    /// Blocked column — writes exactly this shape (a bare state change, no
    /// reason), so every dashboard-originated "block" contradicted its own
    /// column.
    #[test]
    fn is_ready_returns_false_for_a_story_in_the_blocked_state() {
        let story = StorySnapshot {
            id: "SH-1".to_string(),
            title: "Blocked".to_string(),
            created_at: "2026-03-13T00:00:00Z".to_string(),
            updated_at: "2026-03-13T00:00:00Z".to_string(),
            state: "blocked".to_string(),
            superstate: SuperState::Open,
            assignee: None,
            awaiting: None,
            priority: Priority::None,
            labels: Vec::new(),
            story_type: None,
            description: None,
            comments: Vec::new(),
            relationships: Vec::new(),
            closed_at: None,
            deleted: false,
            deleted_reason: None,
            hidden_at: None,
        };

        assert!(!is_ready(&story, &BTreeMap::new()));
    }

    #[test]
    fn deleted_blocker_no_longer_blocks_dependent() {
        // Regression test for #18: before the fix, a deleted `blocked-by`
        // blocker still had `superstate: OPEN`, so `is_ready` kept treating
        // its dependent as blocked forever.
        let blocker = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Blocker".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    reason: "no longer needed".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        let dependent = fold_story(
            "SH-2",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Dependent".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryRelationshipAdded {
                    at: "2026-03-13T00:00:01Z".to_string(),
                    other_id: "SH-1".to_string(),
                    relation: "blocked-by".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        let mut all_stories = BTreeMap::new();
        all_stories.insert(blocker.id.clone(), blocker);
        all_stories.insert(dependent.id.clone(), dependent.clone());

        assert!(is_ready(&dependent, &all_stories));
    }

    #[test]
    fn fold_story_tracks_priority() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Priority".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryPrioritySet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    priority: Priority::High,
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.priority, Priority::High);
    }

    #[test]
    fn fold_story_priority_defaults_to_none() {
        let story = fold_story(
            "SH-1",
            &[StoryEvent::StoryCreated {
                at: "2026-03-13T00:00:00Z".to_string(),
                title: "No priority".to_string(),
                state: "todo".to_string(),
            }],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.priority, Priority::None);
    }

    #[test]
    fn priority_ord_ranks_critical_first() {
        assert!(Priority::Critical < Priority::High);
        assert!(Priority::High < Priority::Medium);
        assert!(Priority::Medium < Priority::Low);
        assert!(Priority::Low < Priority::None);
    }

    #[test]
    fn derive_family_relationships_omits_immediate_edges() {
        let stories = sample_story_map();
        let derived = derive_family_relationships(&stories);

        assert_eq!(
            derived.get("SH-1").unwrap(),
            &vec![StoryRelation {
                relation: "ancestor-of".to_string(),
                other_id: "SH-3".to_string(),
            }]
        );
        assert_eq!(
            derived.get("SH-3").unwrap(),
            &vec![StoryRelation {
                relation: "descendent-of".to_string(),
                other_id: "SH-1".to_string(),
            }]
        );
        assert!(derived.get("SH-2").unwrap().is_empty());
    }

    #[test]
    fn detects_prospective_parent_cycle() {
        let stories = sample_story_map();
        assert!(would_create_parent_cycle(&stories, "SH-3", "SH-1"));
        assert!(!would_create_parent_cycle(&stories, "SH-1", "SH-4"));
    }

    fn state_map() -> BTreeMap<String, StateDef> {
        vec![
            StateDef {
                slug: "todo".to_string(),
                super_state: SuperState::Open,
                role: None,
                description: None,
            },
            StateDef {
                slug: "done".to_string(),
                super_state: SuperState::Closed,
                role: None,
                description: None,
            },
        ]
        .into_iter()
        .map(|state| (state.slug.clone(), state))
        .collect()
    }

    fn sample_story_map() -> BTreeMap<String, StorySnapshot> {
        let stories = vec![
            StorySnapshot {
                id: "SH-1".to_string(),
                title: "A".to_string(),
                created_at: "2026-03-13T00:00:00Z".to_string(),
                updated_at: "2026-03-13T00:00:00Z".to_string(),
                state: "todo".to_string(),
                superstate: SuperState::Open,
                assignee: None,
                awaiting: None,
                priority: Priority::None,
                labels: Vec::new(),
                story_type: None,
                description: None,
                comments: Vec::new(),
                relationships: vec![StoryRelation {
                    relation: "parent-of".to_string(),
                    other_id: "SH-2".to_string(),
                }],
                closed_at: None,
                deleted: false,
                deleted_reason: None,
                hidden_at: None,
            },
            StorySnapshot {
                id: "SH-2".to_string(),
                title: "B".to_string(),
                created_at: "2026-03-13T00:00:00Z".to_string(),
                updated_at: "2026-03-13T00:00:00Z".to_string(),
                state: "todo".to_string(),
                superstate: SuperState::Open,
                assignee: None,
                awaiting: None,
                priority: Priority::None,
                labels: Vec::new(),
                story_type: None,
                description: None,
                comments: Vec::new(),
                relationships: vec![
                    StoryRelation {
                        relation: "child-of".to_string(),
                        other_id: "SH-1".to_string(),
                    },
                    StoryRelation {
                        relation: "parent-of".to_string(),
                        other_id: "SH-3".to_string(),
                    },
                ],
                closed_at: None,
                deleted: false,
                deleted_reason: None,
                hidden_at: None,
            },
            StorySnapshot {
                id: "SH-3".to_string(),
                title: "C".to_string(),
                created_at: "2026-03-13T00:00:00Z".to_string(),
                updated_at: "2026-03-13T00:00:00Z".to_string(),
                state: "todo".to_string(),
                superstate: SuperState::Open,
                assignee: None,
                awaiting: None,
                priority: Priority::None,
                labels: Vec::new(),
                story_type: None,
                description: None,
                comments: Vec::new(),
                relationships: vec![StoryRelation {
                    relation: "child-of".to_string(),
                    other_id: "SH-2".to_string(),
                }],
                closed_at: None,
                deleted: false,
                deleted_reason: None,
                hidden_at: None,
            },
            StorySnapshot {
                id: "SH-4".to_string(),
                title: "D".to_string(),
                created_at: "2026-03-13T00:00:00Z".to_string(),
                updated_at: "2026-03-13T00:00:00Z".to_string(),
                state: "todo".to_string(),
                superstate: SuperState::Open,
                assignee: None,
                awaiting: None,
                priority: Priority::None,
                labels: Vec::new(),
                story_type: None,
                description: None,
                comments: Vec::new(),
                relationships: Vec::new(),
                closed_at: None,
                deleted: false,
                deleted_reason: None,
                hidden_at: None,
            },
        ];

        stories
            .into_iter()
            .map(|story| (story.id.clone(), story))
            .collect()
    }

    // -----------------------------------------------------------------------
    // `scan_story_refs` — the boundary rules, unchanged from `extract_story_ids`
    //
    // These six re-point at the new scanner. What they pin — where an id starts
    // and ends — is exactly what it always was, and must stay pinned: SH-124
    // changed why a commit names a story, never what counts as a name.
    // -----------------------------------------------------------------------

    /// Every id the scanner found, in order, ignoring intent.
    fn ids(prefix: &str, text: &str) -> Vec<String> {
        super::scan_story_refs(prefix, text)
            .into_iter()
            .map(|reference| reference.id)
            .collect()
    }

    /// Only the ids the message *claims*.
    fn claimed(text: &str) -> Vec<String> {
        super::scan_story_refs("SH", text)
            .into_iter()
            .filter(super::StoryReference::claims)
            .map(|reference| reference.id)
            .collect()
    }

    /// Whether a message claims exactly the ids named, and nothing else.
    #[track_caller]
    fn claims_exactly(text: &str, expected: &[&str]) {
        assert_eq!(claimed(text), expected, "claims of {text:?}");
    }

    #[test]
    fn scan_finds_a_single_id() {
        assert_eq!(ids("SH", "Fix SH-1 bug"), vec!["SH-1"]);
    }

    #[test]
    fn scan_finds_several_ids() {
        assert_eq!(ids("SH", "SH-1 and SH-2"), vec!["SH-1", "SH-2"]);
    }

    #[test]
    fn scan_finds_nothing_in_text_without_ids() {
        let none: Vec<String> = Vec::new();
        assert_eq!(ids("SH", "no matches here"), none);
    }

    #[test]
    fn scan_honours_a_custom_prefix() {
        assert_eq!(ids("API", "API-42 done"), vec!["API-42"]);
    }

    #[test]
    fn scan_does_not_match_inside_a_word() {
        let none: Vec<String> = Vec::new();
        assert_eq!(ids("SH", "PUSH-123"), none);
    }

    #[test]
    fn scan_needs_a_boundary_between_two_ids() {
        assert_eq!(ids("SH", "SH-1SH-2"), vec!["SH-1"]);
    }

    // -----------------------------------------------------------------------
    // The claim grammar (SH-124)
    // -----------------------------------------------------------------------

    /// The reported defect, verbatim from the story: the trailer shape that
    /// moved five stories in `scad-caliper` must claim nothing.
    #[test]
    fn a_refs_trailer_over_a_list_claims_nothing() {
        let scanned = super::scan_story_refs("CAL", "Refs CAL-21, CAL-28, CAL-29");
        assert_eq!(
            scanned.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["CAL-21", "CAL-28", "CAL-29"],
            "all three still link"
        );
        assert!(
            scanned.iter().all(|r| !r.claims()),
            "none of them may claim: {scanned:?}"
        );
    }

    #[test]
    fn a_bare_mention_links_without_claiming() {
        claims_exactly("This is groundwork for SH-1, which stays open.", &[]);
        assert_eq!(ids("SH", "groundwork for SH-1"), vec!["SH-1"]);
    }

    #[test]
    fn a_claim_word_before_the_id_claims_it() {
        claims_exactly("Closes SH-1", &["SH-1"]);
        claims_exactly("feat: land the thing\n\nCloses SH-1", &["SH-1"]);
    }

    #[test]
    fn a_claim_word_is_matched_case_insensitively() {
        for spelling in ["Closes", "closes", "CLOSES", "CloSeS"] {
            claims_exactly(&format!("{spelling} SH-1"), &["SH-1"]);
        }
    }

    /// Position is unanchored, and this is why: all three are real commit lines
    /// from this repository's own history.
    #[test]
    fn a_claim_word_claims_from_the_middle_of_a_line() {
        claims_exactly(
            "Reconciled stale state from sessions 1-5, fixed SH-20",
            &["SH-20"],
        );
        claims_exactly("archive corruption, completed SH-27 (clippy).", &["SH-27"]);
        claims_exactly(
            "chore(storyhook): start SH-41 and file the defect",
            &["SH-41"],
        );
    }

    // --- the colon tier -----------------------------------------------------

    /// A git trailer: `token: value` where the value is the whole rest of the
    /// line.
    #[test]
    fn a_colon_trailer_claims_when_the_id_is_the_whole_remainder() {
        claims_exactly("Closes: SH-1", &["SH-1"]);
        claims_exactly("Closes: SH-1.", &["SH-1"]);
        claims_exactly("Closes: SH-1;", &["SH-1"]);
        claims_exactly("Closes: SH-1, SH-2", &["SH-1", "SH-2"]);
    }

    /// The decisive case. Here the colon is a Conventional Commits *type*, not a
    /// trailer key, and the id is merely the first word of a description.
    #[test]
    fn a_conventional_commits_subject_does_not_claim() {
        claims_exactly("fix: SH-12 broken parser", &[]);
        claims_exactly("fix: SH-12 and SH-13 both need attention", &[]);
        assert_eq!(
            ids("SH", "fix: SH-12 broken parser"),
            vec!["SH-12"],
            "it still links"
        );
    }

    /// A colon with nothing after it is not a trailer.
    #[test]
    fn a_colon_without_whitespace_never_claims() {
        claims_exactly("fix:SH-1", &[]);
        claims_exactly("Closes:SH-1", &[]);
    }

    /// The whitespace tier carries no whole-remainder rule, and must not: both
    /// of these are real commit lines here, and a global rule would break them.
    #[test]
    fn the_whole_remainder_rule_binds_only_the_colon_tier() {
        claims_exactly("Closes SH-123. Refs SH-113, SH-112.", &["SH-123"]);
        claims_exactly("Closes SH-46. Part of the W7 repo cutover.", &["SH-46"]);
        claims_exactly("Fixes SH-1 and SH-2 in one go", &["SH-1", "SH-2"]);
    }

    /// This repository writes `Story: SH-107` as a per-commit trailer. It is a
    /// bookkeeping pointer, never a claim.
    #[test]
    fn a_story_trailer_is_a_mention_not_a_claim() {
        claims_exactly("Story: SH-107", &[]);
        assert_eq!(ids("SH", "Story: SH-107"), vec!["SH-107"], "it still links");
    }

    // --- runs ---------------------------------------------------------------

    #[test]
    fn a_claim_distributes_over_a_joined_run() {
        claims_exactly("Closes SH-1, SH-2 and SH-3", &["SH-1", "SH-2", "SH-3"]);
        claims_exactly("Fixes SH-1 & SH-2", &["SH-1", "SH-2"]);
    }

    /// The run ends at the first token that is neither an id nor a joiner.
    #[test]
    fn a_run_ends_at_a_word_that_is_not_a_joiner() {
        claims_exactly("Closes SH-1, see SH-2", &["SH-1"]);
        assert_eq!(ids("SH", "Closes SH-1, see SH-2"), vec!["SH-1", "SH-2"]);
    }

    // --- merging ------------------------------------------------------------

    #[test]
    fn one_id_claimed_and_mentioned_yields_one_claim() {
        claims_exactly("Refs SH-1\n\nCloses SH-1", &["SH-1"]);
        claims_exactly("Closes SH-1\n\nRefs SH-1", &["SH-1"]);
        assert_eq!(
            super::scan_story_refs("SH", "Refs SH-1\n\nCloses SH-1").len(),
            1,
            "one entry per id"
        );
    }

    /// A claim word and its id must share a line — `%B` is multi-line, and a
    /// trailer key at the end of one line does not reach across to the next.
    #[test]
    fn a_claim_word_does_not_reach_across_a_line_break() {
        claims_exactly("Closes\nSH-1", &[]);
    }

    // --- the two demote-only gates -----------------------------------------

    #[test]
    fn a_revert_subject_claims_nothing_on_its_own_line() {
        claims_exactly("Revert \"feat: closes SH-1\"", &[]);
        assert_eq!(
            ids("SH", "Revert \"feat: closes SH-1\""),
            vec!["SH-1"],
            "a revert still links, which is what you want to read later"
        );
    }

    /// The ceiling is line 1 only: below it are the reverter's own words.
    #[test]
    fn a_reverts_own_body_still_claims() {
        claims_exactly(
            "Revert \"feat: closes SH-1\"\n\nThis reverts commit abc123.\nCloses SH-9",
            &["SH-9"],
        );
    }

    #[test]
    fn a_negation_before_the_claim_word_cancels_the_claim() {
        claims_exactly("This does not close SH-5", &[]);
        claims_exactly("won't fix SH-5", &[]);
        claims_exactly("doesn't close SH-5", &[]);
        claims_exactly("never fixes SH-5", &[]);
        claims_exactly("without closing SH-5", &[]);
    }

    /// A negation only ever demotes, so the link survives it.
    #[test]
    fn a_negated_claim_still_links() {
        assert_eq!(ids("SH", "This does not close SH-5"), vec!["SH-5"]);
    }

    // --- the tables are the documentation ----------------------------------

    /// Every row of the registry, exercised in both directions. A word moved
    /// between the two groups fails here rather than drifting silently.
    #[test]
    fn every_word_in_the_table_behaves_as_the_table_says() {
        for entry in super::REF_WORDS {
            let message = format!("{} SH-1", entry.word);
            let claims = !claimed(&message).is_empty();
            assert_eq!(
                claims, entry.claims,
                "`{}` is registered claims={} but behaves claims={claims}",
                entry.word, entry.claims
            );
            assert_eq!(
                ids("SH", &message),
                vec!["SH-1"],
                "`{}` must link either way",
                entry.word
            );
        }
    }

    /// Frozen at five. Growing this list is a design change, not a bug fix.
    #[test]
    fn the_negation_list_is_frozen_at_five() {
        assert_eq!(
            super::NEGATIONS.len(),
            5,
            "the negation list is frozen; growing it is a design change"
        );
        claims_exactly("hardly fixes SH-5", &["SH-5"]);
    }

    /// Closing implies working, so every word the post-merge hook closes on must
    /// also claim here. Otherwise a story could be closed by a merge without
    /// commit-sync ever having seen it active.
    #[test]
    fn every_closing_keyword_the_merge_hook_knows_also_claims() {
        for word in ["close", "closes", "fix", "fixes", "resolve", "resolves"] {
            assert!(
                super::claim_word(word),
                "`{word}` closes a story in the post-merge hook but does not claim here"
            );
        }
    }

    #[test]
    fn last_activity_type_returns_correct_types() {
        assert_eq!(last_activity_type(&[]), "unknown");
        assert_eq!(
            last_activity_type(&[StoryEvent::StoryCreated {
                at: "2026-03-13T00:00:00Z".to_string(),
                title: "Test".to_string(),
                state: "todo".to_string(),
            }]),
            "created"
        );
        assert_eq!(
            last_activity_type(&[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Test".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryCommentAdded {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    text: "a comment".to_string(),
                },
            ]),
            "comment"
        );
        assert_eq!(
            last_activity_type(&[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Test".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "in-progress".to_string(),
                },
            ]),
            "state-change"
        );
        assert_eq!(
            last_activity_type(&[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Test".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryPrioritySet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    priority: Priority::High,
                },
            ]),
            "priority-set"
        );
        assert_eq!(
            last_activity_type(&[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Test".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryTypeSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    story_type: "epic".to_string(),
                },
            ]),
            "type-set"
        );
        assert_eq!(
            last_activity_type(&[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Test".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDescriptionSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    description: "Some description".to_string(),
                },
            ]),
            "description-set"
        );
    }

    #[test]
    fn fold_story_tracks_story_type() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Typed story".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryTypeSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    story_type: "epic".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.story_type.as_deref(), Some("epic"));
        assert_eq!(story.updated_at, "2026-03-13T00:01:00Z");
    }

    #[test]
    fn fold_story_story_type_defaults_to_none() {
        let story = fold_story(
            "SH-1",
            &[StoryEvent::StoryCreated {
                at: "2026-03-13T00:00:00Z".to_string(),
                title: "No type".to_string(),
                state: "todo".to_string(),
            }],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.story_type, None);
    }

    #[test]
    fn fold_story_story_type_can_be_changed() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Changing type".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryTypeSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    story_type: "epic".to_string(),
                },
                StoryEvent::StoryTypeSet {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    story_type: "bug".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.story_type.as_deref(), Some("bug"));
        assert_eq!(story.updated_at, "2026-03-13T00:02:00Z");
    }

    #[test]
    fn fold_story_tracks_description() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Described story".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDescriptionSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    description: "What this story is about".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(
            story.description.as_deref(),
            Some("What this story is about")
        );
        assert_eq!(story.updated_at, "2026-03-13T00:01:00Z");
    }

    #[test]
    fn fold_story_description_defaults_to_none() {
        let story = fold_story(
            "SH-1",
            &[StoryEvent::StoryCreated {
                at: "2026-03-13T00:00:00Z".to_string(),
                title: "No description".to_string(),
                state: "todo".to_string(),
            }],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.description, None);
    }

    #[test]
    fn fold_story_description_last_write_wins() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Changing description".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDescriptionSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    description: "First draft".to_string(),
                },
                StoryEvent::StoryDescriptionSet {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    description: "Final draft".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.description.as_deref(), Some("Final draft"));
        assert_eq!(story.updated_at, "2026-03-13T00:02:00Z");
    }

    #[test]
    fn snapshot_without_description_deserializes() {
        let json = r#"{
            "id": "SH-1",
            "title": "Legacy snapshot",
            "created_at": "2026-03-13T00:00:00Z",
            "updated_at": "2026-03-13T00:00:00Z",
            "state": "todo",
            "superstate": "OPEN"
        }"#;

        let story: StorySnapshot = serde_json::from_str(json).unwrap();

        assert_eq!(story.description, None);
    }

    #[test]
    fn has_children_true_for_parent_of() {
        let stories = sample_story_map();
        assert!(has_children(stories.get("SH-1").unwrap()));
    }

    #[test]
    fn has_children_false_for_leaf() {
        let stories = sample_story_map();
        assert!(!has_children(stories.get("SH-3").unwrap()));
        assert!(!has_children(stories.get("SH-4").unwrap()));
    }

    #[test]
    fn compute_progress_returns_rollup_for_parent() {
        let stories = sample_story_map();
        let progress = compute_progress(stories.get("SH-1").unwrap(), &stories);
        assert!(progress.is_some());
        let p = progress.unwrap();
        assert_eq!(p.children_total, 1);
        assert_eq!(p.children_done, 0);
    }

    #[test]
    fn compute_progress_counts_closed_children() {
        let mut stories = sample_story_map();
        // Close child SH-2
        stories.get_mut("SH-2").unwrap().superstate = SuperState::Closed;
        stories.get_mut("SH-2").unwrap().state = "done".to_string();

        let progress = compute_progress(stories.get("SH-1").unwrap(), &stories);
        let p = progress.unwrap();
        assert_eq!(p.children_total, 1);
        assert_eq!(p.children_done, 1);
    }

    #[test]
    fn compute_progress_returns_none_for_leaf() {
        let stories = sample_story_map();
        let progress = compute_progress(stories.get("SH-4").unwrap(), &stories);
        assert!(progress.is_none());
    }

    #[test]
    fn compute_progress_only_counts_direct_children() {
        let stories = sample_story_map();
        // SH-1 is parent-of SH-2, SH-2 is parent-of SH-3
        // SH-1 should only count SH-2 as direct child, not SH-3
        let progress = compute_progress(stories.get("SH-1").unwrap(), &stories);
        let p = progress.unwrap();
        assert_eq!(p.children_total, 1);
    }

    // --- active_state / default_open_state (moved here from service::git by
    // SH-165, which needed both for compute_epic_display_state below and
    // found them pure over &[StateDef] with no git dependency) -------------

    #[test]
    fn an_explicit_active_role_decides_where_a_claimed_story_moves() {
        let states = [
            state("todo", SuperState::Open, None),
            state("in-progress", SuperState::Open, Some(STATE_ROLE_ACTIVE)),
            state("blocked", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];
        assert_eq!(
            active_state(&states).map(|state| state.slug),
            Some("in-progress".to_string())
        );
    }

    /// The inherited two-OPEN fallback, tested here because it can no longer
    /// be reached through the CLI.
    ///
    /// It answers only for a project with exactly two OPEN states and no role,
    /// and the required-state floor (SH-125) obliges every project to hold
    /// `todo`, `in-progress` **and** `blocked` as OPEN — three. So the input
    /// below is a catalog a conforming project cannot have: it survives for
    /// data written before the floor, which reaches this code through a read
    /// rather than through `story state`.
    #[test]
    fn two_open_states_and_no_role_means_the_second_one() {
        let states = [
            state("todo", SuperState::Open, None),
            state("doing", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];
        assert_eq!(
            active_state(&states).map(|state| state.slug),
            Some("doing".to_string())
        );
    }

    #[test]
    fn a_project_with_no_role_and_three_open_states_gets_no_guess() {
        // What a conforming project looks like when nothing carries the role:
        // `commit-sync` comments and links, and moves nothing.
        let states = [
            state("todo", SuperState::Open, None),
            state("in-progress", SuperState::Open, None),
            state("blocked", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];
        assert_eq!(active_state(&states), None);
    }

    #[test]
    fn one_open_state_and_no_role_gets_no_guess() {
        let states = [
            state("todo", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];
        assert_eq!(active_state(&states), None);
    }

    // --- compute_epic_display_state (SH-165) -------------------------------

    /// A project's default `REQUIRED_STATES` set: `todo`/`in-progress` (role
    /// `active`)/`blocked` all OPEN, `done` CLOSED — what `default_states()`
    /// (`service::project`) hands every new project.
    fn conforming_states() -> Vec<StateDef> {
        vec![
            state("todo", SuperState::Open, None),
            state("in-progress", SuperState::Open, Some(STATE_ROLE_ACTIVE)),
            state("blocked", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ]
    }

    #[test]
    fn epic_in_todo_with_an_in_progress_child_is_promoted() {
        let mut stories = sample_story_map();
        stories.get_mut("SH-2").unwrap().state = "in-progress".to_string();
        let epic = stories.get("SH-1").unwrap();

        assert_eq!(
            compute_epic_display_state(epic, &stories, &conforming_states()),
            Some("in-progress".to_string())
        );
    }

    #[test]
    fn epic_in_todo_with_no_active_child_is_not_promoted() {
        let stories = sample_story_map(); // every story starts "todo"
        let epic = stories.get("SH-1").unwrap();

        assert_eq!(
            compute_epic_display_state(epic, &stories, &conforming_states()),
            None
        );
    }

    #[test]
    fn a_blocked_epic_is_never_promoted_even_with_an_active_child() {
        let mut stories = sample_story_map();
        stories.get_mut("SH-1").unwrap().state = "blocked".to_string();
        stories.get_mut("SH-2").unwrap().state = "in-progress".to_string();
        let epic = stories.get("SH-1").unwrap();

        assert_eq!(
            compute_epic_display_state(epic, &stories, &conforming_states()),
            None,
            "blocked is a deliberate human signal (SH-126); an active child must not paper over it"
        );
    }

    #[test]
    fn an_epic_already_in_progress_is_not_re_promoted() {
        let mut stories = sample_story_map();
        stories.get_mut("SH-1").unwrap().state = "in-progress".to_string();
        stories.get_mut("SH-2").unwrap().state = "in-progress".to_string();
        let epic = stories.get("SH-1").unwrap();

        assert_eq!(
            compute_epic_display_state(epic, &stories, &conforming_states()),
            None,
            "already showing in-progress, so there is nothing to override"
        );
    }

    #[test]
    fn a_closed_epic_is_never_promoted() {
        let mut stories = sample_story_map();
        stories.get_mut("SH-1").unwrap().state = "done".to_string();
        stories.get_mut("SH-1").unwrap().superstate = SuperState::Closed;
        stories.get_mut("SH-2").unwrap().state = "in-progress".to_string();
        let epic = stories.get("SH-1").unwrap();

        assert_eq!(
            compute_epic_display_state(epic, &stories, &conforming_states()),
            None
        );
    }

    #[test]
    fn a_leaf_story_with_no_children_is_never_promoted() {
        let mut stories = sample_story_map();
        stories.get_mut("SH-4").unwrap().state = "in-progress".to_string();
        let leaf = stories.get("SH-3").unwrap(); // childless, parked in "todo"

        assert_eq!(
            compute_epic_display_state(leaf, &stories, &conforming_states()),
            None
        );
    }

    #[test]
    fn only_direct_children_count_toward_promotion() {
        let mut stories = sample_story_map();
        // SH-1 -> SH-2 -> SH-3; only SH-3 (a grandchild of SH-1) goes active.
        stories.get_mut("SH-3").unwrap().state = "in-progress".to_string();
        let epic = stories.get("SH-1").unwrap();

        assert_eq!(
            compute_epic_display_state(epic, &stories, &conforming_states()),
            None,
            "SH-3 is SH-2's child, not SH-1's — compute_progress makes the same direct-only cut"
        );
    }

    #[test]
    fn a_custom_active_role_state_is_what_gets_promoted_to() {
        let mut stories = sample_story_map();
        stories.get_mut("SH-2").unwrap().state = "doing".to_string();
        let epic = stories.get("SH-1").unwrap();
        let states = [
            state("todo", SuperState::Open, None),
            state("doing", SuperState::Open, Some(STATE_ROLE_ACTIVE)),
            state("done", SuperState::Closed, None),
        ];

        assert_eq!(
            compute_epic_display_state(epic, &stories, &states),
            Some("doing".to_string())
        );
    }

    #[test]
    fn no_active_state_resolvable_means_no_promotion() {
        let mut stories = sample_story_map();
        stories.get_mut("SH-2").unwrap().state = "in-progress".to_string();
        let epic = stories.get("SH-1").unwrap();
        // Three custom OPEN states, no role configured: active_state() can't guess.
        let states = [
            state("todo", SuperState::Open, None),
            state("in-progress", SuperState::Open, None),
            state("blocked", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];

        assert_eq!(compute_epic_display_state(epic, &stories, &states), None);
    }

    // --- ready_order / story_number (SH-63) -------------------------------

    fn ready_snapshot(id: &str, priority: Priority, created_at: &str) -> StorySnapshot {
        StorySnapshot {
            id: id.to_string(),
            title: id.to_string(),
            created_at: created_at.to_string(),
            updated_at: created_at.to_string(),
            state: "todo".to_string(),
            superstate: SuperState::Open,
            assignee: None,
            awaiting: None,
            comments: Vec::new(),
            relationships: Vec::new(),
            priority,
            labels: Vec::new(),
            story_type: None,
            description: None,
            closed_at: None,
            deleted: false,
            deleted_reason: None,
            hidden_at: None,
        }
    }

    fn ready_order_ids(mut stories: Vec<StorySnapshot>) -> Vec<String> {
        stories.sort_by(ready_order);
        stories.into_iter().map(|s| s.id).collect()
    }

    #[test]
    fn ready_order_breaks_a_priority_tie_by_story_number() {
        let ids = ready_order_ids(vec![
            ready_snapshot("SH-10", Priority::High, "2026-01-01T00:00:00Z"),
            ready_snapshot("SH-9", Priority::High, "2026-01-01T00:00:00Z"),
        ]);
        assert_eq!(
            ids,
            ["SH-9", "SH-10"],
            "the number wins across the 9/10 boundary, not the id string"
        );
    }

    #[test]
    fn ready_order_ignores_created_at_entirely() {
        // SH-1 was created LATER than SH-2 but is numbered lower. The legacy
        // `priority, created_at` comparator would have put SH-2 first; this
        // one does not look at `created_at` at all.
        let ids = ready_order_ids(vec![
            ready_snapshot("SH-2", Priority::High, "2026-01-01T00:00:00Z"),
            ready_snapshot("SH-1", Priority::High, "2026-06-01T00:00:00Z"),
        ]);
        assert_eq!(ids, ["SH-1", "SH-2"], "story number decides, not age");
    }

    #[test]
    fn ready_order_ranks_priority_above_story_number() {
        let ids = ready_order_ids(vec![
            ready_snapshot("SH-1", Priority::Low, "2026-01-01T00:00:00Z"),
            ready_snapshot("SH-99", Priority::Critical, "2026-01-01T00:00:00Z"),
        ]);
        assert_eq!(ids, ["SH-99", "SH-1"]);
    }

    #[test]
    fn ready_order_falls_back_to_the_id_string_when_neither_number_parses() {
        // Both `story_number("MY-APP-3")` and `story_number("MY-APP-9")` are
        // `u64::MAX` — the number half is "APP", not a number — so the id
        // string is what is left to break the tie.
        let ids = ready_order_ids(vec![
            ready_snapshot("MY-APP-9", Priority::High, "2026-01-01T00:00:00Z"),
            ready_snapshot("MY-APP-3", Priority::High, "2026-01-01T00:00:00Z"),
        ]);
        assert_eq!(ids, ["MY-APP-3", "MY-APP-9"]);
    }

    #[test]
    fn story_number_parses_the_numeric_half() {
        assert_eq!(story_number("SH-1"), 1);
        assert_eq!(story_number("SH-10"), 10);
        assert_eq!(story_number("SH-999"), 999);
    }

    #[test]
    fn story_number_sorts_an_unparseable_id_last() {
        assert_eq!(story_number("garbage"), u64::MAX);
        assert_eq!(story_number("SH-abc"), u64::MAX);
        assert_eq!(story_number(""), u64::MAX);
    }
}

#[cfg(test)]
mod event_kind_tests {
    use super::*;

    /// Reads the variant list back out of serde's own derive and compares it to
    /// [`EVENT_KINDS`], so adding a `StoryEvent` variant without listing its tag
    /// here is a red test rather than a legacy log the importer quietly
    /// misfiles as corrupt.
    #[test]
    fn every_known_kind_is_a_variant_and_every_variant_is_known() {
        let error = serde_json::from_str::<StoryEvent>(r#"{"kind":"NoSuchEventKind"}"#)
            .expect_err("a made-up kind must not deserialize");
        let message = error.to_string();
        let (_, listed) = message
            .split_once("expected one of ")
            .unwrap_or_else(|| panic!("serde no longer lists the variants it expected: {message}"));
        // serde appends ` at line 1 column N` after the list; the last variant
        // is the text between the final pair of backticks.
        let last_backtick = listed
            .rfind('`')
            .unwrap_or_else(|| panic!("serde's variant list is no longer backticked: {message}"));
        let from_serde: Vec<String> = listed[..=last_backtick]
            .split(',')
            .map(|name| name.trim().trim_matches('`').to_string())
            .filter(|name| !name.is_empty())
            .collect();

        assert_eq!(
            from_serde,
            EVENT_KINDS.to_vec(),
            "EVENT_KINDS must list exactly the variants `StoryEvent` defines, in order"
        );
    }

    /// `event_kind` must agree with the tag serde actually writes, or the
    /// store's `kind` column describes a payload it does not match.
    #[test]
    fn every_variants_tag_is_the_one_serde_writes() {
        for event in [
            StoryEvent::StoryCreated {
                at: "t".into(),
                title: "t".into(),
                state: "todo".into(),
            },
            StoryEvent::StoryRelationshipRemoved {
                at: "t".into(),
                other_id: "SH-2".into(),
                relation: "child-of".into(),
            },
            StoryEvent::StoryDeleted {
                at: "t".into(),
                reason: "r".into(),
            },
        ] {
            let encoded: serde_json::Value = serde_json::to_value(&event).unwrap();
            assert_eq!(
                encoded["kind"].as_str().unwrap(),
                event_kind(&event),
                "event_kind disagrees with serde for {event:?}"
            );
            assert!(is_known_event_kind(event_kind(&event)));
        }
    }

    #[test]
    fn a_kind_from_a_newer_storyhook_is_not_known() {
        assert!(is_known_event_kind("StoryCreated"));
        assert!(!is_known_event_kind("StoryPinned"));
        assert!(!is_known_event_kind("storycreated"));
    }
}

/// [`ready_order`] is a total order (SH-63): the whole point of dropping
/// `created_at` is that identical input can no longer produce two different
/// orderings depending on the order it arrived in. Property-tested rather
/// than asserted for one arrangement, because "total order" is a claim about
/// *every* arrangement.
#[cfg(test)]
mod ready_order_properties {
    use proptest::prelude::*;

    use super::{Priority, StorySnapshot, SuperState, ready_order};

    /// Five fixed ids, so ties are possible (only five priority buckets
    /// exist) but the id set itself never varies between the two orderings
    /// being compared.
    const IDS: [&str; 5] = ["SH-1", "SH-2", "SH-3", "SH-4", "SH-5"];

    fn priority_at(index: u8) -> Priority {
        match index % 5 {
            0 => Priority::Critical,
            1 => Priority::High,
            2 => Priority::Medium,
            3 => Priority::Low,
            _ => Priority::None,
        }
    }

    fn snapshot(id: &str, priority: Priority) -> StorySnapshot {
        StorySnapshot {
            id: id.to_string(),
            title: id.to_string(),
            // Every story shares one instant, so `created_at` cannot break
            // any tie even by accident — the property has to hold on the
            // hardest case, not an easy one.
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            state: "todo".to_string(),
            superstate: SuperState::Open,
            assignee: None,
            awaiting: None,
            comments: Vec::new(),
            relationships: Vec::new(),
            priority,
            labels: Vec::new(),
            story_type: None,
            description: None,
            closed_at: None,
            deleted: false,
            deleted_reason: None,
            hidden_at: None,
        }
    }

    /// Reorders `items` by pairing each with a tag from `seed` and sorting on
    /// the tag — a cheap way to get an arbitrary permutation out of proptest
    /// without needing a dedicated permutation strategy. A tag collision just
    /// means that pair keeps its relative order, which is still a valid
    /// permutation of the whole set.
    fn permute<T: Clone>(items: &[T], seed: &[u32]) -> Vec<T> {
        let mut tagged: Vec<(u32, &T)> = seed.iter().copied().zip(items.iter()).collect();
        tagged.sort_by_key(|&(tag, _)| tag);
        tagged.into_iter().map(|(_, item)| item.clone()).collect()
    }

    proptest! {
        /// Two different arrival orders of the same five stories sort to the
        /// same output, however their priorities collide.
        #[test]
        fn sorted_output_does_not_depend_on_arrival_order(
            priority_indices in prop::collection::vec(0u8..5, IDS.len()),
            seed_a in prop::collection::vec(any::<u32>(), IDS.len()),
            seed_b in prop::collection::vec(any::<u32>(), IDS.len()),
        ) {
            let canonical: Vec<StorySnapshot> = IDS
                .iter()
                .zip(priority_indices.iter())
                .map(|(id, &index)| snapshot(id, priority_at(index)))
                .collect();

            let mut a = permute(&canonical, &seed_a);
            let mut b = permute(&canonical, &seed_b);
            a.sort_by(ready_order);
            b.sort_by(ready_order);

            let ids_of = |stories: &[StorySnapshot]| -> Vec<String> {
                stories.iter().map(|s| s.id.clone()).collect()
            };
            prop_assert_eq!(ids_of(&a), ids_of(&b));
        }
    }
}
