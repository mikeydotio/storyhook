//! `GET /api/repos/{project}/agents` — whether each claimed story's agent is
//! still working (SH-850).
//!
//! The dashboard gates its Resume action on this census. Answering it runs a
//! tmux probe and Git reads for every claimed story, so it is intercepted in
//! [`crate::daemon::serve::worker`] before a store-pool `Job` is built, for
//! the reason [`crate::api::engine`] gives: no subprocess may hold one of the
//! fixed store dispatchers. It shares the engine controller's own store
//! handle rather than opening a second one.
//!
//! A read, token-gated like the engine's status read: it names panes, tmux
//! servers and worktree paths on this machine.

use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::api::admission::named_token_ok;
use crate::api::engine::{EngineController, valid_segment};
use crate::api::http::{Reply, error_reply, json_reply, text_reply};
use crate::api::rpc::token_ok;
use crate::api::tokens::TokenRegistry;
use crate::daemon::http1::{Header, Method};
use crate::service::agents::AgentService;

/// Whether `segments` name the agents census.
pub(crate) fn is_agents_path(segments: &[&str]) -> bool {
    matches!(segments, ["api", "repos", _, "agents"])
}

/// Answers the census, or returns `None` when the path belongs to another
/// API family.
#[allow(clippy::too_many_arguments)]
pub(crate) fn intercept(
    segments: &[&str],
    method: &Method,
    headers: &[Header],
    token: &str,
    controller: &Arc<EngineController>,
    tokens: &TokenRegistry,
    cookie_name: &str,
    wall_now: DateTime<Utc>,
) -> Option<Reply> {
    if !is_agents_path(segments) {
        return None;
    }
    if !valid_segment(segments[2]) {
        return Some(text_reply(404, "Not found"));
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
    if !matches!(method, Method::Get) {
        return Some(text_reply(405, "Method Not Allowed"));
    }
    let census = controller
        .context(segments[2])
        .and_then(|ctx| AgentService::new(&ctx).census());
    Some(match census {
        Ok(agents) => json_reply(
            200,
            serde_json::json!({"result": "ok", "agents": agents}).to_string(),
        )
        .no_cache(),
        Err(error) => error_reply(&error),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_census_path_is_claimed() {
        assert!(is_agents_path(&["api", "repos", "proj", "agents"]));
        for other in [
            &["api", "repos", "proj", "engine"][..],
            &["api", "repos", "proj", "agents", "SH-1"],
            &["api", "repos", "proj"],
            &["api", "agents"],
        ] {
            assert!(!is_agents_path(other), "{other:?}");
        }
    }
}
