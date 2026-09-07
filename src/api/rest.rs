//! The dashboard's resource API, served from the store through the service
//! layer.
//!
//! # What changed at the daemon wave, and why it matters
//!
//! This surface used to reach `.storyhook/` directories through
//! `app::run`, and it had a hack at its centre: a `PATCH` answered with a plain
//! success message rather than the updated story, so the route ran the
//! invocation and then ran a *second* invocation to fetch the story back —
//! taking and releasing the project's file lock twice to answer one request. The
//! comment on it explained, correctly, that nesting the second call inside the
//! first would deadlock.
//!
//! There is no file lock any more, and no reason to dispatch twice. A route
//! makes one service call and then reads the view it wants, the same way every
//! `dispatch` arm does. That is the whole of the "double dispatch" removal: same
//! response bytes, half the work, and no lock to be careful about.
//!
//! # One rule set, not two
//!
//! Where the CLI's answer *is* the answer the dashboard wants — which is almost
//! everywhere — the route builds an [`Invocation`] and hands it to
//! [`crate::invoke::dispatch`]. That is not indirection for its own sake: it is
//! what makes `POST /api/repos/x/story` and `story new` provably the same
//! operation, validated by the same code, firing the same hooks, rather than two
//! implementations that agree until one of them is edited.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::daemon::http1::{Header, Method};
use crate::daemon::verification::VerificationActivity;

use crate::api::http::{
    Reply, TrustedHosts, error_reply, get_bool, get_str, get_str_array, guarded, guarded_no_body,
    html_reply, json_reply, parse_json_object, path_segments, require_str, text_reply, to_json,
};
use crate::api::routes::{ProjectRoute, Route, StoryAction, classify};
use crate::cli::{Invocation, ProjectAction, StateAction};
use crate::domain::provenance::{ActorLabel, Provenance};
use crate::domain::{Priority, default_open_state, default_type};
use crate::env::Environment;
use crate::error::AppError;
use crate::invoke::dispatch;
use crate::output::{ReportData, Response, render_response};
use crate::service::{CatalogService, ConfigService, Ctx, FieldEdits, QueryService, StoryService};
use crate::store::{ProjectId, ReadOps, Store, WriteOps};

const DASHBOARD_HTML: &str = include_str!("../web_dashboard.html");
const DASHBOARD_VERSION_PLACEHOLDER: &str = "__STORYHOOK_VERSION__";

/// The transport-independent parts of one request routed through the REST API.
///
/// Keeping these values together makes the daemon and direct tests pass the
/// same coherent request shape while socket ownership remains outside this
/// module.
pub struct RouteRequest<'a> {
    method: &'a Method,
    path: &'a str,
    headers: &'a [Header],
    body: &'a str,
}

impl<'a> RouteRequest<'a> {
    /// Creates a request from an already parsed HTTP method, path, headers,
    /// and body.
    pub fn new(method: &'a Method, path: &'a str, headers: &'a [Header], body: &'a str) -> Self {
        Self {
            method,
            path,
            headers,
            body,
        }
    }
}

/// Builds the self-contained dashboard shell with this binary's package
/// version. The embedded HTML owns the presentation while the serving binary
/// remains the only version authority.
fn dashboard_html() -> String {
    debug_assert_eq!(
        DASHBOARD_HTML
            .matches(DASHBOARD_VERSION_PLACEHOLDER)
            .count(),
        1,
        "the dashboard must contain exactly one version placeholder"
    );
    DASHBOARD_HTML.replacen(DASHBOARD_VERSION_PLACEHOLDER, env!("CARGO_PKG_VERSION"), 1)
}

/// All priority levels, in the order the frontend should offer them.
const PRIORITIES: [Priority; 4] = [
    Priority::Critical,
    Priority::High,
    Priority::Medium,
    Priority::Low,
];

/// All relationship kinds a story can be linked with, in canonical form (not
/// including the `related-to` alias of `relates-to`). See
/// `domain::relation_edges` for the authoritative parser.
const RELATIONS: [&str; 8] = [
    "relates-to",
    "blocks",
    "blocked-by",
    "parent-of",
    "child-of",
    "duplicate-of",
    "obviates",
    "obviated-by",
];

/// What a just-answered request changed, so the accept loop can tell every
/// connected browser about it *after* the write has committed.
///
/// Only successful (2xx) writes count: a rejected edit changed nothing, and
/// telling every client to refetch for it would be noise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Changed {
    /// This project's stories or configuration moved.
    Project(String),
    /// The set of projects moved.
    Catalog,
}

/// A routed request's answer, plus what it changed.
pub struct Routed {
    /// The HTTP response.
    pub reply: Reply,
    /// What to publish on the change feed, if anything.
    pub changed: Option<Changed>,
}

impl Routed {
    fn quiet(reply: Reply) -> Self {
        Routed {
            reply,
            changed: None,
        }
    }

    /// Marks a reply as having changed `what` — if it succeeded, and if the
    /// request was one that could change anything at all.
    ///
    /// Both halves are load-bearing. A rejected edit changed nothing, and a
    /// `GET` changed nothing *by definition*: publishing for a read tells every
    /// connected browser to refetch because one of them just fetched, which is
    /// a feedback loop with a 250ms period.
    fn changing(method: &Method, reply: Reply, what: Changed) -> Self {
        let changed = (mutating(method) && (200..300).contains(&reply.status)).then_some(what);
        Routed { reply, changed }
    }
}

/// Whether a method can change anything.
///
/// `pub(crate)`: [`crate::api::admission`] needs the identical rule to
/// decide whether a request must pass [`crate::api::http::mutation_guard_ok`]
/// before its token — one definition, so the two can never disagree about
/// which methods count as mutating.
pub(crate) fn mutating(method: &Method) -> bool {
    matches!(method, Method::Post | Method::Patch | Method::Delete)
}

