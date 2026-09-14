//! Authenticated asynchronous reset operations, off the store dispatcher pool.
use crate::api::http::{
    Reply, TrustedHosts, content_type_is_json, error_reply, json_reply, text_reply,
};
use crate::daemon::http1::{Header, Method};
use crate::daemon::verification::VerificationActivity;
use crate::env::Environment;
use crate::error::AppError;
use crate::service::{Ctx, story_reset::StoryResetService};
use crate::store::{ReadOps, SqliteStore, Store, StoryReset};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Owns active reset workers; durable reservations survive this controller.
pub(crate) struct ResetController {
    store: SqliteStore,
    env: Environment,
    activity: VerificationActivity,
    inflight: Arc<crate::daemon::lifecycle::InFlight>,
    running: Mutex<HashSet<String>>,
}

impl ResetController {
    /// Shares verifier ownership and shutdown supervision with the daemon.
    pub(crate) fn open(
        env: &Environment,
        activity: VerificationActivity,
        inflight: Arc<crate::daemon::lifecycle::InFlight>,
    ) -> Result<Self, AppError> {
        Ok(Self {
            store: crate::invoke::open_store(env)?,
            env: env.clone(),
            activity,
            inflight,
            running: Mutex::new(HashSet::new()),
        })
    }

    fn context(&self, slug: &str) -> Result<Ctx<'_, SqliteStore>, AppError> {
        let project = self.store.read(|tx| {
            tx.project_by_slug(slug)?
                .ok_or_else(|| crate::store::StoreError::NotFound(format!("project {slug}")))
        })?;
        Ok(Ctx::new(
            &self.store,
            project.id,
            self.env.home().to_path_buf(),
            self.env.clone(),
        )
        .no_hooks(true))
    }

    fn envelope(&self, reset: &StoryReset) -> serde_json::Value {
        let running = self
            .running
            .lock()
            .expect("reset registry")
            .contains(&reset.token);
        serde_json::json!({"result":"ok", "reset": {
            "handle":reset.token, "story":reset.story_id,
            "state":if reset.completed { "ok" } else if running { "running" } else { "error" },
            "detail":reset.failure.clone().or_else(|| (!reset.completed && !running).then(|| "Reset was interrupted. Retry Reset to finish the same cleanup operation.".into()))
        }})
    }

    fn start(
        self: &Arc<Self>,
        project: &str,
        id: &str,
        body: &str,
        dispatch: &Arc<super::dispatch::DispatchRegistry>,
        bus: &crate::daemon::bus::ChangeBus,
    ) -> Result<Reply, AppError> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            confirmation: String,
        }
        let request: Request = serde_json::from_str(body)
            .map_err(|e| AppError::Validation(format!("invalid reset request: {e}")))?;
        let ctx = self.context(project)?;
        let mut running = self.running.lock().expect("reset registry");
        if running.len() >= super::dispatch::MAX_RUNNING {
            let duplicate = self.store.read(|tx| {
                let prefix = crate::service::project_prefix(tx, ctx.project())?;
                let no = crate::store::StoryNo::parse_id(&prefix, id)
                    .map_err(|_| crate::store::StoreError::NotFound(format!("story {id}")))?;
                Ok(tx
                    .story_reset(ctx.project(), no)?
                    .is_some_and(|reset| !reset.completed && running.contains(&reset.token)))
            })?;
            if !duplicate {
                return Ok(text_reply(
                    429,
                    "Too many resets are running; retry when one finishes",
                ));
            }
        }
        let reset = StoryResetService::new(&ctx).reserve(id, &request.confirmation)?;
        let start = running.insert(reset.token.clone());
        drop(running);
        if start {
            let controller = Arc::clone(self);
            let dispatch = Arc::clone(dispatch);
            let project = project.to_string();
            let target = reset.clone();
            let bus = bus.clone();
            let name = format!("reset-{}", reset.token);
            if let Err(error) = std::thread::Builder::new().name(name).spawn(move || {
                let entry = controller.inflight.enter();
                entry.name(crate::daemon::lifecycle::CurrentRequest {
                    request_id: target.token.clone(), command: "story-reset".into(), project: Some(project.clone()), pid: std::process::id(), started_at: controller.env.now(), served_deadline_secs: 600, cwd: controller.env.home().to_path_buf(),
                });
                let result = (|| {
                    let ctx = controller.context(&project)?;
                    let service = StoryResetService::new(&ctx);
                    service.execute(&target.story_id, &target.token, || {
                        let deadline = Instant::now() + crate::service::engine::DISPATCH_TIMEOUT;
                        loop {
                            let engine_dispatching = controller.store.read(|tx| {
                                for owner in &target.lanes {
                                    if tx.engine_lanes(&owner.run_id)?.iter().any(|lane| lane.lane_index == owner.lane_index && lane.story_id.as_deref() == Some(&target.story_id) && lane.state == crate::store::EngineLaneState::Dispatching) { return Ok(true); }
                                }
                                Ok(false)
                            })?;
                            if dispatch.running_handle(&target.story_id).is_none() && !engine_dispatching { break; }
                            if Instant::now() >= deadline { return Err(AppError::Validation("Reset is waiting for an existing dispatch to finish; retry reset".into())); }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        controller.activity.cancel_story_and_wait(target.project, &target.story_id, deadline)
                    })
                })();
                if let Err(error) = result { crate::daemon::activity::emit("ERROR", "reset", "event", &target.story_id, &error.to_string()); }
                controller.running.lock().expect("reset registry").remove(&target.token);
                bus.publish(crate::daemon::bus::Change::Project(project));
            }) {
                self.running.lock().expect("reset registry").remove(&reset.token);
                return Err(AppError::Storage(format!("starting reset worker: {error}")));
            }
        }
        Ok(json_reply(202, self.envelope(&reset).to_string()).no_cache())
    }
}

