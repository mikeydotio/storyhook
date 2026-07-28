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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app;
use crate::cli::{
    CliOptions, EpicAction, HELP_TEXT, HistoryAction, HooksAction, Invocation, PhaseAction,
    PluginAction, StateAction, TypeAction, WebAction,
};
use crate::domain::{FieldEdit, ImportStory, StateChanges, SuperState};
use crate::error::AppError;
use crate::help_topics;
use crate::output::{Response, render_html_report};
use crate::service::{
    CatalogService, Clock, ConfigService, Ctx, FieldEdits, GitService, GroupingService,
    ImportBatch, InitOptions, InitOutcome, IntegrityService, ListFilters, NewStoryInput,
    PhaseCleared, ProjectService, QueryService, RelationOutcome, RelationService, ReopenOutcome,
    SessionService, StateListing, StoryService, SystemService, TransferService, session, system,
    transfer,
};
use crate::storage::ProjectExport;
use crate::store::{ProjectId, ReadOps, Store};

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
/// **Every [`Invocation`] variant dispatches.** The match is exhaustive without
/// a catch-all, so a new variant stops this file compiling until somebody
/// decides what it does — which is the property the port was building towards
/// and the reason [`not_yet_ported`] now has only one caller left.
///
/// One *action* is still owed a design rather than a port:
/// `History::Restore` replaces a story's history, which an append-only store
/// cannot do; it answers loudly and points at the flip checklist.
///
/// `tests/differential_lifecycle.rs` holds the roster and asserts that it
/// accounts for every variant.
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
        Invocation::Phase { action } => dispatch_phase(ctx, action),
        Invocation::Epic { action } => dispatch_epic(ctx, action),
        Invocation::List {
            state,
            assignee,
            flagged,
            priority,
            label,
            created_after,
            updated_after,
            blocked,
            ready,
            stale,
            phase,
            story_type,
        } => {
            let filters = ListFilters {
                state,
                assignee,
                flagged,
                priority,
                label,
                created_after,
                updated_after,
                blocked,
                ready,
                stale,
                phase,
                story_type,
            };
            query(ctx, |service| service.list(&filters)).map(|views| Response::Stories(views, None))
        }
        Invocation::Show { id } => {
            query(ctx, |service| service.show(&id)).map(|view| Response::Story(Box::new(view)))
        }
        Invocation::Search { query: needle } => query(ctx, |service| service.search(&needle))
            .map(|views| Response::Stories(views, None)),
        Invocation::Next { count, phase } => {
            let mut ready = query(ctx, |service| service.next(count, phase.as_deref()))?;
            // One story is answered as a story, not as a list of one: `story
            // next` is a question with a singular answer, and its `--json`
            // consumers read `.story`.
            if ready.is_empty() {
                Ok(Response::Message("no ready stories".to_string()))
            } else if count == 1 {
                Ok(Response::Story(Box::new(ready.remove(0))))
            } else {
                Ok(Response::Stories(ready, None))
            }
        }
        Invocation::Summary => query(ctx, |service| service.summary())
            .map(|summary| Response::Summary(Box::new(summary))),
        Invocation::Report { html } => {
            if html {
                let data = query(ctx, |service| service.report_data())?;
                let ready: BTreeSet<&str> = data.ready_ids.iter().map(String::as_str).collect();
                let blocked: BTreeSet<&str> = data.blocked_ids.iter().map(String::as_str).collect();
                Ok(Response::Message(render_html_report(
                    &data.summary,
                    &data.stories,
                    &|id| ready.contains(id),
                    &|id| blocked.contains(id),
                )))
            } else {
                query(ctx, |service| service.report_summary())
                    .map(|summary| Response::Summary(Box::new(summary)))
            }
        }
        Invocation::Graph { mode } => {
            query(ctx, |service| service.graph(&mode)).map(|graph| Response::Graph(Box::new(graph)))
        }
        Invocation::Context { format } => {
            let json = format.as_deref() == Some("json");
            query(ctx, |service| service.context(json)).map(Response::Message)
        }
        Invocation::Handoff { since } => {
            query(ctx, |service| service.handoff(since.as_deref())).map(Response::Message)
        }
        Invocation::ProjectSnapshot => query(ctx, |service| service.project_snapshot())
            .map(|view| Response::ProjectSnapshot(Box::new(view))),
        Invocation::Doctor { fix } => {
            let service = IntegrityService::new(ctx);
            if fix {
                service.fix().map(Response::Message)
            } else {
                match service.report()? {
                    issues if issues.is_empty() => Ok(Response::Issues(Vec::new())),
                    issues => Err(AppError::Integrity(issues.join("\n"))),
                }
            }
        }
        Invocation::SessionStart => SessionService::new(ctx)
            .context()
            .map(Response::RawJson),
        Invocation::History { action } => match action {
            HistoryAction::Read { id } => session::history(ctx, &id).map(Response::StoryHistory),
            // Restoring means *replacing* a story's history, which an
            // append-only store cannot do and which needs a compensating-event
            // design rather than a port. `docs/rearch/flip-checklist.md`
            // category C carries the item; until it is answered the TUI's undo
            // works on the legacy path only.
            HistoryAction::Restore { .. } => Err(AppError::Storage(
                "internal: `history restore` is not yet ported to the store-backed                  dispatcher — see docs/rearch/flip-checklist.md, category C"
                    .to_string(),
            )),
        },
        Invocation::Web { action } => dispatch_web(ctx.store(), action),
        Invocation::Update { check, force } => update(check, force),
        Invocation::GithubSync { id, dry_run } => {
            #[cfg(feature = "github-sync")]
            {
                crate::service::GithubSyncService::new(ctx).sync(id.as_deref(), dry_run)
            }
            #[cfg(not(feature = "github-sync"))]
            {
                let _ = (id, dry_run);
                Err(AppError::Usage(
                    "github-sync requires the `github-sync` feature. \
                     Rebuild with: cargo install storyhook --features github-sync"
                        .to_string(),
                ))
            }
        }
        Invocation::CommitSync { since } => GitService::new(ctx)
            .commit_sync(since.as_deref())
            .map(Response::Message),
        Invocation::Export => TransferService::new(ctx)
            .export()
            .and_then(|export| Ok(serde_json::to_string_pretty(&export)?))
            // `RawJson`, not `Message`: the export document *is* the result, so
            // wrapping it in the `--json` envelope would make it an escaped
            // string that `story import-project` then refuses.
            .map(Response::RawJson),
        Invocation::Import { file } => {
            let stories: Vec<ImportStory> = serde_json::from_str(&read_input(file.as_deref())?)?;
            if stories.is_empty() {
                return Ok(Response::Message("no stories to import".to_string()));
            }
            TransferService::new(ctx)
                .import(&stories)
                .map(|batch| Response::Stories(batch.views, None))
        }
        Invocation::Decompose {
            file,
            stdin,
            dry_run,
        } => {
            let content = decompose_input(file.as_deref(), stdin)?;
            let stories = crate::decompose::decompose(file.as_deref(), &content)?;
            if dry_run {
                return Ok(Response::Message(serde_json::to_string_pretty(&stories)?));
            }
            if stories.is_empty() {
                return Ok(Response::Message("no stories to import".to_string()));
            }
            let batch = TransferService::new(ctx).import(&stories)?;
            let summary = decompose_summary(&batch);
            Ok(Response::Stories(batch.views, Some(summary)))
        }
        Invocation::ImportProject { .. }
        | Invocation::Init { .. }
        | Invocation::Help
        | Invocation::HelpTopic { .. }
        | Invocation::HelpCompact
        | Invocation::HelpAll
        | Invocation::Version => dispatch_unscoped(ctx.store(), ctx.cwd(), &ctx.now(), invocation),
    }
}