/// The provenance a per-project write is recorded with (SH-312).
///
/// Before this, [`Ctx::new`] at this door carried no provenance at all, so
/// every dashboard write folded to [`Provenance::unrecorded`] — the same
/// answer a pre-SH-246 row or a test fixture gives, and indistinguishable
/// from either without reading the store's bytes directly. That is what made
/// diagnosing SH-310/SH-311 (two identical stories, 24 seconds apart, filed
/// from the dashboard) take a forensic pass over the raw database instead of
/// a `story log`.
///
/// Labelled `web:<verb> (web:user)` rather than reusing
/// [`crate::invoke::invocation_name`] bare: several of these routes
/// (`route_patch_story` most notably) answer
/// without ever building an [`Invocation`] at all, so there is no single verb
/// source every arm shares — and prefixing makes a REST-door write visually
/// distinct from a CLI one at a glance, which is the more useful fact for a
/// reader asking "did this come from the terminal or the browser?" The actor
/// names the person using this interactive surface, so an agent does not
/// mistake the manual change for an unlabelled agent action (SH-527). A read
/// route gets both labels too, even though nothing it does ever appends an
/// event that would render them — assigning them here costs nothing and keeps
/// this function total over [`ProjectRoute`] rather than reaching for a `_`
/// arm a route added later could silently fall into.
fn route_provenance(route: &ProjectRoute<'_>) -> Provenance {
    let verb = match route {
        ProjectRoute::Data => "data",
        ProjectRoute::VerificationAck => "verification-ack",
        ProjectRoute::StoryCreate => "new",
        ProjectRoute::StoryShow { .. } => "show",
        ProjectRoute::StoryPatch { .. } => "set-fields",
        ProjectRoute::StoryDelete { .. } => "delete",
        ProjectRoute::StoryAction { action, .. } => match action {
            StoryAction::Move => "move",
            StoryAction::Comment => "comment",
            StoryAction::Priority => "set-priority",
            StoryAction::Assign => "assign",
            StoryAction::Labels => "set-labels",
            StoryAction::Block => "set-awaiting",
            StoryAction::Unblock => "clear-awaiting",
            StoryAction::Reopen => "reopen",
            StoryAction::Archive => "archive",
            StoryAction::Unarchive => "unarchive",
            StoryAction::Publish => "publish",
            StoryAction::LinkPr => "link-pr",
            StoryAction::UnlinkPr => "unlink-pr",
        },
        ProjectRoute::StoryActionUnknown => "unknown-action",
        ProjectRoute::Dispatch { .. } => "dispatch",
        ProjectRoute::DispatchPoll => "dispatch-poll",
        ProjectRoute::Engine => "engine",
        ProjectRoute::EngineAction { .. } => "engine-action",
        ProjectRoute::EngineActionUnknown => "unknown-engine-action",
        ProjectRoute::States => "states",
        ProjectRoute::StateCreate => "state-create",
        ProjectRoute::StatesReorder => "states-reorder",
        ProjectRoute::StatePatch { .. } => "state-patch",
        ProjectRoute::StateDelete { .. } => "state-delete",
        ProjectRoute::StateArchive { .. } => "state-archive",
        ProjectRoute::Relate => "relate",
        ProjectRoute::Unrelate => "unrelate",
        ProjectRoute::MethodNotAllowed => "method-not-allowed",
        ProjectRoute::NotFound => "not-found",
    };
    Provenance::command(format!("web:{verb}")).with_actor(Some(
        ActorLabel::try_from("web:user".to_string()).expect("web:user is a valid actor label"),
    ))
}

/// Decides how to respond to a request against `store`.
///
/// The path is turned into a [`Route`] by [`classify`] and answered by the
/// `match` below, which has **no wildcard arm**: a route added to
/// [`crate::api::routes`] does not compile until it is answered here.
/// Authentication is decided earlier and uniformly, in
/// [`crate::api::admission::admission`] — this module never reasons about
/// credentials at all (SH-255; before it, a route added here also had to be
/// classified in a since-deleted `crate::api::routes::authority`, SH-254).
///
/// **Classification does not resolve the project.** `{id}` is split off here
/// and looked up below, which is what keeps `PUT /api/repos/<unknown>/data` a
/// 404 (no such project) rather than the 405 its shape suggests — an ordering
/// `tests/rest_routing.rs` pins.
pub fn route<S: Store>(
    store: &S,
    env: &Environment,
    method: &Method,
    path: &str,
    headers: &[Header],
    body: &str,
    trusted_hosts: &TrustedHosts,
) -> Routed {
    route_with_activity(
        store,
        env,
        &VerificationActivity::new(),
        RouteRequest::new(method, path, headers, body),
        trusted_hosts,
    )
}

/// [`route`] with the daemon's live verifier-ownership registry. Kept as a
/// separate entry point so direct router tests remain process-independent
/// while the serving daemon can expose authoritative active status (SH-549).
pub fn route_with_activity<S: Store>(
    store: &S,
    env: &Environment,
    verification_activity: &VerificationActivity,
    request: RouteRequest<'_>,
    trusted_hosts: &TrustedHosts,
) -> Routed {
    let RouteRequest {
        method,
        path,
        headers,
        body,
    } = request;
    match classify(&path_segments(path), method) {
        Route::Shell => Routed::quiet(html_reply(dashboard_html()).no_cache()),
        Route::Repos => Routed::quiet(match repos_json(store, env) {
            Ok(json) => json_reply(200, json).no_cache(),
            Err(e) => error_reply(&e),
        }),
        Route::ReposCreate => Routed::changing(
            method,
            guarded(headers, trusted_hosts, body, |b| {
                route_init_repo(store, env, b)
            }),
            Changed::Catalog,
        ),
        Route::RepoDelete { id } => Routed::changing(
            method,
            guarded(headers, trusted_hosts, body, |b| {
                route_delete_repo(store, env, id, b)
            }),
            Changed::Catalog,
        ),
        Route::Project { id, route } => match resolve_repo(store, id) {
            // One door for the whole per-project surface. A project with no
            // checkout on this machine is served read-only: reads need no
            // working directory, and every write does — that is where the
            // project's event hooks and its git repository are. Refusing here
            // rather than in each handler means a route added later cannot
            // miss it.
            Ok(Some(Repo { project, checkout })) => {
                if mutating(method) && checkout.is_none() {
                    return Routed::quiet(error_reply(&pathless_refusal(id)));
                }
                let hookless = checkout.is_none();
                let root = checkout.unwrap_or_else(|| no_checkout_placeholder(id));
                let ctx = Ctx::new(store, project, root, env.clone())
                    .no_hooks(hookless)
                    .with_provenance(route_provenance(&route));
                let reply = route_project(
                    &ctx,
                    verification_activity,
                    route,
                    headers,
                    body,
                    trusted_hosts,
                );
                Routed::changing(method, reply, Changed::Project(id.to_string()))
            }
            Ok(None) => Routed::quiet(text_reply(404, "Not found")),
            Err(e) => Routed::quiet(error_reply(&e)),
        },
        // Both are answered in full by [`crate::daemon::serve`]'s worker,
        // which never lets them reach the router: the change feed is a
        // long-lived stream that must not occupy the dispatcher pool, and the
        // control surface is [`crate::api::rpc`]'s. Reaching here means
        // something upstream stopped answering, and the reply is what an
        // unrouted path has always got.
        Route::Events | Route::ControlSurface | Route::DispatchLog | Route::DispatchOptions => {
            Routed::quiet(text_reply(404, "Not found"))
        }
        Route::MethodNotAllowed => Routed::quiet(text_reply(405, "Method not allowed")),
        Route::NotFound => Routed::quiet(text_reply(404, "Not found")),
    }
}

