//! Provider-independent discovery. Observations never authorize deletion.
pub mod git;
pub(crate) mod tmux;

use super::Ctx;
use crate::domain::{CLEANUP_LEASE_VERSION, StoryCleanupLease, StoryEvent};
use crate::error::AppError;
use crate::store::{ReadOps, Store, StoryNo};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
pub use tmux::ResourcePane;

/// Explicit discovery inputs crossing the CLI/daemon boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceOptions {
    /// Exact lease supplied by an existing lifecycle owner.
    pub lease_json: Option<String>,
    /// Additional legacy window/branch naming convention.
    pub window_name: Option<String>,
    /// Additional legacy worktree container, relative to the repository.
    pub worktree_root: Option<PathBuf>,
    /// Legacy terminal locator explicitly carried from the caller.
    pub tmux_socket: Option<PathBuf>,
}

/// One candidate and the observed facts establishing its identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceCandidate {
    /// Canonical main checkout containing this registration.
    pub repository: PathBuf,
    /// Exact worktree, absent for a branch-only candidate.
    pub worktree: Option<PathBuf>,
    /// Exact local branch.
    pub branch: String,
    /// Git still carries this worktree registration.
    pub registered: bool,
    /// Worktree is present on disk.
    pub exists: bool,
    /// Git protects the registration.
    pub locked: bool,
    /// Verified lease, if this candidate has one.
    pub lease: Option<StoryCleanupLease>,
    /// How this candidate was identified.
    pub sources: BTreeSet<String>,
}

/// Complete read-only result; ambiguity is evidence, never a chosen target.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceReport {
    /// Canonical project slug.
    pub project: String,
    /// Canonical story id.
    pub story_id: String,
    /// resolved, absent, ambiguous, invalid, or unavailable.
    pub status: String,
    /// Selected repository, only usable for resolved/absent reports.
    pub repository: Option<PathBuf>,
    /// Selected worktree, including an exactly leased absent path.
    pub worktree: Option<PathBuf>,
    /// Selected branch, including an absent expected branch.
    pub branch: Option<String>,
    /// Name used for the selected story window.
    pub window_name: String,
    /// Selected creation-time tmux server, when recorded.
    pub socket_path: Option<PathBuf>,
    /// Exact selected pane, when present and unambiguous.
    pub pane: Option<ResourcePane>,
    /// Target provider evidence, not the caller's environment.
    pub provider: Option<String>,
    /// All observed candidates; retained on every refusal.
    pub candidates: Vec<ResourceCandidate>,
    /// Failed invariants and underlying observation diagnostics.
    pub diagnostics: Vec<String>,
}

/// A resource reader bound to a project and its durable dispatch evidence.
pub struct ResourceService<'a, S: Store> {
    ctx: &'a Ctx<'a, S>,
}

impl<'a, S: Store> ResourceService<'a, S> {
    /// Creates a reader without acquiring any mutation authority.
    pub fn new(ctx: &'a Ctx<'a, S>) -> Self {
        Self { ctx }
    }

