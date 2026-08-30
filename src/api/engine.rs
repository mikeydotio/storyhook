//! The dashboard's Full Auto engine control surface (SH-468).
//!
//! These routes are intercepted in [`crate::daemon::serve::worker`] before a
//! store-pool `Job` is built. An immediate stop can invoke `story.sh unclaim`,
//! which calls back into this daemon over `/api/v1/invoke`; occupying one of
//! the fixed store dispatchers while waiting for that child would reproduce
//! the deadlock [`crate::api::dispatch`] exists to avoid.

use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::api::admission::named_token_ok;
use crate::api::dispatch::{DispatchAgent, resolve_dispatch_script};
use crate::api::http::{
    Reply, TrustedHosts, content_type_is_json, error_reply, json_reply, mutation_guard_ok,
    text_reply,
};
use crate::api::rpc::token_ok;
use crate::api::tokens::TokenRegistry;
use crate::daemon::bus::{Change, ChangeBus};
use crate::daemon::http1::{Header, Method};
use crate::env::Environment;
use crate::error::AppError;
use crate::output::render_error;
use crate::service::Ctx;
use crate::service::engine::{
    DispatchOutcome, DispatchRequest, Dispatcher, EngineService, RunView, ShellDispatcher,
    StartRequest, UnclaimRequest,
};
use crate::store::{
    EngineAgent, EngineLaneRecord, EngineLaneState, EngineRunRecord, EngineScope, ReadOps,
    SqliteStore, Store,
};

/// One persistent store handle for engine requests, shared by every worker.
///
/// The ordinary REST pool remains borrowed by `Serving`; this independently
/// opened handle is owned, so the `'static` per-connection workers can use it
/// without moving engine controls back onto the fixed dispatcher pool.
pub(crate) struct EngineController {
    store: SqliteStore,
    env: Environment,
}

impl EngineController {
    pub(crate) fn open(env: &Environment) -> Result<Self, AppError> {
        Ok(Self {
            store: crate::invoke::open_store(env)?,
            env: env.clone(),
        })
    }

    fn context(&self, slug: &str) -> Result<Ctx<'_, SqliteStore>, AppError> {
        let (project, checkout) = self.store.read(|tx| {
            let project = tx.project_by_slug(slug)?.ok_or_else(|| {
                crate::store::StoreError::NotFound(format!("project `{slug}` not found"))
            })?;
            let checkout = tx.checkout_path(project.id)?;
            Ok((project.id, checkout))
        })?;
        let cwd = checkout.unwrap_or_else(|| self.env.home().to_path_buf());
        Ok(Ctx::new(&self.store, project, cwd, self.env.clone()).no_hooks(true))
    }

    fn start(&self, project: &str, body: &str) -> Result<RunView, AppError> {
        let request: StartBody = parse_body(body, "engine start")?;
        let ctx = self.context(project)?;
        let dispatcher = NoopDispatcher;
        let service = EngineService::new(&ctx, &dispatcher);
        let run = service.start(StartRequest {
            scope: request.epic.map_or(EngineScope::Project, EngineScope::Epic),
            lanes: request.lanes,
            agent: request.agent.into(),
        })?;
        service
            .status(Some(&run.id))?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::NotFound(format!("engine run `{}` not found", run.id)))
    }

    fn status(&self, project: &str, run: Option<&str>) -> Result<Vec<RunView>, AppError> {
        let ctx = self.context(project)?;
        let run = run.map(str::to_string);
        EngineService::new(&ctx, &NoopDispatcher).status(run.as_ref())
    }

    fn action(&self, project: &str, action: EngineAction, body: &str) -> Result<RunView, AppError> {
        match action {
            EngineAction::Stop => {
                let request: StopBody = parse_body(body, "engine stop")?;
                let ctx = self.context(project)?;
                if !request.now {
                    return EngineService::new(&ctx, &NoopDispatcher).stop(&request.run, false);
                }

                let current = EngineService::new(&ctx, &NoopDispatcher)
                    .status(Some(&request.run))?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        AppError::NotFound(format!("engine run `{}` not found", request.run))
                    })?;
                let needs_helper = current.lanes.iter().any(|lane| {
                    !matches!(
                        lane.state,
                        EngineLaneState::Idle | EngineLaneState::Quarantined
                    )
                });
                if !needs_helper {
                    return EngineService::new(&ctx, &NoopDispatcher).stop(&request.run, true);
                }

                let script =
                    resolve_dispatch_script(current.run.agent.into()).map_err(AppError::Storage)?;
                let dispatcher = ShellDispatcher::new(script, self.env.clone());
                EngineService::new(&ctx, &dispatcher).stop(&request.run, true)
            }
            EngineAction::Pause | EngineAction::Resume | EngineAction::Ack => {
                let request: ActionBody = parse_body(body, action.label())?;
                let ctx = self.context(project)?;
                let service = EngineService::new(&ctx, &NoopDispatcher);
                match action {
                    EngineAction::Pause => service.pause(&request.run),
                    EngineAction::Resume => service.resume(&request.run),
                    EngineAction::Ack => service.acknowledge(&request.run),
                    EngineAction::Stop => unreachable!("handled above"),
                }
            }
        }
    }
}