/// The per-project API surface — everything under `/api/repos/<id>/...` once
/// `<id>` has resolved to a project.
///
/// The method is already decided: it was consumed by [`classify`], which is why
/// this function no longer takes one. Every 405 and every 404 that used to be
/// spelled at the bottom of a nested `match` is now a variant, so this `match`
/// is exhaustive with no wildcard and a new per-project route cannot be added
/// without an answer here.
fn route_project<S: Store>(
    ctx: &Ctx<'_, S>,
    verification_activity: &VerificationActivity,
    route: ProjectRoute<'_>,
    headers: &[Header],
    body: &str,
    trusted_hosts: &TrustedHosts,
) -> Reply {
    match route {
        ProjectRoute::Data => match project_data_json(ctx, verification_activity) {
            Ok(json) => json_reply(200, json).no_cache(),
            Err(e) => error_reply(&e),
        },
        ProjectRoute::VerificationAck => guarded(headers, trusted_hosts, body, |b| {
            route_ack_verification(ctx, b)
        }),
        // Answered by [`crate::api::engine::intercept`] in the per-connection
        // worker so a stop-now helper can call back into the daemon without
        // occupying this fixed store pool.
        ProjectRoute::Engine
        | ProjectRoute::EngineAction { .. }
        | ProjectRoute::EngineActionUnknown => text_reply(404, "Not found"),
        ProjectRoute::StoryCreate => {
            guarded(headers, trusted_hosts, body, |b| route_create_story(ctx, b))
        }
        ProjectRoute::StoryShow { id } => {
            reply_with(ctx, 200, Invocation::Show { id: id.to_string() })
        }
        ProjectRoute::StoryPatch { id } => guarded(headers, trusted_hosts, body, |b| {
            route_patch_story(ctx, id, b)
        }),
        ProjectRoute::StoryDelete { id } => guarded(headers, trusted_hosts, body, |b| {
            route_delete_story(ctx, id, b)
        }),
        ProjectRoute::StoryAction { id, action } => match action {
            StoryAction::Move => guarded(headers, trusted_hosts, body, |b| {
                route_move_story(ctx, id, b)
            }),
            StoryAction::Comment => guarded(headers, trusted_hosts, body, |b| {
                route_comment_story(ctx, id, b)
            }),
            StoryAction::Priority => guarded(headers, trusted_hosts, body, |b| {
                route_priority_story(ctx, id, b)
            }),
            StoryAction::Assign => guarded(headers, trusted_hosts, body, |b| {
                route_assign_story(ctx, id, b)
            }),
            StoryAction::Labels => guarded(headers, trusted_hosts, body, |b| {
                route_labels_story(ctx, id, b)
            }),
            StoryAction::Block => guarded(headers, trusted_hosts, body, |b| {
                route_block_story(ctx, id, b)
            }),
            StoryAction::Unblock => {
                guarded_no_body(headers, trusted_hosts, || route_unblock_story(ctx, id))
            }
            StoryAction::Reopen => guarded(headers, trusted_hosts, body, |b| {
                route_reopen_story(ctx, id, b)
            }),
            StoryAction::Archive => {
                guarded_no_body(headers, trusted_hosts, || route_hide_story(ctx, id))
            }
            StoryAction::Unarchive => {
                guarded_no_body(headers, trusted_hosts, || route_unhide_story(ctx, id))
            }
            StoryAction::Publish => {
                guarded_no_body(headers, trusted_hosts, || route_publish_story(ctx, id))
            }
            StoryAction::LinkPr => guarded(headers, trusted_hosts, body, |b| {
                route_link_pr_story(ctx, id, b)
            }),
            StoryAction::UnlinkPr => guarded(headers, trusted_hosts, body, |b| {
                route_unlink_pr_story(ctx, id, b)
            }),
        },
        // Reordering is a PATCH of the *collection*, not a
        // `/states/reorder` sub-path: `reorder` is a legal state slug, and
        // that route would shadow the state with that name.
        ProjectRoute::States => match states_json(ctx, None) {
            Ok(json) => json_reply(200, json).no_cache(),
            Err(e) => error_reply(&e),
        },
        ProjectRoute::StateCreate => {
            guarded(headers, trusted_hosts, body, |b| route_create_state(ctx, b))
        }
        ProjectRoute::StatesReorder => guarded(headers, trusted_hosts, body, |b| {
            route_reorder_states(ctx, b)
        }),
        ProjectRoute::StatePatch { slug } => guarded(headers, trusted_hosts, body, |b| {
            route_patch_state(ctx, slug, b)
        }),
        ProjectRoute::StateDelete { slug } => guarded(headers, trusted_hosts, body, |b| {
            route_delete_state(ctx, slug, b)
        }),
        ProjectRoute::StateArchive { slug } => guarded(headers, trusted_hosts, body, |b| {
            route_hide_state(ctx, slug, b)
        }),
        ProjectRoute::Relate => guarded(headers, trusted_hosts, body, |b| {
            route_relate(ctx, b, false)
        }),
        ProjectRoute::Unrelate => {
            guarded(headers, trusted_hosts, body, |b| route_relate(ctx, b, true))
        }
        // Answered by [`crate::api::dispatch::intercept`] in the accept loop,
        // so neither reaches the router in production. They are variants
        // rather than a fall-through because a route that spawns an agent
        // process has to be *nameable* by the authority table that refuses it.
        ProjectRoute::Dispatch { .. } | ProjectRoute::DispatchPoll => text_reply(404, "Not found"),
        ProjectRoute::StoryActionUnknown => text_reply(404, "Not found"),
        ProjectRoute::MethodNotAllowed => text_reply(405, "Method not allowed"),
        ProjectRoute::NotFound => text_reply(404, "Not found"),
    }
}

/// A project the dashboard was asked for, and the checkout it may act in.
///
/// `checkout: None` is a project the store knows and this machine has no copy
/// of — its directory was deleted, or it arrived by import. Separating that
/// from "no such project" is the whole reason this type exists: they are
/// different answers, and only one of them is a 404.
struct Repo {
    project: ProjectId,
    checkout: Option<PathBuf>,
}

/// Why a write against a checkout-less project is refused, and what to do.
fn pathless_refusal(slug: &str) -> AppError {
    AppError::Validation(format!(
        "`{slug}` has {NO_CHECKOUT}, so there is nowhere to run this change: a write fires the \
         project's event hooks and its git operations in its working directory. Its stories stay \
         readable here.\n\nRun `story project new --prefix <PREFIX>` in a checkout of it to \
         adopt one, or `story --project {slug} project link checkout <path>` if the checkout \
         simply moved."
    ))
}

/// A working directory for a project that has none.
///
/// [`Ctx`] requires one, and every caller that would *use* it is refused at the
/// door above — so this exists to satisfy the type rather than to be read. It
/// is deliberately a path that cannot exist, so that a read path which starts
/// consulting the working directory fails loudly here instead of quietly
/// finding whatever the daemon's own directory happens to hold.
fn no_checkout_placeholder(slug: &str) -> PathBuf {
    PathBuf::from(format!("/nonexistent/storyhook/{slug}/{NO_CHECKOUT}"))
}

