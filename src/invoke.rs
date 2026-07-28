//! The seam between *deciding what to do* and *doing it*.
//!
//! Everything that can run a storyhook command — the CLI, the web dashboard,
//! and later the TUI — goes through [`Invoker`]. Today there is exactly one
//! implementation, [`LegacyInvoker`], which forwards to [`crate::app::run`]
//! in the same process. The point of introducing the trait before there is
//! anything to choose between is that adopting it is provably behavior-
//! preserving *now*, when the only implementation is the existing call, and
//! therefore cheap to verify; a later implementation that talks to a store or
//! a daemon becomes a constructor swap rather than a rewrite of every call
//! site.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::app;
use crate::cli::{
    CliOptions, HELP_TEXT, HooksAction, Invocation, PluginAction, StateAction, TypeAction,
};
use crate::domain::{FieldEdit, StateChanges, SuperState};
use crate::error::AppError;
use crate::help_topics;
use crate::output::Response;
use crate::service::{
    Clock, ConfigService, Ctx, FieldEdits, InitOptions, InitOutcome, NewStoryInput, ProjectService,
    RelationOutcome, RelationService, ReopenOutcome, StateListing, StoryService, SystemService,
};
use crate::store::Store;

/// One unit of work for an [`Invoker`]: the command, plus the execution
/// context that has to travel with it.
///
/// Only settings that change *what happens* belong here. `--json` and
/// `--quiet` do not: they are rendering decisions, applied by
/// [`crate::output::render_response`] once the work is done and the caller
/// has the answer back. Keeping them out is what lets one process do the
/// work and another do the rendering.
///
/// `#[non_exhaustive]`: this struct is expected to grow — the working
/// directory and project selector once root resolution moves off the
/// caller, and a hook-recursion depth once hooks can re-enter through a
/// daemon. Construct it with [`InvokeRequest::new`] so that growth is not a
/// breaking change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct InvokeRequest {
    /// What to do.
    pub invocation: Invocation,
    /// Suppress the project's event hooks for this invocation, as
    /// `--no-hooks` does.
    pub no_hooks: bool,
}

impl InvokeRequest {
    /// A request to run `invocation` with hooks enabled.
    pub fn new(invocation: Invocation) -> Self {
        Self {
            invocation,
            no_hooks: false,
        }
    }

    /// Sets whether event hooks are suppressed.
    #[must_use]
    pub fn no_hooks(mut self, no_hooks: bool) -> Self {
        self.no_hooks = no_hooks;
        self
    }
}

/// Executes storyhook commands.
///
/// Implementations differ in *where* the work happens, never in what the
/// answer means: every one of them returns the same
/// [`Response`]/[`AppError`] envelope, which the caller renders itself.
pub trait Invoker {
    /// Runs `request`, returning the unrendered result.
    fn invoke(&self, request: InvokeRequest) -> Result<Response, AppError>;
}

/// Runs commands in this process against a project directory, by calling
/// [`crate::app::run`].
///
/// This is the pre-rearchitecture path, wrapped rather than reimplemented:
/// it forwards verbatim, so it behaves identically to a direct call by
/// construction. `app::run` reads only `no_hooks` and `invocation` off
/// [`CliOptions`], so the `json`/`quiet` fields filled in here are inert —
/// they exist because the struct still carries them for the CLI's own use.
pub struct LegacyInvoker<'a> {
    root: &'a Path,
}

impl<'a> LegacyInvoker<'a> {
    /// An invoker for the project rooted at `root`.
    pub fn new(root: &'a Path) -> Self {
        Self { root }
    }
}

impl Invoker for LegacyInvoker<'_> {
    fn invoke(&self, request: InvokeRequest) -> Result<Response, AppError> {
        app::run(
            self.root,
            CliOptions {
                json: false,
                quiet: false,
                no_hooks: request.no_hooks,
                invocation: request.invocation,
            },
        )
    }
}

