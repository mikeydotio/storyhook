//! A project's configuration: its states, its story types, its members.
//!
//! Every edit here used to be a read-modify-write of a whole TOML document.
//! Adding a state meant loading `states.toml`, pushing onto a `Vec`,
//! serializing the `Vec` back over the file — so a field the running binary
//! did not know about was silently dropped on the way through, and two
//! concurrent edits kept whichever one finished last. Removing a state was
//! worse: the stories sitting in it had to be moved first, in a separate pass
//! of separate writes, and a failure between the last move and the config
//! write left the project in a state the tool could not describe.
//!
//! Here a state set is a table, an edit is an `UPDATE`, and an edit that
//! migrates occupants does so in the same transaction that changes the
//! configuration which required the migration.
//!
//! # The migration rule
//!
//! Two edits can leave stories stranded: removing a state, and changing a
//! state's superstate. Both reclassify every story sitting in it — a story in
//! a now-CLOSED state would report itself closed while still being editable —
//! so both refuse to proceed while the state holds open stories unless the
//! caller names where those stories go.
//!
//! What the store adds is that the moves and the configuration change are one
//! transaction. The legacy path deliberately moved the stories *first* and
//! wrote the configuration afterwards, because of the two bad orders that one
//! was recoverable: the user could re-run the command. There is no bad order
//! left to choose between.

use std::collections::{BTreeMap, BTreeSet};

use crate::cli::MemberInput;
use crate::domain::provenance::Provenance;
use crate::domain::{
    FieldEdit, Member, StateChanges, StateDef, StateUsage, StoryEvent, SuperState, TypeChanges,
    TypeDef, validate_required_states, validate_state_defs_for_write, validate_type_glyph,
    validate_type_slug,
};
use crate::error::AppError;
use crate::store::{ExpectedSeq, ProjectId, ReadOps, Store, StoryQuery, WriteOps};

use super::state_set::write_states;
use super::story::state_transition_events;
use super::{Ctx, append_and_fold, project_prefix, refold_story};

/// A configured state together with how many stories sit in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateListing {
    /// The state's definition.
    pub state: StateDef,
    /// How many stories occupy it, split open versus archived.
    pub usage: StateUsage,
}

/// The result of an edit that may have migrated stories out of the state it
/// changed, so a caller can report both halves of what happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateEdit {
    /// The state as it now reads.
    pub state: StateDef,
    /// How many open stories were moved out of it.
    pub moved: usize,
}