/// Runs one read-only question against the project, in its own read
/// transaction.
///
/// The service handed to `f` borrows a [`ReadOps`](crate::store::ReadOps)
/// transaction and nothing else, so a query arm is *structurally* unable to
/// write — there is no store in scope to write to.
fn query<S: Store, T>(
    ctx: &Ctx<'_, S>,
    f: impl FnOnce(&QueryService<'_, S::ReadTx<'_>>) -> Result<T, AppError>,
) -> Result<T, AppError> {
    let now = ctx.now();
    ctx.store()
        .read(|tx| Ok(f(&QueryService::new(tx, ctx.project(), &now))))?
}

/// The `story web …` family.
///
/// Only the catalog arms are ported. The daemon commands — start, stop, status,
/// open, address — are process management rather than storage, they are
/// identical on both sides of the flip, and the wave that promotes the daemon
/// owns them; they delegate to the same functions the legacy path calls.
fn dispatch_web<S: Store>(store: &S, action: WebAction) -> Result<Response, AppError> {
    let service = CatalogService::new(store);
    match action {
        WebAction::Register { path, name } => {
            let entry = service.register(Path::new(&path), name.as_deref())?;
            Ok(Response::Message(format!(
                "Registered `{}` as `{}`",
                entry.path.unwrap_or_default().display(),
                entry.id
            )))
        }
        WebAction::Deregister { target } => {
            let entry = service.deregister(&target)?;
            Ok(Response::Message(format!(
                "Deregistered `{}` ({})",
                entry.id,
                entry.path.unwrap_or_default().display()
            )))
        }
        WebAction::List => {
            let entries = service.list()?;
            if entries.is_empty() {
                return Ok(Response::Message(
                    "No repos registered. Run `story web register` from a project to add one."
                        .to_string(),
                ));
            }
            let mut lines = vec![format!("{} registered repo(s):", entries.len())];
            for entry in &entries {
                lines.push(format!(
                    "  {} — {} ({})",
                    entry.id,
                    entry.name,
                    entry.path.clone().unwrap_or_default().display()
                ));
            }
            Ok(Response::Message(lines.join("\n")))
        }
        WebAction::Start { port } => crate::web::handle_start(port).map(Response::Message),
        WebAction::Stop => crate::web::handle_stop().map(Response::Message),
        WebAction::Status => crate::web::handle_status().map(Response::Message),
        WebAction::Open => crate::web::handle_open().map(Response::Message),
        WebAction::Address => crate::web::handle_address().map(Response::Message),
        // `main` intercepts this before any dispatcher sees it: the foreground
        // server is a process that never returns, not a command with an answer.
        WebAction::Serve { .. } => Err(AppError::Usage(
            "`story web --serve` is handled before dispatch".to_string(),
        )),
    }
}