/// Runs one invocation against the store, in this process.
///
/// This is the new stack's entry point, and the eventual replacement for
/// [`crate::app::run`]. It is deliberately thin: every arm validates the
/// CLI-shaped arguments it was handed, makes **one** service call, and turns
/// the answer into a [`Response`]. An arm that wants to do more than that is an
/// arm whose logic belongs in a service — that is where the invariants live,
/// and a rule enforced in a dispatch arm is a rule the web dashboard and the
/// daemon do not get.
///
/// # Completeness
///
/// The story lifecycle, relations, project initialization, configuration and
/// the system commands are ported. The query surfaces (`list`, `show`,
/// `summary`, `graph`, `phase`, `epic`, …), the integrity commands and the
/// git/GitHub family are not. Everything unported answers with
/// [`not_yet_ported`], which is an internal error naming the variant: nothing
/// routes production traffic here yet, and the wave that finishes the port has
/// "every variant dispatches" as its exit criterion. A silent fallback to the
/// legacy path is exactly what this must not do — a half-ported dispatcher
/// that quietly works is one nobody finishes.
///
/// `tests/differential_lifecycle.rs` holds the roster and asserts that the
/// ported and unported lists together account for every variant.
pub fn dispatch<S: Store>(ctx: &Ctx<'_, S>, invocation: Invocation) -> Result<Response, AppError> {
    match invocation {
        Invocation::New {
            title,
            state,
            story_type,
            description,
            priority,
            labels,
            assignee,
        } => {
            let input = NewStoryInput {
                title,
                state,
                story_type,
                description,
                priority,
                labels,
                assignee,
            };
            let story = StoryService::new(ctx).create(&input)?;
            ctx.story_view(&story.id)
        }
        Invocation::Comment { id, text } => {
            StoryService::new(ctx).comment(&id, &text)?;
            ctx.story_view(&id)
        }
        Invocation::Assign { id, member } => {
            StoryService::new(ctx).assign(&id, &member)?;
            ctx.story_view(&id)
        }
        Invocation::SetPriority { id, priority } => {
            StoryService::new(ctx).set_priority(&id, &priority)?;
            ctx.story_view(&id)
        }
        Invocation::SetLabels { id, add, remove } => {
            StoryService::new(ctx).set_labels(&id, &add, &remove)?;
            ctx.story_view(&id)
        }
        Invocation::SetAwaiting { id, awaiting } => {
            StoryService::new(ctx).set_awaiting(&id, &awaiting)?;
            ctx.story_view(&id)
        }
        Invocation::ClearAwaiting { id } => {
            StoryService::new(ctx).clear_awaiting(&id)?;
            ctx.story_view(&id)
        }
        Invocation::SetState {
            id,
            state,
            comment,
            if_state,
        } => {
            StoryService::new(ctx).set_state(
                &id,
                &state,
                comment.as_deref(),
                if_state.as_deref(),
            )?;
            ctx.story_view(&id)
        }
        Invocation::SetFields {
            id,
            title,
            state,
            priority,
            assignee,
            labels,
            blocked,
            unblocked,
            json,
            story_type,
            description,
        } => {
            let edits = FieldEdits {
                title,
                state,
                priority,
                assignee,
                labels,
                blocked,
                unblocked,
                json,
                story_type,
                description,
            };
            StoryService::new(ctx)
                .set_fields(&id, &edits)
                .map(Response::Message)
        }
        Invocation::BulkUpdate { updates } => StoryService::new(ctx)
            .bulk_update(&updates)
            .map(Response::Message),
        Invocation::Delete { id, reason } => StoryService::new(ctx)
            .delete(&id, &reason)
            .map(Response::Message),
        Invocation::Reopen { id, force } => match StoryService::new(ctx).reopen(&id, force)? {
            ReopenOutcome::Reopened(_) => ctx.story_view(&id),
            ReopenOutcome::Aborted(message) => Ok(Response::Message(message)),
        },
        Invocation::Relate {
            a,
            relation,
            b,
            remove,
        } => match RelationService::new(ctx).relate(&a, &relation, &b, remove)? {
            RelationOutcome::Changed(_) => ctx.story_view(&a),
            RelationOutcome::Unchanged { remove } => Ok(Response::Message(format!(
                "no changes; relationship already {}",
                if remove { "removed" } else { "added" }
            ))),
        },
        Invocation::State { action } => dispatch_state(ctx, action),
        Invocation::Type { action } => dispatch_type(ctx, action),
        Invocation::MemberAdd { input } => ConfigService::new(ctx)
            .add_member(&input)
            .map(|member| Response::Message(format!("added member {}", member.id))),
        Invocation::Scaffold { kind } => SystemService::new(ctx)
            .scaffold(&kind)
            .map(Response::Message),
        Invocation::Hooks { action } => dispatch_hooks(ctx, action),
        Invocation::Plugin { action } => {
            let service = SystemService::new(ctx);
            match action {
                PluginAction::Install { target } => service.install_plugin(&target),
                PluginAction::Uninstall { target } => service.uninstall_plugin(&target),
            }
            .map(Response::Message)
        }
        Invocation::Init { .. }
        | Invocation::Help
        | Invocation::HelpTopic { .. }
        | Invocation::HelpCompact
        | Invocation::HelpAll
        | Invocation::Version => dispatch_unscoped(ctx.store(), ctx.cwd(), &ctx.now(), invocation),
        other => Err(not_yet_ported(&other)),
    }
}

