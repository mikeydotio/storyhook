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
    AbandonedAction, Attach, CrashesAction, DaemonAction, EpicAction, HELP_TEXT, HistoryAction,
    HooksAction, Invocation, NewProjectRequest, PhaseAction, PluginAction, ProjectAction,
    SettingsAction, StateAction, StoreAction, TokenAction, TypeAction, WebAction,
};
use crate::domain::provenance::{ActorLabel, Provenance};
use crate::domain::{FieldEdit, ImportStory, StateChanges, SuperState, TypeChanges, TypeDef};
use crate::env::Environment;
use crate::error::AppError;
use crate::help_topics;
use crate::output::{ConfirmationPlan, Response, render_html_report};
use crate::service::transfer::ProjectExport;
use crate::service::{
    CatalogService, Clock, ConfigService, Ctx, DeleteOutcome, FieldEdits, GitService,
    GroupingService, ImportBatch, InitOptions, InitOutcome, IntegrityService, ListFilters,
    NewStoryInput, PhaseCleared, PointerUpdate, ProjectService, QueryService, RelationOutcome,
    RelationService, SessionService, SetPrefixOutcome, SettingsService, StateListing, StoryService,
    SystemService, TransferService, migrate, session, system, transfer,
};
use crate::store::{ProjectId, ReadOps, Store};

pub(crate) mod story_ids;

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
    /// The caller's GitHub credential, when the command spends one.
    ///
    /// Read by the client from its own environment and carried here for the
    /// same reason [`stdin`](Self::stdin) is: the daemon's environment belongs
    /// to whichever process happened to start it, not to whoever typed the
    /// command. Before SH-153 the daemon read `$STORYHOOK_GITHUB_TOKEN`
    /// directly, so a caller who exported one was told it was unset while a
    /// caller who had not exported one silently spent the daemon's.
    ///
    /// `None` means "this caller supplied none", and it is never a licence to
    /// look elsewhere: the refusal is raised where the work runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_token: Option<crate::domain::secret::GithubToken>,
    /// Who the caller says it is, from `$STORYHOOK_ACTOR` (SH-246).
    ///
    /// Read by the client from its own environment and carried here for the
    /// same reason [`stdin`](Self::stdin) and
    /// [`github_token`](Self::github_token) are: the daemon's environment
    /// belongs to whichever process happened to start it, so a daemon reading
    /// `$STORYHOOK_ACTOR` directly would label every write with whatever the
    /// *first* caller of the day had exported.
    ///
    /// `None` means the caller declared nothing, which is the common case and
    /// is never filled in from the command beside it — an undeclared actor and
    /// a declared one must stay distinguishable, or the record answers a
    /// question it was not asked.
    ///
    /// Typed rather than a bare `String`: [`ActorLabel`]'s `TryFrom<String>`
    /// runs on deserialization, so a label that crossed the wire is bounded and
    /// control-character-free by the time anything can store or render it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<ActorLabel>,
}

impl InvokeRequest {
    /// A request to run `invocation` with hooks enabled.
    pub fn new(invocation: Invocation) -> Self {
        Self {
            invocation,
            no_hooks: false,
            stdin: None,
            project: None,
            github_token: None,
            actor: None,
        }
    }

    /// Supplies the caller's GitHub credential.
    #[must_use]
    pub fn github_token(
        mut self,
        github_token: Option<crate::domain::secret::GithubToken>,
    ) -> Self {
        self.github_token = github_token;
        self
    }