/// `story update` — self-update, which touches no project data at all.
fn update(check: bool, force: bool) -> Result<Response, AppError> {
    #[cfg(feature = "github-sync")]
    {
        crate::update::run(check, force).map(Response::Message)
    }
    #[cfg(not(feature = "github-sync"))]
    {
        let _ = (check, force);
        Err(AppError::Usage(
            "self-update requires the `github-sync` feature. \
             Reinstall via the official installer \
             (curl -fsSL https://raw.githubusercontent.com/mikeydotio/storyhook/main/install.sh | sh) \
             or rebuild with: cargo install storyhook --features github-sync"
                .to_string(),
        ))
    }
}

/// The `story phase …` family.
fn dispatch_phase<S: Store>(ctx: &Ctx<'_, S>, action: PhaseAction) -> Result<Response, AppError> {
    let service = GroupingService::new(ctx);
    match action {
        PhaseAction::List => service.phases().map(Response::PhaseList),
        PhaseAction::Show { phase } => service
            .phase_stories(&phase)
            .map(|views| Response::Stories(views, None)),
        PhaseAction::Add { id, phase } => {
            service.assign_phase(&id, &phase)?;
            Ok(Response::Message(format!("assigned {id} to phase {phase}")))
        }
        PhaseAction::Remove { id } => Ok(Response::Message(match service.clear_phase(&id)? {
            PhaseCleared::Removed(_) => format!("removed phase assignment from {id}"),
            PhaseCleared::NoAssignment => format!("{id} has no phase assignment"),
        })),
        PhaseAction::Create { phase, title } => {
            let story = service.create_phase(&phase, title.as_deref())?;
            ctx.story_view(&story.id)
        }
    }
}

