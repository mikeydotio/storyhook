//! The seam between *deciding what to do* and *doing it*.
//!
//! Everything that can run a storyhook command — the CLI, the TUI and the web
//! dashboard — goes through [`Invoker`]. The trait was introduced when there
//! was only one implementation and nothing to choose between, which is exactly
//! what made adopting it provably behaviour-preserving; the flip was then a
//! constructor swap rather than a rewrite of every call site.
//!
//! Two implementations, and they are not peers:
//!
//! * [`StoreInvoker`] runs the work in this process, against the store. It is
//!   the **executor**, not a transport: the daemon uses it to run what a client
//!   sent, and the TUI uses it to run its own work.
//! * [`HttpInvoker`] sends the work to the daemon, which runs it against the
//!   store there. It is the CLI's **only** door. A second one, `--local`, chose
//!   `StoreInvoker` from `main` and was deleted in SH-114.
//!
//! A third, `LegacyInvoker`, forwarded to the pre-rearchitecture `app::run`
//! and read and wrote `.storyhook/` directly. It was quarantined at the flip
//! and deleted with `app.rs` once the dashboard — its last caller — moved onto
//! the services. `tests/invoker_seam.rs` now asserts that neither it nor
//! anything it reached can come back.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::{
    Attach, DaemonAction, EpicAction, HELP_TEXT, HistoryAction, HooksAction, Invocation,
    NewProjectRequest, PhaseAction, PluginAction, ProjectAction, SettingsAction, StateAction,
    TypeAction, WebAction,
};
use crate::domain::{FieldEdit, ImportStory, StateChanges, SuperState};
use crate::env::Environment;
use crate::error::AppError;
use crate::help_topics;
use crate::output::{ConfirmationPlan, Response, render_html_report};
use crate::service::transfer::ProjectExport;
use crate::service::{
    CatalogService, Clock, ConfigService, Ctx, DeleteOutcome, FieldEdits, GitService,
    GroupingService, ImportBatch, InitOptions, InitOutcome, IntegrityService, ListFilters,
    NewStoryInput, PhaseCleared, ProjectService, QueryService, RelationOutcome, RelationService,
    ReopenOutcome, SessionService, SettingsService, StateListing, StoryService, SystemService,
    TransferService, migrate, session, system, transfer,
};
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
    /// The standard input this invocation should read.
    ///
    /// `None` means "this process's own", which is what a local run wants. A
    /// request that crossed a wire carries the content instead: the daemon has
    /// no way to reach the terminal the user is typing into.
    #[serde(default)]
    pub stdin: Option<String>,
    /// The project this invocation names, and how it named it.
    ///
    /// `None` means "nothing named one", which sends the resolver to the working
    /// directory. It does **not** mean "fall back to a default": there is no
    /// default, and a `Some` naming no project is refused rather than falling
    /// through — which is the whole of SH-116's refuse-don't-guess invariant,
    /// expressed as a type rather than as a check. See
    /// [`ProjectSelector`](crate::api::wire::ProjectSelector).
    #[serde(default)]
    pub project: Option<crate::api::wire::ProjectSelector>,
}

impl InvokeRequest {
    /// A request to run `invocation` with hooks enabled.
    pub fn new(invocation: Invocation) -> Self {
        Self {
            invocation,
            no_hooks: false,
            stdin: None,
            project: None,
        }
    }

    /// Supplies the standard input this invocation should read.
    #[must_use]
    pub fn stdin(mut self, stdin: Option<String>) -> Self {
        self.stdin = stdin;
        self
    }

    /// Names the project this invocation acts on.
    #[must_use]
    pub fn project(mut self, project: Option<crate::api::wire::ProjectSelector>) -> Self {
        self.project = project;
        self
    }

    /// Sets whether event hooks are suppressed.
    #[must_use]
    pub fn no_hooks(mut self, no_hooks: bool) -> Self {
        self.no_hooks = no_hooks;
        self
    }

    /// The same request, with its confirmation already given.
    ///
    /// The second half of the two-step a destructive command runs: the first
    /// invocation answers [`Response::ConfirmationRequired`] and writes
    /// nothing, the client asks the user, and this is what it sends back. The
    /// invocation is otherwise untouched — the *same* target, resolved the
    /// same way — so the thing that gets destroyed is the thing that was
    /// described.
    ///
    /// A request with nothing to confirm is returned unchanged, which is what
    /// makes this safe to call unconditionally.
    ///
    /// # Why the `Project` arm is exhaustive
    ///
    /// It used to be `ProjectAction::Deinit { force, .. }` beside a `_ => {}`,
    /// and a destructive project verb added later would have fallen through it
    /// silently — the client would ask the user, get a yes, re-send a request
    /// that is still unforced, and be answered with the same question forever.
    /// A confirmation loop with no error and no compile failure. Listing every
    /// variant means the next one is a compile error here instead.
    #[must_use]
    pub fn forced(mut self) -> Self {
        match &mut self.invocation {
            Invocation::Project { action } => match action {
                ProjectAction::Delete { force } => *force = true,
                ProjectAction::New(_)
                | ProjectAction::List
                | ProjectAction::Link(_)
                | ProjectAction::Unlink(_)
                | ProjectAction::Settings(_) => {}
            },
            Invocation::Purge { force, .. } => *force = true,
            _ => {}
        }
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

/// Opens the machine's store, ready to serve an invocation.
///
/// Three steps, in this order, and every entry point takes all three:
///
/// 1. Open `$STORYHOOK_DATA_DIR`/`$XDG_DATA_HOME`'s `store.db`, creating it if
///    this is the first run.
/// 2. Migrate. Schema changes are applied on open rather than by a separate
///    command, behind a verified backup, so a binary can never read a database
///    it does not understand.
/// 3. Adopt the legacy dashboard registry, if there is one — recording the
///    checkouts it names against the projects they belong to. Idempotent and
///    non-destructive: `registry.toml` is neither written nor deleted, because
///    the legacy dashboard still reads it.
///
/// Shared by the CLI and the TUI so that "where is the store, and what does
/// opening it entail" has one answer rather than one per entry point.
pub fn open_store(env: &Environment) -> Result<crate::store::SqliteStore, AppError> {
    use crate::store::Store as _;
    let mut config = crate::store::StoreConfig::new(env.store_path());
    // Pre-migration backups join the daily snapshots under the state home
    // rather than sitting beside the database: `data_home` is what a user might
    // point at a synced directory, and a backup of a database is exactly the
    // thing that should not be synced back over the database.
    config.backup_dir = env.backups_dir();
    config.busy_timeout = env.busy_timeout_value();
    let store = crate::store::SqliteStore::open_with(config)?;
    store.migrate()?;
    // A failure here is not a reason to refuse the command: the registry
    // belongs to the dashboard, and every storyhook command works without it.
    let _ = crate::service::adopt_legacy_registry(
        &store,
        &env.legacy_global_dir().join("registry.toml"),
    );
    Ok(store)
}

/// `story store new <path>` — creates an empty store where nothing is.
///
/// **Deliberately not routed through an [`Environment`].** Every other command
/// resolves the ambient store before it does anything, and doing that here would
/// have two consequences, both wrong: creating the real store as a side effect
/// of asking for a different one, and refusing outright in a test build — which
/// is the one build that most needs to be able to make a scratch store.
///
/// The default store is refused because it is the daemon's to create on first
/// run. Creating it by hand would put an empty database where the daemon expects
/// either nothing or its own, and the failure would surface later as a tracker
/// that had lost everything.
pub fn create_store(cwd: &Path, requested: &str) -> Result<Response, AppError> {
    use crate::store::Store as _;

    let requested = crate::env::canonical_ish(&resolve_against(cwd, requested))?;
    let default = crate::env::default_store_path()?;
    if requested == default {
        return Err(AppError::Validation(format!(
            "refusing to create `{}`: that is the default store, which storyhook's daemon \
             creates for itself on first run.\n\nNothing has been written. `story store new` is \
             for a store *beside* the default one — a scratch store for a test suite, or a \
             second tracker — so give it a path of its own and reach it with `--store-path` or \
             $STORYHOOK_STORE_PATH.",
            requested.display()
        )));
    }
    if requested.exists() {
        return Err(AppError::Validation(format!(
            "refusing to create `{}`: it already exists.\n\nNothing has been written. Use it \
             with `--store-path {}`, or delete it first if you meant to start over.",
            requested.display(),
            requested.display()
        )));
    }
    if let Some(parent) = requested.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AppError::Storage(format!(
                "failed to create `{}` for the new store: {e}",
                parent.display()
            ))
        })?;
    }

    let store = crate::store::SqliteStore::open(&requested)?;
    store.migrate()?;
    Ok(Response::Message(format!(
        "Created an empty store at {}\nUse it with `story --store-path {} <command>`.",
        requested.display(),
        requested.display()
    )))
}

