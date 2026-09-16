//! Project identity and checkout authority for GitHub operations.

use super::{Ctx, project};
use crate::error::AppError;
use crate::github_access::Repository;
use crate::store::{ReadOps, Store, StoreError};
#[cfg(feature = "github-pr")]
use serde::Deserialize;

#[cfg(feature = "github-pr")]
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct GithubSettings {
    poll: bool,
}

/// Reads the project's explicit polling consent, independently of credentials.
#[cfg(feature = "github-pr")]
pub(crate) fn poll_enabled<S: Store>(ctx: &Ctx<'_, S>) -> Result<bool, AppError> {
    let (record, checkout) = ctx.store().read(|tx| {
        Ok((
            tx.project(ctx.project())?
                .ok_or_else(|| StoreError::NotFound("project not found".into()))?,
            tx.checkout_path(ctx.project())?,
        ))
    })?;
    let Some(checkout) = checkout else {
        return Ok(false);
    };
    let Some(pointer) = project::read_pointer(&checkout)? else {
        return Ok(false);
    };
    if pointer.uuid != record.uuid {
        return Err(AppError::Validation(format!(
            "GitHub configuration in {} belongs to another project",
            checkout.display()
        )));
    }
    let Some(table) = pointer.github else {
        return Ok(false);
    };
    if table.get("api_url").is_some() {
        return Err(AppError::Validation(format!(
            "[github].api_url in {} is obsolete; remove it and configure the intended HTTPS origin. gh derives the API endpoint from that host",
            checkout.display()
        )));
    }
    let settings: GithubSettings = table.try_into().map_err(|error: toml::de::Error| {
        AppError::Validation(format!(
            "invalid [github] configuration in {}: {}",
            checkout.display(),
            error.message()
        ))
    })?;
    Ok(settings.poll)
}

/// Resolves the registered checkout, never a historical remote or daemon cwd.
pub(crate) fn repository<S: Store>(ctx: &Ctx<'_, S>) -> Result<Repository, AppError> {
    let (record, checkout) = ctx.store().read(|tx| {
        let record = tx
            .project(ctx.project())?
            .ok_or_else(|| StoreError::NotFound("project not found".into()))?;
        Ok((record, tx.checkout_path(ctx.project())?))
    })?;
    let checkout = checkout.ok_or_else(|| AppError::Validation(format!(
        "project {} has no registered checkout; use story project link checkout PATH before GitHub operations", record.slug)))?;
    let pointer = project::read_pointer(&checkout)?.ok_or_else(|| AppError::Validation(format!(
        "registered checkout {} has no .storyhook.toml; restore the project pointer before GitHub operations", checkout.display())))?;
    if pointer.uuid != record.uuid {
        return Err(AppError::Validation(format!(
            "registered checkout {} names project {}, but {} requires {}; refusing cross-project GitHub access",
            checkout.display(),
            pointer.uuid,
            record.slug,
            record.uuid
        )));
    }
    let repository = Repository::resolve(&checkout)?;
    // A matching local pointer identifies an explicit project lane. An unrelated
    // cwd (including the daemon's) cannot replace the registered authority.
    for root in project::ancestors(ctx.cwd()) {
        if let Some(pointer) = project::read_pointer(&root)? {
            if pointer.uuid == record.uuid {
                let local = Repository::resolve(&root)?;
                if local.identity() != repository.identity() {
                    return Err(AppError::Validation(format!(
                        "project lane {} has a different origin from registered checkout {}",
                        root.display(),
                        checkout.display()
                    )));
                }
            }
            break;
        }
    }
    Ok(repository)
}