/// The four actions accepted after `/engine/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngineAction {
    Pause,
    Resume,
    Stop,
    Ack,
}

impl EngineAction {
    fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "pause" => Self::Pause,
            "resume" => Self::Resume,
            "stop" => Self::Stop,
            "ack" => Self::Ack,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pause => "engine pause",
            Self::Resume => "engine resume",
            Self::Stop => "engine stop",
            Self::Ack => "engine ack",
        }
    }
}

/// Whether this path belongs to the engine interceptor, independent of method.
pub(crate) fn is_engine_path(segments: &[&str]) -> bool {
    matches!(segments, ["api", "repos", _, "engine"])
        || matches!(segments, ["api", "repos", _, "engine", _])
}

/// Handles one engine request, or returns `None` when the path belongs to a
/// different API family.
#[allow(clippy::too_many_arguments)]
pub(crate) fn intercept(
    segments: &[&str],
    method: &Method,
    query: Option<&str>,
    headers: &[Header],
    body: &str,
    trusted_hosts: &TrustedHosts,
    token: &str,
    controller: &Arc<EngineController>,
    bus: &ChangeBus,
    tokens: &TokenRegistry,
    cookie_name: &str,
    wall_now: DateTime<Utc>,
) -> Option<Reply> {
    if !is_engine_path(segments) {
        return None;
    }
    if !valid_segment(segments[2]) {
        return Some(text_reply(404, "Not found"));
    }
    if crate::api::rest::mutating(method) && !mutation_guard_ok(headers, trusted_hosts) {
        return Some(text_reply(403, "Forbidden"));
    }
    if !token_ok(headers, token)
        && !named_token_ok(
            headers,
            method,
            cookie_name,
            tokens,
            wall_now,
            Instant::now(),
        )
    {
        return Some(text_reply(
            401,
            "storyhook daemon: missing or invalid token",
        ));
    }

    let project = segments[2];
    let reply = match (method, segments) {
        (Method::Get, ["api", "repos", _, "engine"]) => {
            let run = match parse_status_query(query) {
                Ok(run) => run,
                Err(reply) => return Some(reply),
            };
            match controller.status(project, run.as_deref()) {
                Ok(runs) => success_reply(200, RunsEnvelope::new(runs)).no_cache(),
                Err(error) => error_reply(&error),
            }
        }
        (Method::Post, ["api", "repos", _, "engine"]) => {
            if !content_type_is_json(headers) {
                return Some(text_reply(415, "Content-Type must be application/json"));
            }
            match controller.start(project, body) {
                Ok(run) => success_reply(201, RunEnvelope::new(run)),
                Err(error) if duplicate_live_run(&error) => {
                    json_reply(409, render_error(&error, true))
                }
                Err(error) => error_reply(&error),
            }
        }
        (Method::Post, ["api", "repos", _, "engine", action]) => {
            let Some(action) = EngineAction::parse(action) else {
                return Some(text_reply(404, "Not found"));
            };
            if !content_type_is_json(headers) {
                return Some(text_reply(415, "Content-Type must be application/json"));
            }
            match controller.action(project, action, body) {
                Ok(run) => success_reply(200, RunEnvelope::new(run)),
                Err(error) => error_reply(&error),
            }
        }
        (_, ["api", "repos", _, "engine"]) | (_, ["api", "repos", _, "engine", _]) => {
            text_reply(405, "Method Not Allowed")
        }
        _ => unreachable!("is_engine_path checked the exact arities"),
    };

    publish_success(method, project, reply.status, bus);
    Some(reply)
}

fn publish_success(method: &Method, project: &str, status: u16, bus: &ChangeBus) {
    if matches!(method, Method::Post) && (200..300).contains(&status) {
        bus.publish(Change::Project(project.to_string()));
    }
}