/// Runs one invocation against the store, in this process.
///
/// This is the stack's entry point, and what replaced the pre-rearchitecture
/// `app::run`. It is deliberately thin: every arm validates the
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
        // Two-step, exactly as `project delete` is: an unforced purge answers
        // with what it would destroy and writes nothing. The client decides
        // whether to ask, because the client is the process with a terminal.
        Invocation::Purge { id, force } => {
            let service = StoryService::new(ctx);
            if force {
                service.purge(&id).map(Response::Message)
            } else {
                Ok(Response::ConfirmationRequired(Box::new(
                    ConfirmationPlan::Purge(service.purge_plan(&id)?),
                )))
            }
        }
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
            let catalog = crate::service::CatalogService::new(ctx.store());
            // An orphaned registration in a *throwaway* store is not a finding:
            // fixtures are supposed to disappear, and reporting them there
            // would make `doctor` depend on which sibling fixtures happened to
            // have been dropped yet.
            let audit_catalog = !crate::service::project::is_under_temp(ctx.env().store_path());
            if fix {
                let mut message = service.fix()?;
                if audit_catalog {
                    let forgotten = catalog.deregister_orphaned()?;
                    if !forgotten.is_empty() {
                        message.push('\n');
                        message.push_str(&deregistered_message(&forgotten));
                    }
                }
                Ok(Response::Message(message))
            } else {
                match service.report()? {
                    issues if issues.is_empty() => {
                        let orphans = if audit_catalog {
                            catalog.orphaned()?
                        } else {
                            Vec::new()
                        };
                        Ok(Response::Issues(orphan_advice(&orphans)))
                    }
                    issues => Err(AppError::Integrity(issues.join("\n"))),
                }
            }
        }
        Invocation::SessionStart => SessionService::new(ctx).context().map(Response::RawJson),
        Invocation::History { action } => match action {
            HistoryAction::Read { id } => session::history(ctx, &id).map(Response::StoryHistory),
            // Restoring does not replace a story's history — an append-only
            // store cannot, and an audit trail whose entries can be deleted is
            // not one. `service::history::restore` appends the events that
            // carry the story back to what the given log folds to. See that
            // module for what it changes for a user.
            HistoryAction::Restore { id, events } => {
                crate::service::history::restore(ctx, &id, &events)?;
                ctx.story_view(&id)
            }
        },
        Invocation::GithubSync {
            id,
            dry_run,
            resolve,
        } => {
            #[cfg(feature = "github-sync")]
            {
                crate::service::GithubSyncService::new(ctx).sync(id.as_deref(), dry_run, resolve)
            }
            #[cfg(not(feature = "github-sync"))]
            {
                let _ = (id, dry_run, resolve);
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
            let stories: Vec<ImportStory> =
                serde_json::from_str(&read_input(ctx.cwd(), ctx.stdin(), file.as_deref())?)?;
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
            let content = decompose_input(ctx.cwd(), ctx.stdin(), file.as_deref(), stdin)?;
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
        // The `project` arms that name a project rather than creating,
        // destroying or enumerating them, so the only ones answered here.
        Invocation::Project {
            action: ProjectAction::Settings(action),
        } => dispatch_project_settings(ctx, action),
        Invocation::Project {
            action: ProjectAction::Link(target),
        } => dispatch_project_link(ctx, target),
        Invocation::Project {
            action: ProjectAction::Unlink(target),
        } => dispatch_project_unlink(ctx, target),
        Invocation::Project {
            action: ProjectAction::Delete { force },
        } => dispatch_project_delete(ctx, force),
        Invocation::Web { .. }
        | Invocation::Daemon { .. }
        | Invocation::Store { .. }
        | Invocation::Update { .. }
        | Invocation::ImportProject { .. }
        | Invocation::Migrate { .. }
        | Invocation::Project { .. }
        | Invocation::Help
        | Invocation::HelpTopic { .. }
        | Invocation::HelpCompact
        | Invocation::HelpAll
        | Invocation::Version => dispatch_unscoped_with(
            ctx.store(),
            ctx.cwd(),
            &ctx.now(),
            invocation,
            true,
            ctx.stdin(),
        ),
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

/// The `story project …` family — a repository's whole lifecycle.
///
/// `root` is the directory the client ran in; an arm that takes a `PATH`
/// resolves it against that with [`target_dir`] rather than against this
/// process's own working directory, which over the daemon is not the user's.
fn dispatch_project<S: Store>(
    store: &S,
    root: &Path,
    now: &str,
    pointer: bool,
    action: ProjectAction,
) -> Result<Response, AppError> {
    match action {
        // The client is the only process with a terminal, so it is the only one
        // that can turn `Ask` into an answer. Reaching here means a caller went
        // round `main.rs` — over the daemon, through a hand-built
        // `InvokeRequest`, from a front-end that has not been taught the rule.
        // Refused rather than defaulted: quietly supplying a prefix nobody
        // chose is SH-109's silent `SH` wearing a new verb.
        ProjectAction::New(NewProjectRequest::Ask) => Err(AppError::Validation(
            "`story project new` was given nothing to work from and there is nobody here to \
             ask.\n\nPass the answers on the command line — `--prefix` is the only required \
             one:\n\n  story project new --prefix <PREFIX>"
                .to_string(),
        )),
        ProjectAction::New(NewProjectRequest::Stated(spec)) => {
            let attach = !matches!(spec.attach, Attach::Nothing);
            let target = match &spec.attach {
                Attach::Cwd | Attach::Nothing => root.to_path_buf(),
                Attach::Path(path) => target_dir(root, Some(path)),
            };
            // Named explicitly rather than left to fail later inside
            // `write_pointer`, and asked only when the directory is going to be
            // used: `--no-attach` does not read it at all.
            if attach && !target.is_dir() {
                return Err(AppError::NotFound(format!(
                    "cannot attach `{}`: no such directory",
                    target.display()
                )));
            }
            let outcome = ProjectService::new(store, target)
                .clock(Clock::Fixed(now.to_string()))
                .init(&InitOptions {
                    prefix: Some(spec.prefix),
                    name: spec.name,
                    agents_md: !spec.no_agents_md,
                    pointer,
                    attach,
                })?;
            let slug = store
                .read(|tx| tx.project(outcome.project))?
                .map(|p| p.slug);
            Ok(Response::Message(new_message(&outcome, attach, &slug)))
        }
        ProjectAction::List => {
            let entries = CatalogService::new(store).all()?;
            if entries.is_empty() {
                return Ok(Response::Message(
                    "No projects yet. Run `story project new` in a repository to add one."
                        .to_string(),
                ));
            }
            let mut lines = vec![format!("{} project(s):", entries.len())];
            for entry in &entries {
                let where_ = entry.path.as_ref().map_or_else(
                    || "no checkout on this machine".to_string(),
                    |path| path.display().to_string(),
                );
                lines.push(format!("  {} — {} ({where_})", entry.id, entry.name));
                // The two git associations, indented under the project they
                // belong to. This is the only way to answer "did my link take?"
                // short of opening the database — and it is what gives
                // `checkout_path` a production reader, since its real consumer
                // (`dispatch`) is SH-120's.
                //
                // The parenthesis above is the *recorded path*, which is what
                // resolution reads; the `checkout` line below is the linked one,
                // which nothing resolves by. They are printed separately because
                // they are different facts, and SH-119 collapses them.
                let (checkout, origins) = store.read(|tx| {
                    Ok((
                        tx.checkout_path(entry.project)?,
                        tx.project_remotes(entry.project)?,
                    ))
                })?;
                if let Some(checkout) = checkout {
                    lines.push(format!("      checkout  {}", checkout.display()));
                }
                for remote in origins {
                    lines.push(format!("      origin    {}", remote.raw));
                }
            }
            Ok(Response::Message(lines.join("\n")))
        }
        // Answered by `dispatch` against a resolved context, because these are
        // the arms of this family that name a project. Reaching one here means a
        // caller went round `is_project_less`.
        ProjectAction::Settings(_) => Err(AppError::Usage(
            "`story project settings` needs a project: run it in a checkout storyhook knows, \
             or run `story project new` first."
                .to_string(),
        )),
        ProjectAction::Link(_) | ProjectAction::Unlink(_) => Err(AppError::Usage(
            "`story project link` and `story project unlink` need a project: name one with \
             `--project <slug>`, or run them in a checkout storyhook already resolves."
                .to_string(),
        )),
        ProjectAction::Delete { .. } => Err(AppError::Usage(
            "`story project delete` needs a project: name one with `--project <slug>`, or run \
             it in a checkout storyhook already resolves."
                .to_string(),
        )),
    }
}

/// `story project delete [--force]` against a resolved project.
///
/// The two-step is the whole of it. An unforced request answers with the plan
/// and writes nothing; the client — the only process with a terminal — turns
/// that into a prompt, and sends the same request back with `force` set. The
/// invocation is otherwise untouched, so the thing destroyed is the thing the
/// plan described.
fn dispatch_project_delete<S: Store>(ctx: &Ctx<'_, S>, force: bool) -> Result<Response, AppError> {
    let service =
        ProjectService::new(ctx.store(), ctx.cwd()).clock(Clock::Fixed(ctx.now().to_string()));
    if !force {
        return Ok(Response::ConfirmationRequired(Box::new(
            ConfirmationPlan::Delete(service.delete_plan(ctx.project())?),
        )));
    }
    let outcome = service.delete(ctx.project())?;
    Ok(Response::Message(delete_message(&outcome)))
}

/// `story project link origin|checkout …` against a resolved project.
fn dispatch_project_link<S: Store>(
    ctx: &Ctx<'_, S>,
    target: crate::cli::LinkTarget,
) -> Result<Response, AppError> {
    let service = crate::service::GitLinkService::new(ctx);
    match target {
        crate::cli::LinkTarget::Origin { url } => {
            let remote = remote_argument(ctx, url.as_deref())?;
            let link = service.link_origin(&remote)?;
            Ok(Response::Message(format!(
                "linked `{}` to project `{}`\ncommands run in a checkout of it now resolve \
                 without --project",
                link.raw, link.project
            )))
        }
        crate::cli::LinkTarget::Checkout { path } => {
            let target = target_dir(ctx.cwd(), path.as_deref());
            let link = service.link_checkout(&target)?;
            // The replaced path is reported rather than dropped: a silent
            // replacement is how somebody discovers weeks later that repo-side
            // work has been running in a tree they stopped using.
            let replaced = link.replaced.as_ref().map_or_else(String::new, |old| {
                format!(", replacing `{}`", old.display())
            });
            Ok(Response::Message(format!(
                "linked checkout `{}` to project `{}`{replaced}\nthis is where repo-side work \
                 runs for it; it is not used to decide which project you are in",
                target.display(),
                link.project
            )))
        }
    }
}

/// `story project unlink origin|checkout …` against a resolved project.
fn dispatch_project_unlink<S: Store>(
    ctx: &Ctx<'_, S>,
    target: crate::cli::UnlinkTarget,
) -> Result<Response, AppError> {
    let service = crate::service::GitLinkService::new(ctx);
    match target {
        crate::cli::UnlinkTarget::Origin { url } => {
            let remote = remote_argument(ctx, url.as_deref())?;
            let link = service.unlink_origin(&remote)?;
            Ok(Response::Message(format!(
                "unlinked `{}` from project `{}`\nthe project and its stories are untouched; \
                 that origin is free for another project",
                link.raw, link.project
            )))
        }
        crate::cli::UnlinkTarget::Checkout => {
            let link = service.unlink_checkout()?;
            Ok(Response::Message(match &link.replaced {
                Some(path) => format!(
                    "unlinked checkout `{}` from project `{}`\nnothing on disk was changed",
                    path.display(),
                    link.project
                ),
                None => format!("project `{}` had no linked checkout", link.project),
            }))
        }
    }
}

/// The URL a `link origin` / `unlink origin` was given, or the one this
/// directory's own repository records.
///
/// The two paths differ in more than convenience. A URL the user typed is
/// normalized and any failure names it; an omitted one goes through
/// [`origin_here`](crate::service::project::origin_here), which additionally
/// requires the working directory to be its repository's *top level* — see that
/// function for why taking the enclosing repository's origin would be a
/// permanent, cross-project mistake rather than a recoverable one.
fn remote_argument<S: Store>(
    ctx: &Ctx<'_, S>,
    url: Option<&str>,
) -> Result<crate::domain::remote::RemoteUrl, AppError> {
    match url {
        Some(url) => crate::domain::remote::RemoteUrl::normalize(url).map_err(|error| {
            AppError::Validation(format!("`{url}` is not a git remote URL: {error}"))
        }),
        None => crate::service::project::origin_here(ctx.cwd()),
    }
}

/// The `story project settings …` forms.
///
/// Every one answers with the same [`Response::ProjectSettings`], a list of one
/// for the three that name a single key — so a caller parses one shape rather
/// than four, and a write reports what it wrote without a second command.
fn dispatch_project_settings<S: Store>(
    ctx: &Ctx<'_, S>,
    action: SettingsAction,
) -> Result<Response, AppError> {
    let service = SettingsService::new(ctx);
    let settings = match action {
        SettingsAction::List => service.list()?,
        SettingsAction::Get { key } => vec![service.get(&key)?],
        SettingsAction::Set { key, value } => vec![service.set(&key, &value)?],
        SettingsAction::Unset { key } => vec![service.unset(&key)?],
    };
    Ok(Response::ProjectSettings(settings))
}

/// What a completed delete tells the user.
fn delete_message(outcome: &DeleteOutcome) -> String {
    let plan = &outcome.plan;
    let mut lines = vec![format!("deleted {} — {}", plan.slug, plan.name)];
    lines.push(format!(
        "  deleted   {} stor{}, {} event{}",
        outcome.removed.stories,
        if outcome.removed.stories == 1 {
            "y"
        } else {
            "ies"
        },
        outcome.removed.events,
        if outcome.removed.events == 1 { "" } else { "s" },
    ));
    for checkout in &plan.checkouts {
        lines.push(format!("  left      {checkout}"));
    }
    lines.join("\n")
}

/// The `story web …` family.
///
/// Process management only, now that `story project` owns the catalog. What is
/// left — start, stop, status, open, address — is about a running daemon rather
/// than about stored data, and the first three are already deprecated aliases
/// for `story daemon`.
fn dispatch_web(action: WebAction) -> Result<Response, AppError> {
    match action {
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

/// The `story daemon …` family.
///
/// Process management, and project-less by nature: the daemon serves every
/// project on the machine, so asking it to start from inside one particular
/// repository is not a question about that repository.
///
/// `--serve` never arrives here. `main` intercepts it, because running the
/// daemon is a process that does not return rather than a command with an
/// answer.
fn dispatch_daemon(action: DaemonAction) -> Result<Response, AppError> {
    let env = Environment::from_process(None)?;
    match action {
        DaemonAction::Start { port } => {
            let info = crate::daemon::commands::start(&env, port)?;
            Ok(Response::Message(format!(
                "storyhook daemon {} running at {} (PID {})",
                info.version,
                info.dashboard_url(),
                info.pid
            )))
        }
        DaemonAction::Stop => crate::daemon::commands::stop(&env).map(Response::Message),
        DaemonAction::Status => crate::daemon::commands::status(&env).map(Response::Message),
        DaemonAction::Install => crate::daemon::commands::install(&env).map(Response::Message),
        DaemonAction::Uninstall => crate::daemon::commands::uninstall(&env).map(Response::Message),
        DaemonAction::Serve { .. } => Err(AppError::Usage(
            "`story daemon --serve` is handled before dispatch".to_string(),
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
/// `story project new` is the reason this exists. Every other command names a project
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
    // The pointer file is the repository's copy of which project it is, and
    // the store is the identity of record — so `init` writes it. The parameter
    // survives on [`dispatch_unscoped_with`] for the legacy web daemon, whose
    // `init` still builds a `.storyhook/` tree and must not also leave a
    // pointer claiming a project that tree is not in.
    dispatch_unscoped_with(store, root, now, invocation, true, None)
}

/// Whether an invocation can be answered without opening a store at all.
///
/// Not a convenience: a command that needs no store **must not open one**,
/// because opening one can fail. `SqliteStore::open_with` connects eagerly and
/// runs the schema-compatibility gate, so a `store.db` that is damaged, absent,
/// a directory, or from a newer build takes down every command routed through
/// it — including the ones a person reaches for *because* it is damaged.
///
/// That was reachable and it made storyhook's own advice impossible to follow.
/// The corruption diagnostic tells the reader to `run \`story daemon stop\`,
/// delete store.db …`, and `story daemon stop` exited 5 with that same message;
/// `story --help` did too, which is what every other error tells them to run
/// (SH-149).
///
/// The set is exactly the set of commands that must not go through the daemon,
/// and that is not a coincidence — both are "this command is not about the
/// data". Keeping one predicate for both is what stops them drifting:
///
/// * **The daemon's own lifecycle.** `story daemon stop` asking the daemon to
///   run `story daemon stop` is circular, and auto-spawn makes it worse — the
///   command that stops a daemon would start one first.
/// * **Self-update.** `story update` replaces the binary the daemon is running.
/// * **Store creation.** `story store new` is about a store that does not exist
///   yet; `main` answers it before anything is opened.
/// * **Pure functions of compiled-in text.** `--help` and `--version` are
///   answered from a string constant.
///
/// A new [`Invocation`] variant defaults to `false` — needing a store — which
/// is the safe direction: it costs a variant that could have skipped the open,
/// where the other default would hand a store-less dispatcher an invocation it
/// cannot answer.
pub fn needs_no_store(invocation: &Invocation) -> bool {
    matches!(
        invocation,
        Invocation::Daemon { .. }
            | Invocation::Web { .. }
            | Invocation::Store { .. }
            | Invocation::Update { .. }
            | Invocation::Help
            | Invocation::HelpTopic { .. }
            | Invocation::HelpCompact
            | Invocation::HelpAll
            | Invocation::Version
    )
}

/// Answers an invocation that [`needs_no_store`] accepts.
///
/// Every arm here is reachable with no database on the machine at all. Callers
/// must gate on [`needs_no_store`] first; an invocation that needs a store
/// reaches the fallback arm, which is an internal error rather than a guess.
///
/// [`dispatch_unscoped_with`] forwards here rather than keeping its own copies,
/// so the CLI's pre-store path and the daemon's dispatcher cannot answer the
/// same invocation differently.
pub fn dispatch_without_store(invocation: Invocation) -> Result<Response, AppError> {
    match invocation {
        // Pure functions of compiled-in text. They need neither a project nor
        // a store, and answering them here is what lets `story --help` work in
        // a directory storyhook has never heard of — or on a machine whose
        // store will not open.
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
        // Self-update touches no project data at all — it replaces the
        // binary. A `story update` that demanded a project would be unusable
        // exactly when it is most wanted: from a shell that is not standing in
        // a repository, or on a build whose store it is about to fix.
        Invocation::Update { check, force } => update(check, force),
        // The whole `web` family. The daemon commands are process management
        // and name no project at all. A `story web status` that failed in a
        // directory storyhook had never heard of would be a regression in the
        // one command a user reaches for when nothing is working.
        Invocation::Web { action } => dispatch_web(action),
        Invocation::Daemon { action } => dispatch_daemon(action),
        // `main` answers this one before a store is ever opened, for the
        // reasons on [`create_store`]. Reaching here means a caller went round
        // the front door, and the honest answer is that no store this arm could
        // reach is the right one to create anything with.
        Invocation::Store { .. } => Err(AppError::Storage(
            "`story store new` is handled before the store is opened".to_string(),
        )),
        other => Err(AppError::Storage(format!(
            "internal: `{}` needs a store and must not be dispatched without one",
            invocation_name(&other)
        ))),
    }
}

/// [`dispatch_unscoped`], told whether `story project new` should write the pointer
/// file.
pub fn dispatch_unscoped_with<S: Store>(
    store: &S,
    root: &Path,
    now: &str,
    invocation: Invocation,
    pointer: bool,
    stdin_input: Option<&str>,
) -> Result<Response, AppError> {
    if needs_no_store(&invocation) {
        return dispatch_without_store(invocation);
    }
    match invocation {
        Invocation::Project { action } => dispatch_project(store, root, now, pointer, action),
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
            let content = decompose_input(root, stdin_input, file.as_deref(), stdin)?;
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
        // Project-less for the same reason `init` is: `story migrate` creates
        // the project it is importing into, so there is none to resolve first.
        // It also deliberately does *not* consult the store's idea of which
        // project this checkout belongs to — the legacy tree is the input, and
        // `MigrationPlan::apply` refuses inside its own transaction if that
        // checkout has already been migrated.
        Invocation::Migrate { path, dry_run } => {
            let source = match path {
                Some(path) => {
                    let path = PathBuf::from(path);
                    crate::legacy::find_root(&path).ok_or_else(|| {
                        AppError::NotFound(format!(
                            "no legacy `.storyhook` project at `{}` or above it",
                            path.display()
                        ))
                    })?
                }
                None => crate::legacy::find_root(root).ok_or_else(|| {
                    AppError::NotFound(format!(
                        "no legacy `.storyhook` project at `{}` or above it; `story migrate` \
                         moves an existing tree into the store, and `story project new` creates \
                         a new project",
                        root.display()
                    ))
                })?,
            };
            migrate::refuse_in_linked_worktree(&source)?;
            let plan = migrate::MigrationPlan::build(crate::legacy::read_project(&source)?)?;
            let report = if dry_run {
                plan.report(true)
            } else {
                plan.apply(store, &source)?
            };
            Ok(Response::Message(report.render()))
        }
        // Project-less for the same reason `init` is: `story import-project`
        // into an empty directory is how a backup is restored, so the arm has
        // to be able to create the project it is importing into.
        Invocation::ImportProject { file } => {
            let raw = std::fs::read_to_string(resolve_against(root, &file))
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
///
/// # Both halves are about *whose* process this is
///
/// A relative path is relative to the directory the **user** ran the command in,
/// which is not the directory the daemon is running in. `cwd` comes from the
/// invocation's context, so `story decompose spec.md` reads the caller's
/// `spec.md` whether the command runs here or across a wire. The failure still
/// names the path the user typed rather than the one this resolved to — an error
/// that reported an absolute path the user never wrote would be a worse message
/// *and* a different one in each mode.
///
/// Standard input cannot be resolved that way: the daemon does not have the
/// client's. So it arrives in the envelope, read by the client before it sends
/// anything, and `stdin` here is that content. `None` means "read this process's
/// own", which is what the TUI and the daemon's own in-process callers do.
fn read_input(cwd: &Path, stdin: Option<&str>, file: Option<&str>) -> Result<String, AppError> {
    match file {
        Some(path) => {
            let resolved = resolve_against(cwd, path);
            std::fs::read_to_string(&resolved)
                .map_err(|e| AppError::Storage(format!("failed to read {path}: {e}")))
        }
        None => match stdin {
            Some(content) => Ok(content.to_string()),
            None => {
                use std::io::Read as _;
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .map_err(|e| AppError::Storage(format!("failed to read stdin: {e}")))?;
                Ok(buffer)
            }
        },
    }
}

/// Resolves a possibly-relative path against the directory the command was run
/// in.
///
/// An absolute path is left alone. Every path a user types on a command line is
/// relative to their shell, and the daemon's own working directory is an
/// accident of how it was started.
fn resolve_against(cwd: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Whether running `invocation` would read this process's standard input.
///
/// The client asks before it sends: if the answer is yes, it reads stdin itself
/// and puts the content in the envelope, because the daemon has no way to reach
/// the terminal the user is typing into.
#[must_use]
pub fn reads_stdin(invocation: &Invocation) -> bool {
    match invocation {
        Invocation::Import { file } => file.is_none(),
        Invocation::Decompose { stdin, .. } => *stdin,
        _ => false,
    }
}

/// The spec text `story decompose` was pointed at.
///
/// One helper for both dispatchers, because a dry run is answered without a
/// project and a real one is not — and the two must not disagree about which
/// argument combinations are usable.
fn decompose_input(
    cwd: &Path,
    input: Option<&str>,
    file: Option<&str>,
    stdin: bool,
) -> Result<String, AppError> {
    if stdin {
        return read_input(cwd, input, None);
    }
    match file {
        Some(path) => read_input(cwd, input, Some(path)),
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

/// What `story project new` tells the user.
///
/// The text still describes the legacy storage model — a `.storyhook/`
/// directory to commit — because it is the text users and scripts see today
/// and byte-compatibility is this port's governing rule. It becomes wrong at
/// the moment the store becomes the identity of record, and the wave that
/// makes that switch owns rewriting it; changing it here would move a
/// user-visible string in a wave whose entire claim is that it moves none.
/// What `story project new` reports.
///
/// Names the slug in both shapes, for different reasons. Attached, it is how
/// the user recognizes the project in `story project list`; detached, it is the
/// *only* way to reach the project at all — no directory resolves to it, so a
/// message without it would leave somebody holding a project they cannot name.
fn new_message(outcome: &InitOutcome, attached: bool, slug: &Option<String>) -> String {
    let mut message = if outcome.created {
        "created story project".to_string()
    } else {
        "this checkout already belongs to a story project".to_string()
    };
    if let Some(slug) = slug {
        message.push_str(&format!(" `{slug}`"));
    }
    if attached {
        message.push_str(
            "\n\nYour stories live in storyhook's own store, outside this repository — one \
             truth\nfor every branch, worktree and clone.",
        );
    } else {
        message.push_str(
            "\n\nNothing on disk was touched, and no directory resolves to it. Name it with\n\
             `--project <slug>`, or attach a checkout later with `story project link checkout`.",
        );
    }
    if outcome.pointer {
        message.push_str(
            "\n\nWrote .storyhook.toml, which names this project. Commit it: a clone \
             without it\ndoes not know which project it is looking at.",
        );
    }
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
        Invocation::Project { .. } => "project",
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
        Invocation::Purge { .. } => "purge",
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
        Invocation::Daemon { .. } => "daemon",
        Invocation::Store { .. } => "store",
        Invocation::SessionStart => "session-start",
        Invocation::Update { .. } => "update",
        Invocation::Version => "version",
        Invocation::Migrate { .. } => "migrate",
        Invocation::ProjectSnapshot => "project-snapshot",
        Invocation::History { .. } => "history",
    }
}

/// Runs one invocation by asking the machine's daemon to run it.
///
/// **The only route.** Every `story` command goes through here — there is no
/// second one since SH-114 — and the reason is that one process owning the store
/// is what makes a dashboard live, a change feed possible, and a second opinion
/// about the data impossible.
///
/// # What this does not do
///
/// It does not render, and it does not decide anything about the answer. The
/// daemon returns a [`Response`] or an [`AppError`], the same values
/// [`StoreInvoker`] returns, and this process renders them exactly as it would
/// have rendered its own — which is the whole byte-compatibility argument, made
/// structural.
///
/// # Retries
///
/// **A refused connection is retried; nothing else is.** A connection the kernel
/// refused is proof the request was never delivered, so re-sending it — mutation
/// or not — cannot repeat anything. Any failure *after* the connection is
/// established is unprovable: the daemon may have committed the write and died
/// before answering, and a client that retried would be guessing. It fails loud
/// instead, and says the command may or may not have run, because that is true
/// and a comforting lie is worse.
pub struct HttpInvoker {
    env: Environment,
    cwd: PathBuf,
    hook_depth: u32,
}

impl HttpInvoker {
    /// An invoker that runs commands from `cwd` through `env`'s daemon.
    pub fn new(env: Environment, cwd: impl Into<PathBuf>) -> Self {
        Self {
            env,
            cwd: cwd.into(),
            hook_depth: 0,
        }
    }

    /// Sets how deep inside an event hook this invocation is running.
    #[must_use]
    pub fn hook_depth(mut self, hook_depth: u32) -> Self {
        self.hook_depth = hook_depth;
        self
    }

    /// The HTTP client this invoker uses. See [`Self::send`] for why the
    /// timeout is on the connection rather than on the exchange.
    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(5)))
            .build()
            .into()
    }

    /// Posts one envelope to a daemon and reconstructs its answer.
    fn send(
        daemon: &crate::daemon::lifecycle::DaemonInfo,
        request: &crate::api::wire::WireRequest,
    ) -> Result<Result<Response, AppError>, Transport> {
        let url = format!("http://127.0.0.1:{}/api/v1/invoke", daemon.port);
        // A connect timeout, and deliberately *not* a global one. This request
        // carries the user's actual work — an import of a large document, a
        // `github-sync` that talks to the network — so a deadline on the whole
        // exchange would abandon operations that are merely long, and abandoning
        // a mutation is the expensive direction: the caller is then told it may
        // or may not have run.
        //
        // Connecting is different. The peer is on loopback and either accepts
        // at once or is not there, so a connect that has not completed in five
        // seconds is not slow, it is a socket nothing is servicing.
        let response = Self::agent()
            .post(&url)
            .header(crate::api::rpc::TOKEN_HEADER, &daemon.token)
            .header("Content-Type", "application/json")
            .send(serde_json::to_string(request).map_err(|e| Transport::Sent(e.to_string()))?)
            .map_err(Transport::from)?;
        let envelope: crate::api::wire::WireResponse = response
            .into_body()
            .read_json()
            .map_err(|e| Transport::Sent(format!("the daemon's answer was unreadable: {e}")))?;
        Ok(envelope.into_result())
    }
}

/// How far a failed request got, which is what decides whether it may be
/// repeated.
enum Transport {
    /// The connection was refused, so nothing was delivered.
    NotDelivered(String),
    /// The request left this process. Whether it ran is unknown.
    Sent(String),
}

impl From<ureq::Error> for Transport {
    fn from(error: ureq::Error) -> Self {
        match error {
            // Nothing is listening. The request cannot have arrived.
            ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => {
                Transport::NotDelivered(error.to_string())
            }
            other => Transport::Sent(other.to_string()),
        }
    }
}

impl Invoker for HttpInvoker {
    fn invoke(&self, request: InvokeRequest) -> Result<Response, AppError> {
        let wire = crate::api::wire::WireRequest::new(request.invocation, &self.cwd)
            .no_hooks(request.no_hooks)
            .hook_depth(self.hook_depth)
            .stdin(request.stdin)
            .project(request.project);

        let daemon = crate::daemon::lifecycle::ensure(&self.env)?;
        match Self::send(&daemon, &wire) {
            Ok(result) => return result,
            Err(Transport::Sent(detail)) => {
                return Err(AppError::Storage(format!(
                    "the storyhook daemon stopped answering: {detail}. This command may or \
                     may not have run — storyhook will not repeat it, because repeating a \
                     write it cannot prove failed is worse than reporting this. Check with \
                     `story show` or `story list`, then try again."
                )));
            }
            // Refused: the daemon went away between the check and the send,
            // which is exactly what a version-skew restart looks like from
            // here. Nothing was delivered, so starting one and sending the
            // original request is a first attempt rather than a retry.
            Err(Transport::NotDelivered(_)) => {}
        }

        let daemon = crate::daemon::lifecycle::ensure(&self.env)?;
        match Self::send(&daemon, &wire) {
            Ok(result) => result,
            Err(Transport::NotDelivered(detail) | Transport::Sent(detail)) => {
                Err(AppError::Storage(format!(
                    "could not reach the storyhook daemon: {detail}. It answered when this \
                     command started and has stopped since. Run `story daemon status` to see \
                     what it thinks it is doing, and `story daemon stop` before trying again \
                     if it is still there.\n{}",
                    crate::daemon::lifecycle::describe_paths(&self.env),
                )))
            }
        }
    }
}

/// Runs one invocation against the store, resolving the project from the
/// working directory first.
///
/// The invoker every `story` command runs through. [`LegacyInvoker`] is the
/// one it replaced, and survives only to serve the web dashboard until the
/// wave that promotes the daemon.
///
/// # Root resolution
///
/// The working directory, then each of its ancestors in turn, until one of them
/// identifies a project: by its committed pointer file first, by its recorded
/// path second. The nearest directory that answers wins.
///
/// **The upward walk is a deliberate behaviour change**, and it is one of the
/// things the flip is *for*. `storage::ensure_project` looked at `<cwd>` and
/// nowhere else, so `story list` from `src/` in a storyhook project failed with
/// "not initialized" — a limitation the plugin worked around by `cd`-ing to the
/// repository root in a subshell before every call, and one a human standing in
/// a subdirectory simply lost to. Nothing about a project's identity was ever
/// per-directory; the tracker's data being per-directory is what made it look
/// that way.
///
/// Within a directory the pointer file outranks the path, because the pointer
/// is the identity that travels with the repository: a checkout that was moved
/// on disk, or cloned onto another machine, still resolves to the project it
/// has always been.
pub struct StoreInvoker<'a, S: Store> {
    store: &'a S,
    cwd: PathBuf,
    env: Environment,
    hook_depth: u32,
    pointer: bool,
}

impl<'a, S: Store> StoreInvoker<'a, S> {
    /// An invoker over `store`, running from `cwd` under `env`.
    ///
    /// Writes the pointer file on `story project new`, because for a process served
    /// this way the store *is* where the project lives, and a checkout with no
    /// pointer is one a fresh clone cannot identify.
    pub fn new(store: &'a S, cwd: impl Into<PathBuf>, env: Environment) -> Self {
        Self {
            store,
            cwd: cwd.into(),
            env,
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

    /// Sets whether `story project new` writes the pointer file.
    #[must_use]
    pub fn pointer(mut self, pointer: bool) -> Self {
        self.pointer = pointer;
        self
    }

    /// The project this invocation acts on, in the order SH-116 fixed.
    ///
    /// Four steps, written as four consecutive early-returns rather than as
    /// nested conditions, and that shape is load-bearing: SH-119 deletes step 2
    /// and the remaining three are literally the order the story states. A nest
    /// would make that deletion a rewrite, and a rewrite is what stops a bisect
    /// attributing a regression.
    ///
    /// 1. **The selector** — `--project`, else `$STORYHOOK_PROJECT`, collapsed
    ///    into one value by the client. **Binding**: it resolves or it refuses,
    ///    and never falls through to the directory. That is the fix for a
    ///    measured defect — `STORYHOOK_PROJECT=nonesuch story list` used to be
    ///    ignored in silence and answer about whatever project the directory
    ///    happened to be.
    /// 2. **The legacy walk** — the pointer file, then the recorded path, at the
    ///    working directory and then each ancestor. SH-119's to delete.
    /// 3. **The origin** — this directory's `origin`, normalized, looked up
    ///    among the registered ones.
    /// 4. Otherwise `None`, and the caller refuses.
    ///
    /// # Why the walk outranks the origin, for now
    ///
    /// Measured, not preferred: when this was written **no project in the store
    /// and no fixture in the suite had a registered origin**, so consulting one
    /// first would spend a `git` subprocess — 14 ms, against an 11.8 ms
    /// whole-command baseline — to learn nothing, on every command on the
    /// machine. Last means it is paid only by a command that is about to refuse
    /// anyway, where it buys the refusal its `origin` line.
    ///
    /// The cost is that a pointer file outranks a registered origin when the two
    /// disagree. That state is a checkout claiming two projects, which is a
    /// defect rather than a preference, so `story doctor` reports it where
    /// reporting is free instead of the resolver paying for it every time.
    fn resolve_project(
        &self,
        selector: Option<&crate::api::wire::ProjectSelector>,
    ) -> Result<Option<ProjectId>, AppError> {
        if let Some(selector) = selector {
            let found = self
                .store
                .read(|tx| tx.project_by_slug(selector.slug()))?
                .ok_or_else(|| crate::service::project::unknown_project_refusal(selector))?;
            return Ok(Some(found.id));
        }

        for dir in ancestors(&self.cwd) {
            if let Some(project) = resolve_at(self.store, &dir)? {
                return Ok(Some(project));
            }
        }

        if let Some(remote) = crate::service::project::origin_of(&self.cwd)
            && let Some(found) = self.store.read(|tx| tx.project_by_remote(&remote))?
        {
            return Ok(Some(found.id));
        }

        Ok(None)
    }

    /// The nearest pointer file at or above the working directory, whatever it
    /// names.
    ///
    /// Only consulted once [`Self::resolve`] has already failed, to tell a
    /// checkout that *claims* a project from a directory that claims nothing.
    /// Errors are swallowed deliberately: an unreadable pointer file has its own
    /// message, raised by resolution, and this is a second attempt at explaining
    /// a failure that has already happened.
    fn pointer_at_or_above(&self) -> Option<crate::service::project::ProjectPointer> {
        ancestors(&self.cwd)
            .into_iter()
            .find_map(|dir| crate::service::project::read_pointer(&dir).ok().flatten())
    }
}

/// A directory and every ancestor of it, nearest first.
///
/// Canonicalized once at the start rather than per level: an uncanonicalized
/// path's ancestors include `..` components that would `stat` the wrong
/// directories, and a directory that cannot be canonicalized (it does not
/// exist) has no meaningful ancestry to walk beyond what it was given.
fn ancestors(cwd: &Path) -> Vec<PathBuf> {
    let root = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    root.ancestors().map(Path::to_path_buf).collect()
}

/// The project `dir` itself identifies — its pointer file, then its recorded
/// path.
///
/// A pointer naming a uuid the store does not hold is **not** an answer here.
/// It falls through, so a directory that carries a stale pointer and a valid
/// path row still resolves; whether an unresolvable pointer should be reported
/// rather than ignored is the guard's question, not resolution's.
fn resolve_at<S: Store>(store: &S, dir: &Path) -> Result<Option<ProjectId>, AppError> {
    let pointer = crate::service::project::read_pointer(dir)?;
    Ok(store.read(|tx| {
        if let Some(pointer) = &pointer
            && let Some(project) = tx.project_by_uuid(&pointer.uuid)?
        {
            return Ok(Some(project.id));
        }
        Ok(tx.project_by_path(dir)?.map(|project| project.id))
    })?)
}

/// The directory a project would be brought into existence at, for the three
/// invocations that create one — and `None` for every invocation that does not.
///
/// Named here, once, rather than guarded arm by arm: `init`, `migrate` and
/// `import-project` all reach `create_project`, and a fourth creating arm added
/// later without a guard is exactly how SH-95 happened the first time.
///
/// A dry-run migration writes nothing and is deliberately not a creation.
///
/// # Why the `Project` arm is exhaustive
///
/// The sentence above is the whole reason this function exists, and until
/// SH-117 the code did not keep it: the `Project` arm matched
/// `ProjectAction::Init` inside a `_ => None` catch-all, so `New` — the arm
/// that replaced `init` — would have fallen straight through it and created
/// projects unguarded, with a green build and a green suite. The guard has
/// exactly one call site, in [`StoreInvoker::invoke`], reachable only through
/// this match; there is no second layer to catch the miss.
///
/// Listing every `ProjectAction` makes the compiler the thing that notices,
/// which is what the paragraph above always claimed and only now enforces.
fn project_creation_target(invocation: &Invocation, cwd: &Path) -> Option<PathBuf> {
    match invocation {
        Invocation::ImportProject { .. } => Some(cwd.to_path_buf()),
        Invocation::Project { action } => match action {
            // `--no-attach` creates nothing at a path, so there is no path for
            // this guard to judge — a deliberate narrowing of SH-95, pinned by
            // a test of its own rather than left to be rediscovered.
            //
            // A request still carrying `Ask` is guarded as though it attached
            // the working directory. The dispatcher refuses it a moment later
            // either way; guarding it is the conservative order, so a caller
            // that went round `main.rs` cannot reach `create_project` by a
            // route the guard declined to look at.
            ProjectAction::New(request) => match request {
                NewProjectRequest::Ask => Some(target_dir(cwd, None)),
                NewProjectRequest::Stated(spec) => match &spec.attach {
                    Attach::Cwd => Some(target_dir(cwd, None)),
                    Attach::Path(path) => Some(target_dir(cwd, Some(path))),
                    Attach::Nothing => None,
                },
            },
            ProjectAction::Delete { .. }
            | ProjectAction::List
            | ProjectAction::Link(_)
            | ProjectAction::Unlink(_)
            | ProjectAction::Settings(_) => None,
        },
        Invocation::Migrate { path, dry_run } if !dry_run => Some(target_dir(cwd, path.as_deref())),
        _ => None,
    }
}

/// The directory a command names by an optional `PATH`, defaulting to the one
/// it ran in.
///
/// The `None` half is why this is named rather than written out at each call
/// site: "no path given" and "the path given is `.`" must reach the store as
/// the same directory, and over the daemon neither of them is *this* process's
/// working directory. See [`resolve_against`] for that rule.
fn target_dir(cwd: &Path, path: Option<&str>) -> PathBuf {
    path.map_or_else(|| cwd.to_path_buf(), |path| resolve_against(cwd, path))
}

impl<S: Store> Invoker for StoreInvoker<'_, S> {
    fn invoke(&self, request: InvokeRequest) -> Result<Response, AppError> {
        let now = self.env.now();
        // Before anything is written, and before the arms diverge: a throwaway
        // project may only be created in a throwaway store (SH-95).
        if let Some(target) = project_creation_target(&request.invocation, &self.cwd) {
            crate::service::project::refuse_temp_project_in_real_store(
                &target,
                self.env.store_path(),
            )?;
        }
        if is_project_less(&request.invocation) {
            // A *flag* naming a project for a command that acts on none is a
            // mistake in this invocation, and saying so is cheap. A *variable*
            // is not: it is set once and inherited by everything, so refusing it
            // would mean one `export` breaks `story project list` — which is
            // precisely the command somebody runs to find out which slugs exist.
            // The asymmetry is the design, not an oversight.
            if let Some(crate::api::wire::ProjectSelector::Flag { slug }) = &request.project {
                return Err(AppError::Usage(format!(
                    "`story {}` does not act on a single project, so `--project {slug}` has \
                     nothing to select. Re-run it without the flag.",
                    describe_unscoped(&request.invocation)
                )));
            }
            return dispatch_unscoped_with(
                self.store,
                &self.cwd,
                &now,
                request.invocation,
                self.pointer,
                request.stdin.as_deref(),
            );
        }

        let Some(project) = self.resolve_project(request.project.as_ref())? else {
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
                    request.stdin.as_deref(),
                );
            }
            // A repository that still has its stories in `.storyhook/` gets a
            // diagnosis rather than an invitation to `story project new`, which would
            // mint an empty second project beside data the user still has.
            if let Some(tree) = crate::service::project::legacy_project_at(&self.cwd) {
                return Err(crate::service::project::unmigrated_error(&tree));
            }
            // A checkout carrying a pointer file *is* initialized — it says so,
            // in a file its repository committed — and what is missing is the
            // project on this machine. That is what a fresh clone looks like,
            // and "not initialized in this directory" sends the reader looking
            // for the wrong thing.
            if let Some(pointer) = self.pointer_at_or_above() {
                return Err(AppError::NotFound(format!(
                    "this checkout belongs to storyhook project {}, which this machine's \
                     store does not have. Run `story project new --prefix <PREFIX>` here to \
                     adopt it — an adopted checkout keeps the prefix its pointer file names — \
                     or `story import-project` if you have an export of it.",
                    pointer.uuid
                )));
            }
            // Nothing named a project and nothing here answers for one. The
            // origin is looked up again rather than threaded down from
            // `resolve_project`: this branch is the refusal, so the second call
            // is paid only by a command that is already failing, and threading
            // it would put a `Some`-carrying parameter on the success path where
            // nothing reads it.
            return Err(crate::service::project::no_project_refusal(
                &self.cwd,
                crate::service::project::origin_of(&self.cwd).as_ref(),
            ));
        };

        let ctx = Ctx::new(self.store, project, &self.cwd, self.env.clone())
            .no_hooks(request.no_hooks)
            .hook_depth(self.hook_depth)
            .with_stdin(request.stdin);
        dispatch(&ctx, request.invocation)
    }
}

/// Whether this invocation must answer `{}` rather than report a failure it
/// could not avoid.
///
/// **One member, and that is the design rather than a starting point.** A list
/// would need a rule for joining it; a single `matches!` cannot drift.
///
/// `session-start` is not a command a person types — it is a hook, fired
/// involuntarily at the top of an agent session, and its output goes into a
/// model's context window rather than onto a terminal. Its contract is already
/// "print `{}` when there is nothing to say" ([`crate::service::session::SILENT`]),
/// and it already keeps that contract for every failure *inside* the daemon. The
/// gap this closes is the failure that happens before a daemon exists to ask: a
/// store that will not open used to put 1.2 kB of corruption diagnosis into a
/// model's context and exit 5.
///
/// # Why this is not the silence SH-114 spent a story removing
///
/// SH-114 made the *transport's* diagnostics loud, for commands a human ran and
/// was waiting on. This exemption touches no such command. A diagnostic here is
/// not louder — it is **misdirected**: it reads as project state to the only
/// reader who receives it, and reaches nobody who can act on it. And nothing is
/// swallowed, only routed: a daemon that fails to start still records why to
/// [`crate::env::Environment::daemon_failure`], and `story daemon status` still
/// reports it, so the person who wants the diagnosis gets it in full from the
/// command they would run to ask.
///
/// The three managed git hooks deliberately have **no** entry here. They run
/// `commit-sync`, `move` and `next` — ordinary verbs a human also runs — so a
/// predicate over [`Invocation`] would silence `story next` for a person
/// standing at a terminal. Their silence belongs to the hook scripts, where
/// `tests/hook_silence.rs` pins it in both directions.
#[must_use]
pub fn failure_is_silent(invocation: &Invocation) -> bool {
    matches!(invocation, Invocation::SessionStart)
}

/// How to spell a project-less invocation back to the user, for the refusal
/// that tells them `--project` has nothing to select.
///
/// Deliberately coarse — the verb and, where a family splits, its subcommand.
/// The reader already typed the command; what they need is confirmation of
/// *which* one storyhook thinks takes no project, not an echo of their
/// arguments.
fn describe_unscoped(invocation: &Invocation) -> String {
    match invocation {
        // Exhaustive rather than `_ => "project list"`: this string is what a
        // refusal calls the command the user typed, and a catch-all here told
        // somebody running `story project link` that `story project list` does
        // not act on a single project.
        Invocation::Project { action } => match action {
            ProjectAction::New(_) => "project new",
            ProjectAction::Delete { .. } => "project delete",
            ProjectAction::List => "project list",
            ProjectAction::Link(_) => "project link",
            ProjectAction::Unlink(_) => "project unlink",
            ProjectAction::Settings(_) => "project settings",
        },
        Invocation::ImportProject { .. } => "import-project",
        Invocation::Migrate { .. } => "migrate",
        Invocation::Plugin { .. } => "plugin",
        Invocation::Hooks { .. } => "hooks",
        Invocation::Decompose { .. } => "decompose",
        Invocation::Daemon { .. } => "daemon",
        Invocation::Web { .. } => "web",
        Invocation::Store { .. } => "store",
        Invocation::Update { .. } => "update",
        Invocation::Version => "version",
        _ => "help",
    }
    .to_string()
}

/// Whether an invocation is answered without resolving a project.
///
/// The list is `dispatch`'s own forwarding set plus `import-project`, and it is
/// an exhaustive-by-inspection match rather than a `matches!` so that the two
/// cannot drift: adding a project-less arm to `dispatch` without adding it here
/// makes `story <verb>` fail in an empty directory, which is exactly the
/// failure this function exists to prevent.
fn is_project_less(invocation: &Invocation) -> bool {
    // Needing no store implies needing no project, and stating it that way
    // rather than repeating the nine variants is what stops the two rosters
    // drifting: a command classified store-less in one place and
    // project-scoped in the other would try to resolve a project it has no
    // store to look in.
    if needs_no_store(invocation) {
        return true;
    }
    match invocation {
        // `new`, `init` and `list` are about projects in general; the other four
        // are about *this* one and cannot be answered without resolving it.
        // Stated positively, so a variant added later is project-less only
        // because somebody said so — the negated form classified every new arm
        // as unscoped, which is how `link` would have been dispatched with no
        // project and no way to see a selector.
        //
        // `delete` moved to the scoped side in SH-117. `deinit` was unscoped
        // because it carried its own target and resolved it itself; `delete`
        // has no target of its own, so the ordinary selector is what names it
        // — which is also what gives it `no_project_refusal` for free.
        Invocation::Project { action } => {
            matches!(action, ProjectAction::New(_) | ProjectAction::List)
        }
        Invocation::ImportProject { .. }
        | Invocation::Migrate { .. }
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

/// How `story doctor` describes registrations pointing at directories that are
/// gone.
///
/// Advisory, not an integrity failure: a missing directory can mean an external
/// disk is not mounted, and exiting non-zero — let alone forgetting the
/// registration — because a volume was unplugged would be worse than the defect
/// it is reporting. The story count is included because it is the whole
/// difference between a fixture worth forgetting and real work whose checkout
/// merely moved.
fn orphan_advice(orphans: &[crate::service::OrphanedRegistration]) -> Vec<String> {
    if orphans.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = orphans
        .iter()
        .map(|orphan| {
            format!(
                "`{}` is registered at `{}`, which no longer exists ({} {})",
                orphan.slug,
                orphan.path.display(),
                orphan.stories,
                if orphan.stories == 1 {
                    "story"
                } else {
                    "stories"
                },
            )
        })
        .collect();
    lines.push(format!(
        "{} stale {}. Run `story doctor --fix` to deregister them, or `story --project <slug> \
         project link checkout <path>` if the checkout moved — deregistering forgets only the \
         path, never the stories.",
        orphans.len(),
        if orphans.len() == 1 {
            "registration"
        } else {
            "registrations"
        },
    ));
    lines
}

/// What `story doctor --fix` reports having forgotten.
fn deregistered_message(forgotten: &[crate::service::OrphanedRegistration]) -> String {
    let mut out = format!(
        "deregistered {} stale {}:",
        forgotten.len(),
        if forgotten.len() == 1 {
            "registration"
        } else {
            "registrations"
        }
    );
    for orphan in forgotten {
        out.push_str(&format!("\n  {} ({})", orphan.slug, orphan.path.display()));
    }
    out.push_str(
        "\n\nOnly the paths were forgotten. Every story is still in the store, they are \
                  still listed by `story project list` and still on the dashboard, and \
                  `story project link checkout` puts a project back where its checkout moved to.",
    );
    out
}