    /// Resolves a story using store evidence and checked external observations.
    pub fn resolve(&self, id: &str, options: &ResourceOptions) -> Result<ResourceReport, AppError> {
        let (project, checkout, mut leases, providers) = self.ctx.store().read(|tx| {
            let project = tx
                .project(self.ctx.project())?
                .ok_or_else(|| crate::store::StoreError::NotFound("project disappeared".into()))?;
            let no = StoryNo::parse_id(&project.prefix, id)
                .map_err(|_| crate::store::StoreError::NotFound(format!("story {id}")))?;
            if tx.story(project.id, no)?.is_none() {
                return Err(crate::store::StoreError::NotFound(format!("story {id}")));
            }
            let mut leases = Vec::new();
            let mut providers = Vec::new();
            if let Some(lease) =
                tx.events_for(project.id, no)?
                    .iter()
                    .rev()
                    .find_map(|e| match e.known() {
                        Some(StoryEvent::StoryCleanupLeaseRecorded { lease, .. }) => {
                            Some(lease.as_ref().clone())
                        }
                        _ => None,
                    })
            {
                leases.push(lease);
            }
            for run in tx.engine_runs(&project.slug)? {
                if run.state == crate::store::EngineRunState::Finished {
                    continue;
                }
                for lane in tx.engine_lanes(&run.id)? {
                    if lane.story_id.as_deref() == Some(id)
                        && let Some(lease) = lane.cleanup_lease
                    {
                        providers.push((lease.clone(), run.agent.as_str().to_string()));
                        leases.push(lease);
                    }
                }
            }
            Ok((
                project.clone(),
                tx.checkout_path(project.id)?,
                leases,
                providers,
            ))
        })?;
        if let Some(checkout) = checkout.as_deref() {
            let root = git::inventory(checkout)?[0].path.clone();
            if let Some(pointer) = super::project::read_pointer(&root)?
                && pointer.uuid != project.uuid
            {
                return Err(AppError::Validation(format!(
                    "configured repository {} identifies project UUID {}, not {}",
                    root.display(),
                    pointer.uuid,
                    project.uuid
                )));
            }
        }
        let explicit = options
            .lease_json
            .as_ref()
            .map(|raw| {
                serde_json::from_str::<StoryCleanupLease>(raw).map_err(|e| {
                    AppError::Validation(format!("invalid explicit cleanup lease: {e}"))
                })
            })
            .transpose()?;
        if let Some(lease) = explicit.as_ref() {
            leases.push(lease.clone());
        }
        for lease in &leases {
            match super::project::read_pointer(&lease.repository_path)? {
                Some(pointer) if pointer.uuid == project.uuid => {}
                Some(pointer) => {
                    return Err(AppError::Validation(format!(
                        "repository {} identifies project UUID {}, not {}",
                        lease.repository_path.display(),
                        pointer.uuid,
                        project.uuid
                    )));
                }
                None if checkout.as_deref().is_some_and(|path| {
                    git::canonical(path).ok().as_ref() == Some(&lease.repository_path)
                }) => {}
                None => {
                    let origin = git::text(
                        &lease.repository_path,
                        &["config", "--get", "remote.origin.url"],
                    )?;
                    let registered = self.ctx.store().read(|tx| tx.project_remotes(project.id))?;
                    if !registered.iter().any(|remote| {
                        crate::domain::remote::RemoteUrl::normalize(origin.trim())
                            .is_ok_and(|url| remote.normalized == url.key())
                    }) {
                        return Err(AppError::Validation(format!(
                            "repository {} has no verified association with project {}",
                            lease.repository_path.display(),
                            project.slug
                        )));
                    }
                }
            }
        }
        let mut report = resolve(
            &project.slug,
            id,
            checkout.as_deref(),
            leases,
            explicit.as_ref(),
            options,
        )?;
        apply_recorded_provider(&mut report, &providers);
        Ok(report)
    }
}

fn apply_recorded_provider(report: &mut ResourceReport, providers: &[(StoryCleanupLease, String)]) {
    if report
        .pane
        .as_ref()
        .is_some_and(|pane| pane.provider.is_some())
    {
        return;
    }
    let matching: BTreeSet<_> = providers
        .iter()
        .filter(|(lease, _)| {
            report
                .candidates
                .iter()
                .any(|candidate| candidate.lease.as_ref() == Some(lease))
        })
        .map(|(_, provider)| provider.clone())
        .collect();
    if !matching.is_empty() {
        report.provider = if matching.len() == 1 {
            matching.into_iter().next()
        } else {
            None
        };
    }
}