/// Reading and editing one project's configuration.
pub struct ConfigService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> ConfigService<'ctx, S> {
    /// A configuration service bound to `ctx`.
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    // --- states ------------------------------------------------------------

    /// Every configured state, in board order, with its occupancy.
    pub fn list_states(&self) -> Result<Vec<StateListing>, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            let usage = state_usage(tx, project)?;
            Ok(tx
                .states(project)?
                .into_iter()
                .map(|state| StateListing {
                    usage: usage.get(&state.slug).copied().unwrap_or_default(),
                    state,
                })
                .collect())
        })?)
    }

    /// Appends a state to the project's board order.
    ///
    /// The whole resulting set is validated, not just the new member: a slug
    /// that cannot be addressed, a second `active` role, or a set left without
    /// an OPEN or a CLOSED state are all rejected before anything is written.
    pub fn add_state(
        &self,
        slug: &str,
        superstate: SuperState,
        role: Option<String>,
        description: Option<String>,
    ) -> Result<StateDef, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let mut states = tx.states(project)?;
            if states.iter().any(|state| state.slug == slug) {
                return Err(AppError::Validation(format!("state `{slug}` already exists")).into());
            }
            let state = StateDef {
                slug: slug.to_string(),
                super_state: superstate,
                role,
                description,
            };
            states.push(state.clone());
            validate_state_defs_for_write(&states)?;
            write_states(tx, project, &states)?;
            Ok(state)
        })?)
    }

    /// Applies `changes` to one state, migrating its open stories into
    /// `move_stories_to` when the edit reclassifies them.
    ///
    /// Only a superstate change reclassifies. Editing a role or a description
    /// leaves every occupant exactly where it is and needs no destination.
    pub fn update_state(
        &self,
        slug: &str,
        changes: &StateChanges,
        move_stories_to: Option<&str>,
    ) -> Result<StateEdit, AppError> {
        let project = self.ctx.project();
        let now = self.ctx.now();
        Ok(self.ctx.store().write(|tx| {
            let states = tx.states(project)?;
            let current = states
                .iter()
                .find(|state| state.slug == slug)
                .ok_or_else(|| AppError::NotFound(format!("state `{slug}` not found")))?
                .clone();

            if changes.is_empty() {
                return Err(AppError::Usage(format!("nothing to change on state `{slug}`")).into());
            }

            let superstate_changed = changes
                .super_state
                .as_ref()
                .is_some_and(|next| *next != current.super_state);
            let updated = StateDef {
                slug: current.slug.clone(),
                super_state: changes
                    .super_state
                    .clone()
                    .unwrap_or_else(|| current.super_state.clone()),
                role: changes.role.clone().apply(current.role.clone()),
                description: changes
                    .description
                    .clone()
                    .apply(current.description.clone()),
            };
            let next_states: Vec<StateDef> = states
                .iter()
                .map(|state| {
                    if state.slug == slug {
                        updated.clone()
                    } else {
                        state.clone()
                    }
                })
                .collect();
            // Both validators run before any story is touched, so an edit that
            // would leave the project without a state it needs fails having
            // changed nothing.
            //
            // The floor goes first, and is asked here as well as at the funnel,
            // for the sake of the sentence the user reads: reclassifying `done`
            // breaks the general "one CLOSED state" rule *and* the floor, and
            // only the floor's message names the state and the way out. Asking
            // it later would also let `resolve_migration` demand a destination
            // for the occupants of an edit that was never going to be allowed.
            validate_required_states(&next_states)?;
            validate_state_defs_for_write(&next_states)?;

            let mut moved = 0;
            if superstate_changed
                && let Some(destination) = resolve_migration(
                    tx,
                    project,
                    &next_states,
                    slug,
                    move_stories_to,
                    "change the superstate of",
                )?
            {
                moved = migrate_occupants(
                    tx,
                    project,
                    slug,
                    &destination,
                    &now,
                    self.ctx.provenance(),
                )?;
            }

            write_states(tx, project, &next_states)?;
            if superstate_changed {
                // Whatever is left in this state — archived stories, which do
                // not migrate — was folded against the old definition and now
                // reports the wrong superstate. The read model is derived data;
                // a definition change re-derives it.
                refold_occupants(tx, project, slug)?;
            }
            Ok(StateEdit {
                state: updated,
                moved,
            })
        })?)
    }

    /// Removes a state, migrating its open stories into `move_stories_to`.
    ///
    /// A state with archived history is a hard stop: those stories' events
    /// name this slug, and reopening one re-folds that history against the
    /// *current* state set, which would then not contain it.
    pub fn remove_state(
        &self,
        slug: &str,
        move_stories_to: Option<&str>,
    ) -> Result<usize, AppError> {
        let project = self.ctx.project();
        let now = self.ctx.now();
        Ok(self.ctx.store().write(|tx| {
            let states = tx.states(project)?;
            if !states.iter().any(|state| state.slug == slug) {
                return Err(AppError::NotFound(format!("state `{slug}` not found")).into());
            }
            let retained: Vec<StateDef> = states
                .iter()
                .filter(|state| state.slug != slug)
                .cloned()
                .collect();
            // The floor first, for the same two reasons as in `update_state`:
            // its message is the specific one, and a required state cannot be
            // removed at any occupancy — so asking where its stories should go
            // would offer a way out that does not exist.
            validate_required_states(&retained)?;
            validate_state_defs_for_write(&retained)?;

            let archived = state_usage(&*tx, project)?
                .get(slug)
                .map_or(0, |usage| usage.archived);
            if archived > 0 {
                return Err(AppError::Validation(format!(
                    "state `{slug}` has {archived} archived {}; a state with archived history cannot be removed, because reopening one would fold against a state that no longer exists",
                    plural_stories(archived)
                ))
                .into());
            }

            let mut moved = 0;
            if let Some(destination) =
                resolve_migration(tx, project, &retained, slug, move_stories_to, "remove")?
            {
                moved = migrate_occupants(tx, project, slug, &destination, &now, self.ctx.provenance())?;
            }
            write_states(tx, project, &retained)?;
            Ok(moved)
        })?)
    }

    /// Rewrites the board order.
    ///
    /// `order` must be a permutation of the configured slugs. A partial order
    /// would silently drop states, and the position a state holds is
    /// user-visible: it drives the dashboard's columns and decides which OPEN
    /// state a new story opens in.
    pub fn reorder_states(&self, order: &[String]) -> Result<Vec<StateDef>, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let states = tx.states(project)?;
            let known: BTreeSet<&str> = states.iter().map(|state| state.slug.as_str()).collect();

            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for slug in order {
                if !known.contains(slug.as_str()) {
                    return Err(AppError::NotFound(format!("state `{slug}` not found")).into());
                }
                if !seen.insert(slug.as_str()) {
                    return Err(AppError::Validation(format!(
                        "state `{slug}` is listed more than once"
                    ))
                    .into());
                }
            }
            let missing: Vec<&str> = known
                .iter()
                .copied()
                .filter(|slug| !seen.contains(slug))
                .collect();
            if !missing.is_empty() {
                return Err(AppError::Validation(format!(
                    "the new order must list every state; missing: {}",
                    missing.join(", ")
                ))
                .into());
            }

            let by_slug: BTreeMap<&str, &StateDef> = states
                .iter()
                .map(|state| (state.slug.as_str(), state))
                .collect();
            let reordered: Vec<StateDef> = order
                .iter()
                .map(|slug| (*by_slug.get(slug.as_str()).expect("checked above")).clone())
                .collect();
            // A permutation cannot break the required floor — every member
            // survives a reorder — but it routes through the funnel anyway, so
            // that "every state set is written by one function" stays a
            // property of the code rather than a claim about it.
            write_states(tx, project, &reordered)?;
            Ok(reordered)
        })?)
    }

    // --- types -------------------------------------------------------------

    /// Every configured story type, in configured order.
    pub fn list_types(&self) -> Result<Vec<TypeDef>, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| tx.types(project))?)
    }

    /// Appends a story type, refusing a slug nothing could address.
    ///
    /// Only the incoming slug is checked, never the resulting set — and that is
    /// deliberate (SH-134). A catalog written before this rule existed can hold
    /// slugs it refuses; re-validating the whole set would mean such a project
    /// could never add a valid type until it was clean, and the symmetrical
    /// rule on [`remove_type`](Self::remove_type) would mean it could never
    /// become clean. Both refusals would police damage by preventing its
    /// repair.
    pub fn add_type(
        &self,
        slug: &str,
        description: Option<&str>,
        emoji: Option<&str>,
    ) -> Result<TypeDef, AppError> {
        let project = self.ctx.project();
        if let Some(glyph) = emoji {
            validate_type_glyph(glyph)?;
        }
        Ok(self.ctx.store().write(|tx| {
            // `none` is the legacy untyped diagnostic sentinel and `default`
            // is reserved configuration vocabulary, so either real slug
            // would make a read ambiguous.
            for reserved in ["none", "default"] {
                if slug.eq_ignore_ascii_case(reserved) {
                    return Err(AppError::Validation(format!(
                        "type slug `{reserved}` is reserved and cannot be used"
                    ))
                    .into());
                }
            }
            // Reserved before shape, and not as a style call: `NONE` breaks
            // both rules, and only the reserved message tells the user that
            // fixing the casing will not help.
            validate_type_slug(slug)?;
            let mut types = tx.types(project)?;
            if types.iter().any(|t| t.slug == slug) {
                return Err(AppError::Validation(format!("type `{slug}` already exists")).into());
            }
            let type_def = TypeDef {
                slug: slug.to_string(),
                description: description.map(str::to_string),
                emoji: emoji.map(str::to_string),
            };
            types.push(type_def.clone());
            tx.put_types(project, &types)?;
            Ok(type_def)
        })?)
    }

    /// Applies `changes` to one type's description and/or emoji.
    pub fn update_type(&self, slug: &str, changes: &TypeChanges) -> Result<TypeDef, AppError> {
        let project = self.ctx.project();
        if let FieldEdit::Set(glyph) = &changes.emoji {
            validate_type_glyph(glyph)?;
        }
        Ok(self.ctx.store().write(|tx| {
            let types = tx.types(project)?;
            let current = types
                .iter()
                .find(|t| t.slug == slug)
                .ok_or_else(|| AppError::NotFound(format!("type `{slug}` not found")))?
                .clone();

            if changes.is_empty() {
                return Err(AppError::Usage(format!("nothing to change on type `{slug}`")).into());
            }

            let updated = TypeDef {
                slug: current.slug.clone(),
                description: changes
                    .description
                    .clone()
                    .apply(current.description.clone()),
                emoji: changes.emoji.clone().apply(current.emoji.clone()),
            };
            let next_types: Vec<TypeDef> = types
                .iter()
                .map(|t| {
                    if t.slug == slug {
                        updated.clone()
                    } else {
                        t.clone()
                    }
                })
                .collect();
            tx.put_types(project, &next_types)?;
            Ok(updated)
        })?)
    }

    /// Removes a story type that no story is using.
    pub fn remove_type(&self, slug: &str) -> Result<(), AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let types = tx.types(project)?;
            if types.len() <= 1 {
                return Err(AppError::Validation("cannot remove the last type".to_string()).into());
            }
            if !types.iter().any(|t| t.slug == slug) {
                return Err(AppError::NotFound(format!("type `{slug}` not found")).into());
            }
            // Every story, archived and deleted ones included: their snapshots
            // still name the type, and a type they name has to keep existing.
            if !tx
                .stories(project, &StoryQuery::all().story_type(slug))?
                .is_empty()
            {
                return Err(AppError::Validation(format!(
                    "type `{slug}` is still used by an existing story"
                ))
                .into());
            }
            let retained: Vec<TypeDef> = types.into_iter().filter(|t| t.slug != slug).collect();
            tx.put_types(project, &retained)?;
            Ok(())
        })?)
    }

    // --- members -----------------------------------------------------------

    /// Every member, ordered by id.
    pub fn list_members(&self) -> Result<Vec<Member>, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| tx.members(project))?)
    }

    /// Adds a member, deriving its id from what the caller typed.
    ///
    /// Two members cannot share an id. The store's `put_member` is an upsert —
    /// which is what a later sync path wants — so the uniqueness rule is
    /// enforced here, where a duplicate is a user error rather than a
    /// correction.
    pub fn add_member(&self, input: &MemberInput) -> Result<Member, AppError> {
        let project = self.ctx.project();
        let member = build_member(input, &self.ctx.now());
        Ok(self.ctx.store().write(|tx| {
            if tx
                .members(project)?
                .iter()
                .any(|existing| existing.id == member.id)
            {
                return Err(
                    AppError::Validation(format!("member `{}` already exists", member.id)).into(),
                );
            }
            tx.put_member(project, &member)?;
            Ok(member)
        })?)
    }
}

