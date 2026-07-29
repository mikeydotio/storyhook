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

use std::path::PathBuf;

use tiny_http::{Header, Method};

use crate::api::http::{
    Reply, error_reply, get_bool, get_str, get_str_array, guarded, guarded_no_body, html_reply,
    json_reply, parse_json_object, path_segments, require_str, text_reply, to_json,
};
use crate::cli::{Invocation, StateAction};
use crate::domain::Priority;
use crate::env::Environment;
use crate::error::AppError;
use crate::invoke::dispatch;
use crate::output::{Response, render_response};
use crate::service::{CatalogService, ConfigService, Ctx, FieldEdits, QueryService, StoryService};
use crate::store::{ProjectId, ReadOps, Store};

const DASHBOARD_HTML: &str = include_str!("../web_dashboard.html");

/// All priority levels, in the order the frontend should offer them.
const PRIORITIES: [Priority; 5] = [
    Priority::Critical,
    Priority::High,
    Priority::Medium,
    Priority::Low,
    Priority::None,
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
fn mutating(method: &Method) -> bool {
    matches!(method, Method::Post | Method::Patch | Method::Delete)
}

/// Decides how to respond to a request against `store`.
///
/// `/` always serves the single-page app; `/api/repos` (list/register) and
/// `/api/repos/<id>` (deregister) operate on the project catalog; every other
/// `/api/repos/<id>/...` path resolves `<id>` to that project and delegates to
/// [`route_project`], the entire per-project API surface.
pub fn route<S: Store>(
    store: &S,
    env: &Environment,
    method: &Method,
    path: &str,
    headers: &[Header],
    body: &str,
    trusted_hosts: &[String],
) -> Routed {
    match path_segments(path).as_slice() {
        [] => match method {
            Method::Get => Routed::quiet(html_reply(DASHBOARD_HTML).no_cache()),
            _ => Routed::quiet(text_reply(405, "Method not allowed")),
        },
        ["api", "repos"] => match method {
            Method::Get => Routed::quiet(match repos_json(store, env) {
                Ok(json) => json_reply(200, json).no_cache(),
                Err(e) => error_reply(&e),
            }),
            Method::Post => Routed::changing(
                method,
                guarded(headers, trusted_hosts, body, |b| {
                    route_register_repo(store, b)
                }),
                Changed::Catalog,
            ),
            _ => Routed::quiet(text_reply(405, "Method not allowed")),
        },
        ["api", "repos", id] => match method {
            Method::Delete => Routed::changing(
                method,
                guarded_no_body(headers, trusted_hosts, || route_deregister_repo(store, id)),
                Changed::Catalog,
            ),
            _ => Routed::quiet(text_reply(405, "Method not allowed")),
        },
        ["api", "repos", id, rest @ ..] => match resolve_project(store, id) {
            Ok(Some((project, root))) => {
                let ctx = Ctx::new(store, project, root, env.clone());
                let reply = route_project(&ctx, method, rest, headers, body, trusted_hosts);
                Routed::changing(method, reply, Changed::Project((*id).to_string()))
            }
            Ok(None) => Routed::quiet(text_reply(404, "Not found")),
            Err(e) => Routed::quiet(error_reply(&e)),
        },
        _ => Routed::quiet(text_reply(404, "Not found")),
    }
}

/// The per-project API surface — everything under `/api/repos/<id>/...` once
/// `<id>` has resolved. `rest` is the path *after* `/api/repos/<id>`, e.g.
/// `["data"]` or `["story", "SH-1", "move"]`.
fn route_project<S: Store>(
    ctx: &Ctx<'_, S>,
    method: &Method,
    rest: &[&str],
    headers: &[Header],
    body: &str,
    trusted_hosts: &[String],
) -> Reply {
    match rest {
        ["data"] => match method {
            Method::Get => match project_data_json(ctx) {
                Ok(json) => json_reply(200, json).no_cache(),
                Err(e) => error_reply(&e),
            },
            _ => text_reply(405, "Method not allowed"),
        },
        ["story"] => match method {
            Method::Post => guarded(headers, trusted_hosts, body, |b| route_create_story(ctx, b)),
            _ => text_reply(405, "Method not allowed"),
        },
        ["story", id] => match method {
            Method::Get => reply_with(
                ctx,
                200,
                Invocation::Show {
                    id: (*id).to_string(),
                },
            ),
            Method::Patch => guarded(headers, trusted_hosts, body, |b| {
                route_patch_story(ctx, id, b)
            }),
            Method::Delete => guarded(headers, trusted_hosts, body, |b| {
                route_delete_story(ctx, id, b)
            }),
            _ => text_reply(405, "Method not allowed"),
        },
        ["story", id, action] => match (method, *action) {
            (Method::Post, "move") => guarded(headers, trusted_hosts, body, |b| {
                route_move_story(ctx, id, b)
            }),
            (Method::Post, "comment") => guarded(headers, trusted_hosts, body, |b| {
                route_comment_story(ctx, id, b)
            }),
            (Method::Post, "priority") => guarded(headers, trusted_hosts, body, |b| {
                route_priority_story(ctx, id, b)
            }),
            (Method::Post, "assign") => guarded(headers, trusted_hosts, body, |b| {
                route_assign_story(ctx, id, b)
            }),
            (Method::Post, "labels") => guarded(headers, trusted_hosts, body, |b| {
                route_labels_story(ctx, id, b)
            }),
            (Method::Post, "block") => guarded(headers, trusted_hosts, body, |b| {
                route_block_story(ctx, id, b)
            }),
            (Method::Post, "unblock") => {
                guarded_no_body(headers, trusted_hosts, || route_unblock_story(ctx, id))
            }
            (Method::Post, "reopen") => guarded(headers, trusted_hosts, body, |b| {
                route_reopen_story(ctx, id, b)
            }),
            (Method::Post, _) => text_reply(404, "Not found"),
            _ => text_reply(405, "Method not allowed"),
        },
        // Reordering is a PATCH of the *collection*, not a
        // `/states/reorder` sub-path: `reorder` is a legal state slug, and
        // that route would shadow the state with that name.
        ["states"] => match method {
            Method::Get => match states_json(ctx, None) {
                Ok(json) => json_reply(200, json).no_cache(),
                Err(e) => error_reply(&e),
            },
            Method::Post => guarded(headers, trusted_hosts, body, |b| route_create_state(ctx, b)),
            Method::Patch => guarded(headers, trusted_hosts, body, |b| {
                route_reorder_states(ctx, b)
            }),
            _ => text_reply(405, "Method not allowed"),
        },
        ["states", slug] => match method {
            Method::Patch => guarded(headers, trusted_hosts, body, |b| {
                route_patch_state(ctx, slug, b)
            }),
            Method::Delete => guarded(headers, trusted_hosts, body, |b| {
                route_delete_state(ctx, slug, b)
            }),
            _ => text_reply(405, "Method not allowed"),
        },
        ["relate"] => match method {
            Method::Post => guarded(headers, trusted_hosts, body, |b| {
                route_relate(ctx, b, false)
            }),
            _ => text_reply(405, "Method not allowed"),
        },
        ["unrelate"] => match method {
            Method::Post => guarded(headers, trusted_hosts, body, |b| route_relate(ctx, b, true)),
            _ => text_reply(405, "Method not allowed"),
        },
        _ => text_reply(404, "Not found"),
    }
}

/// Resolves a catalog id (the project's slug) to the project and the checkout
/// the dashboard should act in.
///
/// A project with no recorded checkout is *not found* rather than served: every
/// operation below runs in a working directory — that is where a project's event
/// hooks and its git repository are — and inventing one would mean firing a
/// user's hooks from somewhere they never asked for.
fn resolve_project<S: Store>(
    store: &S,
    slug: &str,
) -> Result<Option<(ProjectId, PathBuf)>, AppError> {
    Ok(store.read(|tx| {
        let Some(project) = tx.project_by_slug(slug)? else {
            return Ok(None);
        };
        let root = tx
            .project_paths(project.id)?
            .into_iter()
            .next()
            .map(|record| PathBuf::from(record.path));
        Ok(root.map(|root| (project.id, root)))
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

/// `GET /api/repos` — one entry per project the store knows a checkout for,
/// driving the repo-selector dropdown, the home screen's summary cards, and the
/// settings screen's project list.
///
/// A project whose data cannot currently be read is reported as
/// `available: false` with an `error` message rather than failing the whole
/// request — one broken project must never take down the view of every other
/// one.
fn repos_json<S: Store>(store: &S, env: &Environment) -> Result<String, AppError> {
    let entries = CatalogService::new(store).list()?;
    let now = env.now();
    let repos: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|entry| {
            let summary = store
                .read(|tx| Ok(QueryService::new(tx, entry.project, &now).report_data()))
                .map_err(AppError::from)
                .and_then(|inner| inner);
            match summary {
                Ok(data) => serde_json::json!({
                    "id": entry.id,
                    "name": entry.name,
                    "path": entry.path,
                    "available": true,
                    "summary": data.summary,
                }),
                Err(e) => serde_json::json!({
                    "id": entry.id,
                    "name": entry.name,
                    "path": entry.path,
                    "available": false,
                    "error": e.to_string(),
                }),
            }
        })
        .collect();

    to_json(&repos)
}

/// `POST /api/repos` — register a checkout. Body: `{"path": "...", "name"?: "..."}`.
fn route_register_repo<S: Store>(store: &S, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let path = require_str(&obj, "path")?;
        let name = get_str(&obj, "name");
        let entry = CatalogService::new(store).register(std::path::Path::new(path), name)?;
        Ok(json_reply(
            201,
            to_json(&serde_json::json!({
                "id": entry.id,
                "name": entry.name,
                "path": entry.path,
            }))?,
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `DELETE /api/repos/{id}` — forget a checkout. Never touches the project's
/// stories, only the record of where it is checked out.
fn route_deregister_repo<S: Store>(store: &S, id: &str) -> Reply {
    match CatalogService::new(store).deregister(id) {
        Ok(entry) => json_reply(
            200,
            serde_json::json!({
                "result": "ok",
                "repo": {"id": entry.id, "name": entry.name, "path": entry.path},
            })
            .to_string(),
        ),
        Err(e) => error_reply(&e),
    }
}

// --- Project data: /api/repos/<id>/data ---

/// Everything the board renders from, in one request: the rollup, every story
/// with its readiness flags, and the project's configuration vocabulary.
fn project_data_json<S: Store>(ctx: &Ctx<'_, S>) -> Result<String, AppError> {
    let now = ctx.now();
    let project = ctx.project();
    ctx.store().read(|tx| {
        Ok((|| -> Result<String, AppError> {
            let query = QueryService::new(tx, project, &now);
            let data = query.report_data()?;

            // Soft-deleted stories are excluded here rather than in `report_data` —
            // `story report`/`story list` intentionally still surface them (marked
            // deleted), but the dashboard has no such treatment and would otherwise
            // show them as live cards in whichever column matches their last state.
            let stories_json: Vec<serde_json::Value> = data
                .stories
                .iter()
                .filter(|view| !view.story.deleted)
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
                    }
                    val
                })
                .collect();

            let response = serde_json::json!({
                "summary": data.summary,
                "stories": stories_json,
                "ready_ids": data.ready_ids,
                "blocked_ids": data.blocked_ids,
                "meta": meta_json(tx, project, &data)?,
            });
            to_json(&response)
        })())
    })?
}

/// The `meta` object describing the project's configuration — states in
/// configured order (which the board's columns must follow), types, members, and
/// the fixed priority/relation vocabularies — so the frontend never has to
/// hardcode anything project-specific.
fn meta_json<R: ReadOps>(
    tx: &R,
    project: ProjectId,
    data: &crate::output::ReportData,
) -> Result<serde_json::Value, AppError> {
    let states: Vec<serde_json::Value> = tx
        .states(project)?
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

    let types: Vec<serde_json::Value> = tx
        .types(project)?
        .into_iter()
        .map(|t| serde_json::json!({ "slug": t.slug, "description": t.description }))
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
    // the legacy `storage::distinct_labels` folded every non-deleted snapshot's
    // labels into a sorted set, and this is that set, over the same stories.
    let labels: std::collections::BTreeSet<&str> = data
        .stories
        .iter()
        .filter(|view| !view.story.deleted)
        .flat_map(|view| view.story.labels.iter().map(String::as_str))
        .collect();

    Ok(serde_json::json!({
        "states": states,
        "types": types,
        "members": members,
        "priorities": priorities,
        "relations": RELATIONS,
        "labels": labels,
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
fn route_patch_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
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
/// `SetState`).
fn route_move_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let state = require_str(&obj, "state")?.to_string();
        let comment = get_str(&obj, "comment").map(str::to_string);
        Ok(reply_with(
            ctx,
            200,
            Invocation::SetState {
                id: id.to_string(),
                state,
                comment,
                if_state: None,
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
                awaiting,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

fn route_unblock_story<S: Store>(ctx: &Ctx<'_, S>, id: &str) -> Reply {
    reply_with(ctx, 200, Invocation::ClearAwaiting { id: id.to_string() })
}

/// `POST /api/repos/{id}/story/{story}/reopen` — reopens a closed story. An
/// optional `"force": true` in the JSON body undeletes a soft-deleted story,
/// mirroring the CLI's `story reopen <id> --force`; absent or `false` performs
/// the guarded (non-force) reopen. An empty body is treated as `{}`.
fn route_reopen_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let force = get_bool(&obj, "force");
        Ok(reply_with(
            ctx,
            200,
            Invocation::Reopen {
                id: id.to_string(),
                force,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `DELETE /api/repos/{id}/story/{story}` — a required, non-empty `reason` is
/// enforced the same way the CLI's `story delete` requires one.
fn route_delete_story<S: Store>(ctx: &Ctx<'_, S>, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let reason = require_str(&obj, "reason")?.to_string();
        Ok(reply_with(
            ctx,
            200,
            Invocation::Delete {
                id: id.to_string(),
                reason,
            },
        ))
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
