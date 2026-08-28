//! What every route of the dashboard's REST surface answers, as one table.
//!
//! # Why this file exists
//!
//! SH-254 gives the dashboard a scoped capability instead of the daemon's
//! master token, and the scope has to be **enforced by construction**: a route
//! added to [`storyhook::api::rest::route`] must not be able to inherit a grant
//! silently. That means the router stops being a tree of nested `match`
//! expressions with `_ => 404` at every level and becomes a classified table —
//! a change to roughly thirty arms of the most security-load-bearing routing in
//! this daemon.
//!
//! A behaviour-preserving refactor of that size needs its behaviour written
//! down *first*, by somebody who has not yet moved anything. That is this file.
//! It lands before the refactor, and it must be **byte-unmodified** across it:
//! an edit to a row here during the refactor commit is the refactor rewriting
//! its own exam paper.
//!
//! # What it pins, and why these four things in particular
//!
//! SH-254's council named four ways this restructure can silently go wrong, and
//! every one of them is a row (or a group of rows) below:
//!
//! 1. **The `(Post, _) => 404` versus `_ => 405` asymmetry** on
//!    `story/{id}/{action}`. A `POST` to an unknown action is a 404; a `GET` to
//!    the same unknown action is a 405. Flattening those two arms into one is
//!    the obvious tidy-up, and it is wrong.
//! 2. **Project resolution happens before per-project routing**, so
//!    `PUT /api/repos/<unknown>/data` is a 404 (no such project) and not the 405
//!    that the shape of the path suggests. A classifier that decides the method
//!    first would flip it.
//! 3. **Every mutating route is wrapped in `guarded` or `guarded_no_body`**,
//!    which is where the CSRF/DNS-rebinding refusal (403) and the JSON
//!    content-type refusal (415) live. Dropping one wrapper while moving a
//!    handler body loses both, and the route keeps working for every well-formed
//!    caller — which is to say, for every test that only sends well-formed
//!    requests. So each mutating route is asserted three times: as the dashboard
//!    sends it, without the CSRF header, and without the content type.
//! 4. **`Routed::changing` versus `Routed::quiet`** — what a route publishes on
//!    the SSE change feed. A route that quietly becomes `quiet` stops every open
//!    browser refreshing, and no status code moves. Each row therefore declares
//!    the feed its route publishes on success, and the table checks the rule
//!    that only a *successful* *mutation* publishes at all.
//!
//! # Why it drives `route` directly rather than over HTTP
//!
//! `Routed::changed` is not observable on the wire — it is consumed by the
//! accept loop. A characterization suite that could not see it would leave
//! failure mode 4 unpinned. Calling `route` in process also keeps the table
//! honest about what it is testing: this file is about **routing**, not about
//! admission, and the token gate that runs in front of these routes in
//! production is `tests/handoff_endpoint.rs`'s and `src/api/admission.rs`'s
//! business.
//!
//! # These are observations, not preferences
//!
//! A characterization test records what the code does today so a refactor can
//! prove it still does it. Where a row looks odd — the 405 on
//! `GET /api/repos/<anything>`, the 404 on an unknown `POST` action — the row is
//! right and the oddity is real. Changing one deliberately is a behaviour
//! change, and belongs in a commit that says so.

use std::sync::Arc;

use storyhook::api::http::TrustedHosts;
use storyhook::api::rest::{Changed, route};
use storyhook::cli::parse_invocation;
use storyhook::daemon::http1::{Header, Method};
use storyhook::env::Environment;
use storyhook::invoke::{dispatch, dispatch_unscoped};
use storyhook::service::Ctx;
use storyhook::store::{ReadOps, SqliteStore, Store, WriteOps};
use storyhook_test_support::{TestEnv, project_id_at, scratch_dir};

/// One project, one story, one store — the smallest fixture every row needs.
///
/// Built fresh per row rather than shared, because the table deliberately
/// includes destructive routes (`DELETE` a project, `DELETE` a state) whose
/// success would otherwise change the answer of a later row. A shared fixture
/// would make this table order-dependent, which is the one property a
/// characterization suite must not have.
struct Fixture {
    _env: TestEnv,
    store: Arc<SqliteStore>,
    environment: Environment,
    dir: tempfile::TempDir,
    /// The project's slug — what a dashboard URL calls a repo id.
    repo: String,
}

