//! Every route the dashboard API serves, named once.
//!
//! # Why this module exists, and why it imports almost nothing
//!
//! [`classify`] turns a request head into a [`Route`], which the router in
//! [`crate::api::rest`] consumes to decide what a request means. **The `match`
//! has no wildcard arm.** A new route is a new variant, and a new variant is a
//! compile error in the router until somebody has answered "what does this
//! do?" — the same discipline [`crate::api::admission`] already documents and
//! relies on for the request family it forbids classifying by path-shape
//! arity.
//!
//! The module's imports are the other half of the guarantee. It reaches
//! [`Method`] and nothing else — no store, no service layer, no
//! [`Environment`](crate::env::Environment), no request body. That is not
//! tidiness: [`crate::daemon::serve`]'s worker decides admission **before it
//! reads a body**, so the classifier the gate consults has to be answerable
//! from the head alone. Keeping it in a module that *cannot* reach a body or a
//! store makes that a fact about the dependency graph rather than a rule
//! somebody has to remember.
//!
//! # What this module used to also do
//!
//! Before SH-255, [`authority`] was a second `match` over the same [`Route`],
//! answering what credential each one needed — `Public`, SH-254's scoped
//! `Session` capability, or the master token. SH-255 replaced that whole
//! three-level model with one: a named token now authenticates everything
//! [`crate::api::admission::admission`] gates, checked uniformly and without
//! consulting this module at all. There is nothing left to classify
//! per-route, so `Authority`, `authority()` and `project_authority()` are
//! gone rather than kept as dead code. See
//! `docs/spec/dashboard-authorization.md`'s "As built" section for the full
//! record of what this replaced and why.

use crate::daemon::http1::Method;

/// The thirteen actions a story exposes under
/// `POST /api/repos/{id}/story/{story}/{action}`.
///
/// Spelled as a type rather than matched as a string at the point of use, so
/// the router cannot answer a fourteenth action without adding it here first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoryAction {
    Move,
    Comment,
    Priority,
    Assign,
    Labels,
    Block,
    Unblock,
    Reopen,
    Archive,
    Unarchive,
    Publish,
    LinkPr,
    UnlinkPr,
}

impl StoryAction {
    /// The action `slug` names, if it names one.
    fn parse(slug: &str) -> Option<StoryAction> {
        Some(match slug {
            "move" => StoryAction::Move,
            "comment" => StoryAction::Comment,
            "priority" => StoryAction::Priority,
            "assign" => StoryAction::Assign,
            "labels" => StoryAction::Labels,
            "block" => StoryAction::Block,
            "unblock" => StoryAction::Unblock,
            "reopen" => StoryAction::Reopen,
            "archive" => StoryAction::Archive,
            "unarchive" => StoryAction::Unarchive,
            "publish" => StoryAction::Publish,
            "link-pr" => StoryAction::LinkPr,
            "unlink-pr" => StoryAction::UnlinkPr,
            _ => return None,
        })
    }
}

