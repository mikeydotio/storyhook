//! Authenticated reset requests: reserve here, execute in the reset runtime.
//!
//! The controller only reserves a reset and hands it to the daemon's reset
//! runtime, which drives it to completion on the daemon's own store and
//! resumes it after a restart (SH-886). A poll therefore never reports an
//! unfinished reset as an error: it is running until it completes.
use crate::api::http::{
    Reply, TrustedHosts, content_type_is_json, error_reply, json_reply, text_reply,
};
use crate::daemon::http1::{Header, Method};
use crate::daemon::reset::ResetRuntime;
use crate::env::Environment;
use crate::error::AppError;
use crate::service::{Ctx, story_reset::StoryResetService};
use crate::store::{ReadOps, SqliteStore, Store, StoryReset};
use std::sync::Arc;
use std::time::Instant;

/// Reserves resets on the connection thread and queues them on the runtime.
pub(crate) struct ResetController {
    store: SqliteStore,
    env: Environment,
    runtime: Arc<ResetRuntime>,
}

impl ResetController {
    /// Shares the daemon's reset runtime.
    pub(crate) fn open(env: &Environment, runtime: Arc<ResetRuntime>) -> Result<Self, AppError> {
        Ok(Self {
            store: crate::invoke::open_store(env)?,
            env: env.clone(),
            runtime,
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

    /// The poll answer: `ok` once finished, otherwise `running` with the last
    /// obstacle the reset is waiting out. Never `error`: the runtime resumes
    /// every unfinished reset.
    fn envelope(&self, reset: &StoryReset) -> Result<serde_json::Value, AppError> {
        let reset = self.store.read(|tx| {
            tx.story_reset(reset.project, reset.story)?
                .filter(|current| current.token == reset.token)
                .ok_or_else(|| {
                    crate::store::StoreError::NotFound(format!(
                        "reset {} for {}",
                        reset.token, reset.story_id
                    ))
                })
        })?;
        if !reset.completed {
            // A receipt nothing drives yet is resumed now, not at the sweep.
            self.runtime.request(&reset);
        }
        Ok(serde_json::json!({"result":"ok", "reset": {
            "handle": reset.token,
            "story": reset.story_id,
            "state": if reset.completed { "ok" } else { "running" },
            "detail": reset.failure,
            "residue": reset.residue,
            "recovery": reset.recovery,
        }}))
    }

    fn start(&self, project: &str, id: &str, body: &str) -> Result<Reply, AppError> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            confirmation: String,
        }
        let request: Request = serde_json::from_str(body)
            .map_err(|e| AppError::Validation(format!("invalid reset request: {e}")))?;
        let ctx = self.context(project)?;
        let reset = StoryResetService::new(&ctx).reserve(id, &request.confirmation)?;
        self.runtime.request(&reset);
        Ok(json_reply(202, self.envelope(&reset)?.to_string()).no_cache())
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
            controller.start(project, id, body)
        }
        (Method::Get, ["api", "repos", project, "story", id, "reset", handle]) => (|| {
            let ctx = controller.context(project)?;
            let reset = StoryResetService::new(&ctx).get(id, handle)?;
            Ok(json_reply(200, controller.envelope(&reset)?.to_string()).no_cache())
        })(),
        _ => Ok(text_reply(405, "Method not allowed")),
    };
    Some(result.unwrap_or_else(|error| error_reply(&error)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller(env: &Environment) -> ResetController {
        ResetController::open(env, Arc::new(ResetRuntime::new())).unwrap()
    }

    #[test]
    fn an_unfinished_reset_polls_as_running_and_is_queued_never_as_an_error() {
        let fixture = storyhook_test_support::ServiceFixture::new();
        let env = Environment::at(fixture.env().home());
        let controller = controller(&env);
        let ctx = controller.context("fixture").unwrap();
        let story = crate::service::StoryService::new(&ctx)
            .create(&crate::service::NewStoryInput {
                title: "Interrupted reset".into(),
                ..Default::default()
            })
            .unwrap();
        let reset = StoryResetService::new(&ctx)
            .reserve(&story.id, &story.id)
            .unwrap();
        // As after a restart: nothing drives the receipt yet.
        assert!(!controller.runtime.is_active(&reset.token));
        let body = controller.envelope(&reset).unwrap();
        assert_eq!(body["reset"]["state"], "running", "{body}");
        assert!(controller.runtime.is_active(&reset.token));
    }

    #[test]
    fn requests_beyond_any_worker_count_are_reserved_and_duplicates_reuse_the_handle() {
        let fixture = storyhook_test_support::ServiceFixture::new();
        let env = Environment::at(fixture.env().home());
        let controller = controller(&env);
        let ctx = controller.context("fixture").unwrap();
        let mut handles = Vec::new();
        for index in 0..6 {
            let story = crate::service::StoryService::new(&ctx)
                .create(&crate::service::NewStoryInput {
                    title: format!("Reset {index}"),
                    ..Default::default()
                })
                .unwrap();
            let body = serde_json::json!({ "confirmation": story.id }).to_string();
            let reply = controller.start("fixture", &story.id, &body).unwrap();
            assert_eq!(reply.status, 202, "no request is refused for capacity");
            let first: serde_json::Value = serde_json::from_slice(reply.body()).unwrap();
            let again = controller.start("fixture", &story.id, &body).unwrap();
            let second: serde_json::Value = serde_json::from_slice(again.body()).unwrap();
            assert_eq!(first["reset"]["handle"], second["reset"]["handle"]);
            handles.push(first["reset"]["handle"].as_str().unwrap().to_string());
        }
        assert!(
            handles
                .iter()
                .all(|handle| controller.runtime.is_active(handle))
        );
    }
}