impl Fixture {
    fn new() -> Fixture {
        let env = TestEnv::isolated();
        let dir = scratch_dir();
        let store = Arc::new(env.open_store());
        let environment = env.environment();

        dispatch_unscoped(
            &*store,
            &environment,
            dir.path(),
            "2026-01-01T00:00:00Z",
            parse_invocation(&[
                "project".to_string(),
                "new".to_string(),
                "--prefix".to_string(),
                "SH".to_string(),
                "--no-agents-md".to_string(),
            ])
            .expect("`story project new` parses"),
        )
        .expect("initializing the fixture project");

        let project =
            project_id_at(&store, dir.path()).expect("the fixture project is in the store");
        let repo = Store::read(&*store, |tx| {
            Ok(tx.project(project)?.expect("the project row").slug)
        })
        .expect("reading the fixture project");

        // One story, so `SH-1` resolves and the id-bearing routes exercise a
        // handler rather than a "no such story" short circuit.
        let ctx = Ctx::new(&*store, project, dir.path(), environment.clone()).no_hooks(true);
        dispatch(
            &ctx,
            parse_invocation(&["new".to_string(), "A story".to_string()])
                .expect("`story new` parses"),
        )
        .expect("seeding the fixture story");

        Fixture {
            _env: env,
            store,
            environment,
            dir,
            repo,
        }
    }

    /// Forgets this project's checkout, which is what a project imported into
    /// the store — or one whose directory was deleted — looks like. Every write
    /// against it is refused at the door in [`route`], because a write fires
    /// hooks and git operations in a working directory it no longer has.
    fn forget_checkout(&self) {
        let project = project_id_at(&self.store, self.dir.path()).expect("the fixture project");
        Store::write(&*self.store, |tx| tx.set_checkout_path(project, None))
            .expect("forgetting the fixture's checkout");
    }
}

/// Which headers a row sends — the three shapes that separate a route's handler
/// from its `guarded` wrapper.
#[derive(Clone, Copy, Debug)]
enum Sent {
    /// What the dashboard's own `fetch` sends: the CSRF header, a loopback
    /// `Host`, and a JSON content type.
    Dashboard,
    /// No `X-Storyhook`. Everything a cross-origin form or a naive drive-by
    /// request can manage, and what `mutation_guard_ok` exists to refuse.
    NoCsrfHeader,
    /// The CSRF header and a loopback `Host`, but no `Content-Type` — a caller
    /// past the guard and stopped by the body-shape check instead.
    NoContentType,
}

impl Sent {
    fn headers(self) -> Vec<Header> {
        let pairs: &[(&str, &str)] = match self {
            Sent::Dashboard => &[
                ("X-Storyhook", "1"),
                ("Host", "127.0.0.1:3456"),
                ("Content-Type", "application/json"),
            ],
            Sent::NoCsrfHeader => &[("Host", "127.0.0.1:3456")],
            Sent::NoContentType => &[("X-Storyhook", "1"), ("Host", "127.0.0.1:3456")],
        };
        pairs
            .iter()
            .map(|(name, value)| Header::from_bytes(*name, *value).unwrap())
            .collect()
    }
}

/// What a route publishes on the change feed **when it succeeds**.
///
/// Declared per row rather than derived, so the table pins *which* feed as well
/// as whether one fires at all — a project route that started publishing
/// `Catalog` would tell every browser to refetch the wrong thing, at the same
/// status code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Feed {
    /// This route never publishes, however well it goes.
    Silent,
    /// The set of projects moved.
    Catalog,
    /// This project's stories or configuration moved.
    Project,
}

/// One (method, path, headers, body) probe and the answer it must get.
struct Row {
    method: Method,
    /// `{repo}` is replaced with the fixture's slug.
    path: String,
    sent: Sent,
    body: &'static str,
    status: u16,
    feed: Feed,
    /// Whether this row needs a project with no checkout on this machine.
    pathless: bool,
}