fn parse_status_query(query: Option<&str>) -> Result<Option<String>, Reply> {
    let Some(query) = query else {
        return Ok(None);
    };
    let pairs = query.split('&').collect::<Vec<_>>();
    if pairs.len() != 1 {
        return Err(text_reply(
            400,
            "engine status accepts only one `run` query",
        ));
    }
    let Some(("run", value)) = pairs[0].split_once('=') else {
        return Err(text_reply(400, "engine status accepts only `?run=<id>`"));
    };
    if !valid_segment(value) {
        return Err(text_reply(400, "engine status needs a valid run id"));
    }
    Ok(Some(value.to_string()))
}

fn valid_segment(raw: &str) -> bool {
    let mut chars = raw.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn duplicate_live_run(error: &AppError) -> bool {
    matches!(error, AppError::Validation(detail) if detail.contains("already has a live engine run"))
}

fn parse_body<T: for<'de> Deserialize<'de>>(body: &str, action: &str) -> Result<T, AppError> {
    serde_json::from_str(body)
        .map_err(|error| AppError::Usage(format!("{action}: invalid JSON body: {error}")))
}

fn success_reply<T: Serialize>(status: u16, value: T) -> Reply {
    json_reply(
        status,
        serde_json::to_string(&value).expect("engine HTTP DTOs are infallible to serialize"),
    )
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum AgentBody {
    #[default]
    Claude,
    Codex,
}

impl From<AgentBody> for EngineAgent {
    fn from(value: AgentBody) -> Self {
        match value {
            AgentBody::Claude => Self::Claude,
            AgentBody::Codex => Self::Codex,
        }
    }
}

impl From<EngineAgent> for DispatchAgent {
    fn from(value: EngineAgent) -> Self {
        match value {
            EngineAgent::Claude => Self::Claude,
            EngineAgent::Codex => Self::Codex,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartBody {
    #[serde(default)]
    epic: Option<String>,
    #[serde(default = "default_lanes")]
    lanes: u32,
    #[serde(default)]
    agent: AgentBody,
}

const fn default_lanes() -> u32 {
    1
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionBody {
    run: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopBody {
    run: String,
    #[serde(default)]
    now: bool,
}

#[derive(Serialize)]
struct RunEnvelope {
    result: &'static str,
    run: HttpRunView,
}

impl RunEnvelope {
    fn new(run: RunView) -> Self {
        Self {
            result: "ok",
            run: run.into(),
        }
    }
}

#[derive(Serialize)]
struct RunsEnvelope {
    result: &'static str,
    runs: Vec<HttpRunView>,
}

impl RunsEnvelope {
    fn new(runs: Vec<RunView>) -> Self {
        Self {
            result: "ok",
            runs: runs.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Serialize)]
struct HttpRunView {
    id: String,
    project: String,
    scope: HttpScope,
    lane_count: u32,
    agent: &'static str,
    state: &'static str,
    lanes: Vec<HttpLaneView>,
    consecutive_hard_stops: u32,
    stop_reason: Option<String>,
    acknowledged_at: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<RunView> for HttpRunView {
    fn from(value: RunView) -> Self {
        let EngineRunRecord {
            id,
            project_slug,
            scope,
            lanes,
            agent,
            state,
            consecutive_hard_stops,
            stop_reason,
            acknowledged_at,
            created_at,
            updated_at,
        } = value.run;
        Self {
            id,
            project: project_slug,
            scope: scope.into(),
            lane_count: lanes,
            agent: agent.as_str(),
            state: state.as_str(),
            lanes: value.lanes.into_iter().map(Into::into).collect(),
            consecutive_hard_stops,
            stop_reason,
            acknowledged_at,
            created_at,
            updated_at,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum HttpScope {
    Project,
    Epic { story: String },
}

impl From<EngineScope> for HttpScope {
    fn from(value: EngineScope) -> Self {
        match value {
            EngineScope::Project => Self::Project,
            EngineScope::Epic(story) => Self::Epic { story },
        }
    }
}

#[derive(Serialize)]
struct HttpLaneView {
    index: u32,
    state: &'static str,
    story: Option<String>,
    window: Option<String>,
    worktree: Option<String>,
    dispatched_at: Option<String>,
    last_observed_at: String,
    outcome: Option<String>,
    outcome_detail: Option<String>,
}

impl From<EngineLaneRecord> for HttpLaneView {
    fn from(value: EngineLaneRecord) -> Self {
        Self {
            index: value.lane_index,
            state: value.state.as_str(),
            story: value.story_id,
            window: value.window_name,
            worktree: value.worktree_path,
            dispatched_at: value.dispatched_at,
            last_observed_at: value.last_observed_at,
            outcome: value.outcome,
            outcome_detail: value.outcome_detail,
        }
    }
}

/// Service methods that do not touch the process boundary still receive a
/// Dispatcher by type. Any accidental call is a loud internal error.
struct NoopDispatcher;

impl Dispatcher for NoopDispatcher {
    fn dispatch(&self, _request: DispatchRequest) -> Result<DispatchOutcome, AppError> {
        Err(AppError::Storage(
            "engine HTTP reached dispatch without a shell dispatcher".to_string(),
        ))
    }

    fn unclaim(&self, _request: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
        Err(AppError::Storage(
            "engine HTTP reached unclaim without a shell dispatcher".to_string(),
        ))
    }

    fn window_alive(&self, _window: &str) -> bool {
        false
    }

    fn kill_window(&self, _window: &str) -> Result<(), AppError> {
        Err(AppError::Storage(
            "engine HTTP reached kill-window without a shell dispatcher".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Vec<Header> {
        pairs
            .iter()
            .map(|(name, value)| Header::from_bytes(*name, *value).unwrap())
            .collect()
    }

    #[test]
    fn engine_paths_are_exact() {
        assert!(is_engine_path(&["api", "repos", "p", "engine"]));
        assert!(is_engine_path(&["api", "repos", "p", "engine", "pause"]));
        assert!(!is_engine_path(&[
            "api", "repos", "p", "engine", "pause", "extra"
        ]));
        assert!(!is_engine_path(&["api", "repos", "p", "story"]));
    }

    #[test]
    fn duplicate_live_run_detection_is_narrow() {
        assert!(duplicate_live_run(&AppError::Validation(
            "project `p` already has a live engine run".to_string()
        )));
        assert!(!duplicate_live_run(&AppError::Validation(
            "some other validation".to_string()
        )));
    }

    #[test]
    fn status_query_refuses_ambiguity() {
        assert_eq!(parse_status_query(None).unwrap(), None);
        assert_eq!(
            parse_status_query(Some("run=abc")).unwrap(),
            Some("abc".into())
        );
        assert_eq!(
            parse_status_query(Some("run=a&run=b")).unwrap_err().status,
            400
        );
        assert_eq!(parse_status_query(Some("other=a")).unwrap_err().status, 400);
    }

    #[test]
    fn the_direct_gate_keeps_mutation_guard_before_token() {
        // `storyhook-test-support` is a *dev*-dependency that itself depends
        // on `storyhook` (Cargo.toml's header comment), so its own compiled
        // `storyhook::env::Environment` is a DIFFERENT type from this crate's
        // — `TestEnv::isolated().environment()` returned one, and passing it
        // to `EngineController::open` (expecting this compilation's own
        // `Environment`) is what actually produced the E0308 "multiple
        // different versions of crate `storyhook`" error, not a real API
        // mismatch. `scratch_dir()` sidesteps this because `tempfile::TempDir`
        // is a third-party type neither compilation defines; `Environment::at`
        // is the in-crate constructor every other unit test in this file's
        // position already uses for exactly that reason.
        let scratch = storyhook_test_support::scratch_dir();
        let env = Environment::at(scratch.path());
        let controller = Arc::new(EngineController::open(&env).unwrap());
        let now = Instant::now();
        let tokens = TokenRegistry::new(Utc::now(), now);
        let reply = intercept(
            &["api", "repos", "p", "engine"],
            &Method::Post,
            None,
            &headers(&[]),
            "{}",
            &TrustedHosts::default(),
            "token",
            &controller,
            &ChangeBus::new(),
            &tokens,
            "storyhook_test",
            Utc::now(),
        )
        .unwrap();
        assert_eq!(reply.status, 403);

        let reply = intercept(
            &["api", "repos", "p", "engine"],
            &Method::Post,
            None,
            &headers(&[("X-Storyhook", "1"), ("Host", "127.0.0.1")]),
            "{}",
            &TrustedHosts::default(),
            "token",
            &controller,
            &ChangeBus::new(),
            &tokens,
            "storyhook_test",
            Utc::now(),
        )
        .unwrap();
        assert_eq!(reply.status, 401);
    }

    #[test]
    fn only_a_successful_mutation_publishes() {
        let bus = ChangeBus::new();
        let subscription = bus.subscribe();

        publish_success(&Method::Get, "p", 200, &bus);
        publish_success(&Method::Post, "p", 422, &bus);
        assert_eq!(subscription.recv(std::time::Duration::ZERO), None);

        publish_success(&Method::Post, "p", 200, &bus);
        assert_eq!(
            subscription.recv(std::time::Duration::ZERO),
            Some(Change::Project("p".to_string()))
        );
    }
}