/// Resolves a catalog id (the project's slug) to the project it names.
///
/// `None` means no such project. A project that exists but has no recorded
/// checkout resolves to a [`Repo`] with `checkout: None`; what the caller may
/// then do with it is the caller's decision, taken at one place in
/// [`route`] rather than scattered through twenty handlers.
fn resolve_repo<S: Store>(store: &S, slug: &str) -> Result<Option<Repo>, AppError> {
    Ok(store.read(|tx| {
        let Some(project) = tx.project_by_slug(slug)? else {
            return Ok(None);
        };
        let checkout = tx.checkout_path(project.id)?;
        Ok(Some(Repo {
            project: project.id,
            checkout,
        }))
    })?)
}

/// Dispatches `invocation` and renders its answer as a `Reply` at `status`, or
/// the standard error envelope on failure.
///
/// One dispatch per request. Every rule the CLI enforces — validation, hook
/// firing, the closed-state archive — applies here because it is the same call,
/// not because it was reimplemented.
fn reply_with<S: Store>(ctx: &Ctx<'_, S>, status: u16, invocation: Invocation) -> Reply {
    match dispatch(ctx, invocation) {
        Ok(response) => json_reply(status, render_response(&response, true, false)),
        Err(e) => error_reply(&e),
    }
}

// --- The project catalog: /api/repos ---

/// What a repo with no checkout on this machine is told, and tells.
///
/// Re-exported from [`crate::output`] rather than spelled again: the CLI's
/// `project show` and `project list` say the same words, and a user who learns
/// the phrase once should not meet a second spelling of it in the dashboard.
use crate::output::NO_CHECKOUT;

/// `GET /api/repos` — one entry per project the store knows, driving the
/// header's project selector, the home screen's summary cards, and the
/// settings screen's project list, and the dashboard-wide Drafts surface.
///
/// **Every** project, not only those with a checkout. A project whose directory
/// was deleted and then forgotten by `story doctor --fix`, or one that arrived
/// by import, still has all of its stories in the database the daemon has open;
/// hiding it made that work unreachable from every surface storyhook has.
///
/// Three fields describe how usable each one is, because two states were being
/// squeezed into one flag:
///
/// * `available` — its data can be read *and* acted on. False for a project
///   with nowhere to act and false for one that genuinely cannot be read.
/// * `read_only` — it can be read but not written. This is the pathless case,
///   and it is what tells the front-end to keep the card clickable.
/// * `reason` — why, in words, for whichever of those is not the happy one.
///
/// A project that cannot be read at all is reported rather than failing the
/// request: one broken project must never take down the view of every other.
fn repos_json<S: Store>(store: &S, env: &Environment) -> Result<String, AppError> {
    let entries = CatalogService::new(store).all()?;
    let now = env.now();
    let repos: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|entry| {
            let summary = store
                .read(|tx| Ok(QueryService::new(tx, entry.project, &now).report_data()))
                .map_err(AppError::from)
                .and_then(|inner| inner);
            let read_only = entry.path.is_none();
            match summary {
                Ok(data) => {
                    let drafts = dashboard_drafts_json(&data);
                    serde_json::json!({
                        "id": entry.id,
                        "name": entry.name,
                        "prefix": entry.prefix,
                        "path": entry.path,
                        "available": !read_only,
                        "read_only": read_only,
                        "reason": read_only.then_some(NO_CHECKOUT),
                        "summary": data.summary,
                        "drafts": drafts,
                    })
                }
                Err(e) => serde_json::json!({
                    "id": entry.id,
                    "name": entry.name,
                    "prefix": entry.prefix,
                    "path": entry.path,
                    "available": false,
                    "read_only": false,
                    "reason": e.to_string(),
                    "error": e.to_string(),
                }),
            }
        })
        .collect();

    to_json(&repos)
}

/// The dashboard's one definition of a visible draft: unpublished. Both the
/// global catalog and a project's board data call this
/// helper, so a draft cannot appear in one Drafts surface but disappear from
/// the other because two filters drifted apart (SH-442).
fn dashboard_drafts_json(data: &ReportData) -> Vec<serde_json::Value> {
    data.stories
        .iter()
        .filter(|view| view.story.draft)
        .map(|view| serde_json::to_value(view).unwrap_or(serde_json::Value::Null))
        .collect()
}