/// The table's shorthand: a row as the dashboard itself would send it.
fn row(method: Method, path: &str, body: &'static str, status: u16, feed: Feed) -> Row {
    Row {
        method,
        path: path.to_string(),
        sent: Sent::Dashboard,
        body,
        status,
        feed,
        pathless: false,
    }
}

/// A row that exercises a `guarded` wrapper rather than its handler.
fn refused(method: Method, path: &str, sent: Sent, status: u16) -> Row {
    Row {
        method,
        path: path.to_string(),
        sent,
        body: "{}",
        status,
        feed: Feed::Silent,
        pathless: false,
    }
}

impl Row {
    /// Marks a row as needing a project the store knows and this machine has no
    /// copy of.
    fn without_a_checkout(mut self) -> Row {
        self.pathless = true;
        self
    }
}

/// Runs one row against a fixture of its own and reports what disagreed.
fn check(row: &Row) -> Option<String> {
    let fixture = Fixture::new();
    if row.pathless {
        fixture.forget_checkout();
    }
    let path = row.path.replace("{repo}", &fixture.repo);

    let routed = route(
        &*fixture.store,
        &fixture.environment,
        &row.method,
        &path,
        &row.sent.headers(),
        row.body,
        &TrustedHosts::default(),
    );

    let mutating = matches!(row.method, Method::Post | Method::Patch | Method::Delete);
    let succeeded = (200..300).contains(&row.status);
    let expected_changed = match (mutating && succeeded, row.feed) {
        (false, _) | (_, Feed::Silent) => None,
        (true, Feed::Catalog) => Some(Changed::Catalog),
        (true, Feed::Project) => Some(Changed::Project(fixture.repo.clone())),
    };

    let label = format!(
        "{} {}{}",
        row.method,
        path,
        match row.sent {
            Sent::Dashboard => "",
            Sent::NoCsrfHeader => " (no X-Storyhook)",
            Sent::NoContentType => " (no Content-Type)",
        }
    );
    let mut wrong = Vec::new();
    if routed.reply.status != row.status {
        wrong.push(format!(
            "status: expected {}, got {}",
            row.status, routed.reply.status
        ));
    }
    if routed.changed != expected_changed {
        wrong.push(format!(
            "changed: expected {expected_changed:?}, got {:?}",
            routed.changed
        ));
    }
    (!wrong.is_empty()).then(|| format!("{label}\n      {}", wrong.join("\n      ")))
}

/// Runs a slice of the table and fails once, listing every disagreement.
///
/// One report rather than one panic per row: a refactor that flips six routes
/// should say so in one run, not six.
fn characterize(rows: &[Row]) {
    let failures: Vec<String> = rows.iter().filter_map(check).collect();
    assert!(
        failures.is_empty(),
        "{} of {} routing rows changed:\n  - {}",
        failures.len(),
        rows.len(),
        failures.join("\n  - ")
    );
}

// --- The shell and the project catalog ---

#[test]
fn the_shell_and_the_catalog_answer_as_they_did() {
    characterize(&[
        // The single-page app itself.
        row(Method::Get, "/", "", 200, Feed::Silent),
        row(Method::Post, "/", "{}", 405, Feed::Silent),
        row(Method::Head, "/", "", 405, Feed::Silent),
        // The catalog.
        row(Method::Get, "/api/repos", "", 200, Feed::Silent),
        row(Method::Post, "/api/repos", "{}", 400, Feed::Catalog),
        refused(Method::Post, "/api/repos", Sent::NoCsrfHeader, 403),
        refused(Method::Post, "/api/repos", Sent::NoContentType, 415),
        row(Method::Patch, "/api/repos", "{}", 405, Feed::Silent),
        row(Method::Delete, "/api/repos", "{}", 405, Feed::Silent),
        // One project, by slug. Only DELETE is routed here -- everything else
        // is a 405 *whether or not the project exists*, because the shape is
        // matched before the slug is resolved.
        row(
            Method::Delete,
            "/api/repos/{repo}",
            "{}",
            409,
            Feed::Catalog,
        ),
        refused(Method::Delete, "/api/repos/{repo}", Sent::NoCsrfHeader, 403),
        refused(
            Method::Delete,
            "/api/repos/{repo}",
            Sent::NoContentType,
            415,
        ),
        row(Method::Get, "/api/repos/{repo}", "", 405, Feed::Silent),
        row(
            Method::Get,
            "/api/repos/no-such-project",
            "",
            405,
            Feed::Silent,
        ),
        row(
            Method::Delete,
            "/api/repos/no-such-project",
            "{}",
            404,
            Feed::Catalog,
        ),
        // Nothing else under /api is this module's.
        row(Method::Get, "/api", "", 404, Feed::Silent),
        row(Method::Get, "/api/nope", "", 404, Feed::Silent),
        // Answered by the accept loop long before routing, so reaching here at
        // all means something upstream stopped answering it.
        row(Method::Get, "/api/events", "", 404, Feed::Silent),
    ]);
}

