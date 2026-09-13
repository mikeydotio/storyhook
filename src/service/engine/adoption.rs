//! Adoption of existing dispatches without changing their claims or resources.
use crate::domain::StoryCleanupLease;
use crate::error::AppError;
use crate::store::EngineAgent;
use std::path::Path;

pub use crate::store::AdoptedIdentity;

/// Validated external evidence for one existing dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectedDispatch {
    /// Validated worktree cleanup marker.
    pub lease: StoryCleanupLease,
    /// Exact server-local pane identifier.
    pub pane_id: String,
    /// Exact story window name.
    pub window_name: String,
    /// Provider and process identity.
    pub identity: AdoptedIdentity,
}

/// Read-only external observations; implementations must never mutate a dispatch.
pub trait DispatchInspector {
    /// Proves one story's worktree, lease, and live provider pane.
    fn inspect(
        &self,
        checkout: &Path,
        project: &str,
        story: &str,
    ) -> Result<InspectedDispatch, AppError>;
}

use crate::env::git_env;
use crate::process::run_captured;
use crate::store::{EngineLaneRecord, Store};
use std::process::Command;

use super::{Dispatcher, EngineService, RunId, RunView, WindowProbe};

/// The production inspector, with no mutating subprocess operations.
#[derive(Default)]
pub struct LiveDispatchInspector;

fn refusal(detail: impl Into<String>) -> AppError {
    AppError::Validation(format!("engine adopt: {}", detail.into()))
}

fn capture(mut command: Command, args: &[&str], context: &str) -> Result<String, AppError> {
    command.args(args);
    let captured = run_captured(command, super::TMUX_TIMEOUT)
        .map_err(|error| refusal(format!("{context}: {}", error.detail())))?;
    if !captured.status.success() {
        return Err(refusal(format!(
            "{context}: {}",
            String::from_utf8_lossy(&captured.stderr).trim()
        )));
    }
    String::from_utf8(captured.stdout)
        .map_err(|error| refusal(format!("{context}: non-UTF-8 response: {error}")))
}

impl DispatchInspector for LiveDispatchInspector {
    fn inspect(
        &self,
        checkout: &Path,
        project: &str,
        story: &str,
    ) -> Result<InspectedDispatch, AppError> {
        let inventory = capture(
            git_env::command(checkout),
            &["worktree", "list", "--porcelain", "-z"],
            "list registered worktrees",
        )?;
        let mut matching = Vec::new();
        for path in inventory
            .split('\0')
            .filter_map(|field| field.strip_prefix("worktree "))
        {
            let path = Path::new(path);
            if let Some(lease) = crate::service::cleanup_lease::marker_at(path)?
                && lease.project_slug == project
                && lease.story_id == story
            {
                matching.push(lease);
            }
        }
        if matching.len() != 1 {
            return Err(refusal(format!(
                "{story}: expected one readable matching worktree lease, found {}",
                matching.len()
            )));
        }
        let lease = matching.remove(0);
        let repository = inventory
            .split('\0')
            .find_map(|field| field.strip_prefix("worktree "))
            .ok_or_else(|| refusal("worktree inventory omits repository"))?;
        if Path::new(repository)
            .canonicalize()
            .map_err(|e| refusal(e.to_string()))?
            != lease
                .repository_path
                .canonicalize()
                .map_err(|e| refusal(e.to_string()))?
        {
            return Err(refusal(format!("{story}: repository mismatch")));
        }
        inspect_lease(lease)
    }
}

fn tmux(lease: &StoryCleanupLease) -> Command {
    let mut command = Command::new("tmux");
    crate::env::spawn_env::apply_dispatch_allowlist(&mut command);
    command.arg("-S").arg(&lease.tmux.socket_path);
    command
}