    /// Supplies who the caller says it is (SH-246).
    #[must_use]
    pub fn actor(mut self, actor: Option<ActorLabel>) -> Self {
        self.actor = actor;
        self
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
                ProjectAction::SetPrefix { force, .. } => *force = true,
                ProjectAction::New(_)
                | ProjectAction::List
                | ProjectAction::Show
                | ProjectAction::Link(_)
                | ProjectAction::Unlink(_)
                | ProjectAction::Settings(_) => {}
            },
            Invocation::Purge { force, .. } => *force = true,
            Invocation::Reopen { force, .. } => *force = true,
            _ => {}
        }
        self
    }

    /// The same request, with its setup answers already given.
    ///
    /// The second half of the two-step SH-153's D2 defines: the first
    /// invocation answers [`Response::SetupRequired`] and writes nothing, the
    /// client asks the user, and this is what carries the answer back. The
    /// invocation is otherwise untouched — same story id, same `dry_run`,
    /// same `resolve`.
    ///
    /// A request carrying anything other than `GithubSync` is returned
    /// unchanged, which is what makes this safe to call unconditionally —
    /// the same shape [`forced`](Self::forced) has.
    #[must_use]
    pub fn with_setup_answers(
        mut self,
        strategy: crate::cli::SetupStrategy,
        mode: crate::cli::SetupMode,
    ) -> Self {
        if let Invocation::GithubSync {
            strategy: s,
            mode: m,
            ..
        } = &mut self.invocation
        {
            *s = Some(strategy);
            *m = Some(mode);
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
///
/// # Story ids are canonical by the time an arm sees one
///
/// The first thing this does is expand every story id the invocation carries
/// (SH-118): a bare `5` becomes `SH-5`, and an id naming a *different*
/// project's prefix is refused here rather than resolved wrongly further down.
/// It is here, ahead of the match, because this is the one call every door
/// passes through — the CLI, the TUI, the dashboard's routes and a hand-built
/// `InvokeRequest` — so no arm has to remember, and the arms below may treat
/// their `id` as canonical. See [`story_ids`].
pub fn dispatch<S: Store>(
    ctx: &Ctx<'_, S>,
    mut invocation: Invocation,
) -> Result<Response, AppError> {
    story_ids::canonicalize(ctx, &mut invocation)?;
    match invocation {
        Invocation::New {
            title,
            state,
            story_type,
            description,
            priority,
            labels,
            assignee,
            draft,
        } => {
            let input = NewStoryInput {
                title,
                state,
                story_type,
                description,
                priority,
                labels,
                assignee,
                draft,
            };
            let story = StoryService::new(ctx).create(&input)?;
            ctx.story_view(&story.id)
        }
        Invocation::Publish { id } => {
            StoryService::new(ctx).publish(&id)?;
            ctx.story_view(&id)
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
            awaiting,
        } => {
            StoryService::new(ctx).set_state(
                &id,
                &state,
                comment.as_deref(),
                if_state.as_deref(),
                awaiting.as_deref(),
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
        // The same two-step `Purge` above is: an unforced reopen of a
        // soft-deleted story answers with what it would undelete and writes
        // nothing. An ordinarily-closed story (`reopen_plan` answers `None`)
        // needs no confirmation at all, so it goes straight through.
        Invocation::Reopen { id, force } => {
            let service = StoryService::new(ctx);
            let plan = if force {
                None
            } else {
                service.reopen_plan(&id)?
            };
            match plan {
                Some(plan) => Ok(Response::ConfirmationRequired(Box::new(
                    ConfirmationPlan::Undelete(plan),
                ))),
                None => {
                    service.reopen(&id)?;
                    ctx.story_view(&id)
                }
            }
        }
        Invocation::Hide { id } => {
            StoryService::new(ctx).hide(&id)?;
            ctx.story_view(&id)
        }
        Invocation::Unhide { id } => {
            StoryService::new(ctx).unhide(&id)?;
            ctx.story_view(&id)
        }
        // The same two-step `Purge`/`Reopen` shape above: an unforced call
        // answers with what it would archive and writes nothing.
        Invocation::HideState { state, force } => {
            let service = StoryService::new(ctx);
            if force {
                service.hide_state(&state).map(Response::Message)
            } else {
                Ok(Response::ConfirmationRequired(Box::new(
                    ConfirmationPlan::HideState(service.hide_state_plan(&state)?),
                )))
            }
        }
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
            drafts,
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
                drafts,
            };
            query(ctx, |service| service.list(&filters)).map(|views| Response::Stories(views, None))
        }
        Invocation::Show { id } => {
            query(ctx, |service| service.show(&id)).map(|view| Response::Story(Box::new(view)))
        }
        Invocation::Log { id } => {
            let (id, title, entries) = session::story_log(ctx, &id)?;
            Ok(Response::StoryLog { id, title, entries })
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
            let document = query(ctx, |service| service.context(json))?;
            // `RawJson` for the JSON form: the document *is* the result, so the
            // `--json` envelope has nothing to add, and wrapping it as an
            // escaped string double-encodes it (SH-66, the `export --json`
            // fix's sibling defect). The markdown form keeps `Message`, which
            // `--json` wraps as an ordinary string — correct, since markdown
            // isn't JSON to begin with.
            Ok(if json {
                Response::RawJson(document)
            } else {
                Response::Message(document)
            })
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
                // The outcome as a *value* (SH-270). This `?` can now only
                // carry a genuine failure — a rolled-back repair write, or the
                // store beneath it — because "repairs ran, findings remain" is
                // no longer spelled as an error. It used to be, and the `?`
                // returned on it, so everything below was skipped whenever the
                // project carried a finding this command could not clear: the
                // remedy `orphan_advice` hands an operator was the one command
                // that would not perform it. See `IntegrityService::repair`.
                let mut outcome = service.repair()?;
                if audit_catalog && let Err(error) = catalog_sweep(&catalog, &mut outcome.advice) {
                    // The sweep is store-wide and the repair above was not, so
                    // a failure here must not discard a verdict already
                    // computed about *this* project — nor, on the healthy path,
                    // hide a failed store-wide write behind exit 0.
                    if outcome.findings.is_empty() {
                        return Err(error.with_context(&format!(
                            "{}\n\n{SWEEP_INCOMPLETE}.\n\n{SWEEP_ATOMICITY}",
                            outcome.message()
                        )));
                    }
                    outcome.advice.push(catalog_sweep_failure(&error));
                }
                outcome.verdict().map(Response::Message)
            } else {
                // One fold for both halves (SH-267): the drift oracle is the
                // expensive half of this read, and it answers both.
                let examination = service.examine()?;
                let advice = doctor_advice(ctx, &catalog, examination.notices, audit_catalog)?;
                let findings = examination.findings;
                // The emptiness question is asked once, by the constructor
                // that owns the invariant (SH-244): `Some` *is* the unhealthy
                // verdict, and neither branch re-decides it.
                match crate::error::IntegrityDetail::report(findings, advice.clone()) {
                    None => Ok(Response::Issues(advice)),
                    // A real finding still fails the command, but everything
                    // this run had to say that is *not* damage must not vanish
                    // just because it played no part in that verdict — it
                    // rides `advice`, where nothing can mistake it for damage.
                    Some(detail) => Err(AppError::Integrity(detail)),
                }
            }
        }
        Invocation::SessionStart => {
            let service = SessionService::new(ctx);
            // Best-effort and unconditional on the message below succeeding —
            // see `publish_sentinel`'s own doc comment for why a write failure
            // must never turn a real context envelope into `{}`.
            service.publish_sentinel();
            service.context().map(Response::RawJson)
        }
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
            strategy,
            mode,
        } => {
            #[cfg(feature = "github-sync")]
            {
                crate::service::GithubSyncService::new(ctx).sync(
                    id.as_deref(),
                    dry_run,
                    resolve,
                    strategy,
                    mode,
                )
            }
            #[cfg(not(feature = "github-sync"))]
            {
                let _ = (id, dry_run, resolve, strategy, mode);
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
        Invocation::LinkPr {
            id,
            url,
            close_on_merge,
        } => {
            crate::service::PrLinkService::new(ctx).link(&id, &url, close_on_merge)?;
            ctx.story_view(&id)
        }
        Invocation::UnlinkPr { id, url } => {
            crate::service::PrLinkService::new(ctx).unlink(&id, &url)?;
            ctx.story_view(&id)
        }
        Invocation::PrCheck { id } => {
            #[cfg(feature = "github-sync")]
            {
                crate::service::PrLinkService::new(ctx).check(id.as_deref())
            }
            #[cfg(not(feature = "github-sync"))]
            {
                let _ = id;
                Err(AppError::Usage(
                    "pr-check requires the `github-sync` feature. \
                     Rebuild with: cargo install storyhook --features github-sync"
                        .to_string(),
                ))
            }
        }
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
        Invocation::Project {
            action: ProjectAction::SetPrefix { new_prefix, force },
        } => dispatch_project_set_prefix(ctx, new_prefix, force),
        Invocation::Project {
            action: ProjectAction::Show,
        } => dispatch_project_show(ctx),
        Invocation::Web { .. }
        | Invocation::Daemon { .. }
        | Invocation::Token { .. }
        | Invocation::DoctorAbandoned { .. }
        | Invocation::DoctorCrashes { .. }
        | Invocation::Store { .. }
        | Invocation::Update { .. }
        | Invocation::ImportProject { .. }
        | Invocation::Migrate { .. }
        | Invocation::Project { .. }
        | Invocation::Help
        | Invocation::HelpTopic { .. }
        | Invocation::HelpCompact
        | Invocation::HelpAll
        | Invocation::GithubAuth { .. }
        | Invocation::Version => dispatch_unscoped_with_stdin(
            ctx.store(),
            ctx.env(),
            ctx.cwd(),
            &ctx.now(),
            invocation,
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
                let where_ = crate::output::checkout_line(entry.path.as_deref());
                lines.push(format!("  {} — {} ({where_})", entry.id, entry.name));
                // The two git associations, indented under the project they
                // belong to. This is the only way to answer "did my link take?"
                // about *every* project at once — `story project show` answers it
                // about the one you are in, and is what `story.sh dispatch`
                // reads (SH-120).
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
        ProjectAction::Show => Err(AppError::Usage(
            "`story project show` needs a project: name one with `--project <slug>`, or run it \
             in a checkout storyhook already resolves. `story project list` shows every project \
             the store knows."
                .to_string(),
        )),
        ProjectAction::Delete { .. } => Err(AppError::Usage(
            "`story project delete` needs a project: name one with `--project <slug>`, or run \
             it in a checkout storyhook already resolves."
                .to_string(),
        )),
        ProjectAction::SetPrefix { .. } => Err(AppError::Usage(
            "`story project set-prefix` needs a project: name one with `--project <slug>`, or \
             run it in a checkout storyhook already resolves."
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

/// `story project set-prefix <NEW-PREFIX> [--force]` against a resolved
/// project.
///
/// The same two-step [`dispatch_project_delete`] uses, for the same reason:
/// an unforced request answers with the plan and writes nothing, and the
/// client turns that into a prompt.
fn dispatch_project_set_prefix<S: Store>(
    ctx: &Ctx<'_, S>,
    new_prefix: String,
    force: bool,
) -> Result<Response, AppError> {
    let service =
        ProjectService::new(ctx.store(), ctx.cwd()).clock(Clock::Fixed(ctx.now().to_string()));
    if !force {
        return Ok(Response::ConfirmationRequired(Box::new(
            ConfirmationPlan::SetPrefix(service.set_prefix_plan(ctx.project(), &new_prefix)?),
        )));
    }
    let backups_dir = ctx.env().maintenance_backups_dir();
    let outcome = service.set_prefix(ctx.project(), &new_prefix, &backups_dir)?;
    Ok(Response::Message(set_prefix_message(&outcome)))
}

/// `story project link origin|checkout …` against a resolved project.
fn dispatch_project_link<S: Store>(
    ctx: &Ctx<'_, S>,
    target: crate::cli::LinkTarget,
) -> Result<Response, AppError> {
    let service = crate::service::GitLinkService::new(ctx);
    match target {
        crate::cli::LinkTarget::Origin { url } => {
            let origin = claimed_remote_argument(ctx, url.as_deref())?;
            let link = service.link_origin(&origin)?;
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
            let mut message = format!(
                "linked checkout `{}` to project `{}`{replaced}\nthis is where repo-side work \
                 runs for it",
                target.display(),
                link.project
            );
            message.push('\n');
            message.push_str(&pointer_outcome_message(&link.pointer));
            Ok(Response::Message(message))
        }
    }
}

/// Reports what [`GitLinkService::link_checkout`] or `unlink_checkout` did to
/// the directory's `.storyhook.toml`, alongside the `checkout_path` change
/// its caller already describes.
fn pointer_outcome_message(pointer: &crate::service::PointerOutcome) -> String {
    use crate::service::PointerOutcome;
    match pointer {
        PointerOutcome::Written(path) => format!(
            "wrote {} — this directory now resolves bare story ids on its own",
            path.display()
        ),
        PointerOutcome::AlreadyCorrect(path) => {
            format!(
                "{} already named this project — nothing changed",
                path.display()
            )
        }
        PointerOutcome::PrefixRepaired { path, was, now } => format!(
            "repaired {}: its prefix was `{was}`, now `{now}`",
            path.display()
        ),
        PointerOutcome::AnotherProject { path, uuid, holder } => {
            let holder = holder.as_deref().map_or_else(
                || format!("project `{uuid}`, which this store does not have"),
                |slug| format!("project `{slug}`"),
            );
            format!(
                "{} names {holder} and was left alone — that directory still resolves to it, \
                 not to this project. A tree can be the work-directory of several projects but \
                 the pointer of only one.",
                path.display()
            )
        }
        PointerOutcome::Unwritable { path, reason } => format!(
            "the checkout is linked, but {} could not be written: {reason}. This directory will \
             not resolve bare story ids until it exists — write it by hand, or fix what's \
             blocking it and link again.",
            path.display()
        ),
        PointerOutcome::LeftInPlace(path) => format!(
            "{} is left in place — it is committed, and other clones resolve by it. Delete it \
             yourself if this repository should stop naming this project.",
            path.display()
        ),
        PointerOutcome::NoPointer => "no pointer file was here to leave".to_string(),
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
                    "unlinked checkout `{}` from project `{}`\n{}",
                    path.display(),
                    link.project,
                    pointer_outcome_message(&link.pointer)
                ),
                None => format!("project `{}` had no linked checkout", link.project),
            }))
        }
    }
}

/// The origin a `link origin` was given, or the one this directory's own
/// repository records — and in both cases, one this directory may claim.
///
/// The two paths differ in more than convenience. An omitted URL goes through
/// [`origin_here`](crate::service::project::origin_here), which computes the
/// entitlement. A URL the user typed is normalized, any failure names it, and
/// then it faces **one** further question (SH-151): is this the enclosing
/// repository's own origin, given from a directory that does not own it?
///
/// That single case is refused because nothing else catches it. The store's
/// uniqueness index refuses a *second* claim on an origin, but the first
/// sub-directory to ask meets no collision at all — it simply takes the
/// repository's identity, permanently, and every sibling project and the
/// repository's own top level are locked out from then on. Every other URL is
/// registered as asked: naming some unrelated repository's origin from a
/// directory that is not a checkout of it is exactly what this verb is for.
fn claimed_remote_argument<S: Store>(
    ctx: &Ctx<'_, S>,
    url: Option<&str>,
) -> Result<crate::domain::remote::OwnedOrigin, AppError> {
    let Some(url) = url else {
        return crate::service::project::origin_here(ctx.cwd());
    };
    crate::service::project::claim_stated(ctx.cwd(), normalized_remote(url)?)
}

/// The URL an `unlink origin` names — with **no** entitlement question asked.
///
/// Removing a registration is not claiming one, and the directory a cleanup is
/// run from says nothing about whether it should happen: `story --project a
/// project unlink origin <url>` is the way to undo a wrong claim, and refusing
/// it because the caller happens to be standing inside the repository would
/// leave the mistake in place. The omitted form still goes through
/// [`origin_here`](crate::service::project::origin_here), because "the origin
/// here" has to mean the same directory's origin for both verbs.
fn remote_argument<S: Store>(
    ctx: &Ctx<'_, S>,
    url: Option<&str>,
) -> Result<crate::domain::remote::RemoteUrl, AppError> {
    match url {
        Some(url) => normalized_remote(url),
        None => Ok(crate::service::project::origin_here(ctx.cwd())?
            .url()
            .clone()),
    }
}

/// A URL the user typed, as a [`RemoteUrl`](crate::domain::remote::RemoteUrl),
/// with the refusal naming what they typed.
fn normalized_remote(url: &str) -> Result<crate::domain::remote::RemoteUrl, AppError> {
    crate::domain::remote::RemoteUrl::normalize(url)
        .map_err(|error| AppError::Validation(format!("`{url}` is not a git remote URL: {error}")))
}

/// `story project show` — which project this is, and where its work runs.
///
/// Answered against a resolved [`Ctx`], so it obeys the ordinary selector:
/// `--project`, `$STORYHOOK_PROJECT`, the committed pointer file, then the
/// registered origin. That is the whole point of it — a caller that needs a
/// project's directory must learn the *slug it resolved to* in the same breath,
/// or it has to resolve twice and the second answer may differ from the first.
///
/// Its consumer is `plugin/claude-code/bin/story.sh`, whose `dispatch` verb is
/// the one operation in storyhook that genuinely needs a directory.
fn dispatch_project_show<S: Store>(ctx: &Ctx<'_, S>) -> Result<Response, AppError> {
    let view = CatalogService::new(ctx.store()).describe(ctx.project())?;
    Ok(Response::Project(Box::new(view)))
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

/// What a completed `story project set-prefix` tells the user.
fn set_prefix_message(outcome: &SetPrefixOutcome) -> String {
    let plan = &outcome.plan;
    let mut lines = vec![format!(
        "renamed {} — {} ({} → {})",
        plan.slug, plan.name, plan.old_prefix, plan.new_prefix
    )];
    lines.push(format!(
        "  refolded  {} stor{}",
        plan.stories,
        if plan.stories == 1 { "y" } else { "ies" },
    ));
    if plan.relationships > 0 {
        lines.push(format!(
            "  rewrote   {} relationship{}",
            plan.relationships,
            if plan.relationships == 1 { "" } else { "s" },
        ));
    }
    if plan.github_bases > 0 {
        lines.push(format!(
            "  rewrote   {} github-sync merge base{}",
            plan.github_bases,
            if plan.github_bases == 1 { "" } else { "s" },
        ));
    }
    lines.push(format!("  backup    {}", outcome.backup_path.display()));
    match &outcome.pointer_updated {
        PointerUpdate::NoCheckout => {}
        PointerUpdate::Updated(path) => {
            lines.push(format!("  updated   {} (checkout pointer)", path.display()));
        }
        PointerUpdate::Failed { path, reason } => {
            lines.push(format!(
                "  the checkout's pointer at {} was not updated: {reason}. Update its `prefix` \
                 to `{}` by hand.",
                path.display(),
                plan.new_prefix
            ));
        }
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

/// The `story token …` family (SH-255).
///
/// Process management, like [`dispatch_web`]: a token record is the daemon's
/// own state, not project data, so every verb speaks to a running daemon's
/// control route rather than through `/api/v1/invoke`. See [`crate::token`].
fn dispatch_token(action: TokenAction) -> Result<Response, AppError> {
    match action {
        TokenAction::New { name } => crate::token::handle_new(&name).map(Response::Message),
        TokenAction::List => crate::token::handle_list().map(Response::Message),
        TokenAction::Revoke { name } => crate::token::handle_revoke(&name).map(Response::Message),
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
            crate::daemon::commands::note_tailnet_pending(&info);
            Ok(Response::Message(format!(
                "storyhook daemon {} running at {} (PID {})",
                info.version,
                info.dashboard_url(),
                info.pid
            )))
        }
        DaemonAction::Stop { force } => {
            crate::daemon::commands::stop(&env, force).map(Response::Message)
        }
        DaemonAction::Status => crate::daemon::commands::status(&env).map(Response::Message),
        DaemonAction::Install => crate::daemon::commands::install(&env).map(Response::Message),
        DaemonAction::Uninstall => crate::daemon::commands::uninstall(&env).map(Response::Message),
        DaemonAction::Token => crate::daemon::commands::token(&env).map(Response::Message),
        DaemonAction::Serve { .. } => Err(AppError::Usage(
            "`story daemon --serve` is handled before dispatch".to_string(),
        )),
    }
}

/// `story doctor abandoned …` — the ledger [`ledger_abandoned`](crate::daemon::lifecycle::ledger_abandoned)
/// writes, reviewed and triaged by hand.
fn dispatch_doctor_abandoned(action: AbandonedAction) -> Result<Response, AppError> {
    let env = Environment::from_process(None)?;
    match action {
        AbandonedAction::List => {
            let ledger = crate::daemon::lifecycle::read_abandoned(&env);
            Ok(Response::Message(abandoned_ledger_message(&ledger)))
        }
        AbandonedAction::Clear { request_id } => {
            let changed = crate::daemon::lifecycle::clear_abandoned(&env, request_id.as_deref());
            Ok(Response::Message(match request_id {
                Some(id) if changed => format!("forgot the abandoned `{id}`."),
                Some(id) => format!("no abandoned entry named `{id}`; nothing changed."),
                None if changed => "forgot every abandoned entry.".to_string(),
                None => "the abandoned-work ledger was already empty.".to_string(),
            }))
        }
    }
}

/// What `story doctor abandoned` prints: every entry, with the recovery this
/// codebase can actually recommend for its kind of work — `github-sync` may
/// have made partial progress against GitHub itself and is safest re-run;
/// anything else changed local data at most, and `story show`/`story list`
/// answer whether it landed.
fn abandoned_ledger_message(ledger: &[crate::daemon::lifecycle::AbandonedRequest]) -> String {
    if ledger.is_empty() {
        return "no abandoned commands.".to_string();
    }
    let mut body = format!(
        "{} abandoned command{}, each one this daemon started but never confirmed \
         finishing:\n\n",
        ledger.len(),
        if ledger.len() == 1 { "" } else { "s" }
    );
    for entry in ledger {
        let recovery = if entry.request.command == "github-sync" {
            "may have made partial progress against GitHub itself; re-running it is safe \
             and picks up where it left off"
        } else {
            "may or may not have written locally; `story show`/`story list` on the story \
             it named answers whether it landed"
        };
        body.push_str(&format!(
            "  {}  `{}`{}\n    started {}, abandoned {}\n    {}\n    {}\n\n",
            entry.request.request_id,
            entry.request.command,
            entry
                .request
                .project
                .as_ref()
                .map(|p| format!(" on `{p}`"))
                .unwrap_or_default(),
            entry.request.started_at,
            entry.abandoned_at,
            entry.reason,
            recovery,
        ));
    }
    body.push_str(
        "`story doctor abandoned clear <request-id>` forgets one once you have checked \
         it; `--all` forgets every entry above.",
    );
    body
}

/// `story doctor crashes [clear (--all | <crash-id>)]` (SH-287) — the same
/// shape [`dispatch_doctor_abandoned`] uses, and for the same reason: reads
/// and writes one file under the daemon's own state directory, no project or
/// store involved.
fn dispatch_doctor_crashes(action: CrashesAction) -> Result<Response, AppError> {
    let env = Environment::from_process(None)?;
    match action {
        CrashesAction::List => {
            let ledger = crate::daemon::crash::read_crashes(&env);
            Ok(Response::Message(crashes_ledger_message(&ledger)))
        }
        CrashesAction::Clear { crash_id } => {
            let changed = crate::daemon::crash::clear_crash(&env, crash_id.as_deref());
            Ok(Response::Message(match crash_id {
                Some(id) if changed => format!("forgot the crash `{id}`."),
                Some(id) => format!("no crash named `{id}`; nothing changed."),
                None if changed => "forgot every crash.".to_string(),
                None => "the crash ledger was already empty.".to_string(),
            }))
        }
    }
}

/// What `story doctor crashes` prints: every crash this daemon has noticed on
/// relaunch, what the evidence says caused it, and what became of its bug
/// report.
fn crashes_ledger_message(ledger: &[crate::daemon::crash::CrashRecord]) -> String {
    use crate::daemon::crash::{CrashClassification, FiledOutcome};

    if ledger.is_empty() {
        return "no crashes.".to_string();
    }
    let mut body = format!(
        "{} crash{}, each one this daemon noticed on relaunch:\n\n",
        ledger.len(),
        if ledger.len() == 1 { "" } else { "s" }
    );
    for entry in ledger {
        let classification = match &entry.classification {
            CrashClassification::Panicked => "panicked".to_string(),
            CrashClassification::FatalSignal(signal) => format!("fatal signal {signal}"),
            CrashClassification::UncleanExit => "unclean exit".to_string(),
        };
        let daemon = entry
            .daemon
            .as_ref()
            .map(|d| format!(", daemon {} pid {}", d.version, d.pid))
            .unwrap_or_default();
        let filed = match &entry.filed {
            FiledOutcome::Pending => "pending — not yet reviewed".to_string(),
            FiledOutcome::Filed(id) => format!("filed as `{id}`"),
            FiledOutcome::Deduped(id) => format!("seen again; folded into `{id}`"),
            FiledOutcome::Withheld(reason) => format!("withheld: {reason}"),
        };
        body.push_str(&format!(
            "  {}  {classification}{daemon}\n    detected {}\n    {filed}\n",
            entry.id, entry.detected_at,
        ));
        if let Some(log_path) = &entry.log_path {
            body.push_str(&format!("    log: `{}`\n", log_path.display()));
        }
        body.push('\n');
    }
    body.push_str(
        "`story doctor crashes clear <crash-id>` forgets one once you have checked it; \
         `--all` forgets every entry above.",
    );
    body
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
        HooksAction::Install => service
            .install_git_hooks()
            .map(|r| Response::MessageWithWarnings(r.message(), r.warnings())),
        HooksAction::Uninstall => service
            .uninstall_git_hooks()
            .map(|r| Response::MessageWithWarnings(r.message(), r.warnings())),
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
                .map(format_type_line)
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        TypeAction::Add {
            slug,
            description,
            emoji,
        } => {
            let added = service.add_type(&slug, description.as_deref(), emoji.as_deref())?;
            let mut message = format!("added type {}", added.slug);
            if added.emoji.is_none() {
                message.push_str(&format!(
                    "; give it a glyph with `story type set {} --emoji <glyph>`",
                    added.slug
                ));
            }
            Ok(Response::Message(message))
        }
        TypeAction::Set {
            slug,
            description,
            clear_description,
            emoji,
            clear_emoji,
        } => {
            let changes = TypeChanges {
                description: if clear_description {
                    FieldEdit::Clear
                } else {
                    description.map_or(FieldEdit::Keep, FieldEdit::Set)
                },
                emoji: if clear_emoji {
                    FieldEdit::Clear
                } else {
                    emoji.map_or(FieldEdit::Keep, FieldEdit::Set)
                },
            };
            let updated = service.update_type(&slug, &changes)?;
            Ok(Response::Message(format!("updated type {}", updated.slug)))
        }
        TypeAction::Remove { slug } => {
            service.remove_type(&slug)?;
            Ok(Response::Message(format!("removed type {slug}")))
        }
    }
}

/// One line of `story type list`: `<emoji> <slug> — <description>`, with
/// either half omitted when the type has none.
fn format_type_line(t: &TypeDef) -> String {
    let mut line = match &t.emoji {
        Some(emoji) => format!("{emoji} {}", t.slug),
        None => t.slug.clone(),
    };
    if let Some(description) = &t.description {
        line.push_str(&format!(" — {description}"));
    }
    line
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
    env: &Environment,
    root: &Path,
    now: &str,
    invocation: Invocation,
) -> Result<Response, AppError> {
    dispatch_unscoped_with_stdin(store, env, root, now, invocation, None)
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
///   yet; `main` answers it before anything is opened. **Only `New`** — its
///   sibling `StoreAction::Backup` is the opposite: it backs up the *ambient*
///   store, so it needs one open like any ordinary command, and is matched
///   for that specifically rather than falling out of a blanket
///   `Invocation::Store { .. }`.
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
            | Invocation::Token { .. }
            | Invocation::DoctorAbandoned { .. }
            | Invocation::DoctorCrashes { .. }
            | Invocation::Store {
                action: StoreAction::New { .. }
            }
            | Invocation::Update { .. }
            | Invocation::Help
            | Invocation::HelpTopic { .. }
            | Invocation::HelpCompact
            | Invocation::HelpAll
            | Invocation::Version
            // The OS keychain, not project data — see `GithubAuthAction`'s
            // own doc. `main.rs` intercepts it even earlier than this, in its
            // own dedicated block, because `Login`'s prompt needs a terminal
            // this predicate has no way to ask about.
            | Invocation::GithubAuth { .. }
    )
}

/// Answers an invocation that [`needs_no_store`] accepts.
///
/// Every arm here is reachable with no database on the machine at all. Callers
/// must gate on [`needs_no_store`] first; an invocation that needs a store
/// reaches the fallback arm, which is an internal error rather than a guess.
///
/// [`dispatch_unscoped_with_stdin`] forwards here rather than keeping its own copies,
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
        // Named tokens (SH-255): daemon process/filesystem state, not store
        // data, for the reason `TokenAction`'s own doc gives — dispatched
        // client-side against the control route, exactly like `web revoke`.
        Invocation::Token { action } => dispatch_token(action),
        // Reads and writes one file under the daemon's own state directory
        // — no project, no store, exactly like the daemon commands above.
        Invocation::DoctorAbandoned { action } => dispatch_doctor_abandoned(action),
        // The same shape as `DoctorAbandoned` immediately above, and for the
        // same reason (SH-287).
        Invocation::DoctorCrashes { action } => dispatch_doctor_crashes(action),
        // `main` answers this one before a store is ever opened, for the
        // reasons on [`create_store`]. Reaching here means a caller went round
        // the front door, and the honest answer is that no store this arm could
        // reach is the right one to create anything with. Narrowed to `New`
        // specifically — `needs_no_store` no longer sends `Backup` here, and a
        // `Backup` reaching this match anyway falls to the `other` arm below,
        // whose message correctly names the invocation rather than blaming
        // `store new` for it.
        Invocation::Store {
            action: StoreAction::New { .. },
        } => Err(AppError::Storage(
            "`story store new` is handled before the store is opened".to_string(),
        )),
        // `main.rs` answers every `GithubAuth` action in its own dedicated
        // block, before this function or even `Environment::from_process`
        // runs — `Login`'s masked prompt needs a terminal, and neither this
        // function nor its caller has one. Reaching here means some other
        // caller (a hand-built request, a future REST route) went round that
        // block; the honest answer is that this command has no meaning
        // outside the CLI client that owns the terminal.
        Invocation::GithubAuth { .. } => Err(AppError::Usage(
            "`story github-auth` only runs in the CLI client, before a store is opened; it \
             cannot be dispatched to a daemon or over the wire"
                .to_string(),
        )),
        other => Err(AppError::Storage(format!(
            "internal: `{}` needs a store and must not be dispatched without one",
            invocation_name(&other)
        ))),
    }
}