/// The `story epic …` family.
fn dispatch_epic<S: Store>(ctx: &Ctx<'_, S>, action: EpicAction) -> Result<Response, AppError> {
    let service = GroupingService::new(ctx);
    match action {
        EpicAction::List => service.epics().map(|views| Response::Stories(views, None)),
        EpicAction::Show { id } => ctx.story_view(&id),
        EpicAction::Create { title } => {
            let story = service.create_epic(&title)?;
            ctx.story_view(&story.id)
        }
        EpicAction::Add { epic_id, story_id } => {
            service.add_to_epic(&epic_id, &story_id)?;
            ctx.story_view(&epic_id)
        }
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
    // The pointer file is the store's claim on the repository, and while the
    // legacy tree is still the identity of record a second claim could only
    // ever disagree with it. `StoreInvoker` — which serves a process for which
    // the store *is* the identity of record — asks for it explicitly.
    dispatch_unscoped_with(store, root, now, invocation, false)
}

/// [`dispatch_unscoped`], told whether `story init` should write the pointer
/// file.
pub fn dispatch_unscoped_with<S: Store>(
    store: &S,
    root: &Path,
    now: &str,
    invocation: Invocation,
    pointer: bool,
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
                    pointer,
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
        // Parsing a spec is a pure function of its text. `--dry-run` prints the
        // stories it *would* create and writes nothing, which the legacy path
        // answered before it ever looked for a project — so this arm does too,
        // and an agent can check its plan in a directory storyhook has never
        // heard of.
        Invocation::Decompose {
            file,
            stdin,
            dry_run,
        } => {
            let content = decompose_input(file.as_deref(), stdin)?;
            let stories = crate::decompose::decompose(file.as_deref(), &content)?;
            if !dry_run {
                return Err(AppError::Storage(
                    "internal: a writing `decompose` reached the project-less dispatcher"
                        .to_string(),
                ));
            }
            Ok(Response::Message(serde_json::to_string_pretty(&stories)?))
        }
        // A directory, not a project: these write `.git/hooks`, read
        // `hooks.toml`, or install an editor plugin, and the legacy path
        // answered all of them in a directory storyhook had never heard of.
        Invocation::Hooks { action } => match action {
            HooksAction::Install => system::install_git_hooks(root).map(Response::Message),
            HooksAction::Uninstall => system::uninstall_git_hooks(root).map(Response::Message),
            HooksAction::List => Ok(Response::Message(system::list_event_hooks(root))),
            // `hooks test` fires a real hook against a real project; it is
            // routed to `dispatch` instead and never arrives here.
            HooksAction::Test { .. } => Err(not_yet_ported(&Invocation::Hooks {
                action: HooksAction::Test {
                    event_type: String::new(),
                },
            })),
        },
        Invocation::Plugin { action } => match action {
            PluginAction::Install { target } => system::install_plugin(&target, root),
            PluginAction::Uninstall { target } => system::uninstall_plugin(&target, root),
        }
        .map(Response::Message),
        // Reached only when no project could be resolved. `claude-md` and
        // `cursor-rules` take nothing from a project at all; `agents-md` falls
        // back to the default prefix and `done`, which is exactly what the
        // legacy path printed in an uninitialized directory. A `scaffold` that
        // refused outside a project would be a user-visible regression in a
        // command whose whole purpose is to be run before anything else.
        Invocation::Scaffold { kind } => match kind.as_str() {
            "agents-md" => Ok(Response::Message(crate::service::templates::agents_md(
                crate::service::project::DEFAULT_PREFIX,
                "done",
            ))),
            "claude-md" => Ok(Response::Message(crate::service::templates::claude_md())),
            "cursor-rules" => Ok(Response::Message(crate::service::templates::cursor_rules())),
            _ => Err(AppError::Usage(
                "usage: story scaffold agents-md|claude-md|cursor-rules".to_string(),
            )),
        },
        // Project-less for the same reason `init` is: `story import-project`
        // into an empty directory is how a backup is restored, so the arm has
        // to be able to create the project it is importing into.
        Invocation::ImportProject { file } => {
            let raw = std::fs::read_to_string(&file)
                .map_err(|e| AppError::Storage(format!("failed to read {file}: {e}")))?;
            let export: ProjectExport = serde_json::from_str(&raw)?;
            let imported =
                transfer::import_project(store, root, &Clock::Fixed(now.to_string()), &export)?;
            Ok(Response::Message(format!(
                "imported project with {imported} stories"
            )))
        }
        other => Err(not_yet_ported(&other)),
    }
}