/// `POST /api/repos` — create a project at a path on this machine.
/// Body: `{"path": "...", "prefix": "...", "name"?: "..."}`.
///
/// The same operation as `story project new --attach <path>`, reached through
/// the same **invoker**, so the browser cannot create a project the CLI would
/// have created differently. There is no longer a "register" that is distinct
/// from this: a project reaches the dashboard by existing.
///
/// # Why `prefix` is required here
///
/// The browser is the one caller that can never be asked. The CLI has a
/// questionnaire for a bare `story project new`; a form has no equivalent, so a
/// server-side default would be SH-109's silent `SH` wearing a form, in the one
/// place nobody would ever notice it happening. Absent is a 400 naming the
/// field, raised by the same `AppError::Usage` the CLI raises.
///
/// The form derives a suggestion from the last path segment client-side, and
/// this route validates whatever arrives through `domain::prefix::validate` —
/// which is what makes an untestable six-line JavaScript derivation safe to
/// ship. A wrong derivation is a 400, not a stored bad value.
///
/// # Why the invoker and not the dispatcher
///
/// That paragraph was false for as long as this route called
/// `dispatch_unscoped` directly. The guard filed after one session put 394 junk
/// projects into a real store — a throwaway project may only be created in a
/// throwaway store — lives in [`StoreInvoker::invoke`], above the dispatcher
/// and below every client. The CLI reaches it because `/api/v1/invoke` goes
/// through the invoker; this route went round it, so a form field naming `/tmp`
/// created exactly the project the guard exists to refuse. Going through the
/// invoker is what makes the claim above a property rather than an intention.
fn route_init_repo<S: Store>(store: &S, env: &Environment, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let path = require_str(&obj, "path")?;
        let prefix = get_str(&obj, "prefix")
            .map(str::trim)
            .filter(|prefix| !prefix.is_empty())
            .ok_or_else(|| {
                AppError::Usage(
                    "`prefix` is required. It is minted into every story id this project ever \
                     creates and cannot be changed afterwards, so it is not defaulted."
                        .to_string(),
                )
            })?;
        let invocation = Invocation::Project {
            action: ProjectAction::New(crate::cli::NewProjectRequest::Stated(
                crate::cli::NewProjectSpec {
                    // Absolute, because a relative path would resolve against
                    // the daemon's own working directory and the browser has no
                    // meaningful one to offer.
                    attach: crate::cli::Attach::Path(path.to_string()),
                    prefix: crate::domain::prefix::validate(prefix)?,
                    name: get_str(&obj, "name").map(str::to_string),
                    no_agents_md: false,
                },
            )),
        };
        let response = invoke_from_browser(store, env, std::path::Path::new(path), invocation)?;
        Ok(json_reply(201, render_response(&response, true, false)))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// Runs `invocation` the way a `story` command runs, on the browser's behalf.
///
/// `cwd` is the directory the work happens *in*. A browser has none, so every
/// caller here supplies one deliberately: the path the form named, or `/` for a
/// request that names its project some other way.
///
/// **The one door for this module's catalog routes.** They used to call
/// [`crate::invoke::dispatch_unscoped`], which is one layer too low: it is
/// below the SH-95 creation guard, so the dashboard could create a project the
/// CLI refuses. Naming the door once means a third catalog route added later
/// inherits the guard instead of having to remember it.
fn invoke_from_browser<S: Store>(
    store: &S,
    env: &Environment,
    cwd: &std::path::Path,
    invocation: Invocation,
) -> Result<Response, AppError> {
    invoke_from_browser_as(store, env, cwd, invocation, None)
}

/// [`invoke_from_browser`], for a route that names its project by slug.
///
/// A browser has no working directory to resolve from, so a scoped verb
/// reached from one has to carry a selector — the same
/// [`ProjectSelector::Flag`](crate::api::wire::ProjectSelector::Flag) a
/// `--project <slug>` on the command line produces. Routing it through the
/// selector rather than through a target field of the action's own is what
/// keeps SH-116's "three ways and no fourth" true of the browser too.
fn invoke_from_browser_as<S: Store>(
    store: &S,
    env: &Environment,
    cwd: &std::path::Path,
    invocation: Invocation,
    project: Option<crate::api::wire::ProjectSelector>,
) -> Result<Response, AppError> {
    use crate::invoke::{InvokeRequest, Invoker as _};
    crate::invoke::StoreInvoker::new(store, cwd, env.clone())
        .invoke(InvokeRequest::new(invocation).project(project))
}

/// `DELETE /api/repos/{id}` — **permanently delete** a project and everything
/// recorded against it.
///
/// Two-step, exactly as the CLI is. Without a body naming the project's slug
/// this answers `409` carrying the same [`DeletePlan`](crate::output::DeletePlan)
/// the terminal prompt is drawn from, and writes nothing; the browser renders
/// that as its confirmation modal and sends the slug back. One value, two
/// front-ends, so the warning cannot say two different things.
///
/// Body: `{"confirm": "<slug>"}`.
fn route_delete_repo<S: Store>(store: &S, env: &Environment, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let confirmed = parse_json_object(body)
            .ok()
            .and_then(|obj| get_str(&obj, "confirm").map(str::to_string))
            .is_some_and(|typed| typed == id);

        let invocation = Invocation::Project {
            action: ProjectAction::Delete { force: confirmed },
        };
        // The project is named the way every other scoped verb names one — the
        // selector — rather than by a target field the action no longer has.
        // The daemon has no working directory of the user's and does not need
        // one: `delete` reads no filesystem, and the slug answers on its own.
        let response = invoke_from_browser_as(
            store,
            env,
            std::path::Path::new("/"),
            invocation,
            Some(crate::api::wire::ProjectSelector::Flag {
                slug: id.to_string(),
            }),
        )?;
        let status = match response {
            Response::ConfirmationRequired(_) => 409,
            _ => 200,
        };
        Ok(json_reply(status, render_response(&response, true, false)))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

// --- Project data: /api/repos/<id>/data ---

/// Everything the board renders from, in one request: the rollup, every story
/// with its readiness flags, and the project's configuration vocabulary.
fn project_data_json<S: Store>(
    ctx: &Ctx<'_, S>,
    verification_activity: &VerificationActivity,
) -> Result<String, AppError> {
    let now = ctx.now();
    let project = ctx.project();
    ctx.store().read(|tx| {
        Ok((|| -> Result<String, AppError> {
            let query = QueryService::new(tx, project, &now);
            let data = query.report_data()?;
            let project_record = tx
                .project(project)?
                .ok_or_else(|| AppError::Storage(format!("project {project} does not exist")))?;
            let highest_story_number = project_record.next_story_no - 1;
            let mut open_prs_by_story = BTreeMap::new();
            for (story_no, link) in tx.open_pr_links(project)? {
                open_prs_by_story
                    .entry(story_no.to_id(&project_record.prefix))
                    .or_insert_with(Vec::new)
                    .push(link);
            }
            let active = verification_activity.active();
            let incident = tx.verification_incident()?;
            let verification = crate::daemon::verification_progress::status_snapshot_with_incident(
                &crate::service::verification::ordered_candidates(tx)?,
                active.as_ref(),
                incident.as_ref(),
                ctx.env(),
                &now,
            );

            // Drafts (SH-175) are excluded from `stories`: the board is a curated
            // "what's actionable" view,
            // deliberately its own filter chain rather than `story list`'s — SH-175's
            // council verdict kept the two separate before `list` had a default
            // exclusion of its own to converge with, and SH-409 didn't revisit that.
            // They are not dropped, only routed — every draft lands in
            // `drafts_json` below instead, which is what powers the Drafts popover and
            // its count badge.
            let stories_json: Vec<serde_json::Value> = data
                .stories
                .iter()
                .filter(|view| !view.story.draft)
                .map(|view| {
                    let mut val = serde_json::to_value(view).unwrap_or(serde_json::Value::Null);
                    if let serde_json::Value::Object(ref mut map) = val {
                        map.insert(
                            "is_ready".to_string(),
                            serde_json::Value::Bool(data.ready_ids.contains(&view.story.id)),
                        );
                        map.insert(
                            "is_blocked".to_string(),
                            serde_json::Value::Bool(data.blocked_ids.contains(&view.story.id)),
                        );
                        if let Some(open_prs) = open_prs_by_story.get(&view.story.id) {
                            map.insert(
                                "open_prs".to_string(),
                                serde_json::to_value(open_prs).unwrap_or(serde_json::Value::Null),
                            );
                        }
                        if let Some((_, _, status)) =
                            verification.iter().find(|(status_project, status_id, _)| {
                                *status_project == project && status_id == &view.story.id
                            })
                        {
                            map.insert(
                                "verification".to_string(),
                                serde_json::to_value(status).unwrap_or(serde_json::Value::Null),
                            );
                        }
                    }
                    val
                })
                .collect();

            let drafts_json = dashboard_drafts_json(&data);

            let incident_json = incident
                .as_ref()
                .map(|incident| -> Result<serde_json::Value, AppError> {
                    let owner = tx.project(incident.project)?.ok_or_else(|| {
                        AppError::NotFound(format!("project {}", incident.project.get()))
                    })?;
                    Ok(serde_json::json!({
                        "incident_id": incident.incident_id,
                        "project": owner.slug,
                        "story_id": incident.story.to_id(&owner.prefix),
                        "generation": incident.generation,
                        "disposition": incident.disposition,
                        "halted": incident.halted,
                        "attempts": incident.attempts,
                        "detail": incident.detail,
                        "first_failed_at": incident.first_failed_at,
                        "last_failed_at": incident.last_failed_at,
                    }))
                })
                .transpose()?;

            let response = serde_json::json!({
                "summary": data.summary,
                "stories": stories_json,
                "drafts": drafts_json,
                "ready_ids": data.ready_ids,
                "blocked_ids": data.blocked_ids,
                "next_ids": data.next_ids,
                "highest_story_number": highest_story_number,
                "meta": meta_json(tx, project, &data)?,
                "verification_incident": incident_json,
            });
            to_json(&response)
        })())
    })?
}

/// Acknowledges exactly the halted incident the browser displayed.
fn route_ack_verification<S: Store>(ctx: &Ctx<'_, S>, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let expected = require_str(&obj, "incident_id")?;
        let current = ctx.store().read(|tx| tx.verification_incident())?;
        let Some(current) = current else {
            return Err(AppError::Validation(
                "no verification incident is active".into(),
            ));
        };
        if !current.halted {
            return Err(AppError::Validation(
                "the verification incident is still retrying".into(),
            ));
        }
        if current.incident_id != expected {
            return Err(AppError::Validation(format!(
                "verification incident `{expected}` is stale; current incident is `{}`",
                current.incident_id
            )));
        }
        ctx.store().write(|tx| {
            tx.clear_verification_incident(expected)?;
            Ok(())
        })?;
        Ok(json_reply(
            200,
            serde_json::json!({"acknowledged": expected}).to_string(),
        ))
    })()
    .unwrap_or_else(|error| error_reply(&error))
}