/// [`dispatch_unscoped`], given whatever the client read on standard input.
pub fn dispatch_unscoped_with_stdin<S: Store>(
    store: &S,
    env: &Environment,
    root: &Path,
    now: &str,
    invocation: Invocation,
    stdin_input: Option<&str>,
) -> Result<Response, AppError> {
    if needs_no_store(&invocation) {
        return dispatch_without_store(invocation);
    }
    match invocation {
        Invocation::Project { action } => dispatch_project(store, root, now, action),
        // `story store new` is intercepted before a store is even opened (see
        // [`create_store`]) and never reaches here; `Backup` is the only
        // `StoreAction` `needs_no_store` lets through. Unconfirmed — it only
        // ever creates a file — and store-wide rather than project-scoped,
        // which is why it is answered here rather than in `dispatch`.
        Invocation::Store {
            action: StoreAction::Backup { label },
        } => dispatch_store_backup(store, env, label.as_deref()),
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
        // A directory, not a project: these write the repository's hook
        // directory (wherever git says that is), read `hooks.toml`, or install
        // an editor plugin, and the legacy path answered all of them in a
        // directory storyhook had never heard of.
        Invocation::Hooks { action } => match action {
            HooksAction::Install => system::install_git_hooks(root)
                .map(|r| Response::MessageWithWarnings(r.message(), r.warnings())),
            HooksAction::Uninstall => system::uninstall_git_hooks(root)
                .map(|r| Response::MessageWithWarnings(r.message(), r.warnings())),
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
        Invocation::ImportProject { file, legacy_links } => {
            let raw = std::fs::read_to_string(resolve_against(root, &file))
                .map_err(|e| AppError::Storage(format!("failed to read {file}: {e}")))?;
            let export: ProjectExport = serde_json::from_str(&raw)?;
            let outcome = transfer::import_project(
                store,
                root,
                &Clock::Fixed(now.to_string()),
                &export,
                legacy_links,
            )?;
            // Best-effort, run only once the restore has committed — not part
            // of the same transaction, and only meaningful when both the
            // document carried a github-sync configuration and this build can
            // interpret it (SH-189). See the function's own doc comment for
            // why it cannot run any earlier or live in `import_project`
            // itself.
            #[cfg(feature = "github-sync")]
            crate::service::github::reconcile_restored_github_remote(store, root)?;
            let message = format!("imported project with {} stories", outcome.stories);
            if outcome.skipped_remotes.is_empty() {
                Ok(Response::Message(message))
            } else {
                let warnings = outcome
                    .skipped_remotes
                    .iter()
                    .map(|skipped| {
                        format!(
                            "`{}` is already registered to project `{}`; not re-registered by \
                             this restore",
                            skipped.url, skipped.holder
                        )
                    })
                    .collect();
                Ok(Response::MessageWithWarnings(message, warnings))
            }
        }
        other => Err(not_yet_ported(&other)),
    }
}

/// `story store backup [--label <text>]` — a verified, on-demand backup of
/// the ambient store, unconfirmed and store-wide (SH-135).
///
/// Delegates to [`crate::daemon::backup::take_manual`], which writes into
/// [`Environment::maintenance_backups_dir`] rather than the directory the
/// daily schedule prunes, so the result survives by construction. `label`
/// defaults to `"manual"` — anything more specific (`pre-migration`,
/// `pre-sh130-purge`) is the caller's to choose and is validated before it
/// becomes part of the filename.
fn dispatch_store_backup<S: Store>(
    store: &S,
    env: &Environment,
    label: Option<&str>,
) -> Result<Response, AppError> {
    let label = label.unwrap_or("manual");
    let path = crate::daemon::backup::take_manual(store, &env.maintenance_backups_dir(), label)?;
    Ok(Response::Message(format!(
        "backup written to {} (label: {label}, verified: VACUUM INTO + integrity_check + page count)\n\
         `story daemon status` / `story web status` report it alongside every other backup.",
        path.display()
    )))
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
        // Claude Code pipes the SessionStart hook payload (session_id, cwd,
        // source, ...) on stdin; session.rs reads `session_id` out of it to
        // publish the dispatch sentinel (SH-231). The hook script always pipes
        // something (real JSON, or `{}`), so this never blocks on a terminal
        // in normal use — same as `Import` with no `--file`, which has read
        // stdin unconditionally since before this function existed.
        Invocation::SessionStart => true,
        _ => false,
    }
}