#[test]
fn project_resolution_happens_before_per_project_routing() {
    // Failure mode 2. `PUT .../data` names a real route with the wrong method,
    // so the shape suggests 405 -- but the project is resolved first, and an
    // unknown one is a 404 whatever the method would have been.
    characterize(&[
        row(
            Method::Get,
            "/api/repos/no-such-project/data",
            "",
            404,
            Feed::Silent,
        ),
        row(
            Method::Put,
            "/api/repos/no-such-project/data",
            "",
            404,
            Feed::Silent,
        ),
        row(
            Method::Post,
            "/api/repos/no-such-project/story",
            "{}",
            404,
            Feed::Project,
        ),
        // And a project the store knows with no checkout on this machine: reads
        // work, every write is refused at the door with the same 422.
        row(Method::Get, "/api/repos/{repo}/data", "", 200, Feed::Silent).without_a_checkout(),
        row(
            Method::Post,
            "/api/repos/{repo}/story",
            "{}",
            422,
            Feed::Project,
        )
        .without_a_checkout(),
    ]);
}

// --- Stories ---

#[test]
fn the_story_routes_answer_as_they_did() {
    characterize(&[
        row(Method::Get, "/api/repos/{repo}/data", "", 200, Feed::Silent),
        row(
            Method::Post,
            "/api/repos/{repo}/data",
            "{}",
            405,
            Feed::Silent,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story",
            "{}",
            400,
            Feed::Project,
        ),
        refused(
            Method::Post,
            "/api/repos/{repo}/story",
            Sent::NoCsrfHeader,
            403,
        ),
        refused(
            Method::Post,
            "/api/repos/{repo}/story",
            Sent::NoContentType,
            415,
        ),
        row(
            Method::Get,
            "/api/repos/{repo}/story",
            "",
            405,
            Feed::Silent,
        ),
        row(
            Method::Get,
            "/api/repos/{repo}/story/SH-1",
            "",
            200,
            Feed::Silent,
        ),
        row(
            Method::Get,
            "/api/repos/{repo}/story/SH-999",
            "",
            404,
            Feed::Silent,
        ),
        // An empty edit is a 400 from `set_fields`, not a no-op 200: the
        // handler was reached, which is what the wrapper rows below cannot
        // show on their own.
        row(
            Method::Patch,
            "/api/repos/{repo}/story/SH-1",
            "{}",
            400,
            Feed::Project,
        ),
        row(
            Method::Patch,
            "/api/repos/{repo}/story/SH-1",
            "{\"title\": \"Renamed\"}",
            200,
            Feed::Project,
        ),
        refused(
            Method::Patch,
            "/api/repos/{repo}/story/SH-1",
            Sent::NoCsrfHeader,
            403,
        ),
        refused(
            Method::Patch,
            "/api/repos/{repo}/story/SH-1",
            Sent::NoContentType,
            415,
        ),
        row(
            Method::Delete,
            "/api/repos/{repo}/story/SH-1",
            "{}",
            409,
            Feed::Project,
        ),
        refused(
            Method::Delete,
            "/api/repos/{repo}/story/SH-1",
            Sent::NoCsrfHeader,
            403,
        ),
        refused(
            Method::Delete,
            "/api/repos/{repo}/story/SH-1",
            Sent::NoContentType,
            415,
        ),
        row(
            Method::Put,
            "/api/repos/{repo}/story/SH-1",
            "",
            405,
            Feed::Silent,
        ),
    ]);
}