/// The `meta` object describing the project's configuration — states in
/// configured order (which the board's columns must follow), types, members,
/// the fixed priority/relation vocabularies, and the default state/type a new
/// story should preselect (SH-44) — so the frontend never has to hardcode
/// anything project-specific.
fn meta_json<R: ReadOps>(
    tx: &R,
    project: ProjectId,
    data: &crate::output::ReportData,
) -> Result<serde_json::Value, AppError> {
    let state_defs = tx.states(project)?;
    // The same pure selection `service::story::default_open_state` delegates
    // to for `story new`'s own default — computed from the identical ordered
    // `Vec` before it is reshaped into `states` below, so the two can't drift
    // apart by reading the catalog differently.
    let default_state_slug = default_open_state(&state_defs).map(|s| s.slug);
    let states: Vec<serde_json::Value> = state_defs
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "slug": s.slug,
                "super_state": s.super_state.as_str(),
                "role": s.role,
                "description": s.description,
            })
        })
        .collect();

    let type_defs = tx.types(project)?;
    let default_type_slug = default_type(&type_defs).map(|t| t.slug);
    let types: Vec<serde_json::Value> = type_defs
        .into_iter()
        .map(|t| {
            serde_json::json!({ "slug": t.slug, "description": t.description, "emoji": t.emoji })
        })
        .collect();

    let members: Vec<serde_json::Value> = tx
        .members(project)?
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "display_name": m.display_name,
                "github": m.github,
            })
        })
        .collect();

    let priorities: Vec<&str> = PRIORITIES.iter().map(Priority::as_str).collect();
    // Derived from the stories already read rather than from a second query:
    // the legacy `storage::distinct_labels` folded every surviving snapshot's
    // labels into a sorted set, and this is that set, over the same stories.
    let labels: std::collections::BTreeSet<&str> = data
        .stories
        .iter()
        .flat_map(|view| view.story.labels.iter().map(String::as_str))
        .collect();

    Ok(serde_json::json!({
        "states": states,
        "types": types,
        "members": members,
        "priorities": priorities,
        "relations": RELATIONS,
        "labels": labels,
        "defaults": {
            "state": default_state_slug,
            "type": default_type_slug,
            "priority": Priority::Low.as_str()
        },
    }))
}

// --- Story mutation routes ---
//
// Each parses its JSON body, builds the matching `Invocation`, and dispatches
// it once. Body-parsing failures short-circuit to `error_reply` via the
// `?`-in-a-closure pattern below.