/// Reads a command's input document from a file, or from standard input when
/// no file is named.
///
/// One helper for the two commands that take a document, because they disagreed
/// about the wording of the failure: `import` said `failed to read stdin` and
/// `decompose` said the same, but only one of them said which *file* it could
/// not read.
fn read_input(file: Option<&str>) -> Result<String, AppError> {
    match file {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| AppError::Storage(format!("failed to read {path}: {e}"))),
        None => {
            use std::io::Read as _;
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|e| AppError::Storage(format!("failed to read stdin: {e}")))?;
            Ok(buffer)
        }
    }
}

/// The spec text `story decompose` was pointed at.
///
/// One helper for both dispatchers, because a dry run is answered without a
/// project and a real one is not — and the two must not disagree about which
/// argument combinations are usable.
fn decompose_input(file: Option<&str>, stdin: bool) -> Result<String, AppError> {
    if stdin {
        return read_input(None);
    }
    match file {
        Some(path) => read_input(Some(path)),
        None => Err(AppError::Usage(
            "usage: story decompose <file> [--dry-run] | story decompose --stdin [--dry-run]"
                .to_string(),
        )),
    }
}

/// The `Created 3 stories with 2 relationships:` block `story decompose` prints
/// under the stories it created.
fn decompose_summary(batch: &ImportBatch) -> String {
    let stories = batch.views.len();
    let relations = batch.relationship_lines.len();
    let mut summary = format!(
        "Created {stories} {} with {relations} {}",
        if stories == 1 { "story" } else { "stories" },
        if relations == 1 {
            "relationship"
        } else {
            "relationships"
        },
    );
    if !batch.relationship_lines.is_empty() {
        summary.push(':');
        for line in &batch.relationship_lines {
            summary.push_str(&format!("\n  {line}"));
        }
    }
    summary
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
/// Loud and specific on purpose. Only [`dispatch_unscoped`] still reaches it,
/// and only for a variant handed to the project-less entry point that does not
/// belong there — a caller mistake rather than an unfinished port. It stays
/// because a silent fallback to the legacy path is exactly what this must not
/// do.
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
        Invocation::ProjectSnapshot => "project-snapshot",
        Invocation::History { .. } => "history",
    }
}

/// Runs one invocation against the store, resolving the project from the
/// working directory first.
///
/// The counterpart to [`LegacyInvoker`]: same seam, same envelope, the new
/// stack underneath. It exists before the flip so that the whole integration
/// suite can be run against the store — `STORYHOOK_INVOKER=local` — which is
/// how the flip's surprises are found while the legacy path is still the
/// default and a surprise is cheap.
///
/// # Root resolution
///
/// The project is the one registered at the working directory, or the one the
/// directory's pointer file names. It does **not** walk upwards, because the
/// legacy path does not: `ensure_project` looks for `<cwd>/.storyhook` and
/// nowhere else, and a store leg that resolved a parent's project would answer
/// questions the legacy leg refuses.
pub struct StoreInvoker<'a, S: Store> {
    store: &'a S,
    cwd: PathBuf,
    hook_depth: u32,
    pointer: bool,
}