fn inspect_lease(lease: StoryCleanupLease) -> Result<InspectedDispatch, AppError> {
    let listing = capture(
        tmux(&lease),
        &[
            "list-panes",
            "-a",
            "-F",
            "#{window_name}\t#{pane_id}\t#{pane_pid}\t#{pane_current_command}\t#{pane_dead}\t#{window_id}\t#{pane_current_path}\t#{@storyhook-agent}\t#{pane_active}",
        ],
        "inspect leased tmux server",
    )?;
    let rows: Vec<Vec<&str>> = listing
        .lines()
        .map(|line| line.split('\t').collect::<Vec<_>>())
        .filter(|fields| fields.first() == Some(&lease.story_id.as_str()))
        .collect();
    let windows: std::collections::BTreeSet<_> = rows.iter().filter_map(|r| r.get(5)).collect();
    if windows.len() != 1 {
        return Err(refusal(format!(
            "{}: expected one exact story window, found {}",
            lease.story_id,
            windows.len()
        )));
    }
    let active: Vec<_> = rows.iter().filter(|row| row.get(8) == Some(&"1")).collect();
    if active.len() != 1 {
        return Err(refusal(format!(
            "{}: expected one active pane",
            lease.story_id
        )));
    }
    let fields = active[0];
    if fields.len() != 9 || !super::valid_pane_id(fields[1]) {
        return Err(refusal("malformed pane identity"));
    }
    let provider = EngineAgent::parse(fields[7])
        .ok_or_else(|| refusal("pane has no valid StoryHook provider identity"))?;
    let pid = fields[2]
        .parse::<i32>()
        .map_err(|_| refusal("invalid pane PID"))?;
    if fields[4] != "0" || !super::pid_is_live(pid) {
        return Err(refusal("pane process is dead"));
    }
    let pattern = match provider {
        EngineAgent::Claude => "^(claude|node)$",
        EngineAgent::Codex => "^(codex)$",
    };
    let identity = super::ProcessIdentity {
        pattern: Some(
            regex::Regex::new(
                &std::env::var("STORY_READY_PROCESS_PATTERN").unwrap_or_else(|_| pattern.into()),
            )
            .map_err(|error| refusal(format!("invalid provider process pattern: {error}")))?,
        ),
        launch_binaries: std::env::var("STORY_LAUNCH_CMD")
            .ok()
            .and_then(|command| command.split_whitespace().next().map(str::to_owned))
            .or_else(|| Some(provider.as_str().into()))
            .and_then(|word| super::resolve_executable(&word))
            .into_iter()
            .collect(),
    };
    if !identity.matches(fields[3]) {
        return Err(refusal(format!(
            "pane no longer runs dispatched {} process: {}",
            provider.as_str(),
            fields[3]
        )));
    }
    let cwd = Path::new(fields[6])
        .canonicalize()
        .map_err(|e| refusal(format!("cannot resolve pane working directory: {e}")))?;
    let worktree = lease
        .worktree_path
        .canonicalize()
        .map_err(|e| refusal(format!("cannot resolve leased worktree: {e}")))?;
    if !cwd.starts_with(&worktree) {
        return Err(refusal("pane worktree identity mismatch"));
    }
    Ok(InspectedDispatch {
        pane_id: fields[1].into(),
        window_name: fields[0].into(),
        identity: AdoptedIdentity {
            provider,
            pane_pid: pid,
            window_id: fields[5].into(),
        },
        lease,
    })
}

/// Observes an adopted process on its captured server, never an ambient server.
pub(super) fn probe(lane: &EngineLaneRecord) -> WindowProbe {
    let Some(lease) = lane.cleanup_lease.clone() else {
        return WindowProbe::Unanswered {
            detail: "adopted lane lost cleanup lease".into(),
        };
    };
    match inspect_lease(lease) {
        Ok(found)
            if lane.pane_id.as_deref() == Some(&found.pane_id)
                && lane.adopted_identity.as_ref() == Some(&found.identity) =>
        {
            let activity = capture(
                tmux(&found.lease),
                &[
                    "display-message",
                    "-p",
                    "-t",
                    &found.pane_id,
                    "#{window_activity}",
                ],
                "read pane activity",
            );
            match activity {
                Ok(at) => WindowProbe::Alive {
                    last_output_at: at.trim().parse().ok(),
                },
                Err(error) => WindowProbe::Unanswered {
                    detail: error.to_string(),
                },
            }
        }
        Ok(_) => WindowProbe::Gone {
            detail: "adopted pane process identity changed".into(),
        },
        Err(error) => {
            let detail = error.to_string();
            if detail.contains("process is dead")
                || detail.contains("no longer runs")
                || detail.contains("found 0")
                || super::tmux_reports_a_missing_target(&detail)
            {
                WindowProbe::Gone { detail }
            } else {
                WindowProbe::Unanswered { detail }
            }
        }
    }
}