/// How many stories sit in each configured state.
///
/// Deleted stories are excluded on purpose: `fold_story` forces them to CLOSED
/// without consulting the state map, so they neither block removing a state
/// nor need migrating out of one. States with no stories are still present,
/// with zero counts.
pub fn state_usage(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<BTreeMap<String, StateUsage>, AppError> {
    let mut usage: BTreeMap<String, StateUsage> = tx
        .states(project)?
        .into_iter()
        .map(|state| (state.slug, StateUsage::default()))
        .collect();
    for row in tx.stories(project, &StoryQuery::all().deleted(false))? {
        let entry = usage.entry(row.state.clone()).or_default();
        if row.archived {
            entry.archived += 1;
        } else {
            entry.open += 1;
        }
    }
    Ok(usage)
}

/// Where the open stories in `slug` go before `slug` is removed or
/// reclassified, given the state set that will exist afterwards.
///
/// `Ok(None)` means nothing needs moving. `action` names the operation for the
/// error messages.
fn resolve_migration(
    tx: &impl ReadOps,
    project: ProjectId,
    next_states: &[StateDef],
    slug: &str,
    move_stories_to: Option<&str>,
    action: &str,
) -> Result<Option<StateDef>, AppError> {
    let open = state_usage(tx, project)?
        .get(slug)
        .map_or(0, |usage| usage.open);
    if open == 0 {
        return Ok(None);
    }

    let candidates: Vec<&StateDef> = next_states
        .iter()
        .filter(|state| state.slug != slug)
        .collect();
    if candidates.is_empty() {
        return Err(AppError::Validation(format!(
            "cannot {action} state `{slug}`: it holds {open} open {}, and there is no other state to move them into",
            plural_stories(open)
        )));
    }

    let Some(target) = move_stories_to else {
        let names: Vec<&str> = candidates.iter().map(|state| state.slug.as_str()).collect();
        return Err(AppError::Validation(format!(
            "cannot {action} state `{slug}` while it holds {open} open {}: name the state they move into (one of: {})",
            plural_stories(open),
            names.join(", ")
        )));
    };

    if target == slug {
        return Err(AppError::Validation(format!(
            "stories in `{slug}` cannot be moved into `{slug}` itself"
        )));
    }
    candidates
        .into_iter()
        .find(|state| state.slug == target)
        .cloned()
        .map(Some)
        .ok_or_else(|| AppError::NotFound(format!("state `{target}` not found")))
}

/// Moves every open, undeleted story in `from` into `to`, leaving a comment on
/// each so the move is traceable to the configuration change that caused it.
///
/// Runs inside the caller's transaction, alongside the configuration change
/// itself: a failure on the fourth story of five rolls back the first three
/// *and* the edit that required them to move.
fn migrate_occupants(
    tx: &mut impl WriteOps,
    project: ProjectId,
    from: &str,
    to: &StateDef,
    now: &str,
    provenance: &Provenance,
) -> Result<usize, AppError> {
    let prefix = project_prefix(&*tx, project)?;
    let states = tx.state_map(project)?;
    let occupants = tx.stories(
        project,
        &StoryQuery::all().state(from).archived(false).deleted(false),
    )?;

    for row in &occupants {
        let mut events = vec![StoryEvent::StoryCommentAdded {
            at: now.to_string(),
            text: format!("[states] moved from `{from}` to `{}`", to.slug),
        }];
        events.extend(state_transition_events(
            to,
            row.awaiting.is_some(),
            now,
            Vec::new(),
        ));
        append_and_fold(
            tx,
            project,
            row.story_no,
            &prefix,
            &states,
            ExpectedSeq::Exact(row.head_seq),
            &events,
            provenance,
        )?;
    }
    Ok(occupants.len())
}

/// Re-derives the read model for every story still sitting in `slug`.
///
/// Called after a state's *definition* changes, which is the one kind of write
/// that can invalidate a story's row without appending anything to that
/// story's history. Nothing here changes what happened; it changes what the
/// stored fold of what happened says.
fn refold_occupants(
    tx: &mut impl WriteOps,
    project: ProjectId,
    slug: &str,
) -> Result<(), AppError> {
    let prefix = project_prefix(&*tx, project)?;
    let states = tx.state_map(project)?;
    let occupants = tx.stories(project, &StoryQuery::all().state(slug))?;
    for row in &occupants {
        refold_story(tx, project, row.story_no, &prefix, &states)?;
    }
    Ok(())
}

/// The member a `story member add` argument describes.
///
/// `Name <email>` is split into its two halves; anything else becomes a
/// display name on its own. The id is always a slug of the display name, so
/// `story assign` has something short to take.
fn build_member(input: &MemberInput, now: &str) -> Member {
    match input {
        MemberInput::Github(handle) => Member {
            id: slugify(handle),
            display_name: handle.clone(),
            email: None,
            github: Some(handle.clone()),
            created_at: now.to_string(),
        },
        MemberInput::Identity(identity) => {
            let trimmed = identity.trim();
            match parse_identity(trimmed) {
                Some((name, email)) => Member {
                    id: slugify(name),
                    display_name: name.to_string(),
                    email: Some(email.to_string()),
                    github: None,
                    created_at: now.to_string(),
                },
                None => Member {
                    id: slugify(trimmed),
                    display_name: trimmed.to_string(),
                    email: None,
                    github: None,
                    created_at: now.to_string(),
                },
            }
        }
    }
}

/// Splits `Ada Lovelace <ada@example.com>` into its name and its address.
fn parse_identity(input: &str) -> Option<(&str, &str)> {
    let start = input.find('<')?;
    let end = input.rfind('>')?;
    if start >= end {
        return None;
    }
    let name = input[..start].trim();
    let email = input[start + 1..end].trim();
    if name.is_empty() || email.is_empty() {
        return None;
    }
    Some((name, email))
}

/// Lowercases `value` and collapses every run of non-alphanumerics into a
/// single dash, falling back to `member` when nothing survives.
fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "member".to_string()
    } else {
        slug
    }
}

/// "story" / "stories" for a count.
fn plural_stories(count: usize) -> &'static str {
    if count == 1 { "story" } else { "stories" }
}