impl<'a, S: Store> StoreInvoker<'a, S> {
    /// An invoker over `store`, running from `cwd`.
    ///
    /// Writes the pointer file on `story init`, because for a process served
    /// this way the store *is* where the project lives, and a checkout with no
    /// pointer is one a fresh clone cannot identify.
    pub fn new(store: &'a S, cwd: impl Into<PathBuf>) -> Self {
        Self {
            store,
            cwd: cwd.into(),
            hook_depth: 0,
            pointer: true,
        }
    }

    /// Sets how deep inside an event hook this invocation is running.
    #[must_use]
    pub fn hook_depth(mut self, hook_depth: u32) -> Self {
        self.hook_depth = hook_depth;
        self
    }

    /// Sets whether `story init` writes the pointer file.
    #[must_use]
    pub fn pointer(mut self, pointer: bool) -> Self {
        self.pointer = pointer;
        self
    }

    /// The project this working directory belongs to, if any.
    fn resolve(&self) -> Result<Option<ProjectId>, AppError> {
        let root = self.cwd.canonicalize().unwrap_or_else(|_| self.cwd.clone());
        let pointer = crate::service::project::read_pointer(&root)?;
        Ok(self.store.read(|tx| {
            // The pointer file wins: it is the identity that travels with the
            // repository, so a checkout that was moved on disk still resolves
            // to the project it has always been.
            if let Some(pointer) = &pointer
                && let Some(project) = tx.project_by_uuid(&pointer.uuid)?
            {
                return Ok(Some(project.id));
            }
            Ok(tx.project_by_path(&root)?.map(|project| project.id))
        })?)
    }
}

impl<S: Store> Invoker for StoreInvoker<'_, S> {
    fn invoke(&self, request: InvokeRequest) -> Result<Response, AppError> {
        let now = Clock::System.now();
        if is_project_less(&request.invocation) {
            return dispatch_unscoped_with(
                self.store,
                &self.cwd,
                &now,
                request.invocation,
                self.pointer,
            );
        }

        let Some(project) = self.resolve()? else {
            // `session-start` is a hook, and a hook that reports an error
            // writes that error into a model's context. Silence is its answer
            // for a directory storyhook has never heard of.
            if matches!(request.invocation, Invocation::SessionStart) {
                return Ok(Response::RawJson(
                    crate::service::session::SILENT.to_string(),
                ));
            }
            // `scaffold` degrades rather than refuses: it prints instruction
            // files, and the legacy path printed them with default values in a
            // directory it knew nothing about.
            if matches!(request.invocation, Invocation::Scaffold { .. }) {
                return dispatch_unscoped_with(
                    self.store,
                    &self.cwd,
                    &now,
                    request.invocation,
                    self.pointer,
                );
            }
            return Err(AppError::NotFound(
                "story project not initialized in this directory; run `story init`".to_string(),
            ));
        };

        let ctx = Ctx::new(self.store, project, &self.cwd)
            .no_hooks(request.no_hooks)
            .hook_depth(self.hook_depth);
        dispatch(&ctx, request.invocation)
    }
}

/// Whether an invocation is answered without resolving a project.
///
/// The list is `dispatch`'s own forwarding set plus `import-project`, and it is
/// an exhaustive-by-inspection match rather than a `matches!` so that the two
/// cannot drift: adding a project-less arm to `dispatch` without adding it here
/// makes `story <verb>` fail in an empty directory, which is exactly the
/// failure this function exists to prevent.
fn is_project_less(invocation: &Invocation) -> bool {
    match invocation {
        Invocation::Init { .. }
        | Invocation::ImportProject { .. }
        | Invocation::Help
        | Invocation::HelpTopic { .. }
        | Invocation::HelpCompact
        | Invocation::HelpAll
        | Invocation::Version
        | Invocation::Plugin { .. } => true,
        // `hooks test` is the exception in its own family: it fires a real hook
        // against a real project, and the legacy path calls `ensure_project`
        // before it does.
        Invocation::Hooks { action } => !matches!(action, HooksAction::Test { .. }),
        // A dry run parses and prints; only a real one writes stories.
        Invocation::Decompose { dry_run, .. } => *dry_run,
        _ => false,
    }
}