#[test]
fn every_story_action_answers_as_it_did() {
    // The thirteen actions, each as the dashboard sends it and each without the
    // CSRF header. `{}` is enough to reach every handler: the ones with a
    // required field answer 400 from their own validation, which is proof the
    // wrapper let them through, and the ones with no required field run.
    characterize(&[
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/move",
            "{}",
            400,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/move",
            "{\"state\": \"in-progress\"}",
            200,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/comment",
            "{}",
            400,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/comment",
            "{\"text\": \"a comment\"}",
            200,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/priority",
            "{}",
            400,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/priority",
            "{\"priority\": \"high\"}",
            200,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/assign",
            "{}",
            400,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/labels",
            "{}",
            400,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/labels",
            "{\"add\": [\"web\"]}",
            200,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/block",
            "{}",
            400,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/block",
            "{\"reason\": \"waiting\"}",
            200,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/unblock",
            "",
            200,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/publish",
            "",
            200,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/link-pr",
            "{}",
            400,
            Feed::Project,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/unlink-pr",
            "{}",
            400,
            Feed::Project,
        ),
    ]);
}

#[test]
fn every_story_action_keeps_its_guard() {
    // Failure mode 3, one row per wrapper. The four `guarded_no_body` actions
    // have no content-type check, which is itself part of the characterization:
    // adding one to them would be a behaviour change.
    let with_body = [
        "move",
        "comment",
        "priority",
        "assign",
        "labels",
        "block",
        "reopen",
        "link-pr",
        "unlink-pr",
    ];
    let without_body = ["unblock", "archive", "unarchive", "publish"];
    let mut rows = Vec::new();
    for action in with_body {
        let path = format!("/api/repos/{{repo}}/story/SH-1/{action}");
        rows.push(refused(Method::Post, &path, Sent::NoCsrfHeader, 403));
        rows.push(refused(Method::Post, &path, Sent::NoContentType, 415));
    }
    for action in without_body {
        let path = format!("/api/repos/{{repo}}/story/SH-1/{action}");
        rows.push(refused(Method::Post, &path, Sent::NoCsrfHeader, 403));
    }
    characterize(&rows);
}

#[test]
fn an_unknown_story_action_is_a_404_on_post_and_a_405_on_everything_else() {
    // Failure mode 1, the asymmetry. Two arms that look like one.
    characterize(&[
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/no-such-action",
            "{}",
            404,
            Feed::Project,
        ),
        row(
            Method::Get,
            "/api/repos/{repo}/story/SH-1/no-such-action",
            "",
            405,
            Feed::Silent,
        ),
        row(
            Method::Get,
            "/api/repos/{repo}/story/SH-1/move",
            "",
            405,
            Feed::Silent,
        ),
        // The dispatch endpoint is answered by the accept loop, so reaching
        // this module means it stopped being intercepted -- and here it is
        // just another unknown action.
        row(
            Method::Post,
            "/api/repos/{repo}/story/SH-1/dispatch",
            "{}",
            404,
            Feed::Project,
        ),
        row(
            Method::Get,
            "/api/repos/{repo}/story/SH-1/dispatch/h4nd1e",
            "",
            404,
            Feed::Silent,
        ),
    ]);
}

// --- States and relations ---