/// Whether running `invocation` would spend the caller's GitHub credential.
///
/// The client asks before it sends, exactly as it does for
/// [`reads_stdin`] — and for the same reason, since the daemon has no
/// legitimate credential of its own to fall back on.
///
/// # Why this one is exhaustive where `reads_stdin` is not
///
/// A wildcard here would mean that a *later* verb needing a token silently gets
/// `None` and fails with an auth error nobody can explain — which is SH-153
/// again, arriving by the same route. Listing every variant makes adding one a
/// compile error at this function, where the question "does this spend a
/// credential?" gets asked once and answered deliberately. The cost is a long
/// match; `invocation_name` below pays the same cost for a weaker reason.
#[must_use]
pub fn needs_github_token(invocation: &Invocation) -> bool {
    match invocation {
        Invocation::GithubSync { .. } | Invocation::PrCheck { .. } => true,
        // `LinkPr`/`UnlinkPr` never call GitHub — a PR URL is parsed, not
        // fetched — so they spend no credential, unlike `PrCheck` above.
        Invocation::LinkPr { .. } | Invocation::UnlinkPr { .. } => false,
        // Everything else, listed rather than defaulted. See above.
        Invocation::Help
        | Invocation::Project { .. }
        | Invocation::New { .. }
        | Invocation::MemberAdd { .. }
        | Invocation::State { .. }
        | Invocation::List { .. }
        | Invocation::Search { .. }
        | Invocation::Next { .. }
        | Invocation::Summary
        | Invocation::Report { .. }
        | Invocation::Doctor { .. }
        | Invocation::DoctorAbandoned { .. }
        | Invocation::DoctorCrashes { .. }
        | Invocation::Show { .. }
        | Invocation::Log { .. }
        | Invocation::Comment { .. }
        | Invocation::Assign { .. }
        | Invocation::SetState { .. }
        | Invocation::SetAwaiting { .. }
        | Invocation::ClearAwaiting { .. }
        | Invocation::SetPriority { .. }
        | Invocation::SetLabels { .. }
        | Invocation::Reopen { .. }
        | Invocation::Hide { .. }
        | Invocation::Unhide { .. }
        | Invocation::HideState { .. }
        | Invocation::Delete { .. }
        | Invocation::Purge { .. }
        | Invocation::BulkUpdate { .. }
        | Invocation::Import { .. }
        | Invocation::Decompose { .. }
        | Invocation::Export
        | Invocation::ImportProject { .. }
        | Invocation::Migrate { .. }
        | Invocation::Context { .. }
        | Invocation::Handoff { .. }
        | Invocation::Phase { .. }
        | Invocation::Type { .. }
        | Invocation::Epic { .. }
        | Invocation::Graph { .. }
        | Invocation::SetFields { .. }
        | Invocation::Relate { .. }
        | Invocation::Hooks { .. }
        | Invocation::Scaffold { .. }
        | Invocation::CommitSync { .. }
        | Invocation::HelpTopic { .. }
        | Invocation::HelpCompact
        | Invocation::HelpAll
        | Invocation::Plugin { .. }
        | Invocation::Web { .. }
        | Invocation::Daemon { .. }
        | Invocation::Token { .. }
        | Invocation::Store { .. }
        | Invocation::SessionStart
        | Invocation::Update { .. }
        | Invocation::Version
        | Invocation::ProjectSnapshot
        | Invocation::History { .. }
        | Invocation::Publish { .. } => false,
        // `Login` does spend a credential, but never through this envelope —
        // it is handled entirely client-side in `main.rs`, which prompts for
        // the PAT itself and writes it straight to the keychain. `false` here
        // is "this invocation never rides `InvokeRequest::github_token`", not
        // "this command needs no credential at all".
        Invocation::GithubAuth { .. } => false,
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
    // Said out loud, because the alternative is a user wondering later why
    // `story project list` shows no origin against this project — or not
    // wondering, and being surprised by a clone that does not resolve (SH-151).
    if let crate::service::OriginOutcome::Inherited { owner, holder } = &outcome.origin {
        let held = holder.as_ref().map_or_else(
            || "no project has registered it".to_string(),
            |slug| format!("project `{slug}` has registered it"),
        );
        message.push_str(&format!(
            "\n\nThis directory does not own its repository's origin — `{}` does, and {held}.\n\
             That is fine for a project inside a larger repository: this one is identified by\n\
             the .storyhook.toml here, so commit it, or name the project with `--project`.",
            owner.display()
        ));
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
///
/// Public because the daemon publishes it in
/// [`crate::daemon::lifecycle::CurrentRequest`] and the client reads it back to
/// choose a deadline — so these strings are a small wire vocabulary shared
/// between the two, not only a diagnostic (SH-144).
pub fn invocation_name(invocation: &Invocation) -> &'static str {
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
        Invocation::Log { .. } => "log",
        Invocation::Comment { .. } => "comment",
        Invocation::Assign { .. } => "assign",
        Invocation::SetState { .. } => "set-state",
        Invocation::SetAwaiting { .. } => "set-awaiting",
        Invocation::ClearAwaiting { .. } => "clear-awaiting",
        Invocation::SetPriority { .. } => "set-priority",
        Invocation::SetLabels { .. } => "set-labels",
        Invocation::Reopen { .. } => "reopen",
        Invocation::Hide { .. } => "hide",
        Invocation::Unhide { .. } => "unhide",
        Invocation::HideState { .. } => "hide-state",
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
        Invocation::LinkPr { .. } => "link-pr",
        Invocation::UnlinkPr { .. } => "unlink-pr",
        Invocation::PrCheck { .. } => "pr-check",
        Invocation::GithubAuth { .. } => "github-auth",
        Invocation::HelpTopic { .. } => "help-topic",
        Invocation::HelpCompact => "help-compact",
        Invocation::HelpAll => "help-all",
        Invocation::Plugin { .. } => "plugin",
        Invocation::Web { .. } => "web",
        Invocation::Daemon { .. } => "daemon",
        Invocation::Token { .. } => "token",
        Invocation::DoctorAbandoned { .. } => "doctor-abandoned",
        Invocation::DoctorCrashes { .. } => "doctor-crashes",
        Invocation::Store { .. } => "store",
        Invocation::SessionStart => "session-start",
        Invocation::Update { .. } => "update",
        Invocation::Version => "version",
        Invocation::Migrate { .. } => "migrate",
        Invocation::ProjectSnapshot => "project-snapshot",
        Invocation::History { .. } => "history",
        Invocation::Publish { .. } => "publish",
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
    announce_waits: bool,
}

impl HttpInvoker {
    /// An invoker that runs commands from `cwd` through `env`'s daemon.
    pub fn new(env: Environment, cwd: impl Into<PathBuf>) -> Self {
        Self {
            env,
            cwd: cwd.into(),
            hook_depth: 0,
            announce_waits: true,
        }
    }

    /// Sets how deep inside an event hook this invocation is running.
    #[must_use]
    pub fn hook_depth(mut self, hook_depth: u32) -> Self {
        self.hook_depth = hook_depth;
        self
    }

    /// Whether a wait past [`crate::daemon::lifecycle::SERVED_PATIENCE`]
    /// prints `storyhook: waiting for the daemon...` to stderr. On by
    /// default, matching every ordinary command.
    ///
    /// `story tui` turns this off: `announce_waiting_on` writes to stderr
    /// whenever it is a terminal, and inside the TUI's alternate screen
    /// stderr *is* the screen — the message would land as garbled text over
    /// whatever the board was drawing, not as the plain-text notice a
    /// scripted or piped caller sees.
    #[must_use]
    pub fn announce_waits(mut self, announce: bool) -> Self {
        self.announce_waits = announce;
        self
    }

    /// The HTTP client this invoker uses.
    ///
    /// The HTTP client this invoker uses.
    ///
    /// Two phase bounds, and **only** two, because only two of ureq's phases can
    /// be bounded without also bounding the command.
    ///
    /// `timeout_connect` is the old one: the peer is on loopback and either
    /// accepts at once or is not there.
    ///
    /// `timeout_recv_body` is safe because the reply is a fully materialised
    /// `String` before the daemon writes a byte of it, so once the head has
    /// arrived the body is already computed and only has to cross loopback.
    ///
    /// # What was tried and taken back out
    ///
    /// `timeout_send_request` and `timeout_send_body` look equally free — the
    /// request is capped at 64 KiB by [`crate::api::http::MAX_BODY_BYTES`] and
    /// goes to loopback — and they are not. ureq checks a phase's *preceding*
    /// deadlines alongside its own (`Timeout::preceeding`, `timings.rs`), and
    /// `RecvResponse` names both of them as predecessors. So a deadline set on
    /// either one keeps running while the client waits for the response head —
    /// which is to say, while the daemon is doing the work. Setting
    /// `timeout_send_request(5s)` made every command slower than five seconds
    /// fail with `timeout: send request`, measured against a peer that had
    /// already read the entire request.
    ///
    /// `timeout_global` and `timeout_recv_response` are left unset for the
    /// reason this whole story exists: waiting for the response head *is*
    /// waiting for the command to finish, and how long that may legitimately
    /// take is not a property of the socket. [`Self::send`] bounds it from the
    /// daemon's own published record instead.
    fn agent() -> ureq::Agent {
        use std::time::Duration;
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_recv_body(Some(Duration::from_secs(30)))
            .build()
            .into()
    }

    /// Posts one envelope to a daemon and reconstructs its answer, giving up if
    /// the daemon stops finishing things.
    ///
    /// # What is bounded, and what is not
    ///
    /// The exchange runs on a worker thread while this one watches the daemon's
    /// [`CurrentRequest`] record. The clock resets whenever that record changes,
    /// appears or disappears — each of which means the daemon **finished
    /// something** — and fires only when it has not moved for the deadline
    /// belonging to the command the record *names*.
    ///
    /// So **queued time is unbounded by construction**. A client behind a long
    /// `github-sync` waits exactly as long as that sync takes, because the
    /// record keeps moving; it is charged only for silence. That is the whole
    /// reason this is not the global deadline the old comment here correctly
    /// refused: the daemon serves one request at a time, so a wall-clock bound
    /// on `story list` would have to be as long as the longest command it might
    /// be queued behind, or it would abandon healthy work.
    ///
    /// `bound` is a parameter rather than a constant so a test can drive it in
    /// milliseconds. A suite that had to outwait 120s to prove a 120s bound
    /// would not be run.
    ///
    /// `announce` gates the stderr notice past `patience` — see
    /// [`Self::announce_waits`]; every existing caller here and in
    /// `tests/daemon_timeouts.rs` passes `true`, unchanged from before this
    /// parameter existed.
    #[allow(clippy::too_many_arguments)]
    pub fn send(
        env: &Environment,
        daemon: &crate::daemon::lifecycle::DaemonInfo,
        request: &crate::api::wire::WireRequest,
        bound: Option<crate::daemon::lifecycle::ExchangeBound>,
        poll: std::time::Duration,
        patience: std::time::Duration,
        announce: bool,
    ) -> Result<Result<Response, AppError>, Transport> {
        use crate::daemon::lifecycle::{self, Observed, Verdict};
        use std::sync::mpsc;
        use std::time::Instant;

        let url = format!("http://127.0.0.1:{}/api/v1/invoke", daemon.port);
        let token = daemon.token.clone();
        let body = serde_json::to_string(request).map_err(|e| Transport::Sent(e.to_string()))?;

        let (tx, rx) = mpsc::channel();
        // Detached rather than joined: on the timeout path this thread is
        // blocked in a read nothing here can interrupt, and leaving it is the
        // price of reporting the failure at all. The process ends either way.
        std::thread::spawn(move || {
            let _ = tx.send(Self::exchange(&url, &token, &body));
        });

        let started = Instant::now();
        let mut seen: Vec<lifecycle::CurrentRequest> = Vec::new();
        let mut changed_at = started;
        // Set the instant this client's own request_id is first observed in
        // the set, and never reset while it stays there — the clock row 2/3
        // of `verdict`'s table bounds *my own* served time by, independent of
        // whatever else the daemon is or is not also finishing (SH-173).
        let mut mine_seen_at: Option<Instant> = None;
        let mut announced = false;

        loop {
            // The wait comes first, so a command that finishes inside one poll
            // interval reads no files at all.
            match rx.recv_timeout(poll) {
                Ok(outcome) => return outcome,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Transport::Sent(
                        "the thread carrying this request to the daemon died".to_string(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }

            let inflight = lifecycle::read_inflight(env);
            if inflight != seen {
                seen = inflight;
                changed_at = Instant::now();
            }
            if mine_seen_at.is_none() && seen.iter().any(|r| r.request_id == request.request_id) {
                mine_seen_at = Some(Instant::now());
            }
            if announce && !announced && started.elapsed() >= patience {
                announced = true;
                announce_waiting_on(&seen);
            }

            let observed = Observed {
                mine: &request.request_id,
                inflight: &seen,
                mine_for: mine_seen_at.map(|at| at.elapsed()),
                since_change: changed_at.elapsed(),
                total: started.elapsed(),
            };
            match lifecycle::verdict(&observed, bound) {
                Verdict::Wait => {}
                Verdict::GiveUpMine(record) => {
                    return Err(Transport::Sent(stalled_message_mine(
                        &record,
                        changed_at.elapsed(),
                        env,
                    )));
                }
                Verdict::GiveUpQueued(records) => {
                    return Err(Transport::Sent(stalled_message_queued(
                        &records,
                        changed_at.elapsed(),
                        env,
                    )));
                }
                Verdict::GaveUpUnpublished => {
                    return Err(Transport::Sent(unpublished_message(daemon, env)));
                }
            }
        }
    }

    /// The exchange itself, on its own thread.
    fn exchange(
        url: &str,
        token: &str,
        body: &str,
    ) -> Result<Result<Response, AppError>, Transport> {
        let response = Self::agent()
            .post(url)
            .header(crate::api::rpc::TOKEN_HEADER, token)
            .header("Content-Type", "application/json")
            .send(body)
            .map_err(Transport::from)?;
        let envelope: crate::api::wire::WireResponse = response
            .into_body()
            .read_json()
            .map_err(|e| Transport::Sent(format!("the daemon's answer was unreadable: {e}")))?;
        Ok(envelope.into_result())
    }
}

/// Tells a human at a terminal what the daemon is doing, once, while they wait.
///
/// TTY-gated for the reason `lifecycle::announce_waiting` records:
/// `tests/cli_error_streams.rs` pins an empty stderr both for a successful
/// command and for anything under `--json`, so an unconditional line here would
/// break a contract the suite already holds. A pipe gets silence and the same
/// exit code it always got.
fn announce_waiting_on(inflight: &[crate::daemon::lifecycle::CurrentRequest]) {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        return;
    }
    if inflight.is_empty() {
        eprintln!(
            "storyhook: waiting for the daemon. `story daemon status` answers without \
             asking it."
        );
        return;
    }
    let names = inflight
        .iter()
        .map(|r| format!("`{}`", r.command))
        .collect::<Vec<_>>()
        .join(", ");
    let plural = if inflight.len() == 1 {
        ("one command".to_string(), "it")
    } else {
        (format!("{} commands", inflight.len()), "them")
    };
    eprintln!(
        "storyhook: waiting for the daemon. It is running {} ({names}); yours may be \
         queued behind {}. `story daemon status` answers without asking it.",
        plural.0, plural.1,
    );
}

/// What the user is told when the daemon stops finishing **my own** command.
///
/// Three rules, each of which cost this council an argument.
///
/// It never says "wedged" or "hung": the daemon may be healthy and slow, and the
/// client provably cannot tell. A conditional handed to the user is allowed; a
/// claim made on their behalf is not.
///
/// It names the **pid**, because `story daemon stop` posts a shutdown request
/// bounded by `CONTROL_DEADLINE` and has no signal fallback — against a daemon
/// in this state it fails in five seconds and kills nothing, so advice that
/// stopped there would hand the user a step that cannot work.
///
/// And it **refuses the remedy its neighbour gives**. The `Transport::Sent`
/// message below recommends `story show` or `story list`; both are invocations,
/// both go back through the same queue, and following the advice buys another
/// deadline of silence per attempt. Correct for a dropped connection, actively
/// wrong here.
fn stalled_message_mine(
    record: &crate::daemon::lifecycle::CurrentRequest,
    stalled_for: std::time::Duration,
    env: &Environment,
) -> String {
    format!(
        "the storyhook daemon has been running your `{}` for {}s without finishing \
         it.\n\n  the daemon    pid {}, since {}\n  its log       {}\n\n\
         This command may or may not have run — storyhook will not repeat it, because \
         repeating a write it cannot prove failed is worse than reporting this.\n\n\
         `story daemon status` and that log answer without going through the daemon's \
         queue; `story show` and `story list` do not, and would wait behind the same \
         work. If it is stuck rather than slow, `story daemon stop` gives up after \
         {}s without killing anything, so `kill {}` is the way out — the next `story` \
         command starts a fresh daemon.",
        record.command,
        stalled_for.as_secs(),
        record.pid,
        record.started_at,
        env.daemon_log().display(),
        crate::daemon::lifecycle::CONTROL_DEADLINE.as_secs(),
        record.pid,
    )
}

/// What the user is told when the daemon stops finishing **somebody else's**
/// work this client is only queued behind.
///
/// Same three rules as [`stalled_message_mine`] — see its doc. A distinct
/// function rather than a shared one with a branch inside, because the two
/// tell a genuinely different story: "your own command has not finished" and
/// "you are behind other people's work, and none of it is finishing" are
/// different diagnoses, not one diagnosis with two subjects.
fn stalled_message_queued(
    records: &[crate::daemon::lifecycle::CurrentRequest],
    stalled_for: std::time::Duration,
    env: &Environment,
) -> String {
    // Every record in the set was named by this one daemon process, so they
    // share a pid — any of them names the process to kill.
    let pid = records.first().map_or(0, |r| r.pid);
    let names = records
        .iter()
        .map(|r| format!("`{}`", r.command))
        .collect::<Vec<_>>()
        .join(", ");
    let them = if records.len() == 1 { "it" } else { "them" };
    format!(
        "the storyhook daemon has been running {names} — not your command, and yours \
         is queued behind {them} — for {}s without finishing anything.\n\n  \
         the daemon    pid {}\n  its log       {}\n\n\
         This command may or may not have run — storyhook will not repeat it, because \
         repeating a write it cannot prove failed is worse than reporting this.\n\n\
         `story daemon status` and that log answer without going through the daemon's \
         queue; `story show` and `story list` do not, and would wait behind the same \
         work. If it is stuck rather than slow, `story daemon stop` gives up after \
         {}s without killing anything, so `kill {}` is the way out — the next `story` \
         command starts a fresh daemon.",
        stalled_for.as_secs(),
        pid,
        env.daemon_log().display(),
        crate::daemon::lifecycle::CONTROL_DEADLINE.as_secs(),
        pid,
    )
}

/// What the user is told when the daemon publishes nothing at all.
///
/// A state the record cannot describe, so it gets its own sentence rather than
/// being folded into [`stalled_message`]: the daemon is alive — it holds its
/// pidfile, which is a fact established without asking it — and has not started
/// this command. Post-SH-144 this is the only shape the unauthenticated wedge
/// can present to a client, and the story that owns it is linked from here.
fn unpublished_message(daemon: &crate::daemon::lifecycle::DaemonInfo, env: &Environment) -> String {
    format!(
        "the storyhook daemon accepted this command but never reported starting it, \
         and has reported nothing for {}s.\n\n  the daemon    pid {}, port {}\n  \
         its log       {}\n\nIt holds its pidfile, so it is running; it has simply not \
         begun your command. This command may or may not have run — storyhook will not \
         repeat it. `story daemon status` answers without going through the daemon's \
         queue; `kill {}` is the way out, and the next `story` command starts a fresh \
         daemon.",
        crate::daemon::lifecycle::UNPUBLISHED_DEADLINE.as_secs(),
        daemon.pid,
        daemon.port,
        env.daemon_log().display(),
        daemon.pid,
    )
}

/// How far a failed request got, which is what decides whether it may be
/// repeated.
///
/// Public because [`HttpInvoker::send`] is, and it is: `tests/daemon_timeouts.rs`
/// drives the bound directly against a peer that never answers, the same way it
/// already calls `lifecycle::hello` and `lifecycle::request_shutdown`.
///
/// **A timeout lands on [`Self::Sent`], and nothing about that needed changing.**
/// `From<ureq::Error>` routes only a refused connection and an unresolvable host
/// to [`Self::NotDelivered`]; everything else, timeouts included, is already
/// `Sent`. The record-based bound inherits the same classification for the same
/// reason — once the request has left this process, whether it ran is unknown —
/// and the two obligations that follow are honoured in the message: never
/// re-send, and never claim to know whether the write committed.
#[derive(Debug)]
pub enum Transport {
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
            .project(request.project)
            .github_token(request.github_token)
            .actor(request.actor);

        // Always `None` in production: `bound` exists only as a test seam
        // (`send`'s own docstring) since SH-174 deleted its sole production
        // producer, `$STORYHOOK_EXCHANGE_DEADLINE_SECS`. `None` means "use the
        // deadline each record already carries," which is now provably
        // sufficient — see `event_hooks::HOOK_TIMEOUT_CEILING_SECS`.
        let bound = None;
        let poll = crate::daemon::lifecycle::RECORD_POLL;
        let patience = crate::daemon::lifecycle::SERVED_PATIENCE;

        let daemon = crate::daemon::lifecycle::ensure(&self.env)?;
        match Self::send(
            &self.env,
            &daemon,
            &wire,
            bound,
            poll,
            patience,
            self.announce_waits,
        ) {
            Ok(result) => return result,
            Err(Transport::Sent(detail)) => {
                // The detail is the whole message when the bound fired — it
                // already carries the no-repeat clause and a remedy that works
                // during this failure. Only a genuinely unexplained stop needs
                // the sentence below wrapped around it.
                return Err(AppError::Storage(
                    if detail.contains("may or may not have run") {
                        detail
                    } else {
                        format!(
                            "the storyhook daemon stopped answering: {detail}. This command may or \
                         may not have run — storyhook will not repeat it, because repeating a \
                         write it cannot prove failed is worse than reporting this. Check with \
                         `story show` or `story list`, then try again."
                        )
                    },
                ));
            }
            // Refused: the daemon went away between the check and the send,
            // which is exactly what a version-skew restart looks like from
            // here. Nothing was delivered, so starting one and sending the
            // original request is a first attempt rather than a retry.
            Err(Transport::NotDelivered(_)) => {}
        }

        let daemon = crate::daemon::lifecycle::ensure(&self.env)?;
        match Self::send(
            &self.env,
            &daemon,
            &wire,
            bound,
            poll,
            patience,
            self.announce_waits,
        ) {
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
}

impl<'a, S: Store> StoreInvoker<'a, S> {
    /// An invoker over `store`, running from `cwd` under `env`.
    pub fn new(store: &'a S, cwd: impl Into<PathBuf>, env: Environment) -> Self {
        Self {
            store,
            cwd: cwd.into(),
            env,
            hook_depth: 0,
        }
    }

    /// Sets how deep inside an event hook this invocation is running.
    #[must_use]
    pub fn hook_depth(mut self, hook_depth: u32) -> Self {
        self.hook_depth = hook_depth;
        self
    }

    /// The project this invocation acts on, in the order SH-116 fixed.
    ///
    /// Four steps, written as four consecutive early-returns rather than as
    /// nested conditions, and that shape is load-bearing: each step stays
    /// separately deletable, and a nest would make any such deletion a rewrite
    /// — a rewrite is what stops a bisect attributing a regression.
    ///
    /// 1. **The selector** — `--project`, else `$STORYHOOK_PROJECT`, collapsed
    ///    into one value by the client. **Binding**: it resolves or it refuses,
    ///    and never falls through to the directory. That is the fix for a
    ///    measured defect — `STORYHOOK_PROJECT=nonesuch story list` used to be
    ///    ignored in silence and answer about whatever project the directory
    ///    happened to be.
    /// 2. **The pointer walk** — the committed `.storyhook.toml`, at the working
    ///    directory and then each ancestor, bounded by the repository top.
    /// 3. **The origin** — this directory's `origin`, normalized, looked up
    ///    among the registered ones.
    /// 4. Otherwise `None`, and the caller refuses.
    ///
    /// # Why step 2 is still here
    ///
    /// SH-119 was written to delete it and deleted half: the directory's row in
    /// `project_paths` went with the index (see [`resolve_at`]). The committed
    /// pointer stayed, and SH-167 then extended it — `story project link
    /// checkout` writes one. Kept deliberately, for two things an origin cannot
    /// do:
    ///
    /// * A **fresh clone resolves immediately**, on a machine whose store has
    ///   registered nothing. An origin registration is a fact about this store;
    ///   a committed uuid travels in the repository.
    /// * **Two projects in one repository** each resolve at their own
    ///   subdirectory. A URL belongs to at most one project by construction, so
    ///   an origin can only ever answer for one of them — pinned by
    ///   `origin_ownership.rs::a_second_project_in_one_repository_resolves_by_its_pointer`.
    ///
    /// This costs the epic nothing it promised. The filesystem is never
    /// *required*: step 1 alone always answers. And a pointer naming a uuid this
    /// store does not hold **refuses** rather than guessing
    /// ([`unresolvable_pointer_refusal`]) — the failure mode SH-112 asked for.
    /// Recorded in `docs/spec/server-owned.md`'s "As built".
    ///
    /// # Why the walk outranks the origin
    ///
    /// Cost, not preference. The walk is a `stat` per ancestor; the origin is a
    /// `git` subprocess — 14 ms, against an 11.8 ms whole-command baseline — so
    /// asking it first would roughly double every command the walk was going to
    /// answer anyway. Last means it is paid by a command that is about to refuse,
    /// where it buys the refusal its `origin` line.
    ///
    /// SH-116 measured that when **no project in the store and no fixture in the
    /// suite had a registered origin**, which made the case overwhelming and is
    /// no longer true — most projects have one now. The subprocess is what
    /// carries the ordering today, not the scarcity.
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

        project_at(self.store, &self.cwd)
    }

    /// The nearest pointer file at or above the working directory, whatever it
    /// names.
    ///
    /// Only consulted once [`Self::resolve`] has already failed, to tell a
    /// checkout that *claims* a project from a directory that claims nothing.
    /// Delegates to [`crate::service::project::pointer_at_or_above`], which
    /// also backs `session::unavailable` — a hook degrading on a slow daemon
    /// asks the identical question this refusal does.
    fn pointer_at_or_above(&self) -> Option<crate::service::project::ProjectPointer> {
        crate::service::project::pointer_at_or_above(&self.cwd)
    }
}

/// Which project a *directory* belongs to — steps 2 to 4 of
/// [`Invoke::resolve_project`], without the selector.
///
/// Free-standing because two callers need this exact walk and disagree only
/// about what its refusals mean. [`Invoke::resolve_project`] propagates them: a
/// command that cannot say which project it is about must not proceed. A
/// project-less command that is merely *curious* — `hooks install`, reading the
/// receipt for a directory it is about to write hooks into — collapses both
/// `Err` and `Ok(None)` to "no project", because refusing would be a regression
/// in a command that has always worked in a directory storyhook has never heard
/// of.
///
/// The one thing that must not happen is a second copy of these rules: the
/// curious caller has to answer `None` in **exactly** the cases the refusing one
/// refuses — a checkout claiming a project this store lacks, and SH-151's
/// inherited-origin probe — or it arms and reads the wrong project's receipt in
/// a monorepo sub-checkout. Two answers to one question is how SH-313 and SH-314
/// both happened.
fn project_at<S: Store>(store: &S, cwd: &Path) -> Result<Option<ProjectId>, AppError> {
    // The nearest directory that *claims* a project this store does not
    // have. It is not an answer, but it outranks every farther one: see
    // `unresolvable_pointer_refusal`.
    let mut claimed: Option<String> = None;
    for dir in crate::service::project::ancestors(cwd) {
        if let Some(project) = resolve_at(store, &dir)? {
            if let Some(uuid) = &claimed {
                return Err(unresolvable_pointer_refusal(uuid));
            }
            return Ok(Some(project));
        }
        // Read only once the directory has failed to resolve, which is what
        // keeps `a_pointer_naming_an_unknown_project_does_not_shadow_a_valid_path_row`
        // true: a stale pointer beside a working path row in the *same*
        // directory still resolves there, because `resolve_at` answered.
        if claimed.is_none()
            && let Ok(Some(pointer)) = crate::service::project::read_pointer(&dir)
        {
            claimed = Some(pointer.uuid);
        }
    }

    if let Some(remote) = crate::service::project::origin_of(cwd)
        && let Some(found) = store.read(|tx| tx.project_by_remote(&remote))?
    {
        // SH-151. An origin answers for the *whole* repository, so it will
        // happily answer for a sub-checkout that claims a project this
        // store does not have — with the enclosing project, silently. That
        // is the fresh-clone-of-a-monorepo shape: `service-b`'s identity
        // travelled in the commit, this machine has never seen it, and the
        // root project's origin is right there to be resolved by mistake.
        //
        // The ownership probe is asked only in that already-rare case, and
        // only to tell this apart from the legitimate one: a checkout at
        // the *top level* whose committed pointer came from somebody else's
        // store, where adopting by the origin the user registered
        // themselves is exactly right.
        if let Some(uuid) = &claimed
            && matches!(
                crate::service::project::origin_at(cwd),
                crate::domain::remote::RepoOrigin::Inherited { .. }
            )
        {
            return Err(unresolvable_pointer_refusal(uuid));
        }
        return Ok(Some(found.id));
    }

    Ok(None)
}

/// What to tell a checkout that names a project this store does not have.
///
/// **One constructor, because two very different paths raise it** and the
/// answer is the same either way. One is the plain fresh clone, where nothing
/// else answers at all. The other is SH-151's: a sub-checkout inside a
/// repository whose origin belongs to *another* project, where something else
/// would very much have answered — with the wrong project, and without saying
/// so. A checkout that states its identity in a committed file is not
/// uninitialized, and neither reader should be told it is.
fn unresolvable_pointer_refusal(uuid: &str) -> AppError {
    AppError::NotFound(format!(
        "this checkout belongs to storyhook project {uuid}, which this machine's store does not \
         have. Run `story project new --prefix <PREFIX>` here to adopt it — an adopted checkout \
         keeps the prefix its pointer file names — or `story import-project` if you have an \
         export of it."
    ))
}

/// A directory and every ancestor of it, nearest first, **stopping at the
/// repository the directory is in**.
///
/// Canonicalized once at the start rather than per level: an uncanonicalized
/// path's ancestors include `..` components that would `stat` the wrong
/// directories, and a directory that cannot be canonicalized (it does not
/// exist) has no meaningful ancestry to walk beyond what it was given.
///
/// # Why it stops (SH-119, R1)
///
/// Unbounded, this walks out of the repository and keeps going — so a scratch
/// directory made under a checkout of storyhook answers as *storyhook*, because
/// storyhook's own committed pointer file is four levels up. A repository is the
/// unit an identity belongs to, so the repository's top level is where looking
/// for one stops.
///
/// The bound is a `stat` for `.git`, not a `git` subprocess: resolution runs on
/// almost every command, and SH-116 measured an 11.8 ms whole-command baseline
/// against which a 14 ms `git` call is not a bound but a doubling.
/// The project `dir` itself identifies — the uuid in its committed pointer file.
///
/// A pointer naming a uuid the store does not hold is **not** an answer here.
/// It falls through, and the caller decides what to do about it; whether an
/// unresolvable pointer should be reported rather than ignored is the guard's
/// question, not resolution's.
///
/// This used to have a second half — the directory's row in `project_paths` —
/// and SH-119 deleted it with the index. A recorded path is a fact about this
/// machine; a committed uuid is a fact about the repository, and it is the one
/// that survives a clone, a move and a rename.
fn resolve_at<S: Store>(store: &S, dir: &Path) -> Result<Option<ProjectId>, AppError> {
    let Some(pointer) = crate::service::project::read_pointer(dir)? else {
        return Ok(None);
    };
    Ok(store.read(|tx| Ok(tx.project_by_uuid(&pointer.uuid)?.map(|project| project.id)))?)
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
///
/// # Why the outer match is exhaustive too (SH-170)
///
/// D8 only reached the inner match: the *outer* one, over [`Invocation`], kept
/// a `_ => None` catch-all, on the reasoning that `New` was the live hazard
/// and the other 49 arms of the day were already correct. That catch-all is
/// exactly the shape D8 had just finished removing one layer down — nothing
/// stops a future top-level verb that creates a project from also falling
/// through it silently, with the same green build and green suite. Naming
/// every variant here, rather than defaulting, is the same fix D8 made,
/// applied to the layer it left alone; [`needs_github_token`] already does
/// this for the same enum, for the same reason (SH-153), and this mirrors it.
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
            | ProjectAction::SetPrefix { .. }
            | ProjectAction::List
            | ProjectAction::Show
            | ProjectAction::Link(_)
            | ProjectAction::Unlink(_)
            | ProjectAction::Settings(_) => None,
        },
        Invocation::Migrate {
            path,
            dry_run: false,
        } => Some(target_dir(cwd, path.as_deref())),
        Invocation::Migrate { dry_run: true, .. } => None,
        // Everything else, listed rather than defaulted. See above.
        Invocation::Help
        | Invocation::New { .. }
        | Invocation::Publish { .. }
        | Invocation::MemberAdd { .. }
        | Invocation::State { .. }
        | Invocation::List { .. }
        | Invocation::Search { .. }
        | Invocation::Next { .. }
        | Invocation::Summary
        | Invocation::Report { .. }
        | Invocation::Doctor { .. }
        | Invocation::DoctorAbandoned { .. }
        | Invocation::DoctorCrashes { .. }
        | Invocation::Show { .. }
        | Invocation::Log { .. }
        | Invocation::Comment { .. }
        | Invocation::Assign { .. }
        | Invocation::SetState { .. }
        | Invocation::SetAwaiting { .. }
        | Invocation::ClearAwaiting { .. }
        | Invocation::SetPriority { .. }
        | Invocation::SetLabels { .. }
        | Invocation::Reopen { .. }
        | Invocation::Hide { .. }
        | Invocation::Unhide { .. }
        | Invocation::HideState { .. }
        | Invocation::Delete { .. }
        | Invocation::Purge { .. }
        | Invocation::BulkUpdate { .. }
        | Invocation::Import { .. }
        | Invocation::Decompose { .. }
        | Invocation::Export
        | Invocation::Context { .. }
        | Invocation::Handoff { .. }
        | Invocation::Phase { .. }
        | Invocation::Type { .. }
        | Invocation::Epic { .. }
        | Invocation::Graph { .. }
        | Invocation::SetFields { .. }
        | Invocation::Relate { .. }
        | Invocation::Hooks { .. }
        | Invocation::Scaffold { .. }
        | Invocation::CommitSync { .. }
        | Invocation::GithubSync { .. }
        | Invocation::LinkPr { .. }
        | Invocation::UnlinkPr { .. }
        | Invocation::PrCheck { .. }
        | Invocation::HelpTopic { .. }
        | Invocation::HelpCompact
        | Invocation::HelpAll
        | Invocation::Plugin { .. }
        | Invocation::Web { .. }
        | Invocation::Daemon { .. }
        | Invocation::Token { .. }
        | Invocation::Store { .. }
        | Invocation::SessionStart
        | Invocation::Update { .. }
        | Invocation::Version
        | Invocation::ProjectSnapshot
        | Invocation::History { .. }
        | Invocation::GithubAuth { .. } => None,
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

/// Whether `invocation` creates one project, interactively named or not.
///
/// Narrower than [`project_creation_target`] on purpose: `ImportProject` and a
/// non-dry-run `Migrate` are bulk verbs a person or a script chose
/// deliberately by typing that command, so they stay under the path-based
/// SH-95 guard alone and are exempt from the SH-122 burst gate below — the
/// gate exists for the caller who never chose to create anything explicitly
/// enough to name a path or a batch. `ProjectAction::New` is the only
/// remaining route to `create_project`, and every one of its variants counts,
/// including `Attach::Nothing`: that request still creates a project, it just
/// creates one with no checkout, which is exactly the shape a test suite's
/// fixture takes.
fn creates_a_project(invocation: &Invocation) -> bool {
    matches!(
        invocation,
        Invocation::Project {
            action: ProjectAction::New(_)
        }
    )
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
        // Same moment, a second question: not "is this path throwaway" but
        // "how many, how fast" — the signature that survives even when the
        // first guard has nothing to judge (SH-122). The read is gated behind
        // `creates_a_project` so no other command pays for the query.
        if creates_a_project(&request.invocation) {
            let projects = self.store.read(|tx| tx.projects())?;
            crate::service::project::refuse_project_burst_in_real_store(
                self.env.store_path(),
                &projects,
                &now,
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
            return dispatch_unscoped_with_stdin(
                self.store,
                &self.env,
                &self.cwd,
                &now,
                request.invocation,
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
                return dispatch_unscoped_with_stdin(
                    self.store,
                    &self.env,
                    &self.cwd,
                    &now,
                    request.invocation,
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
                return Err(unresolvable_pointer_refusal(&pointer.uuid));
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

        // Provenance is assembled here, at the one place a scoped invocation
        // becomes a context, so every event the command goes on to write carries
        // the same answer to "who did this" (SH-246). The command half is read
        // off the invocation itself rather than accepted from the caller —
        // that is precisely what makes it the attested half.
        let provenance =
            Provenance::command(invocation_name(&request.invocation)).with_actor(request.actor);
        let ctx = Ctx::new(self.store, project, &self.cwd, self.env.clone())
            .no_hooks(request.no_hooks)
            .hook_depth(self.hook_depth)
            .with_stdin(request.stdin)
            .with_github_token(request.github_token)
            .with_provenance(provenance);
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
            ProjectAction::SetPrefix { .. } => "project set-prefix",
            ProjectAction::List => "project list",
            ProjectAction::Show => "project show",
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
        Invocation::Token { .. } => "token",
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
        // Every `story store` verb is about the store as a whole, never about
        // a project inside one — `StoreAction::New` already returns above via
        // `needs_no_store`; `StoreAction::Backup` snapshots the *whole*
        // ambient store, so it belongs here for the same reason.
        Invocation::Store { .. } => true,
        _ => false,
    }
}

/// Everything `story doctor` has to say that is not damage.
///
/// **The single assembly, and the reason it is one** (SH-266). These eight
/// sources used to be gathered inside the healthy branch of the `doctor` arm,
/// with the unhealthy branch passing `notices` alone — so a project was told
/// about its orphaned registrations, its unregistered origins, a github remote
/// that had drifted, a command the daemon abandoned, a stale pointer file or a
/// legacy commit link **only while nothing else was wrong**. One real finding
/// and seven of the eight went silent, which is exactly when an operator is
/// most likely to be reading. The list is built once here and both outcomes
/// carry it; there is no second copy to fall behind, and an advisory added
/// later reaches a damaged project by construction rather than by remembering.
///
/// Nothing in it can decide health: advice is a `Vec<String>` and
/// [`IntegrityDetail::report`](crate::error::IntegrityDetail::report) asks its
/// emptiness question of the *findings* alone, which is what makes handing the
/// same list to both outcomes safe (SH-185's separation, by type rather than
/// by convention).
///
/// `audit_catalog` is the caller's answer to "is this a real store", not a
/// preference: an orphaned registration in a throwaway store is a fixture that
/// was supposed to disappear.
/// `story doctor --fix`'s catalog half: forget the links pointing at nothing,
/// record the origins a checkout owns and the store does not.
///
/// Appends what it did to `advice` **as it goes**, one self-describing entry
/// per half, rather than returning a report — so a failure in the second half
/// cannot discard the first half's account of a write that already landed.
/// That is also why `advice` is `&mut` rather than a return value.
///
/// **Each half is one transaction** (SH-275), which is what makes that account
/// complete rather than merely prompt: a half either landed everything it
/// reports or nothing at all, so the entry above a failure is true of the store
/// and the failure needs no count of its own. What it cannot say is *which* of
/// the two the failing half left — see [`SWEEP_ATOMICITY`].
///
/// Store-wide, unlike the per-project repair that runs before it
/// ([`crate::service::CatalogService`] is deliberately not `Ctx`-shaped), which
/// is why its outcome must not be gated on one project's health — SH-270.
///
/// The order is not arbitrary and does not commute with the repair above it:
/// [`orphaned`] counts a project's stories from read-model rows, and
/// `repair_read_model` has just put missing ones back, so a sweep running first
/// would quote a pre-repair count.
///
/// [`orphaned`]: crate::service::CatalogService::orphaned
/// What `story doctor --fix` says when a half of the sweep failed.
const SWEEP_INCOMPLETE: &str = "the catalog sweep did not complete";

/// What it is entitled to say about the store afterwards — **and no more**.
///
/// It claims atomicity and a safe retry. It does **not** claim absence, and the
/// wording is load-bearing rather than stylistic (SH-275): a failure between
/// `COMMIT` and the acknowledgement is reachable in production — that is what
/// [`FaultPoint::AfterCommitBeforeAck`](crate::store::FaultPoint::AfterCommitBeforeAck)
/// models — so from here the caller **cannot know which side of the commit the
/// failure fell on**. "Nothing was registered" would therefore be a claim this
/// code cannot back, and would be false at exactly the moment it mattered.
///
/// The operator does not need the answer they cannot have, because
/// `register_origin` and `forget_checkout` are both re-runnable: what a retry
/// costs is one `git` probe per project.
const SWEEP_ATOMICITY: &str = "Each half of the sweep is one transaction, so the store holds a \
                               half's changes in full or not at all — never part of them. Which \
                               of the two this failure left is not knowable from here and does \
                               not need to be: re-run `story doctor --fix` to settle it, since \
                               re-forgetting a path or re-recording an origin the store already \
                               holds changes nothing.";

/// The whole account of a failed sweep, for the advice channel.
fn catalog_sweep_failure(error: &AppError) -> String {
    format!("{SWEEP_INCOMPLETE}: {error}\n\n{SWEEP_ATOMICITY}")
}

fn catalog_sweep<S: Store>(
    catalog: &crate::service::CatalogService<'_, S>,
    advice: &mut Vec<String>,
) -> Result<(), AppError> {
    let forgotten = catalog.deregister_orphaned()?;
    if !forgotten.is_empty() {
        advice.push(deregistered_message(&forgotten));
    }
    let sweep = catalog.register_found_origins()?;
    if !sweep.is_empty() {
        advice.push(registered_origins_message(&sweep));
    }
    Ok(())
}

fn doctor_advice<S: Store>(
    ctx: &Ctx<'_, S>,
    catalog: &crate::service::CatalogService<'_, S>,
    notices: Vec<String>,
    audit_catalog: bool,
) -> Result<Vec<String>, AppError> {
    let (orphans, origins) = if audit_catalog {
        (catalog.orphaned()?, catalog.unregistered_origins()?)
    } else {
        (Vec::new(), Vec::new())
    };
    // The notice channel SH-185's council settled on leads: it never made the
    // project unhealthy, so it rides along as advice rather than as a finding.
    let mut advice = notices;
    advice.extend(orphan_advice(&orphans));
    advice.extend(origin_advice(&origins));
    advice.extend(github_remote_advice(ctx)?);
    advice.extend(abandoned_advice(ctx.env()));
    advice.extend(crash_advice(ctx.env()));
    advice.extend(pointer_origin_advice(ctx)?);
    advice.extend(pointer_prefix_advice(ctx)?);
    advice.extend(legacy_link_advice(ctx)?);
    Ok(advice)
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

/// What `story doctor` says about a github-synced project whose configured
/// target repository does not match this checkout's own git remote (SH-189).
///
/// Replaces the old `backup_advice`, which warned that `story export` did not
/// carry github-sync's configuration at all — SH-189 made that carry
/// complete, so that warning went false and this is what took its place: not
/// "does a backup carry this" but "does the configured `github.owner`/
/// `github.repo` still name *this* checkout's repository". That question
/// matters most right after a restore into a fork or a relocated clone — the
/// one case [`crate::service::github::reconcile_restored_github_remote`]
/// cannot always answer for certain, since it runs best-effort at restore
/// time and a checkout may not have `origin` set yet — but it is evaluated
/// fresh on every `story doctor` run rather than left as a one-shot flag, so
/// drift introduced at any point (an `origin` changed later, a document
/// restored by an older binary) is still caught.
///
/// Advisory, like [`origin_advice`], and for the same reason: nothing is
/// broken, the project works, and a non-zero exit would make `doctor` red for
/// every github-synced project whose checkout simply has no `origin` yet.
///
/// Under `--no-default-features` this reads *presence* only, the same
/// fallback its `backup_advice` predecessor used: the type describing the
/// document's shape, and the git-remote detector that would confirm or
/// refute it, do not exist in that build.
fn github_remote_advice<S: Store>(ctx: &Ctx<'_, S>) -> Result<Vec<String>, AppError> {
    #[cfg(feature = "github-sync")]
    {
        let document = ctx
            .store()
            .read(|tx| Ok(tx.settings(ctx.project())?.github_sync))?;
        let Some(document) = document else {
            return Ok(Vec::new());
        };
        let Ok(config) =
            serde_json::from_value::<crate::github::sync_state::GithubSyncConfig>(document)
        else {
            // Not this advisory's failure to report: an unparseable blob is
            // exactly what `story github-sync` itself will refuse on, loudly,
            // the next time it runs — nothing silent about it either way.
            return Ok(Vec::new());
        };
        let detected = crate::github::sync_state::detect_github_remote(ctx.cwd())?;
        Ok(match detected {
            Some(detected)
                if detected.owner == config.github.owner && detected.repo == config.github.repo =>
            {
                Vec::new()
            }
            Some(detected) => vec![format!(
                "github-sync is configured for `{}/{}`, but this checkout's `origin` points at \
                 `{}/{}` — the next `story github-sync` will push there, not to this \
                 checkout's own repository. This is common right after a restore into a fork \
                 or a relocated clone; confirm which repository is intended before syncing.",
                config.github.owner, config.github.repo, detected.owner, detected.repo,
            )],
            None => vec![format!(
                "github-sync is configured for `{}/{}`, but this checkout has no GitHub \
                 `origin` to confirm that against — common right after a restore, before `git \
                 remote add origin` runs. The configured repository has not been verified for \
                 this checkout.",
                config.github.owner, config.github.repo,
            )],
        })
    }
    #[cfg(not(feature = "github-sync"))]
    {
        let configured = ctx
            .store()
            .read(|tx| Ok(tx.settings(ctx.project())?.github_sync.is_some()))?;
        if !configured {
            return Ok(Vec::new());
        }
        Ok(vec![
            "github-sync is configured, but this build (--no-default-features) cannot verify \
             its target repository against this checkout's git remote. Rebuild with the \
             `github-sync` feature to check."
                .to_string(),
        ])
    }
}

/// What `story doctor` says when the abandoned-command ledger is not empty
/// (SH-173).
///
/// Advisory rather than an integrity failure, for the same reason
/// [`github_remote_advice`] is: an abandoned command *may* have landed — a forced
/// shutdown does not roll anything back, it only stops confirming — so a
/// non-zero exit here would tell a script something is broken when the most
/// likely truth is that nothing is. `story doctor abandoned` is where the
/// detail and the recovery advice live; this is only the pointer to it, kept
/// brief so a machine that has never forced a shutdown never sees it grow.
fn abandoned_advice(env: &Environment) -> Vec<String> {
    let ledger = crate::daemon::lifecycle::read_abandoned(env);
    if ledger.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "{} command{} the daemon abandoned rather than confirmed finishing — most likely \
         from `story daemon stop --force`, or a crash. `story doctor abandoned` lists each \
         with a recovery suggestion; `story doctor abandoned clear` forgets one once you \
         have checked it.",
        ledger.len(),
        if ledger.len() == 1 { "" } else { "s" }
    )]
}

/// What `story doctor` says about a non-empty crash ledger (SH-287).
///
/// Advisory for the same reason [`abandoned_advice`] is: a crash may already
/// have become a bug report, or been withheld with a reason that is nobody's
/// emergency (no project registered, say) — `story doctor crashes` is where
/// the detail and each one's actual outcome live; this is only the pointer to
/// it, kept brief so a machine that has never crashed never sees it grow.
fn crash_advice(env: &Environment) -> Vec<String> {
    let ledger = crate::daemon::crash::read_crashes(env);
    if ledger.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "{} crash{} this daemon noticed on relaunch. `story doctor crashes` lists each one's \
         classification and what became of its bug report; `story doctor crashes clear` \
         forgets one once you have checked it.",
        ledger.len(),
        if ledger.len() == 1 { "" } else { "s" }
    )]
}

/// What `story doctor` says about a `story_commit_links` row with no backing
/// `StoryCommitLinked` event (SH-70's council, `.council/
/// sh70-import-project-git-link-source/DECISION.md`).
///
/// Every such row was projected from a `[git]`-shaped comment rather than from
/// the event itself: schema migration 2's backfill, a `story migrate` replay,
/// or an `import-project --legacy-links` restore. The first two are always
/// correct — their whole input predates kind #18 by construction. The third is
/// an operator's assertion about one document, and the store cannot verify it
/// (the ambiguity [`crate::store::LinkSource`]'s doc comment describes — "who
/// is speaking" does not survive a round trip through an export document). So
/// this reports every row in the category, regardless of source: most of the
/// time the answer is "yes, that's right, it's from `migrate` or the
/// backfill," and this advisory is silence. It only earns its keep on the rare
/// restore that got `--legacy-links` wrong.
///
/// Advisory, never `--fix`-repaired, for the same reason [`pointer_origin_
/// advice`] is: there is no default that is obviously right, because the store
/// cannot distinguish "this is a genuine pre-#18 link" from "this was a live
/// comment a `--legacy-links` restore misclassified." Whoever restored the
/// document has to say which.
///
/// Store-pure, unlike `pointer_origin_advice`:
/// [`ReadOps::unbacked_commit_links`](crate::store::ReadOps::unbacked_commit_links)
/// is a join over rows this project already owns, no `cwd` or `git` needed.
fn legacy_link_advice<S: Store>(ctx: &Ctx<'_, S>) -> Result<Vec<String>, AppError> {
    let found = ctx
        .store()
        .read(|tx| tx.unbacked_commit_links(ctx.project()))?;
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let prefix = ctx
        .store()
        .read(|tx| tx.project(ctx.project()))?
        .map(|project| project.prefix)
        .unwrap_or_default();
    let mut lines: Vec<String> = found
        .iter()
        .map(|(story_no, sha)| {
            format!(
                "`{}` links commit `{sha}` from a `[git]` comment, not from a `StoryCommitLinked` \
                 event",
                story_no.to_id(&prefix)
            )
        })
        .collect();
    lines.push(format!(
        "{} commit link{} recorded from comment text rather than from event kind #18. This is \
         expected for a project moved in with `story migrate`, or restored from a genuine \
         pre-#18 backup with `import-project --legacy-links` — nothing to do. If neither \
         happened here, one of these may be a live comment a restore misclassified; storyhook \
         cannot tell the two apart and will not guess which.",
        found.len(),
        if found.len() == 1 { "" } else { "s" },
    ));
    Ok(lines)
}

/// What `story doctor` says about a checkout whose pointer file and whose
/// registered origin name different projects (SH-116, narrowed by SH-151's
/// council, built here as SH-161).
///
/// SH-116 wanted this and it was refused: a directory two sibling projects
/// share legitimately disagrees this way, because only one of them can ever
/// own the repository's origin, so the other's pointer file "disagreeing"
/// with it was noise on the exact layout SH-151 was filed to support. SH-151
/// closed that gap by making ownership a precondition of registration, which
/// is why this asks the same [`crate::service::project::origin_at`] ownership
/// question the resolver and the registration path already do: a directory
/// that does not *own* its origin cannot be one of the two projects the
/// finding is about, so it is silent, not a false positive.
///
/// Advisory rather than an integrity failure, and never repaired by `--fix`:
/// unlike an unregistered origin, there is no default that is obviously right
/// — the pointer could be stale, or the registration could be. Whoever is
/// standing here has to say which.
///
/// Store-pure it is not: it reads `cwd`'s pointer file and asks `git`, which
/// is exactly what keeps it out of [`IntegrityService`], the project-scoped
/// store-pure half of `doctor`. Paid only when `story doctor` runs, the same
/// bargain [`StoreInvoker::resolve_project`] documents for why a pointer file
/// is left to outrank a registered origin at resolution time rather than
/// reconciling the two on every command.
fn pointer_origin_advice<S: Store>(ctx: &Ctx<'_, S>) -> Result<Vec<String>, AppError> {
    use crate::domain::remote::RepoOrigin;

    let RepoOrigin::Owned(owned) = crate::service::project::origin_at(ctx.cwd()) else {
        return Ok(Vec::new());
    };
    let Some(pointer) = crate::service::project::pointer_at_or_above(ctx.cwd()) else {
        return Ok(Vec::new());
    };
    let Some(registered) = ctx.store().read(|tx| tx.project_by_remote(owned.url()))? else {
        return Ok(Vec::new());
    };
    if registered.uuid == pointer.uuid {
        return Ok(Vec::new());
    }

    let pointer_name = ctx
        .store()
        .read(|tx| tx.project_by_uuid(&pointer.uuid))?
        .map_or(pointer.uuid, |project| project.slug);

    Ok(vec![format!(
        "`{}` owns the origin `{}`, which is registered to `{}` — but its pointer file names \
         `{pointer_name}`. This checkout claims two projects, and storyhook will not guess which \
         one is right: correct the pointer file, or move the registration with `story --project \
         <slug> project unlink origin` and `project link origin`.",
        ctx.cwd().display(),
        owned.url().raw(),
        registered.slug,
    )])
}

/// What `story doctor` says about a checkout whose pointer names a different
/// story-id prefix than the project it resolves to actually has (SH-190).
///
/// `import_project`'s restore path adopts a stale pointer's *uuid* when the
/// store lacks it, but deliberately leaves `prefix` bound to the export
/// document's own value rather than the pointer's — the document's prefix is
/// what every restored story's id is already rendered against, so overwriting
/// it with the pointer's would corrupt them (see that function's own doc
/// comment). A mismatch this leaves behind, or one from a hand-edited or
/// copy-pasted `.storyhook.toml`, is otherwise silent: nothing else ever
/// compares the two.
///
/// Advisory, like [`pointer_origin_advice`], and never repaired by `--fix` for
/// the same reason: which side is stale is not this command's to guess.
fn pointer_prefix_advice<S: Store>(ctx: &Ctx<'_, S>) -> Result<Vec<String>, AppError> {
    let Some(pointer) = crate::service::project::pointer_at_or_above(ctx.cwd()) else {
        return Ok(Vec::new());
    };
    let Some(project) = ctx.store().read(|tx| tx.project(ctx.project()))? else {
        return Ok(Vec::new());
    };
    if pointer.prefix == project.prefix || pointer.uuid != project.uuid {
        return Ok(Vec::new());
    }

    Ok(vec![format!(
        "`{}`'s pointer file names prefix `{}`, but its project `{}` actually uses `{}` — one \
         of the two is stale. storyhook never rewrites either for you: if the project's ids are \
         the ones you trust, correct the pointer file's `prefix` field to match.",
        ctx.cwd().display(),
        pointer.prefix,
        project.slug,
        project.prefix,
    )])
}

/// What `story doctor` says about a project whose checkout knows an origin the
/// store does not (SH-119, R4).
///
/// Advisory, like [`orphan_advice`], and for a sharper reason: this is the
/// state every project created before origins existed is in, so exiting
/// non-zero would make `doctor` red on a machine where nothing is wrong. What
/// it costs is a fresh clone of that repository failing to resolve, which is a
/// thing to fix at leisure rather than an emergency.
fn origin_advice(found: &[crate::service::UnregisteredOrigin]) -> Vec<String> {
    use crate::service::OriginFinding;

    if found.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = found
        .iter()
        .map(|item| match &item.finding {
            OriginFinding::Registrable(owned) => format!(
                "`{}` has no registered origin, and its checkout owns `{}` — a clone of it \
                 cannot resolve until that is recorded",
                item.slug,
                owned.url().raw()
            ),
            OriginFinding::Inherited { origin, owner } => format!(
                "`{}` has no registered origin. Its checkout `{}` reports `{}`, but that origin \
                 belongs to `{}` — a project inside a repository is identified by its committed \
                 `.storyhook.toml`, not by the repository's origin",
                item.slug,
                item.checkout.display(),
                origin.raw(),
                owner.display()
            ),
            OriginFinding::HeldBy { origin, holder } => format!(
                "`{}` has no registered origin, and `{}` — the one its checkout owns — is \
                 already registered to `{holder}`",
                item.slug,
                origin.raw()
            ),
            OriginFinding::Unknown(command) => format!(
                "`{}` has no registered origin, and `{command}` failed in `{}`, so storyhook \
                 cannot tell whether that checkout owns one",
                item.slug,
                item.checkout.display()
            ),
        })
        .collect();
    // Distinct origins, not findings: two checkouts of one repository both
    // classify `Registrable` for the *same* URL, but only the first `--fix`
    // write can ever land — `register_origin` refuses the second as `HeldBy`
    // (SH-274). Counting findings here would promise a write that cannot
    // happen.
    let registrable = found
        .iter()
        .filter_map(|item| match &item.finding {
            OriginFinding::Registrable(owned) => Some(owned.url().key().to_string()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    lines.push(format!(
        "{} {} without a registered origin, {registrable} of which `story doctor --fix` can \
         record. The rest need a decision: `story --project <slug> project link origin <url>` \
         names one explicitly.",
        found.len(),
        if found.len() == 1 {
            "project"
        } else {
            "projects"
        },
    ));
    lines
}

/// What `story doctor --fix` reports having registered.
///
/// Reads `sweep.recorded`, never `sweep.left_alone`'s size against what was
/// merely looked at — the whole point of [`OriginSweep`](crate::service::OriginSweep)
/// existing is that "recorded" and "classified `Registrable`" can differ
/// (SH-274): two checkouts of one repository both look registrable before
/// either write runs, but only the first write can land.
fn registered_origins_message(sweep: &crate::service::OriginSweep) -> String {
    let recorded = &sweep.recorded;
    let mut out = if recorded.is_empty() {
        "registered no origins:".to_string()
    } else {
        format!(
            "registered {} {}:",
            recorded.len(),
            if recorded.len() == 1 {
                "origin"
            } else {
                "origins"
            }
        )
    };
    for item in recorded {
        out.push_str(&format!("\n  {} -> {}", item.slug, item.origin.raw()));
    }
    let left = sweep.left_alone.len();
    if left > 0 {
        out.push_str(&format!(
            "\n\n{left} {} left alone, because storyhook will not guess at an origin a checkout \
             does not own, nor move one another project already holds. `story doctor` names \
             each of them.",
            if left == 1 { "project" } else { "projects" }
        ));
    }
    out
}

#[cfg(test)]
mod registered_origins_message_tests {
    //! `registered_origins_message` used to be handed the pre-write
    //! classification and count `Registrable` findings as recorded (SH-274).
    //! These pin it to the sweep's actual outcome instead — including the
    //! `HeldBy`/"registered no origins" branches nothing else in the suite
    //! reaches, because every CLI-level fixture keeps its collision to one
    //! finding per origin.

    use super::*;
    use crate::domain::remote::RemoteUrl;
    use crate::service::{OriginFinding, OriginSweep, RecordedOrigin, UnregisteredOrigin};
    use crate::store::ProjectId;

    fn url(raw: &str) -> RemoteUrl {
        RemoteUrl::normalize(raw).expect("a valid remote url")
    }

    /// An entry a sweep looked at and refused to write, because another
    /// project already held the origin — the shape `register_found_origins`
    /// reclassifies a `Registrable` finding into once its write loses.
    fn held_by(slug: &str, origin_raw: &str, holder: &str) -> UnregisteredOrigin {
        UnregisteredOrigin {
            project: ProjectId::new(1),
            slug: slug.to_string(),
            checkout: PathBuf::from("/checkouts/whatever"),
            finding: OriginFinding::HeldBy {
                origin: url(origin_raw),
                holder: holder.to_string(),
            },
        }
    }

    /// **The sentence a failed sweep is entitled to** — pinned by what it must
    /// *not* say as much as by what it must (SH-275).
    ///
    /// Every wording this replaced claimed absence. All of them are false on the
    /// path where the write committed and the acknowledgement was lost, which
    /// `tests/service_catalog.rs::a_failure_after_a_commit_never_leaves_an_
    /// observable_half_sweep` produces on purpose — so shipping one would be a
    /// message a test in this repo proves wrong.
    #[test]
    fn a_failed_sweep_claims_atomicity_and_a_safe_retry_but_never_absence() {
        let message = catalog_sweep_failure(&AppError::LockTimeout("database is locked".into()));

        assert!(
            message.contains("database is locked"),
            "the underlying failure must travel with the sentence: {message}"
        );
        assert!(
            message.contains("in full or not at all"),
            "the claim it *is* entitled to make is atomicity: {message}"
        );
        assert!(
            message.contains("re-run `story doctor --fix`"),
            "and the action that settles it: {message}"
        );
        for absolute in [
            "no origins were registered",
            "nothing was registered",
            "did not happen",
        ] {
            assert!(
                !message.contains(absolute),
                "`{absolute}` claims absence, which a lost acknowledgement makes false: {message}"
            );
        }
    }

    #[test]
    fn an_empty_sweep_reports_registered_no_origins() {
        let message = registered_origins_message(&OriginSweep::default());
        assert_eq!(message, "registered no origins:");
    }

    #[test]
    fn a_refused_write_is_counted_left_alone_and_never_as_recorded() {
        let sweep = OriginSweep {
            recorded: Vec::new(),
            left_alone: vec![held_by(
                "collision",
                "https://github.com/acme/one.git",
                "owner",
            )],
        };
        let message = registered_origins_message(&sweep);
        assert!(
            message.starts_with("registered no origins:"),
            "nothing was written: {message}"
        );
        assert!(
            message.contains("1 project left alone"),
            "the refused write must still be counted, not silently dropped: {message}"
        );
        assert!(
            !message.contains("collision"),
            "a project this sweep never wrote to is not named here — `story doctor` names it, \
             with its actual reason: {message}"
        );
    }

    #[test]
    fn a_mixed_sweep_names_only_what_landed_and_counts_the_rest() {
        let sweep = OriginSweep {
            recorded: vec![RecordedOrigin {
                slug: "landed".to_string(),
                origin: url("https://github.com/acme/two.git"),
            }],
            left_alone: vec![held_by(
                "collision",
                "https://github.com/acme/two.git",
                "landed",
            )],
        };
        let message = registered_origins_message(&sweep);
        assert!(message.contains("registered 1 origin:"), "{message}");
        assert!(
            message.contains("landed -> https://github.com/acme/two.git"),
            "{message}"
        );
        assert!(message.contains("1 project left alone"), "{message}");
        assert!(
            !message.contains("collision"),
            "the loser of the collision is not named by this message: {message}"
        );
    }
}

#[cfg(test)]
mod creates_a_project_tests {
    use super::*;
    use crate::cli::NewProjectSpec;

    fn stated(attach: Attach) -> Invocation {
        Invocation::Project {
            action: ProjectAction::New(NewProjectRequest::Stated(NewProjectSpec {
                attach,
                prefix: "SH".to_string(),
                name: None,
                no_agents_md: false,
            })),
        }
    }

    #[test]
    fn every_new_project_variant_counts_including_no_checkout() {
        for invocation in [
            stated(Attach::Cwd),
            stated(Attach::Path("/some/path".to_string())),
            stated(Attach::Nothing),
            Invocation::Project {
                action: ProjectAction::New(NewProjectRequest::Ask),
            },
        ] {
            assert!(
                creates_a_project(&invocation),
                "{invocation:?} must be gated by the burst check"
            );
        }
    }

    /// Bulk verbs a person or a script chose deliberately by typing that
    /// command — including every other `ProjectAction` — stay exempt from the
    /// SH-122 burst gate. They remain under the path-based SH-95 guard, which
    /// `project_creation_target` still routes them through.
    #[test]
    fn bulk_verbs_and_every_other_project_action_are_exempt() {
        for invocation in [
            Invocation::ImportProject {
                file: "export.json".to_string(),
                legacy_links: false,
            },
            Invocation::Migrate {
                path: None,
                dry_run: false,
            },
            Invocation::Migrate {
                path: None,
                dry_run: true,
            },
            Invocation::Project {
                action: ProjectAction::List,
            },
            Invocation::Project {
                action: ProjectAction::Show,
            },
        ] {
            assert!(
                !creates_a_project(&invocation),
                "{invocation:?} must not be gated by the burst check"
            );
        }
    }

    /// **Only `github-sync` spends a credential**, and the check that says so is
    /// exhaustive over `Invocation` rather than defaulted.
    ///
    /// The positive half is the point of SH-153. The negative half is worth a
    /// test of its own: `story list` is the overwhelming majority of traffic,
    /// and an envelope that carries a secret it has no use for is a secret in a
    /// place nobody thought about.
    #[test]
    fn only_github_sync_carries_a_credential() {
        assert!(needs_github_token(&Invocation::GithubSync {
            id: None,
            dry_run: false,
            resolve: None,
            strategy: None,
            mode: None,
        }));
        for invocation in [
            Invocation::Summary,
            Invocation::Version,
            Invocation::Export,
            Invocation::SessionStart,
            Invocation::Show {
                id: "SH-1".to_string(),
            },
            Invocation::CommitSync { since: None },
            Invocation::Update {
                check: true,
                force: false,
            },
        ] {
            assert!(
                !needs_github_token(&invocation),
                "{invocation:?} has no GitHub credential to spend"
            );
        }
    }
}

#[cfg(test)]
mod project_creation_target_tests {
    use super::*;
    use crate::cli::{MemberInput, NewProjectSpec};

    /// The three named creating routes still resolve to a path — pinned
    /// separately from the exhaustive match below, because this half is
    /// about *what* they resolve to, not about the wildcard SH-170 removed.
    #[test]
    fn every_creating_route_returns_a_target() {
        assert_eq!(
            project_creation_target(
                &Invocation::ImportProject {
                    file: "export.json".to_string(),
                    legacy_links: false,
                },
                Path::new("/cwd"),
            ),
            Some(PathBuf::from("/cwd"))
        );
        assert_eq!(
            project_creation_target(
                &Invocation::Project {
                    action: ProjectAction::New(NewProjectRequest::Ask),
                },
                Path::new("/cwd"),
            ),
            Some(PathBuf::from("/cwd"))
        );
        assert_eq!(
            project_creation_target(
                &Invocation::Project {
                    action: ProjectAction::New(NewProjectRequest::Stated(NewProjectSpec {
                        attach: Attach::Path("elsewhere".to_string()),
                        prefix: "SH".to_string(),
                        name: None,
                        no_agents_md: false,
                    })),
                },
                Path::new("/cwd"),
            ),
            Some(PathBuf::from("/cwd/elsewhere"))
        );
        assert_eq!(
            project_creation_target(
                &Invocation::Migrate {
                    path: None,
                    dry_run: false,
                },
                Path::new("/cwd"),
            ),
            Some(PathBuf::from("/cwd"))
        );
    }

    /// `Attach::Nothing` and a dry-run `Migrate` are the two narrowings
    /// within the creating arms themselves — still `None` despite the arm
    /// they live in, and worth pinning apart from the wildcard removal.
    #[test]
    fn attach_nothing_and_dry_run_migrate_create_no_target() {
        assert_eq!(
            project_creation_target(
                &Invocation::Project {
                    action: ProjectAction::New(NewProjectRequest::Stated(NewProjectSpec {
                        attach: Attach::Nothing,
                        prefix: "SH".to_string(),
                        name: None,
                        no_agents_md: false,
                    })),
                },
                Path::new("/cwd"),
            ),
            None
        );
        assert_eq!(
            project_creation_target(
                &Invocation::Migrate {
                    path: None,
                    dry_run: true,
                },
                Path::new("/cwd"),
            ),
            None
        );
    }

    /// SH-170: the outer match used to fall through a `_ => None` catch-all
    /// for every non-creating `Invocation`. That catch-all is gone — this
    /// samples across unit variants, single-field variants and
    /// nested-`*Action` variants (the shapes most likely to hide a mistake)
    /// to pin that removing it changed nothing about today's behaviour.
    #[test]
    fn a_representative_sample_of_non_creating_invocations_return_none() {
        for invocation in [
            Invocation::Help,
            Invocation::Summary,
            Invocation::Export,
            Invocation::HelpCompact,
            Invocation::HelpAll,
            Invocation::SessionStart,
            Invocation::Version,
            Invocation::ProjectSnapshot,
            Invocation::Show {
                id: "SH-1".to_string(),
            },
            Invocation::Publish {
                id: "SH-1".to_string(),
            },
            Invocation::MemberAdd {
                input: MemberInput::Identity("alice".to_string()),
            },
            Invocation::State {
                action: StateAction::List,
            },
            Invocation::Project {
                action: ProjectAction::List,
            },
            Invocation::Project {
                action: ProjectAction::Show,
            },
            Invocation::GithubSync {
                id: None,
                dry_run: false,
                resolve: None,
                strategy: None,
                mode: None,
            },
            Invocation::Daemon {
                action: DaemonAction::Status,
            },
            Invocation::History {
                action: HistoryAction::Read {
                    id: "SH-1".to_string(),
                },
            },
        ] {
            assert_eq!(
                project_creation_target(&invocation, Path::new("/cwd")),
                None,
                "{invocation:?} must not name a creation target"
            );
        }
    }
}
