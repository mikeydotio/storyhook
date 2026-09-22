//! Thin policy API over the same service used by the CLI.
use crate::api::http::{Reply, error_reply, json_reply, parse_json_object, require_str};
use crate::domain::Complexity;
use crate::error::AppError;
use crate::output::{Response, render_response};
use crate::service::dispatch_policy;
use crate::store::{EngineAgent, ProjectId, Store};

pub(super) fn show<S: Store>(
    store: &S,
    project: Option<ProjectId>,
    target: Option<(&str, &str)>,
) -> Reply {
    let result = (|| {
        let target = target
            .map(|(id, agent)| Ok::<_, AppError>((id, parse_agent(agent)?)))
            .transpose()?;
        dispatch_policy::show(store, project, target)
    })();
    reply(result)
}

pub(super) fn patch<S: Store>(store: &S, project: Option<ProjectId>, body: &str) -> Reply {
    reply((|| {
        let mut object = parse_json_object(body)?;
        let agent = parse_agent(require_str(&object, "agent")?)?;
        let complexity = Complexity::parse(require_str(&object, "complexity")?)?;
        object.remove("agent");
        object.remove("complexity");
        dispatch_policy::patch(store, project, agent, complexity, &object)
    })())
}

fn parse_agent(raw: &str) -> Result<EngineAgent, AppError> {
    EngineAgent::parse(raw)
        .ok_or_else(|| AppError::Validation(format!("invalid agent `{raw}`; use codex or claude")))
}

fn reply(result: Result<dispatch_policy::PolicyView, AppError>) -> Reply {
    match result {
        Ok(view) => json_reply(
            200,
            render_response(&Response::DispatchPolicy(view), true, false),
        )
        .no_store(),
        Err(error) => error_reply(&error),
    }
}