/// The `story hooks …` family.
fn dispatch_hooks<S: Store>(ctx: &Ctx<'_, S>, action: HooksAction) -> Result<Response, AppError> {
    let service = SystemService::new(ctx);
    match action {
        HooksAction::Install => service.install_git_hooks().map(Response::Message),
        HooksAction::Uninstall => service.uninstall_git_hooks().map(Response::Message),
        HooksAction::List => Ok(Response::Message(service.list_event_hooks())),
        HooksAction::Test { event_type } => {
            service.test_event_hook(&event_type).map(Response::Message)
        }
    }
}

/// The `story state …` family.
fn dispatch_state<S: Store>(ctx: &Ctx<'_, S>, action: StateAction) -> Result<Response, AppError> {
    let service = ConfigService::new(ctx);
    match action {
        StateAction::List => Ok(Response::Message(
            service
                .list_states()?
                .iter()
                .map(format_state_line)
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        StateAction::Add {
            slug,
            superstate,
            role,
            description,
        } => {
            let state =
                service.add_state(&slug, parse_superstate(&superstate)?, role, description)?;
            Ok(Response::Message(format!(
                "added state {} ({})",
                state.slug,
                state.super_state.as_str()
            )))
        }
        StateAction::Set {
            slug,
            superstate,
            role,
            description,
            clear_description,
            move_stories_to,
        } => {
            let changes = StateChanges {
                super_state: superstate.as_deref().map(parse_superstate).transpose()?,
                // `--role none` clears; `active` is the only real role, so
                // `none` cannot collide with one.
                role: match role.as_deref() {
                    None => FieldEdit::Keep,
                    Some("none") => FieldEdit::Clear,
                    Some(value) => FieldEdit::Set(value.to_string()),
                },
                description: if clear_description {
                    FieldEdit::Clear
                } else {
                    description.map_or(FieldEdit::Keep, FieldEdit::Set)
                },
            };
            let edit = service.update_state(&slug, &changes, move_stories_to.as_deref())?;
            let mut message = format!(
                "updated state {} ({})",
                edit.state.slug,
                edit.state.super_state.as_str()
            );
            message.push_str(&moved_suffix(edit.moved, move_stories_to.as_deref()));
            Ok(Response::Message(message))
        }
        StateAction::Remove {
            slug,
            move_stories_to,
        } => {
            let moved = service.remove_state(&slug, move_stories_to.as_deref())?;
            let mut message = format!("removed state {slug}");
            message.push_str(&moved_suffix(moved, move_stories_to.as_deref()));
            Ok(Response::Message(message))
        }
        StateAction::Reorder { order } => Ok(Response::Message(format!(
            "reordered states: {}",
            service
                .reorder_states(&order)?
                .iter()
                .map(|state| state.slug.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// The `story type …` family.
fn dispatch_type<S: Store>(ctx: &Ctx<'_, S>, action: TypeAction) -> Result<Response, AppError> {
    let service = ConfigService::new(ctx);
    match action {
        TypeAction::List => Ok(Response::Message(
            service
                .list_types()?
                .iter()
                .map(|t| match &t.description {
                    Some(description) => format!("{} — {description}", t.slug),
                    None => t.slug.clone(),
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        TypeAction::Add { slug, description } => Ok(Response::Message(format!(
            "added type {}",
            service.add_type(&slug, description.as_deref())?.slug
        ))),
        TypeAction::Remove { slug } => {
            service.remove_type(&slug)?;
            Ok(Response::Message(format!("removed type {slug}")))
        }
    }
}

/// `OPEN` or `CLOSED`, as a superstate.
fn parse_superstate(raw: &str) -> Result<SuperState, AppError> {
    SuperState::parse(raw)
        .ok_or_else(|| AppError::Validation("superstate must be OPEN or CLOSED".to_string()))
}

/// The `; moved 2 stories to done` half of a state edit's answer.
fn moved_suffix(moved: usize, destination: Option<&str>) -> String {
    if moved == 0 {
        return String::new();
    }
    format!(
        "; moved {moved} {} to {}",
        if moved == 1 { "story" } else { "stories" },
        destination.unwrap_or("another state")
    )
}

/// One `story state list` row: `in-progress (OPEN, active) — 2 open — desc`.
fn format_state_line(listing: &StateListing) -> String {
    let state = &listing.state;
    let mut attributes = vec![state.super_state.as_str().to_string()];
    if let Some(role) = &state.role {
        attributes.push(role.clone());
    }
    let mut line = format!("{} ({})", state.slug, attributes.join(", "));

    let mut counts = Vec::new();
    if listing.usage.open > 0 {
        counts.push(format!("{} open", listing.usage.open));
    }
    if listing.usage.archived > 0 {
        counts.push(format!("{} archived", listing.usage.archived));
    }
    if !counts.is_empty() {
        line.push_str(&format!(" — {}", counts.join(", ")));
    }
    if let Some(description) = &state.description {
        line.push_str(&format!(" — {description}"));
    }
    line
}

/// Dispatch for the invocations that run *before* a project is resolved.
///
/// `story init` is the reason this exists. Every other command names a project
/// and therefore takes a [`Ctx`]; init is the command that creates one, so on
/// a virgin store there is no [`ProjectId`](crate::store::ProjectId) for a
/// context to hold. Rather than let a caller invent one, the arms that do not
/// need a project live here and take the store and the checkout directly.
///
/// [`dispatch`] forwards its own project-less variants here, so the two entry
/// points cannot answer the same invocation differently and the roster of
/// ported arms stays a property of one function.
///
/// `now` is passed rather than read so that the answer is stamped once per
/// invocation, from whichever clock the caller is using.
pub fn dispatch_unscoped<S: Store>(
    store: &S,
    root: &Path,
    now: &str,
    invocation: Invocation,
) -> Result<Response, AppError> {
    match invocation {
        Invocation::Init {
            prefix,
            no_agents_md,
        } => {
            let outcome = ProjectService::new(store, root)
                .clock(Clock::Fixed(now.to_string()))
                .init(&InitOptions {
                    prefix,
                    agents_md: !no_agents_md,
                    // The pointer file is the store's claim on the repository,
                    // and while the legacy tree is still the identity of record
                    // a second claim could only ever disagree with it. The wave
                    // that flips the default turns this on.
                    pointer: false,
                })?;
            Ok(Response::Message(init_message(&outcome)))
        }
        // Pure functions of compiled-in text. They need neither a project nor
        // a store, and answering them here is what lets `story --help` work in
        // a directory storyhook has never heard of.
        Invocation::Help => Ok(Response::Message(HELP_TEXT.to_string())),
        Invocation::HelpCompact => Ok(Response::Message(
            help_topics::compact_reference().to_string(),
        )),
        Invocation::HelpAll => Ok(Response::Message(help_topics::all_topics_text())),
        Invocation::HelpTopic { topic } => match help_topics::get_help_topic(&topic) {
            Some(text) => Ok(Response::Message(text.to_string())),
            None => Err(AppError::Usage(format!(
                "unknown help topic `{topic}`. Available: {}",
                help_topics::list_topics().join(", ")
            ))),
        },
        Invocation::Version => Ok(Response::Message(format!(
            "story {}",
            env!("CARGO_PKG_VERSION")
        ))),
        other => Err(not_yet_ported(&other)),
    }
}

/// What `story init` tells the user.
///
/// The text still describes the legacy storage model — a `.storyhook/`
/// directory to commit — because it is the text users and scripts see today
/// and byte-compatibility is this port's governing rule. It becomes wrong at
/// the moment the store becomes the identity of record, and the wave that
/// makes that switch owns rewriting it; changing it here would move a
/// user-visible string in a wave whose entire claim is that it moves none.
fn init_message(outcome: &InitOutcome) -> String {
    let mut message = "initialized story project\n\n\
         The .storyhook/ directory contains your project data.\n\
         Remember to commit it to git — it should travel with the repository."
        .to_string();
    if outcome.agents_md {
        message.push_str("\n\nGenerated AGENTS.md for AI agent discoverability.");
    }
    message
}

/// The error an unported [`Invocation`] answers with.
///
/// Loud and specific on purpose. This dispatcher is built additively while the
/// legacy path keeps serving users, so an unported arm has to be impossible to
/// mistake for a working one — in a test, in a differential run, or in the
/// wave that switches the default over.
fn not_yet_ported(invocation: &Invocation) -> AppError {
    AppError::Storage(format!(
        "internal: `{}` is not yet ported to the store-backed dispatcher",
        invocation_name(invocation)
    ))
}

/// An [`Invocation`]'s variant name, for diagnostics.
///
/// An exhaustive match rather than `Debug`: a fifteenth story-lifecycle
/// variant added tomorrow stops this file compiling until somebody decides
/// whether it dispatches, which is the only reliable way to keep an additive
/// port honest.
fn invocation_name(invocation: &Invocation) -> &'static str {
    match invocation {
        Invocation::Help => "help",
        Invocation::Init { .. } => "init",
        Invocation::New { .. } => "new",
        Invocation::MemberAdd { .. } => "member-add",
        Invocation::State { .. } => "state",
        Invocation::List { .. } => "list",
        Invocation::Search { .. } => "search",
        Invocation::Next { .. } => "next",
        Invocation::Summary => "summary",
        Invocation::Report { .. } => "report",
        Invocation::Doctor { .. } => "doctor",
        Invocation::Show { .. } => "show",
        Invocation::Comment { .. } => "comment",
        Invocation::Assign { .. } => "assign",
        Invocation::SetState { .. } => "set-state",
        Invocation::SetAwaiting { .. } => "set-awaiting",
        Invocation::ClearAwaiting { .. } => "clear-awaiting",
        Invocation::SetPriority { .. } => "set-priority",
        Invocation::SetLabels { .. } => "set-labels",
        Invocation::Reopen { .. } => "reopen",
        Invocation::Delete { .. } => "delete",
        Invocation::BulkUpdate { .. } => "bulk-update",
        Invocation::Import { .. } => "import",
        Invocation::Decompose { .. } => "decompose",
        Invocation::Export => "export",
        Invocation::ImportProject { .. } => "import-project",
        Invocation::Context { .. } => "context",
        Invocation::Handoff { .. } => "handoff",
        Invocation::Phase { .. } => "phase",
        Invocation::Type { .. } => "type",
        Invocation::Epic { .. } => "epic",
        Invocation::Graph { .. } => "graph",
        Invocation::SetFields { .. } => "set-fields",
        Invocation::Relate { .. } => "relate",
        Invocation::Hooks { .. } => "hooks",
        Invocation::Scaffold { .. } => "scaffold",
        Invocation::CommitSync { .. } => "commit-sync",
        Invocation::GithubSync { .. } => "github-sync",
        Invocation::HelpTopic { .. } => "help-topic",
        Invocation::HelpCompact => "help-compact",
        Invocation::HelpAll => "help-all",
        Invocation::Plugin { .. } => "plugin",
        Invocation::Web { .. } => "web",
        Invocation::SessionStart => "session-start",
        Invocation::Update { .. } => "update",
        Invocation::Version => "version",
    }
}