/// Everything served under `/api/repos/{id}/…`, once `{id}` has been split off.
///
/// The project itself is **not** resolved here — that is a store read, and this
/// module cannot reach a store. [`crate::api::rest::route`] resolves it after
/// classifying, which is what keeps `PUT /api/repos/<unknown>/data` a 404 (no
/// such project) rather than the 405 its shape suggests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectRoute<'a> {
    /// `GET .../data` — the whole board in one request.
    Data,
    /// `POST .../story`
    StoryCreate,
    /// `GET .../story/{id}`
    StoryShow { id: &'a str },
    /// `PATCH .../story/{id}`
    StoryPatch { id: &'a str },
    /// `DELETE .../story/{id}`
    StoryDelete { id: &'a str },
    /// `POST .../story/{id}/{action}`, for an action that exists.
    StoryAction { id: &'a str, action: StoryAction },
    /// `POST .../story/{id}/{action}` for an action that does not exist.
    ///
    /// A 404 rather than a 405, and **only on `POST`** — a `GET` to the same
    /// unknown action is a 405, because the path shape is routed and the method
    /// is not. The two arms look like one and are not; `tests/rest_routing.rs`
    /// pins both.
    StoryActionUnknown,
    /// `POST .../story/{id}/dispatch` — spawns an agent process.
    ///
    /// Claimed by [`crate::api::dispatch::intercept`] in the accept loop, so it
    /// never reaches the router in production. Named here anyway so
    /// `tests/route_authority.rs` can assert this module's notion of the
    /// dispatch route family agrees with
    /// [`crate::api::dispatch::is_dispatch_path`]'s own independent check —
    /// two gates disagreeing about which route spawns a process is the
    /// failure that guards against.
    Dispatch { id: &'a str },
    /// `GET .../story/{id}/dispatch/{handle}` — the poll beside it. Also
    /// answered by the accept loop; also named here for the same cross-check.
    DispatchPoll,
    /// `GET .../states`
    States,
    /// `POST .../states`
    StateCreate,
    /// `PATCH .../states` — the board's column order.
    StatesReorder,
    /// `PATCH .../states/{slug}`
    StatePatch { slug: &'a str },
    /// `DELETE .../states/{slug}`
    StateDelete { slug: &'a str },
    /// `POST .../states/{slug}/archive`
    StateArchive { slug: &'a str },
    /// `POST .../relate`
    Relate,
    /// `POST .../unrelate`
    Unrelate,
    /// A per-project path this daemon routes, with a method it does not.
    MethodNotAllowed,
    /// No such per-project path.
    NotFound,
}

/// Every route the daemon's HTTP surface can be asked for, named.
///
/// Borrowed rather than owned (`&'a str` for the captured segments) because a
/// classification is consumed in the same stack frame that produced it, and the
/// gate that runs first — [`crate::api::admission::admission`] — is on the path
/// of every single request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route<'a> {
    /// `GET /` — the single-page app itself, served so the browser has
    /// something to bootstrap from.
    Shell,
    /// `GET /api/repos`
    Repos,
    /// `POST /api/repos` — creates a project at a caller-named filesystem path.
    ReposCreate,
    /// `DELETE /api/repos/{id}` — permanently deletes a project and every story
    /// recorded against it.
    RepoDelete { id: &'a str },
    /// Anything under `/api/repos/{id}/…`.
    Project {
        id: &'a str,
        route: ProjectRoute<'a>,
    },
    /// `GET /api/events` — the live change feed.
    ///
    /// Answered in full by the accept loop, which never lets it reach the
    /// router; named here because the gate in front of the accept loop
    /// classifies it like everything else.
    Events,
    /// Anything under `/api/v1/…` — the daemon's control surface.
    ///
    /// [`crate::api::rpc::admission`] owns it entirely and
    /// [`crate::api::admission::admission`] returns before consulting this
    /// module, so no classification here can widen it. Named so this route
    /// table is a complete statement about the surface this daemon serves,
    /// rather than a statement about the part somebody remembered.
    ControlSurface,
    /// A path this daemon routes, with a method it does not.
    MethodNotAllowed,
    /// No such path.
    NotFound,
}

/// Which route `segments` and `method` name.
///
/// Pure, total, and decided from the request head alone: no store, no body, no
/// clock. [`crate::daemon::serve`]'s worker calls the gate that calls this
/// before it has read a byte of body, and an unauthenticated peer must never be
/// able to make this daemon wait on a body it has no right to send.
pub fn classify<'a>(segments: &[&'a str], method: &Method) -> Route<'a> {
    match segments {
        [] => match method {
            Method::Get => Route::Shell,
            _ => Route::MethodNotAllowed,
        },
        ["api", "v1", ..] => Route::ControlSurface,
        ["api", "events"] => match method {
            Method::Get => Route::Events,
            _ => Route::MethodNotAllowed,
        },
        ["api", "repos"] => match method {
            Method::Get => Route::Repos,
            Method::Post => Route::ReposCreate,
            _ => Route::MethodNotAllowed,
        },
        ["api", "repos", id] => match method {
            Method::Delete => Route::RepoDelete { id },
            _ => Route::MethodNotAllowed,
        },
        ["api", "repos", id, rest @ ..] => Route::Project {
            id,
            route: classify_project(rest, method),
        },
        _ => Route::NotFound,
    }
}

/// Which per-project route `rest` names — the path *after* `/api/repos/{id}`.
fn classify_project<'a>(rest: &[&'a str], method: &Method) -> ProjectRoute<'a> {
    match rest {
        ["data"] => match method {
            Method::Get => ProjectRoute::Data,
            _ => ProjectRoute::MethodNotAllowed,
        },
        ["story"] => match method {
            Method::Post => ProjectRoute::StoryCreate,
            _ => ProjectRoute::MethodNotAllowed,
        },
        ["story", id] => match method {
            Method::Get => ProjectRoute::StoryShow { id },
            Method::Patch => ProjectRoute::StoryPatch { id },
            Method::Delete => ProjectRoute::StoryDelete { id },
            _ => ProjectRoute::MethodNotAllowed,
        },
        ["story", id, "dispatch"] => match method {
            Method::Post => ProjectRoute::Dispatch { id },
            _ => ProjectRoute::MethodNotAllowed,
        },
        ["story", _id, "dispatch", _handle] => ProjectRoute::DispatchPoll,
        ["story", id, action] => match (method, StoryAction::parse(action)) {
            (Method::Post, Some(action)) => ProjectRoute::StoryAction { id, action },
            // The asymmetry, deliberately: an unknown action on a `POST` is a
            // 404, and any other method on the same path is a 405.
            (Method::Post, None) => ProjectRoute::StoryActionUnknown,
            _ => ProjectRoute::MethodNotAllowed,
        },
        ["states"] => match method {
            Method::Get => ProjectRoute::States,
            Method::Post => ProjectRoute::StateCreate,
            Method::Patch => ProjectRoute::StatesReorder,
            _ => ProjectRoute::MethodNotAllowed,
        },
        ["states", slug] => match method {
            Method::Patch => ProjectRoute::StatePatch { slug },
            Method::Delete => ProjectRoute::StateDelete { slug },
            _ => ProjectRoute::MethodNotAllowed,
        },
        ["states", slug, "archive"] => match method {
            Method::Post => ProjectRoute::StateArchive { slug },
            _ => ProjectRoute::MethodNotAllowed,
        },
        ["relate"] => match method {
            Method::Post => ProjectRoute::Relate,
            _ => ProjectRoute::MethodNotAllowed,
        },
        ["unrelate"] => match method {
            Method::Post => ProjectRoute::Unrelate,
            _ => ProjectRoute::MethodNotAllowed,
        },
        _ => ProjectRoute::NotFound,
    }
}

impl Route<'_> {
    /// This route's variant name, as declared.
    ///
    /// Exists so a test can ask *which* route a path classified to without
    /// spelling a pattern per case — and, more importantly, so
    /// `tests/route_authority.rs` can check the set of names the route table
    /// actually produces against the set of variants declared in this file's
    /// own source. A variant nobody probes is a route nobody has ever routed a
    /// real request to, however exhaustive the `match` looks.
    ///
    /// The `match` is exhaustive, so a new variant does not compile until it is
    /// named here too.
    pub fn name(&self) -> &'static str {
        match self {
            Route::Shell => "Shell",
            Route::Repos => "Repos",
            Route::ReposCreate => "ReposCreate",
            Route::RepoDelete { .. } => "RepoDelete",
            Route::Project { .. } => "Project",
            Route::Events => "Events",
            Route::ControlSurface => "ControlSurface",
            Route::MethodNotAllowed => "MethodNotAllowed",
            Route::NotFound => "NotFound",
        }
    }

    /// The per-project route inside this one, if it is one.
    pub fn project_route(&self) -> Option<&ProjectRoute<'_>> {
        match self {
            Route::Project { route, .. } => Some(route),
            _ => None,
        }
    }
}