/// Proves exact repository/worktree/branch identity without requiring clean work.
pub fn validate_lease(lease: &StoryCleanupLease) -> Result<(), AppError> {
    if lease.version != CLEANUP_LEASE_VERSION
        || !lease.repository_path.is_absolute()
        || !lease.worktree_path.is_absolute()
        || !lease.tmux.socket_path.is_absolute()
    {
        return Err(AppError::Validation(
            "malformed or unsupported cleanup lease".into(),
        ));
    }
    let records = git::inventory(&lease.repository_path)?;
    let repository = git::canonical(&records[0].path)?;
    if repository != lease.repository_path || repository == lease.worktree_path {
        return Err(AppError::Validation(format!(
            "cleanup lease repository/worktree mismatch: {} / {}",
            repository.display(),
            lease.worktree_path.display()
        )));
    }
    git::text(
        &repository,
        &["check-ref-format", "--branch", &lease.branch],
    )?;
    let target = git::canonical(&lease.worktree_path)?;
    if target != lease.worktree_path {
        return Err(AppError::Validation(format!(
            "cleanup lease path alias: {} resolves to {}",
            lease.worktree_path.display(),
            target.display()
        )));
    }
    let mut registered = false;
    for record in &records {
        if git::canonical(&record.path)? == target {
            registered = true;
            if record.prunable
                || !target.try_exists().map_err(|e| {
                    AppError::Validation(format!("cannot inspect {}: {e}", target.display()))
                })?
            {
                return Err(AppError::Validation(format!(
                    "stale worktree registration at {}; repair registration before cleanup",
                    target.display()
                )));
            }
            if record.branch.as_deref() != Some(&lease.branch) {
                return Err(AppError::Validation(format!(
                    "{} is registered on {:?}, not {}",
                    target.display(),
                    record.branch,
                    lease.branch
                )));
            }
        } else if record.branch.as_deref() == Some(&lease.branch) {
            return Err(AppError::Validation(format!(
                "leased branch {} is also registered at {}",
                lease.branch,
                record.path.display()
            )));
        }
    }
    if target.exists() && !registered {
        return Err(AppError::Validation(format!(
            "leased path {} exists but is unregistered",
            target.display()
        )));
    }
    if target.exists() {
        let actual = git::text(
            &target,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        let expected = git::text(
            &repository,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        if git::canonical(Path::new(actual.trim_end_matches('\n')))?
            != git::canonical(Path::new(expected.trim_end_matches('\n')))?
        {
            return Err(AppError::Validation(format!(
                "{} belongs to another repository",
                target.display()
            )));
        }
    }
    Ok(())
}

fn resolve(
    project: &str,
    id: &str,
    checkout: Option<&Path>,
    leases: Vec<StoryCleanupLease>,
    explicit: Option<&StoryCleanupLease>,
    options: &ResourceOptions,
) -> Result<ResourceReport, AppError> {
    let mut names = BTreeSet::from([id.to_string()]);
    if let Some(name) = &options.window_name {
        if name.is_empty() || name.contains(['/', '\n', '\t']) {
            return Err(AppError::Validation("invalid resource window name".into()));
        }
        names.insert(name.clone());
    }
    let branches: BTreeSet<_> = names
        .iter()
        .map(|name| format!("worktree-{name}"))
        .collect();
    let mut report = ResourceReport {
        project: project.into(),
        story_id: id.into(),
        status: "absent".into(),
        repository: None,
        worktree: None,
        branch: Some(format!(
            "worktree-{}",
            options.window_name.as_deref().unwrap_or(id)
        )),
        window_name: options.window_name.clone().unwrap_or_else(|| id.into()),
        socket_path: None,
        pane: None,
        provider: None,
        candidates: Vec::new(),
        diagnostics: Vec::new(),
    };
    let mut repositories = BTreeSet::new();
    if let Some(checkout) = checkout {
        repositories.insert(git::canonical(checkout)?);
    }
    for lease in &leases {
        if lease.project_slug != project
            || lease.story_id != id
            || lease.version != CLEANUP_LEASE_VERSION
        {
            report.status = "invalid".into();
            report.diagnostics.push(format!(
                "lease for {}/{} does not match {project}/{id}",
                lease.project_slug, lease.story_id
            ));
            return Ok(report);
        }
        repositories.insert(lease.repository_path.clone());
    }
    if repositories.is_empty() {
        return Err(AppError::Validation(format!(
            "project {project} has no recorded repository"
        )));
    }
    let mut candidates = BTreeMap::<(PathBuf, Option<PathBuf>, String), ResourceCandidate>::new();
    for repository in repositories {
        let records = match git::inventory(&repository) {
            Ok(records) => records,
            Err(e) => {
                report.status = "unavailable".into();
                report.diagnostics.push(e.to_string());
                continue;
            }
        };
        let repository = git::canonical(&records[0].path)?;
        if report.repository.is_none() {
            report.repository = Some(repository.clone());
        }
        let mut expected_paths = BTreeSet::new();
        for name in &names {
            for root in [
                Path::new(".claude/worktrees"),
                Path::new(".codex/worktrees"),
            ]
            .into_iter()
            .chain(options.worktree_root.as_deref())
            {
                expected_paths.insert(git::canonical(&repository.join(root).join(name))?);
            }
        }
        let mut repo_leases: Vec<_> = leases
            .iter()
            .filter(|l| l.repository_path == repository)
            .cloned()
            .collect();
        let mut current_markers = Vec::new();
        for record in records.iter() {
            if record.path == repository || !record.path.exists() {
                continue;
            }
            match super::cleanup_lease::marker_at_registered(&record.path) {
                Ok(Some(lease)) if lease.story_id == id && lease.project_slug == project => {
                    current_markers.push(lease.clone());
                    repo_leases.push(lease);
                }
                Ok(Some(lease))
                    if branches.contains(record.branch.as_deref().unwrap_or_default())
                        || expected_paths.contains(&record.path) =>
                {
                    report.status = "invalid".into();
                    report.diagnostics.push(format!(
                        "{} is marked for {}/{}",
                        record.path.display(),
                        lease.project_slug,
                        lease.story_id
                    ));
                }
                Ok(_) => {}
                Err(e) => {
                    report.status = "invalid".into();
                    report.diagnostics.push(e.to_string());
                }
            }
        }
        let mut retained = Vec::new();
        for lease in repo_leases {
            let replacement = current_markers.iter().any(|current| current != &lease);
            let old_registered = records.iter().any(|r| r.path == lease.worktree_path);
            let same_git_new_socket = current_markers.iter().any(|current| {
                current.worktree_path == lease.worktree_path
                    && current.branch == lease.branch
                    && current.tmux != lease.tmux
            });
            if explicit != Some(&lease)
                && replacement
                && (same_git_new_socket
                    || (!old_registered
                        && !lease.worktree_path.try_exists().map_err(|e| {
                            AppError::Validation(format!(
                                "cannot inspect {}: {e}",
                                lease.worktree_path.display()
                            ))
                        })?
                        && !git::branch_exists(&repository, &lease.branch)?))
                && tmux::panes(&lease.tmux.socket_path, &lease_names(&lease, &names))?.is_empty()
            {
                continue;
            }
            retained.push(lease);
        }
        let repo_leases = retained;
        for lease in &repo_leases {
            expected_paths.insert(lease.worktree_path.clone());
        }
        for record in records.iter() {
            let path = git::canonical(&record.path)?;
            let lease = repo_leases.iter().find(|lease| lease.worktree_path == path);
            if !branches.contains(record.branch.as_deref().unwrap_or_default())
                && !expected_paths.contains(&path)
                && lease.is_none()
            {
                continue;
            }
            if path == repository {
                report.status = "invalid".into();
                report.diagnostics.push(format!(
                    "main checkout {} holds the story branch; refusing disposable ownership",
                    path.display()
                ));
                continue;
            }
            let Some(branch) = record.branch.clone() else {
                report.status = "invalid".into();
                report
                    .diagnostics
                    .push(format!("story candidate {} is detached", path.display()));
                continue;
            };
            if lease.is_none() && !branches.contains(&branch) {
                report.status = "invalid".into();
                report.diagnostics.push(format!(
                    "candidate {} holds unrelated branch {branch}",
                    path.display()
                ));
                continue;
            }
            let exists = path.try_exists().map_err(|e| {
                AppError::Validation(format!("cannot inspect {}: {e}", path.display()))
            })?;
            if !exists || record.prunable {
                report.status = "invalid".into();
                report.diagnostics.push(format!(
                    "stale registration at {}; repair Git registration before retrying",
                    path.display()
                ));
            }
            let candidate = ResourceCandidate {
                repository: repository.clone(),
                worktree: Some(path.clone()),
                branch: branch.clone(),
                registered: true,
                exists,
                locked: record.locked,
                lease: lease.cloned(),
                sources: BTreeSet::from([if lease.is_some() {
                    "lease+git".into()
                } else {
                    "git".into()
                }]),
            };
            candidates.insert((repository.clone(), Some(path), branch), candidate);
        }
        for path in expected_paths {
            if path.try_exists().map_err(|e| {
                AppError::Validation(format!("cannot inspect {}: {e}", path.display()))
            })? && !records
                .iter()
                .any(|r| git::canonical(&r.path).ok().as_ref() == Some(&path))
            {
                report.status = "invalid".into();
                report.diagnostics.push(format!(
                    "candidate {} exists but is not registered; refusing unclassified artifacts",
                    path.display()
                ));
            }
        }
        for lease in &repo_leases {
            if let Err(e) = validate_lease(lease) {
                report.status = "invalid".into();
                report.diagnostics.push(e.to_string());
                continue;
            }
            let key = (
                repository.clone(),
                Some(lease.worktree_path.clone()),
                lease.branch.clone(),
            );
            if let Some(candidate) = candidates.get_mut(&key) {
                if candidate.lease.as_ref().is_some_and(|old| old != lease) {
                    report.status = "ambiguous".into();
                    report.diagnostics.push(format!(
                        "conflicting leases for {}",
                        lease.worktree_path.display()
                    ));
                }
                candidate.lease = Some(lease.clone());
            } else {
                candidates.insert(
                    key,
                    ResourceCandidate {
                        repository: repository.clone(),
                        worktree: Some(lease.worktree_path.clone()),
                        branch: lease.branch.clone(),
                        registered: false,
                        exists: false,
                        locked: false,
                        lease: Some(lease.clone()),
                        sources: BTreeSet::from(["recorded-lease".into()]),
                    },
                );
            }
        }
        for branch in &branches {
            if git::branch_exists(&repository, branch)?
                && !candidates
                    .values()
                    .any(|c| c.repository == repository && c.branch == *branch)
            {
                candidates.insert(
                    (repository.clone(), None, branch.clone()),
                    ResourceCandidate {
                        repository: repository.clone(),
                        worktree: None,
                        branch: branch.clone(),
                        registered: false,
                        exists: false,
                        locked: false,
                        lease: None,
                        sources: BTreeSet::from(["local-branch".into()]),
                    },
                );
            }
        }
    }
    report.candidates = candidates.into_values().collect();
    if report.candidates.len() > 1 {
        report.status = "ambiguous".into();
        report
            .diagnostics
            .push("multiple resources claim this story; none was selected".into());
    }
    if !report.diagnostics.is_empty() {
        return Ok(report);
    }
    if let Some(candidate) = report.candidates.first() {
        if explicit.is_some_and(|lease| {
            candidate.repository != lease.repository_path
                || candidate.worktree.as_ref() != Some(&lease.worktree_path)
                || candidate.branch != lease.branch
        }) {
            report.status = "invalid".into();
            report
                .diagnostics
                .push("discovery contradicts the explicit cleanup lease".into());
            return Ok(report);
        }
        report.status = "resolved".into();
        report.repository = Some(candidate.repository.clone());
        report.worktree = candidate.worktree.clone();
        report.branch = Some(candidate.branch.clone());
        report.window_name = candidate
            .branch
            .strip_prefix("worktree-")
            .unwrap_or(&report.window_name)
            .to_string();
        if let Some(lease) = &candidate.lease {
            report.socket_path = Some(lease.tmux.socket_path.clone());
        }
        if let Some(path) = &candidate.worktree {
            if path
                == &candidate
                    .repository
                    .join(".claude/worktrees")
                    .join(&report.window_name)
            {
                report.provider = Some("claude".into());
            }
            if path
                == &candidate
                    .repository
                    .join(".codex/worktrees")
                    .join(&report.window_name)
            {
                report.provider = Some("codex".into());
            }
        }
    }
    if report.socket_path.is_none() {
        report.socket_path = options.tmux_socket.clone();
    }
    report.socket_path = report
        .socket_path
        .as_deref()
        .map(git::canonical)
        .transpose()?;
    if let Some(socket) = &report.socket_path {
        names.insert(report.window_name.clone());
        match tmux::panes(socket, &names) {
            Ok(panes) if panes.len() > 1 => {
                report.status = "ambiguous".into();
                report.diagnostics.push(format!(
                    "multiple tmux windows on {}: {panes:?}",
                    socket.display()
                ));
            }
            Ok(mut panes) => {
                report.pane = panes.pop();
                if let Some(pane) = &report.pane {
                    if let Some(worktree) = report.worktree.as_ref() {
                        if !pane.cwd.starts_with(worktree) {
                            report.status = "invalid".into();
                            report.diagnostics.push(format!(
                                "window {} pane {} is in {}, not {}",
                                pane.window_id,
                                pane.pane_id,
                                pane.cwd.display(),
                                worktree.display()
                            ));
                        }
                    } else if !leases.iter().any(|l| {
                        l.tmux.socket_path == *socket && pane.cwd.starts_with(&l.worktree_path)
                    }) {
                        report.status = "invalid".into();
                        report.diagnostics.push(format!(
                            "window {} has no matching worktree ownership evidence",
                            pane.window_id
                        ));
                    }
                    report.provider = pane.provider.clone().or(report.provider);
                }
            }
            Err(e) => {
                report.status = "unavailable".into();
                report.diagnostics.push(e.to_string());
            }
        }
    }
    Ok(report)
}

/// Every legacy name established by a lease, including custom dispatch names.
pub(crate) fn lease_names(
    lease: &StoryCleanupLease,
    additional: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut names = additional.clone();
    names.insert(lease.story_id.clone());
    if let Some(name) = lease.branch.strip_prefix("worktree-") {
        names.insert(name.into());
    }
    names
}