use crate::domain::{self, StorySnapshot, SuperState};
use crate::store::{
    EngineRunRecord, EngineRunState, EngineScope, ReadOps, StoreError, StoryQuery, StoryRow,
    WriteOps,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn snapshots(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
) -> Result<BTreeMap<String, StorySnapshot>, StoreError> {
    Ok(tx
        .stories(project, &StoryQuery::all())?
        .into_iter()
        .map(|row| (row.snapshot.id.clone(), row.snapshot))
        .collect())
}

fn eligible(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
    run: &EngineRunRecord,
    ids: &[String],
) -> Result<Vec<StoryRow>, StoreError> {
    super::require_state(
        run,
        "adopt",
        &[EngineRunState::Running, EngineRunState::Paused],
    )?;
    let all = snapshots(tx, project)?;
    let scope = match &run.scope {
        EngineScope::Project => None,
        EngineScope::Epic(id) => {
            if !all.get(id).is_some_and(domain::is_epic) {
                return Err(refusal("run epic is unavailable").into());
            }
            Some(domain::descendant_ids(&all, id))
        }
    };
    let prefix = super::project_prefix(tx, project)?;
    let mut seen = BTreeSet::new();
    ids.iter()
        .map(|id| {
            let (_, row) =
                super::resolve_story(tx, project, &prefix, id).map_err(StoreError::from)?;
            let story = &row.snapshot;
            if !seen.insert(story.id.clone()) {
                return Err(refusal(format!("duplicate story {}", story.id)).into());
            }
            if row.state != "in-progress"
                || row.superstate != SuperState::Open
                || story.draft
                || domain::is_epic(story)
                || domain::is_human_only(story)
                || domain::is_blocked(story, &all)
                || story
                    .relationships
                    .iter()
                    .any(|edge| edge.relation == "obviated-by")
                || scope
                    .as_ref()
                    .is_some_and(|scope| !scope.contains(&story.id))
            {
                return Err(refusal(format!(
                    "{} must be claimed, unblocked, non-human-only work within the run scope",
                    story.id
                ))
                .into());
            }
            let events = tx
                .events_for(project, row.story_no)?
                .into_iter()
                .filter_map(|e| e.known().cloned())
                .collect::<Vec<_>>();
            if domain::state_claimed_from(&events, "in-progress").is_none() {
                return Err(refusal(format!("{} has no claim origin", story.id)).into());
            }
            Ok(row)
        })
        .collect()
}

fn same_binding(lane: &EngineLaneRecord, found: &InspectedDispatch) -> bool {
    lane.state == super::EngineLaneState::Working
        && lane.story_id.as_deref() == Some(&found.lease.story_id)
        && lane.pane_id.as_deref() == Some(&found.pane_id)
        && lane.cleanup_lease.as_ref() == Some(&found.lease)
        && lane.adopted_identity.as_ref() == Some(&found.identity)
}

impl<S: Store, D: Dispatcher> EngineService<'_, S, D> {
    /// Atomically binds existing, independently inspected dispatches to idle lanes.
    pub fn adopt(
        &self,
        run_id: &RunId,
        ids: &[String],
        inspector: &impl DispatchInspector,
    ) -> Result<RunView, AppError> {
        if ids.is_empty() || ids.len() > super::MAX_ENGINE_LANES as usize {
            return Err(refusal(format!(
                "between 1 and {} stories are required",
                super::MAX_ENGINE_LANES
            )));
        }
        let project = self.ctx.project();
        let (slug, checkout, before) = self.ctx.store().read(|tx| {
            let slug = super::project_slug(tx, project)?;
            let run = super::run_for_project(tx, &slug, run_id)?;
            let checkout = tx
                .checkout_path(project)?
                .ok_or_else(|| StoreError::from(refusal("project has no registered checkout")))?;
            Ok((slug, checkout, eligible(tx, project, &run, ids)?))
        })?;
        let mut found = Vec::new();
        for row in &before {
            found.push(inspector.inspect(&checkout, &slug, &row.snapshot.id)?);
        }
        // No external observation holds a store lock. Replacing a dispatch
        // between the two observations invalidates the whole batch.
        for (row, first) in before.iter().zip(&found) {
            if first.lease.version != domain::CLEANUP_LEASE_VERSION
                || first.lease.project_slug != slug
                || first.lease.story_id != row.snapshot.id
                || first.window_name != row.snapshot.id
                || !super::valid_pane_id(&first.pane_id)
                || !first.lease.tmux.socket_path.is_absolute()
                || &inspector.inspect(&checkout, &slug, &row.snapshot.id)? != first
            {
                return Err(refusal(format!(
                    "{} dispatch identity changed or is invalid",
                    row.snapshot.id
                )));
            }
        }
        let now = self.ctx.now();
        let adopted = self.ctx.store().write(|tx| {
            let mut run = super::run_for_project(tx, &slug, run_id)?;
            let current = eligible(tx, project, &run, ids)?;
            if current.iter().zip(&before).any(|(a,b)| a.head_global_seq != b.head_global_seq || a.story_no != b.story_no)
                || tx.checkout_path(project)?.as_ref() != Some(&checkout) {
                return Err(refusal("story or checkout changed during inspection; retry adoption").into());
            }
            let lanes = tx.engine_lanes(run_id)?;
            let mut additions = Vec::new();
            for (row, dispatch) in current.iter().zip(&found) {
                let mut existing = None;
                for other_run in tx.engine_runs(&slug)? {
                    for lane in tx.engine_lanes(&other_run.id)? {
                        if other_run.project_slug == slug && lane.story_id.as_deref() == Some(&row.snapshot.id) {
                            if existing.is_some() || other_run.id != *run_id || !same_binding(&lane, dispatch) { return Err(refusal(format!("{} already has conflicting lane ownership", row.snapshot.id)).into()); }
                            existing = Some(lane);
                        }
                    }
                }
                if existing.is_none() { additions.push((row, dispatch)); }
            }
            let idle: Vec<_> = lanes.iter().filter(|lane| lane.state == super::EngineLaneState::Idle && lane.lane_index < run.lanes).collect();
            if !additions.is_empty() && (additions.len() > idle.len() || super::occupied_run_lane_count(&lanes) + additions.len() > run.lanes as usize) {
                return Err(refusal(format!("insufficient capacity in run {run_id}; configure more lanes or wait for idle capacity")).into());
            }
            let mut adopted = Vec::new();
            for ((row, dispatch), slot) in additions.into_iter().zip(idle) {
                let mut lane = super::idle_lane(run_id, slot.lane_index, &now);
                lane.state = super::EngineLaneState::Working;
                lane.story_id = Some(row.snapshot.id.clone());
                lane.pane_id = Some(dispatch.pane_id.clone());
                lane.window_name = Some(dispatch.window_name.clone());
                lane.worktree_path = Some(dispatch.lease.worktree_path.to_string_lossy().into_owned());
                lane.cleanup_lease = Some(dispatch.lease.clone());
                lane.adopted_identity = Some(dispatch.identity.clone());
                lane.dispatched_at = Some(now.clone());
                lane.last_progress_at = Some(now.clone());
                lane.last_progress_seq = Some(row.head_global_seq);
                tx.put_engine_lane(&lane)?;
                adopted.push((slot.lane_index, row.snapshot.id.clone()));
            }
            if !adopted.is_empty() { run.updated_at = now.clone(); tx.update_engine_run(&run)?; }
            Ok(adopted)
        })?;
        for (lane, story) in adopted {
            crate::daemon::activity::emit(
                "INFO",
                "engine/adopt",
                "event",
                &format!("project={slug} run={run_id} lane={lane} story={story}"),
                "adopted existing manual dispatch",
            );
        }
        self.one_view(run_id)
    }
}
