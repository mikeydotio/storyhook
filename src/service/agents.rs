//! Whether each claimed story's agent is still working (SH-850).
//!
//! The dashboard offers Resume only for an in-progress story whose agent was
//! lost -- a reboot, a crashed tmux server -- and never for one whose agent is
//! working. That needs, for every open ordinary story in the project's
//! active-role state, the answer the resume helper itself would reach. So the
//! census asks the same reader the helper asks, [`ResourceService::resolve`]:
//! the story's recorded worktree, branch and tmux server, and its window's
//! pane on that server. It is a read for a menu, never an authority: the
//! helper's `--if-absent` guard decides again, under the workspace lock,
//! before anything is replaced.
use super::launch_record::{self, LaunchRecord};
use super::resources::{ResourceOptions, ResourceReport, ResourceService};
use super::{Ctx, QueryService};
use crate::domain::{SuperState, is_epic};
use crate::error::AppError;
use crate::store::{ReadOps, Store};
use serde::Serialize;

/// What the census could establish about one story's agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    /// A live process occupies the story's window on its recorded server.
    Live,
    /// The story has dispatch evidence and no live pane: the window or its
    /// server is gone, or the pane's process exited.
    Lost,
    /// The evidence is ambiguous, invalid, or could not be read.
    Unknown,
}

/// One claimed story's agent, as the dashboard shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AgentView {
    /// Canonical story id.
    pub story: String,
    /// What the census established.
    pub state: AgentState,
    /// The provider the dispatch recorded (window tag or worktree container).
    pub provider: Option<String>,
    /// The settings of the story's last confirmed launch, when recorded.
    pub launch: Option<LaunchRecord>,
    /// The evidence behind `state`, in words.
    pub detail: String,
}

/// The census over one project.
pub struct AgentService<'a, S: Store> {
    ctx: &'a Ctx<'a, S>,
}

impl<'a, S: Store> AgentService<'a, S> {
    /// A census over `ctx`'s project, with no authority to change anything.
    pub fn new(ctx: &'a Ctx<'a, S>) -> Self {
        Self { ctx }
    }

    /// The agent of every open ordinary story in the active-role state that
    /// has dispatch evidence, resolved as the resume helper resolves it.
    pub fn census(&self) -> Result<Vec<AgentView>, AppError> {
        let resources = ResourceService::new(self.ctx);
        self.census_with(|id| resources.resolve(id, &ResourceOptions::default()))
    }

    /// [`census`](Self::census) with the resource reader injected, so every
    /// mapping from a report to a view is testable without tmux or Git.
    pub fn census_with(
        &self,
        resolve: impl Fn(&str) -> Result<ResourceReport, AppError>,
    ) -> Result<Vec<AgentView>, AppError> {
        let (project, claimed) = self.ctx.store().read(|tx| {
            let project = tx
                .project(self.ctx.project())?
                .ok_or_else(|| crate::store::StoreError::NotFound("project disappeared".into()))?;
            Ok((project.slug, tx.states(self.ctx.project())?))
        })?;
        let Some(active) = crate::domain::active_state(&claimed) else {
            return Ok(Vec::new());
        };
        let stories = self.ctx.store().read(|tx| {
            QueryService::new(tx, self.ctx.project(), &self.ctx.now())
                .story_map()
                .map_err(|error| crate::store::StoreError::Validation(error.to_string()))
        })?;
        let mut views = Vec::new();
        for (id, story) in &stories {
            if story.superstate != SuperState::Open || story.state != active.slug || is_epic(story)
            {
                continue;
            }
            if let Some(view) = view_of(&project, id, resolve(id)) {
                views.push(view);
            }
        }
        Ok(views)
    }
}

/// Maps one story's resource report to its view; `None` when the story has
/// no dispatch evidence at all (it was claimed by hand, never dispatched).
fn view_of(project: &str, id: &str, report: Result<ResourceReport, AppError>) -> Option<AgentView> {
    let report = match report {
        Ok(report) => report,
        Err(error) => {
            return Some(AgentView {
                story: id.to_string(),
                state: AgentState::Unknown,
                provider: None,
                launch: None,
                detail: format!("the story's resources could not be read: {error}"),
            });
        }
    };
    if report.candidates.is_empty() && report.pane.is_none() {
        return None;
    }
    let (launch, launch_note) = match report.worktree.as_deref() {
        Some(worktree) => match launch_record::read(worktree, project, id) {
            Ok(record) => (record, String::new()),
            Err(reason) => (
                None,
                format!(" Its launch settings cannot be offered: {reason}."),
            ),
        },
        None => (None, String::new()),
    };
    let (state, detail) = if !matches!(report.status.as_str(), "resolved" | "absent") {
        (
            AgentState::Unknown,
            format!(
                "the story's resources are {}: {}",
                report.status,
                report.diagnostics.join("; ")
            ),
        )
    } else {
        match &report.pane {
            Some(pane) if !pane.dead => (
                AgentState::Live,
                format!(
                    "pane {} in window {} still runs a process",
                    pane.pane_id, pane.window_name
                ),
            ),
            Some(pane) => (
                AgentState::Lost,
                format!(
                    "the agent in pane {} of window {} has exited",
                    pane.pane_id, pane.window_name
                ),
            ),
            None => (
                AgentState::Lost,
                match &report.socket_path {
                    Some(socket) => format!(
                        "no window named {} on tmux server {}",
                        report.window_name,
                        socket.display()
                    ),
                    None => format!("no window named {} was found", report.window_name),
                },
            ),
        }
    };
    Some(AgentView {
        story: id.to_string(),
        state,
        provider: report.provider.clone(),
        launch,
        detail: detail + &launch_note,
    })
}

#[cfg(test)]
mod tests;