/// `POST /api/repos/{id}/story` — create a new story.
fn route_create_story<S: Store>(ctx: &Ctx<'_, S>, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let title = require_str(&obj, "title")?.to_string();
        let state = get_str(&obj, "state").map(str::to_string);
        let story_type = get_str(&obj, "type").map(str::to_string);
        let description = get_str(&obj, "description").map(str::to_string);
        let priority = get_str(&obj, "priority").map(str::to_string);
        let labels = if obj.contains_key("labels") {
            Some(get_str_array(&obj, "labels"))
        } else {
            None
        };
        let draft = get_bool(&obj, "draft");
        Ok(reply_with(
            ctx,
            201,
            Invocation::New {
                title,
                state,
                story_type,
                description,
                priority,
                labels,
                assignee: None,
                draft,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `PATCH /api/repos/{id}/story/{story}` — update title/state/priority/assignee/type.
///
/// Label changes go through the dedicated `/labels` route instead: `SetFields`
/// only ever *adds* labels via this field, which would be a confusing PATCH
/// semantic (callers would reasonably expect a full replace).
///
/// The one route whose CLI answer is the wrong shape for a client: `story set`
/// reports a plain message, and every mutation route here answers with the story
/// so the frontend can reconcile optimistic UI state uniformly. It therefore
/// calls the service and reads the view itself, rather than dispatching twice —
/// which is what this route used to do, and what cost it two acquisitions of a
/// lock that no longer exists.
///
/// Being the one route that skips [`dispatch`] makes it the one route that
/// would also skip the story-id canonicalization every other door gets there
/// (SH-118), so it asks for the same expansion explicitly. That call is the
/// price of the shortcut above, and it is written here rather than hidden
/// inside `set_fields` because the rule belongs to the *door*, not to the
/// service.
fn route_patch_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let id = &crate::invoke::story_ids::canonicalize_one(ctx, id)?;
        let obj = parse_json_object(body)?;
        let edits = FieldEdits {
            title: get_str(&obj, "title").map(str::to_string),
            state: get_str(&obj, "state").map(str::to_string),
            priority: get_str(&obj, "priority").map(str::to_string),
            assignee: get_str(&obj, "assignee").map(str::to_string),
            labels: None,
            blocked: None,
            unblocked: false,
            json: None,
            story_type: get_str(&obj, "type").map(str::to_string),
            description: get_str(&obj, "description").map(str::to_string),
        };
        StoryService::new(ctx).set_fields(id, &edits)?;
        let response = ctx.story_view(id)?;
        Ok(json_reply(200, render_response(&response, true, false)))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `POST /api/repos/{id}/story/{story}/move` — the board's drag-and-drop
/// endpoint. Moving into a CLOSED state archives the story (handled inside
/// `SetState`). An optional `reason` (SH-205) sets `awaiting` atomically with
/// the move — the Blocked-column drop prompt's skippable field; omitted by
/// every other drop, so no existing caller's payload shape needs to change.
fn route_move_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let state = require_str(&obj, "state")?.to_string();
        let comment = get_str(&obj, "comment").map(str::to_string);
        let awaiting = get_str(&obj, "reason").map(str::to_string);
        Ok(reply_with(
            ctx,
            200,
            Invocation::SetState {
                id: id.to_string(),
                state,
                comment,
                if_state: None,
                awaiting,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

fn route_comment_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let text = require_str(&obj, "text")?.to_string();
        Ok(reply_with(
            ctx,
            200,
            Invocation::Comment {
                id: id.to_string(),
                text,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

fn route_priority_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let priority = require_str(&obj, "priority")?.to_string();
        Ok(reply_with(
            ctx,
            200,
            Invocation::SetPriority {
                id: id.to_string(),
                priority,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

fn route_assign_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let member = require_str(&obj, "member")?.to_string();
        Ok(reply_with(
            ctx,
            200,
            Invocation::Assign {
                id: id.to_string(),
                member,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

fn route_labels_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let add = get_str_array(&obj, "add");
        let remove = get_str_array(&obj, "remove");
        if add.is_empty() && remove.is_empty() {
            return Err(AppError::Usage(
                "request body must include a non-empty `add` or `remove` array".to_string(),
            ));
        }
        Ok(reply_with(
            ctx,
            200,
            Invocation::SetLabels {
                id: id.to_string(),
                add,
                remove,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

fn route_block_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let awaiting = require_str(&obj, "reason")?.to_string();
        Ok(reply_with(
            ctx,
            200,
            Invocation::SetAwaiting {
                id: id.to_string(),
                awaiting: Some(awaiting),
                on: Vec::new(),
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `POST /api/repos/{id}/story/{story}/link-pr` — links a GitHub pull
/// request to a story (SH-49). Body: `{"url": "...", "close_on_merge": true}`;
/// `close_on_merge` defaults to `true` when absent, matching `story link-pr`'s
/// own default. Never touches GitHub — see `Invocation::LinkPr`'s doc for why
/// there is no REST route for `pr-check`, which does.
fn route_link_pr_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let url = require_str(&obj, "url")?.to_string();
        let close_on_merge = obj
            .get("close_on_merge")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        Ok(reply_with(
            ctx,
            200,
            Invocation::LinkPr {
                id: id.to_string(),
                url,
                close_on_merge,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `POST /api/repos/{id}/story/{story}/unlink-pr` — the inverse of
/// [`route_link_pr_story`]. Body: `{"url": "..."}`.
fn route_unlink_pr_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let url = require_str(&obj, "url")?.to_string();
        Ok(reply_with(
            ctx,
            200,
            Invocation::UnlinkPr {
                id: id.to_string(),
                url,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

fn route_unblock_story<S: Store>(ctx: &Ctx<'_, S>, id: &str) -> Reply {
    reply_with(
        ctx,
        200,
        Invocation::ClearAwaiting {
            id: id.to_string(),
            on: Vec::new(),
        },
    )
}

/// `POST /api/repos/{id}/story/{story}/reopen` — reopens a closed story.
fn route_reopen_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, _body: &str) -> Reply {
    reply_with(ctx, 200, Invocation::Reopen { id: id.to_string() })
}

/// `DELETE /api/repos/{id}/story/{story}` — permanently deletes one story.
/// Without `{"force":true}` this answers 409 with the server-authored plan
/// and writes nothing; the browser renders that plan before retrying forced.
fn route_delete_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let force = get_bool(&obj, "force");
        let response = dispatch(
            ctx,
            Invocation::Delete {
                id: id.to_string(),
                force,
            },
        )?;
        let status = match response {
            Response::ConfirmationRequired(_) => 409,
            _ => 200,
        };
        Ok(json_reply(status, render_response(&response, true, false)))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `POST /api/repos/{id}/story/{story}/archive` — the "Archive" action
/// (SH-43). Refuses an open story.
fn route_hide_story<S: Store>(ctx: &Ctx<'_, S>, id: &str) -> Reply {
    reply_with(ctx, 200, Invocation::Hide { id: id.to_string() })
}

/// `POST /api/repos/{id}/story/{story}/unarchive` — the inverse of
/// [`route_hide_story`].
fn route_unhide_story<S: Store>(ctx: &Ctx<'_, S>, id: &str) -> Reply {
    reply_with(ctx, 200, Invocation::Unhide { id: id.to_string() })
}

/// `POST /api/repos/{id}/story/{story}/publish` — makes a draft live
/// (SH-175). One-way; idempotent on a story that is already live.
fn route_publish_story<S: Store>(ctx: &Ctx<'_, S>, id: &str) -> Reply {
    reply_with(ctx, 200, Invocation::Publish { id: id.to_string() })
}

/// `POST /api/repos/{id}/states/{slug}/archive` — the CLOSED-superstate
/// column's bulk "Archive" button (SH-43). Body `{"force": bool}`, absent
/// treated as `false`. Shaped exactly like [`route_reopen_story`]: an
/// unforced call answers `409` carrying the
/// [`HideStatePlan`](crate::output::HideStatePlan) the confirmation modal
/// reads its count and id list from; `force: true` commits.
fn route_hide_state<S: Store>(ctx: &Ctx<'_, S>, slug: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let force = get_bool(&obj, "force");
        let response = dispatch(
            ctx,
            Invocation::HideState {
                state: slug.to_string(),
                force,
            },
        )?;
        let status = match response {
            Response::ConfirmationRequired(_) => 409,
            _ => 200,
        };
        Ok(json_reply(status, render_response(&response, true, false)))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

// --- Project state configuration: /api/repos/<id>/states ---

/// The states payload every state route replies with: the configured states in
/// order — which is the board's column order — each with the story counts the
/// editor needs to decide whether a change is destructive.
///
/// `open_count` stories can be migrated elsewhere; `archived_count` ones cannot,
/// so the two stay separate rather than summed.
fn states_json<S: Store>(ctx: &Ctx<'_, S>, message: Option<&str>) -> Result<String, AppError> {
    let states: Vec<serde_json::Value> = ConfigService::new(ctx)
        .list_states()?
        .into_iter()
        .map(|listing| {
            serde_json::json!({
                "slug": listing.state.slug,
                "super_state": listing.state.super_state.as_str(),
                "role": listing.state.role,
                "description": listing.state.description,
                "open_count": listing.usage.open,
                "archived_count": listing.usage.archived,
            })
        })
        .collect();

    to_json(&serde_json::json!({
        "result": "ok",
        "message": message,
        "states": states,
    }))
}

/// Dispatches a state mutation, then replies with the refreshed list rather than
/// the bare success message, so the editor always redraws from server truth
/// instead of guessing what its own edit did (an edit can move stories, which
/// changes counts on *two* states at once).
fn state_mutation<S: Store>(ctx: &Ctx<'_, S>, status: u16, action: StateAction) -> Reply {
    match dispatch(ctx, Invocation::State { action }) {
        Ok(response) => {
            let message = match response {
                Response::Message(text) => text,
                _ => String::new(),
            };
            match states_json(ctx, Some(&message)) {
                Ok(json) => json_reply(status, json).no_cache(),
                Err(e) => error_reply(&e),
            }
        }
        Err(e) => error_reply(&e),
    }
}

/// `POST /api/repos/{id}/states` — add a state.
fn route_create_state<S: Store>(ctx: &Ctx<'_, S>, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let slug = require_str(&obj, "slug")?.to_string();
        let superstate = require_str(&obj, "super_state")?.to_string();
        Ok(state_mutation(
            ctx,
            201,
            StateAction::Add {
                slug,
                superstate,
                role: get_str(&obj, "role").map(str::to_string),
                description: get_str(&obj, "description").map(str::to_string),
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `PATCH /api/repos/{id}/states/{slug}` — edit one state.
///
/// `role` and `description` are three-valued: absent leaves the field alone,
/// `null` clears it, a string sets it. `move_stories_to` names where open
/// stories go when the edit reclassifies the state they sit in.
fn route_patch_state<S: Store>(ctx: &Ctx<'_, S>, slug: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let field_edit = |key: &str| match obj.get(key) {
            None => Ok(None),
            Some(serde_json::Value::Null) => Ok(Some(None)),
            Some(serde_json::Value::String(value)) => Ok(Some(Some(value.clone()))),
            Some(_) => Err(AppError::Usage(format!("`{key}` must be a string or null"))),
        };
        let role = field_edit("role")?;
        let description = field_edit("description")?;

        Ok(state_mutation(
            ctx,
            200,
            StateAction::Set {
                slug: slug.to_string(),
                superstate: get_str(&obj, "super_state").map(str::to_string),
                // A literal "none" reads as "clear the role".
                role: role.map(|value| value.unwrap_or_else(|| "none".to_string())),
                description: description.clone().flatten(),
                clear_description: matches!(description, Some(None)),
                move_stories_to: get_str(&obj, "move_stories_to").map(str::to_string),
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `DELETE /api/repos/{id}/states/{slug}` — remove a state, optionally
/// migrating the open stories still in it.
fn route_delete_state<S: Store>(ctx: &Ctx<'_, S>, slug: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        Ok(state_mutation(
            ctx,
            200,
            StateAction::Remove {
                slug: slug.to_string(),
                move_stories_to: get_str(&obj, "move_stories_to").map(str::to_string),
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `PATCH /api/repos/{id}/states` — reorder the whole set, i.e. the board's
/// column order. The body must list every state; a partial order is refused
/// rather than interpreted as a deletion.
fn route_reorder_states<S: Store>(ctx: &Ctx<'_, S>, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let order = get_str_array(&obj, "order");
        if order.is_empty() {
            return Err(AppError::Usage(
                "`order` is required and must be a non-empty array of state slugs".to_string(),
            ));
        }
        Ok(state_mutation(ctx, 200, StateAction::Reorder { order }))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// Backs both `POST /api/repos/{id}/relate` (`remove: false`) and
/// `POST /api/repos/{id}/unrelate` (`remove: true`).
fn route_relate<S: Store>(ctx: &Ctx<'_, S>, body: &str, remove: bool) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let a = require_str(&obj, "a")?.to_string();
        let relation = require_str(&obj, "relation")?.to_string();
        let b = require_str(&obj, "b")?.to_string();
        Ok(reply_with(
            ctx,
            200,
            Invocation::Relate {
                a,
                relation,
                b,
                remove,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::http::json_reply;

    fn ok_reply() -> Reply {
        json_reply(200, "{}")
    }

    /// The headers a same-origin dashboard `fetch` sends, which is the only
    /// shape [`guarded`] lets through.
    fn dashboard_headers() -> Vec<Header> {
        vec![
            Header::from_bytes("X-Storyhook", "1").unwrap(),
            Header::from_bytes("Host", "127.0.0.1:3456").unwrap(),
            Header::from_bytes("Content-Type", "application/json").unwrap(),
        ]
    }

    /// **The browser must not be able to create a project the CLI would
    /// refuse** — which is what this module's own header has claimed since it
    /// was written, and what the route did not do.
    ///
    /// The SH-95 guard lives in `StoreInvoker::invoke`; both `/api/repos`
    /// handlers called `dispatch_unscoped` directly and went round it. So a
    /// `POST` naming a path under `/tmp`, against a store that is not itself
    /// throwaway, created exactly the project the guard exists to refuse — the
    /// shape that put 394 junk projects into a real store, reachable from a web
    /// form instead of from a test fixture.
    #[test]
    fn creating_a_project_at_a_throwaway_path_in_a_real_store_is_refused() {
        let home = storyhook_test_support::non_temporary_dir("sh117-rest-guard-home");
        // `non_temporary_dir` removes and recreates `home` on every call
        // (SH-404's migration guard permits it purely on that account: the
        // store `open_store` finds here is always fresh, `from_version == 0`,
        // never on the strength of `Environment::at`'s `is_default() == true`
        // agreeing with the real default path — it does).
        let env = Environment::at(&home);
        let store = crate::invoke::open_store(&env).expect("opening the store");
        let throwaway = crate::env::canonical_ish(&std::env::temp_dir())
            .expect("canonicalizing the temporary root")
            .join("sh117-rest-guard-project");
        std::fs::create_dir_all(&throwaway).expect("creating the throwaway checkout");

        let routed = route(
            &store,
            &env,
            &Method::Post,
            "/api/repos",
            &dashboard_headers(),
            &format!(
                "{{\"path\": {}, \"prefix\": \"RG\"}}",
                to_json(&throwaway.display().to_string()).expect("encoding the path")
            ),
            &TrustedHosts::default(),
        );

        assert_ne!(
            routed.reply.status, 201,
            "the dashboard created a project at a throwaway path in a real store"
        );
        assert!(
            store
                .read(|tx| tx.projects())
                .expect("reading the store")
                .is_empty(),
            "the refusal still wrote a project"
        );
        assert_eq!(
            routed.changed, None,
            "a refused create must not tell every connected browser the catalog moved"
        );
    }

    #[test]
    fn a_successful_write_reports_what_it_changed() {
        for method in [Method::Post, Method::Patch, Method::Delete] {
            let routed = Routed::changing(&method, ok_reply(), Changed::Project("app".into()));
            assert_eq!(
                routed.changed,
                Some(Changed::Project("app".into())),
                "{method:?}"
            );
        }
    }

    /// A rejected edit changed nothing — telling every client to refetch for it
    /// would be pure noise, and worse, would make a failing client look like a
    /// busy project.
    #[test]
    fn a_rejected_write_reports_no_change() {
        for status in [400, 403, 404, 415, 422, 500] {
            let routed =
                Routed::changing(&Method::Post, json_reply(status, "{}"), Changed::Catalog);
            assert_eq!(routed.changed, None, "status {status}");
        }
    }

    /// A read publishing a change is a feedback loop: every client refetches
    /// because a client fetched, forever, at the rate the browser retries.
    /// Found by `sse_disconnect_does_not_break_server_for_other_clients`, which
    /// went quiet because a `GET /data` published a `repo-changed` that then
    /// coalesced away the real one that followed it.
    #[test]
    fn a_read_reports_no_change_however_well_it_went() {
        for method in [Method::Get, Method::Head, Method::Options] {
            let routed = Routed::changing(&method, ok_reply(), Changed::Project("app".into()));
            assert_eq!(routed.changed, None, "{method:?}");
        }
    }
}