impl ProjectRoute<'_> {
    /// This route's variant name, as declared. See [`Route::name`].
    pub fn name(&self) -> &'static str {
        match self {
            ProjectRoute::Data => "Data",
            ProjectRoute::StoryCreate => "StoryCreate",
            ProjectRoute::StoryShow { .. } => "StoryShow",
            ProjectRoute::StoryPatch { .. } => "StoryPatch",
            ProjectRoute::StoryDelete { .. } => "StoryDelete",
            ProjectRoute::StoryAction { .. } => "StoryAction",
            ProjectRoute::StoryActionUnknown => "StoryActionUnknown",
            ProjectRoute::Dispatch { .. } => "Dispatch",
            ProjectRoute::DispatchPoll => "DispatchPoll",
            ProjectRoute::States => "States",
            ProjectRoute::StateCreate => "StateCreate",
            ProjectRoute::StatesReorder => "StatesReorder",
            ProjectRoute::StatePatch { .. } => "StatePatch",
            ProjectRoute::StateDelete { .. } => "StateDelete",
            ProjectRoute::StateArchive { .. } => "StateArchive",
            ProjectRoute::Relate => "Relate",
            ProjectRoute::Unrelate => "Unrelate",
            ProjectRoute::MethodNotAllowed => "MethodNotAllowed",
            ProjectRoute::NotFound => "NotFound",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments(path: &str) -> Vec<&str> {
        path.split('/').filter(|s| !s.is_empty()).collect()
    }

    fn at<'a>(path: &'a str, method: &Method) -> Route<'a> {
        // Leaked deliberately: `classify` borrows from its input, and a test
        // helper returning a borrow of a local cannot compile otherwise.
        classify(Box::leak(segments(path).into_boxed_slice()), method)
    }

    #[test]
    fn the_shell_is_a_get_and_nothing_else() {
        assert_eq!(at("/", &Method::Get), Route::Shell);
        assert_eq!(at("/", &Method::Post), Route::MethodNotAllowed);
        assert_eq!(at("/", &Method::Head), Route::MethodNotAllowed);
    }

    #[test]
    fn a_project_is_classified_without_being_resolved() {
        // The property that keeps `PUT /api/repos/<unknown>/data` a 404: this
        // module cannot tell whether the project exists, and does not try.
        assert_eq!(
            at("/api/repos/whatever/data", &Method::Put),
            Route::Project {
                id: "whatever",
                route: ProjectRoute::MethodNotAllowed
            }
        );
    }

    #[test]
    fn an_unknown_story_action_splits_on_the_method() {
        assert_eq!(
            at("/api/repos/p/story/SH-1/nope", &Method::Post),
            Route::Project {
                id: "p",
                route: ProjectRoute::StoryActionUnknown
            }
        );
        assert_eq!(
            at("/api/repos/p/story/SH-1/nope", &Method::Get),
            Route::Project {
                id: "p",
                route: ProjectRoute::MethodNotAllowed
            }
        );
    }

    #[test]
    fn every_story_action_slug_classifies() {
        // The slugs the dashboard's own JavaScript is written against. A
        // fourteenth action added to `StoryAction` without a slug here would
        // route nowhere; this is what notices.
        for (slug, expected) in [
            ("move", StoryAction::Move),
            ("comment", StoryAction::Comment),
            ("priority", StoryAction::Priority),
            ("assign", StoryAction::Assign),
            ("labels", StoryAction::Labels),
            ("block", StoryAction::Block),
            ("unblock", StoryAction::Unblock),
            ("reopen", StoryAction::Reopen),
            ("archive", StoryAction::Archive),
            ("unarchive", StoryAction::Unarchive),
            ("publish", StoryAction::Publish),
            ("link-pr", StoryAction::LinkPr),
            ("unlink-pr", StoryAction::UnlinkPr),
        ] {
            let path = format!("/api/repos/p/story/SH-1/{slug}");
            assert_eq!(
                at(Box::leak(path.into_boxed_str()), &Method::Post),
                Route::Project {
                    id: "p",
                    route: ProjectRoute::StoryAction {
                        id: "SH-1",
                        action: expected
                    }
                },
                "{slug}"
            );
        }
    }

    #[test]
    fn the_control_surface_is_claimed_whole() {
        // Not enumerated route by route: `rpc::admission` owns every path under
        // it, and a classification that tried to be specific here would be a
        // second opinion about a surface this module does not serve.
        for path in ["/api/v1", "/api/v1/hello", "/api/v1/invoke", "/api/v1/nope"] {
            assert_eq!(at(path, &Method::Post), Route::ControlSurface, "{path}");
        }
    }

    #[test]
    fn the_dispatch_route_is_spelled_the_same_here_as_in_the_gate_that_refuses_it() {
        // `dispatch::is_dispatch_path` is [`crate::api::dispatch::intercept`]'s
        // own arity check on segments, independent of this module. Calling it
        // from here would cost the property that makes this module
        // trustworthy, that it imports nothing but a `Method` and so cannot
        // reach a store or a body.
        //
        // The two spellings therefore coexist, and this asserts they agree:
        // exactly the paths one calls the dispatch endpoint are the paths the
        // other classifies into the dispatch family. Two gates disagreeing
        // about which route spawns a process is the failure this forecloses.
        //
        // `Post` is the probe method because the poll route classifies the same
        // under any method, and a non-`Post` on the six-segment form never
        // reaches this module at all: `dispatch::intercept` claims it first.
        for path in [
            "/api/repos/p/story/SH-1/dispatch",
            "/api/repos/p/story/SH-1/dispatch/h4nd1e",
            "/api/repos/p/data",
            "/api/repos/p/story/SH-1",
            "/api/repos/p/story/SH-1/move",
            "/api/repos/p/story/SH-1/dispatch/h4nd1e/extra",
            "/api/repos",
            "/",
        ] {
            let parts = segments(path);
            let classified = at(path, &Method::Post);
            let is_dispatch_family = matches!(
                classified.project_route(),
                Some(ProjectRoute::Dispatch { .. } | ProjectRoute::DispatchPoll)
            );
            assert_eq!(
                crate::api::dispatch::is_dispatch_path(&parts),
                is_dispatch_family,
                "{path}: the dispatch endpoint's two spellings disagree"
            );
        }
    }
}
