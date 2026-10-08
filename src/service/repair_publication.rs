//! Durable commit notifications. Publication owns no verification generation.
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Ctx, cleanup_lease, resources::git};
use crate::domain::StoryCleanupLease;
use crate::env::Environment;
use crate::error::AppError;
use crate::store::{ReadOps, Store, StoryNo};

/// One immutable commit notification, with mutable retry evidence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Request {
    /// Spool protocol version.
    pub version: u32,
    /// Project authority at enqueue; old spools cannot survive a manual-mode boundary.
    #[serde(default)]
    pub automation_generation: Option<i64>,
    /// Exact dispatch resources that produced the commit.
    pub lease: StoryCleanupLease,
    /// Full commit ID captured by the notification.
    pub head: String,
    /// Existing closing PR; publication must never replace it.
    pub pull_request: String,
    /// Earliest automatic retry after a failed attempt.
    pub retry_at: Option<String>,
    /// Last reported failure, retained across daemon restart.
    pub error: Option<String>,
}

/// Refuse stale requests after a worktree has been reassigned.
pub(crate) fn validate_marker(
    env: &Environment,
    lease: &StoryCleanupLease,
) -> Result<(), AppError> {
    if cleanup_lease::marker_at_registered(
        env.subprocess_bound(std::time::Duration::from_secs(60)),
        &lease.worktree_path,
    )?
    .as_ref()
        != Some(lease)
    {
        return Err(AppError::Validation(
            "publication lease no longer matches its worktree marker".into(),
        ));
    }
    Ok(())
}

/// A merged link cannot authorize deletion of commits absent from that PR.
pub(crate) fn guard_merged_pr<S: Store>(
    ctx: &Ctx<'_, S>,
    lease: &StoryCleanupLease,
) -> Result<Option<String>, AppError> {
    let links = ctx.store().read(|tx| {
        let prefix = super::project_prefix(tx, ctx.project())?;
        let number = StoryNo::parse_id(&prefix, &lease.story_id)?;
        Ok(tx
            .pr_links(ctx.project())?
            .into_iter()
            .filter(|(story, link)| {
                *story == number && link.close_on_merge && link.status == "merged"
            })
            .map(|(_, link)| link)
            .collect::<Vec<_>>())
    })?;
    if links.is_empty() {
        return Ok(None);
    }
    if links.len() != 1 {
        return Err(AppError::Validation(
            "multiple merged PRs; preserve the branch".into(),
        ));
    }
    let repository =
        crate::github_access::Repository::resolve_with_env(&lease.repository_path, ctx.env())?;
    let bytes = repository.gh(&[
        "pr".into(),
        "view".into(),
        links[0].url.clone(),
        "--json".into(),
        "url,state,headRefOid,headRefName,isCrossRepository".into(),
    ])?;
    let metadata: serde_json::Value = serde_json::from_slice(&bytes)?;
    let head = metadata["headRefOid"].as_str().unwrap_or("");
    if metadata["url"] != links[0].url
        || metadata["state"] != "MERGED"
        || metadata["headRefName"] != lease.branch
        || metadata["isCrossRepository"] != false
        || !matches!(head.len(), 40 | 64)
        || !head.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(AppError::Validation(
            "merged PR evidence is invalid; preserve the branch".into(),
        ));
    }
    repository.git(&[
        "fetch".into(),
        "--quiet".into(),
        "--no-write-fetch-head".into(),
        "origin".into(),
        head.into(),
    ])?;
    guard_tip_with_bound(
        ctx.env()
            .subprocess_bound(std::time::Duration::from_secs(60)),
        &lease.repository_path,
        &lease.branch,
        head,
    )?;
    Ok(Some(head.into()))
}

/// Check the exact branch against the observed merged head, not a newer base.
#[cfg(test)]
fn guard_tip(repository: &Path, branch: &str, merged_head: &str) -> Result<(), AppError> {
    guard_tip_with_bound(
        crate::testing::load_grace::graced_now(std::time::Duration::from_secs(60)),
        repository,
        branch,
        merged_head,
    )
}

fn guard_tip_with_bound(
    bound: std::time::Duration,
    repository: &Path,
    branch: &str,
    merged_head: &str,
) -> Result<(), AppError> {
    if !super::resources::git::branch_exists_with_bound(bound, repository, branch)? {
        return Ok(());
    }
    let tip = git::text_with_bound(
        bound,
        repository,
        &["rev-parse", &format!("refs/heads/{branch}")],
    )?;
    git::text_with_bound(bound, repository, &["merge-base", "--is-ancestor", tip.trim(), merged_head])
        .map_err(|e| AppError::Validation(format!("unpublished repair: branch {branch} at {} is absent from merged PR head {merged_head}; preserve it: {e}", tip.trim())))?;
    Ok(())
}