/// Handles reset requests after standard admission and body acquisition.
#[allow(clippy::too_many_arguments)]
pub(crate) fn intercept(
    segments: &[&str],
    method: &Method,
    headers: &[Header],
    body: &str,
    trusted_hosts: &TrustedHosts,
    token: &str,
    controller: &Arc<ResetController>,
    dispatch: &Arc<super::dispatch::DispatchRegistry>,
    bus: &crate::daemon::bus::ChangeBus,
    tokens: &super::tokens::TokenRegistry,
    cookie_name: &str,
) -> Option<Reply> {
    if !matches!(
        segments,
        ["api", "repos", _, "story", _, "reset"] | ["api", "repos", _, "story", _, "reset", _]
    ) {
        return None;
    }
    if let Some(reply) = super::admission::admission(
        segments,
        method,
        headers,
        trusted_hosts,
        token,
        Instant::now(),
        tokens,
        cookie_name,
        chrono::Utc::now(),
    ) {
        return Some(reply);
    }
    let result = match (method, segments) {
        (Method::Post, ["api", "repos", project, "story", id, "reset"]) => {
            if !content_type_is_json(headers) {
                return Some(text_reply(415, "Content-Type must be application/json"));
            }
            controller.start(project, id, body, dispatch, bus)
        }
        (Method::Get, ["api", "repos", project, "story", id, "reset", handle]) => (|| {
            let ctx = controller.context(project)?;
            let reset = StoryResetService::new(&ctx).get(id, handle)?;
            Ok(json_reply(200, controller.envelope(&reset).to_string()).no_cache())
        })(),
        _ => Ok(text_reply(405, "Method not allowed")),
    };
    Some(result.unwrap_or_else(|error| error_reply(&error)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_capacity_duplicates_reuse_the_handle_and_restart_reports_interruption() {
        let fixture = storyhook_test_support::ServiceFixture::new();
        let env = Environment::at(fixture.env().home());
        let controller = Arc::new(
            ResetController::open(
                &env,
                VerificationActivity::default(),
                Arc::new(crate::daemon::lifecycle::InFlight::new(env.clone())),
            )
            .unwrap(),
        );
        let ctx = controller.context("fixture").unwrap();
        let story = crate::service::StoryService::new(&ctx)
            .create(&crate::service::NewStoryInput {
                title: "Duplicate reset".into(),
                ..Default::default()
            })
            .unwrap();
        let reset = StoryResetService::new(&ctx)
            .reserve(&story.id, &story.id)
            .unwrap();
        assert_eq!(controller.envelope(&reset)["reset"]["state"], "error");
        assert!(
            controller.envelope(&reset)["reset"]["detail"]
                .as_str()
                .unwrap()
                .contains("interrupted")
        );
        {
            let mut running = controller.running.lock().unwrap();
            running.insert(reset.token.clone());
            for index in 1..super::super::dispatch::MAX_RUNNING {
                running.insert(format!("other-{index}"));
            }
        }
        let reply = controller
            .start(
                "fixture",
                &story.id,
                &serde_json::json!({"confirmation":story.id}).to_string(),
                &Arc::new(super::super::dispatch::DispatchRegistry::new()),
                &crate::daemon::bus::ChangeBus::new(),
            )
            .unwrap();
        assert_eq!(reply.status, 202);
        let body: serde_json::Value = serde_json::from_slice(reply.body()).unwrap();
        assert_eq!(body["reset"]["handle"], reset.token);
        assert_eq!(body["reset"]["state"], "running");
        let other = crate::service::StoryService::new(&ctx)
            .create(&crate::service::NewStoryInput {
                title: "Over capacity".into(),
                ..Default::default()
            })
            .unwrap();
        let denied = controller
            .start(
                "fixture",
                &other.id,
                &serde_json::json!({"confirmation":other.id}).to_string(),
                &Arc::new(super::super::dispatch::DispatchRegistry::new()),
                &crate::daemon::bus::ChangeBus::new(),
            )
            .unwrap();
        assert_eq!(denied.status, 429);
        assert!(
            controller
                .store
                .read(|tx| tx.story_reset(ctx.project(), crate::store::StoryNo::new(2)))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            StoryResetService::new(&ctx)
                .get(&story.id, &reset.token)
                .unwrap()
                .token,
            reset.token
        );
    }
}