#[test]
fn the_state_routes_answer_as_they_did() {
    characterize(&[
        row(
            Method::Get,
            "/api/repos/{repo}/states",
            "",
            200,
            Feed::Silent,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/states",
            "{}",
            400,
            Feed::Project,
        ),
        refused(
            Method::Post,
            "/api/repos/{repo}/states",
            Sent::NoCsrfHeader,
            403,
        ),
        refused(
            Method::Post,
            "/api/repos/{repo}/states",
            Sent::NoContentType,
            415,
        ),
        row(
            Method::Patch,
            "/api/repos/{repo}/states",
            "{}",
            400,
            Feed::Project,
        ),
        refused(
            Method::Patch,
            "/api/repos/{repo}/states",
            Sent::NoCsrfHeader,
            403,
        ),
        refused(
            Method::Patch,
            "/api/repos/{repo}/states",
            Sent::NoContentType,
            415,
        ),
        row(
            Method::Delete,
            "/api/repos/{repo}/states",
            "{}",
            405,
            Feed::Silent,
        ),
        // As with a story PATCH, an edit that names no field is a 400.
        row(
            Method::Patch,
            "/api/repos/{repo}/states/todo",
            "{}",
            400,
            Feed::Project,
        ),
        row(
            Method::Patch,
            "/api/repos/{repo}/states/todo",
            "{\"description\": \"Not started\"}",
            200,
            Feed::Project,
        ),
        refused(
            Method::Patch,
            "/api/repos/{repo}/states/todo",
            Sent::NoCsrfHeader,
            403,
        ),
        refused(
            Method::Patch,
            "/api/repos/{repo}/states/todo",
            Sent::NoContentType,
            415,
        ),
        refused(
            Method::Delete,
            "/api/repos/{repo}/states/todo",
            Sent::NoCsrfHeader,
            403,
        ),
        refused(
            Method::Delete,
            "/api/repos/{repo}/states/todo",
            Sent::NoContentType,
            415,
        ),
        row(
            Method::Get,
            "/api/repos/{repo}/states/todo",
            "",
            405,
            Feed::Silent,
        ),
        refused(
            Method::Post,
            "/api/repos/{repo}/states/todo/archive",
            Sent::NoCsrfHeader,
            403,
        ),
        refused(
            Method::Post,
            "/api/repos/{repo}/states/todo/archive",
            Sent::NoContentType,
            415,
        ),
        row(
            Method::Get,
            "/api/repos/{repo}/states/todo/archive",
            "",
            405,
            Feed::Silent,
        ),
    ]);
}

#[test]
fn the_relation_routes_answer_as_they_did() {
    characterize(&[
        row(
            Method::Post,
            "/api/repos/{repo}/relate",
            "{}",
            400,
            Feed::Project,
        ),
        refused(
            Method::Post,
            "/api/repos/{repo}/relate",
            Sent::NoCsrfHeader,
            403,
        ),
        refused(
            Method::Post,
            "/api/repos/{repo}/relate",
            Sent::NoContentType,
            415,
        ),
        row(
            Method::Get,
            "/api/repos/{repo}/relate",
            "",
            405,
            Feed::Silent,
        ),
        row(
            Method::Post,
            "/api/repos/{repo}/unrelate",
            "{}",
            400,
            Feed::Project,
        ),
        refused(
            Method::Post,
            "/api/repos/{repo}/unrelate",
            Sent::NoCsrfHeader,
            403,
        ),
        refused(
            Method::Post,
            "/api/repos/{repo}/unrelate",
            Sent::NoContentType,
            415,
        ),
        row(
            Method::Get,
            "/api/repos/{repo}/unrelate",
            "",
            405,
            Feed::Silent,
        ),
        // An unknown per-project path, on a method that routes and on one that
        // does not: both 404, because the sub-path is matched before the method.
        row(Method::Get, "/api/repos/{repo}/nope", "", 404, Feed::Silent),
        row(
            Method::Post,
            "/api/repos/{repo}/nope",
            "{}",
            404,
            Feed::Project,
        ),
    ]);
}

/// The fixture itself is a claim: `SH-1` must exist, or half this table would
/// be characterizing "no such story" instead of the routes it names.
#[test]
fn the_fixture_holds_the_story_the_table_names() {
    let fixture = Fixture::new();
    let path = format!("/api/repos/{}/story/SH-1", fixture.repo);
    let routed = route(
        &*fixture.store,
        &fixture.environment,
        &Method::Get,
        &path,
        &Sent::Dashboard.headers(),
        "",
        &TrustedHosts::default(),
    );
    assert_eq!(
        routed.reply.status, 200,
        "the fixture must hold SH-1, or the id-bearing rows prove nothing"
    );
}