/// Store-scoped spool, independent of project verification controls.
pub(crate) fn directory(env: &Environment) -> PathBuf {
    env.daemon_state_dir().join("repair-publication")
}

/// Capture the caller's committed HEAD even when its message names no story.
pub(crate) fn enqueue<S: Store>(ctx: &Ctx<'_, S>) -> Result<(), AppError> {
    let Some(_automation) = super::automations::enter(ctx.store(), ctx.env(), ctx.project())?
    else {
        return Ok(());
    };
    let Some(lease) = cleanup_lease::marker_at_registered(
        ctx.env()
            .subprocess_bound(std::time::Duration::from_secs(60)),
        ctx.cwd(),
    )?
    else {
        return Ok(());
    };
    let (project, links) = ctx.store().read(|tx| {
        let project = tx.project(ctx.project())?.ok_or_else(|| {
            crate::store::StoreError::NotFound("publication project disappeared".into())
        })?;
        let number = StoryNo::parse_id(&project.prefix, &lease.story_id)?;
        let links = tx.open_pr_links_for_story(project.id, number)?;
        Ok((project, links))
    })?;
    if project.slug != lease.project_slug {
        return Err(AppError::Validation(
            "publication lease belongs to another project".into(),
        ));
    }
    let links: Vec<_> = links.into_iter().filter(|p| p.close_on_merge).collect();
    if links.is_empty() {
        return Ok(());
    }
    if links.len() != 1 {
        return Err(AppError::Validation(format!(
            "{} has multiple publication PRs",
            lease.story_id
        )));
    }
    let head = git::text_with_bound(
        ctx.env()
            .subprocess_bound(std::time::Duration::from_secs(60)),
        ctx.cwd(),
        &["rev-parse", "--verify", "HEAD^{commit}"],
    )?
    .trim()
    .to_owned();
    let request = Request {
        version: 1,
        automation_generation: ctx
            .store()
            .read(|tx| Ok(tx.settings(ctx.project())?.automations_after))?,
        lease,
        head,
        pull_request: links[0].url.clone(),
        retry_at: None,
        error: None,
    };
    let root = directory(ctx.env());
    std::fs::create_dir_all(&root)?;
    let path = root.join(format!(
        "{}-{}-{}.json",
        project.id.get(),
        request.lease.story_id,
        request.head
    ));
    let mut file = tempfile::NamedTempFile::new_in(&root)?;
    file.write_all(&serde_json::to_vec(&request)?)?;
    file.as_file().sync_all()?;
    match file.persist_noclobber(&path) {
        Ok(_) => std::fs::File::open(&root)?.sync_all()?,
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.error.into()),
    }
    Ok(())
}

/// Read retained requests after startup or a notification, without losing failures.
pub(crate) fn pending(env: &Environment) -> Result<Vec<(PathBuf, Request)>, AppError> {
    let root = directory(env);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut requests = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let request: Request = serde_json::from_slice(&std::fs::read(&path)?).map_err(|e| {
            AppError::Storage(format!(
                "invalid publication request {}: {e}",
                path.display()
            ))
        })?;
        if request.version != 1 {
            return Err(AppError::Storage(format!(
                "unsupported publication request {}",
                path.display()
            )));
        }
        requests.push((path, request));
    }
    requests.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(requests)
}

/// Atomically retain retry evidence. A duplicate notification cannot overwrite it.
pub(crate) fn retain(path: &Path, request: &Request) -> Result<(), AppError> {
    let root = path
        .parent()
        .ok_or_else(|| AppError::Storage("publication path has no parent".into()))?;
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(&serde_json::to_vec(request)?)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_on_a_newer_base_but_absent_from_the_merged_pr_is_preserved() {
        let workspace = storyhook_test_support::StoryWorkspace::new("SH-884", true);
        let merged_head = git::text(&workspace.worktree, &["rev-parse", "HEAD"]).unwrap();
        let out = crate::env::git_env::command(&workspace.worktree)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-qm",
                "repair after PR publication",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        git::text(
            &workspace.checkout,
            &["merge", "--no-edit", &workspace.branch],
        )
        .unwrap();
        git::text(&workspace.checkout, &["push", "origin", "dev"]).unwrap();
        let error =
            guard_tip(&workspace.checkout, &workspace.branch, merged_head.trim()).unwrap_err();
        assert!(error.to_string().contains("unpublished repair"));
        assert!(workspace.local_branch_exists());
        let repaired_head = git::text(&workspace.worktree, &["rev-parse", "HEAD"]).unwrap();
        guard_tip(&workspace.checkout, &workspace.branch, repaired_head.trim()).unwrap();
    }
}
