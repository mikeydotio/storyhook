//! Common-Git retention authority survives linked-worktree removal.
use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(super) const ROOT: &str = "storyhook-retained-products-v2";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetentionMode {
    #[default]
    DryRun,
    Apply,
}
fn default_keep() -> usize {
    2
}
fn default_days() -> u64 {
    7
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionConfig {
    #[serde(default)]
    pub mode: RetentionMode,
    #[serde(default = "default_keep")]
    pub keep: usize,
    #[serde(default = "default_days")]
    pub min_age_days: u64,
    /// Project-owned foreground runner, without subcommand or generated arguments.
    pub runner: Vec<String>,
}
impl RetentionConfig {
    pub(super) fn validate(&self) -> Result<(), AppError> {
        if self.keep == 0
            || self.min_age_days == 0
            || self.min_age_days > 36500
            || self.runner.is_empty()
            || self.runner.iter().any(String::is_empty)
        {
            return Err(refusal("invalid retention policy or runner"));
        }
        Ok(())
    }
}

fn ensure_directory(path: &Path) -> Result<Identity, AppError> {
    match fs::create_dir(path) {
        Ok(()) => {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            open_directory(
                path.parent()
                    .ok_or_else(|| refusal("missing authority parent"))?,
            )?
            .sync_all()?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let expected = Identity::private_directory(path)?;
    if Identity::of(&open_directory(path)?.metadata()?) != expected {
        return Err(refusal("retention authority changed"));
    }
    Ok(expected)
}

fn register(path: &Path, value: &Value) -> Result<(), AppError> {
    use std::io::Write;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(&serde_json::to_vec(value).map_err(|e| refusal(&e.to_string()))?)?;
            file.sync_all()?;
            open_directory(
                path.parent()
                    .ok_or_else(|| refusal("missing manifest parent"))?,
            )?
            .sync_all()?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if read_json::<Value>(path)? != *value {
                return Err(refusal("registered retention ownership changed"));
            }
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

pub(super) struct Destination {
    pub source: PathBuf,
    pub proof: Value,
    common: PathBuf,
    common_identity: Identity,
    project: PathBuf,
    project_identity: Identity,
    namespace: PathBuf,
    namespace_identity: Identity,
    manifest: Value,
    source_identity: Identity,
    source_manifest: Value,
    _lock: File,
}
impl Destination {
    pub fn check(&self) -> Result<(), AppError> {
        self.common_identity.check(&self.common)?;
        self.namespace_identity.check(&self.namespace)?;
        self.project_identity.check(&self.project)?;
        self.source_identity.check(&self.source)?;
        if read_json::<Value>(&self.project.join("namespace.json"))? != self.manifest
            || read_json::<Value>(&self.source.join("source.json"))? != self.source_manifest
            || Identity::of(&private_file(&self.project.join("retention.lock"), false)?.metadata()?)
                != Identity::of(&self._lock.metadata()?)
        {
            return Err(refusal("retention namespace changed before detach"));
        }
        Ok(())
    }
}

pub(super) fn prepare<S: Store>(
    ctx: &Ctx<'_, S>,
    private: &Path,
    enrollment: &Enrollment,
    custody: Vec<Value>,
) -> Result<Destination, AppError> {
    let nonce = enrollment
        .nonce
        .as_deref()
        .filter(|n| n.len() == 32 && n.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| refusal("common retention requires fresh nonce enrollment"))?;
    let uuid = ctx.store().read(|tx| {
        Ok(tx
            .project(ctx.project())?
            .ok_or_else(|| StoreError::NotFound("retention project disappeared".into()))?
            .uuid)
    })?;
    let pointer: toml::Value = toml::from_str(&fs::read_to_string(
        enrollment.lease.worktree_path.join(".storyhook.toml"),
    )?)
    .map_err(|e| refusal(&e.to_string()))?;
    if enrollment.project_uuid.as_deref() != Some(uuid.as_str())
        || pointer.get("uuid").and_then(toml::Value::as_str) != Some(uuid.as_str())
    {
        return Err(refusal(
            "retention enrollment belongs to a different project",
        ));
    }
    let bound = ctx.env().subprocess_bound(Duration::from_secs(30));
    let common = PathBuf::from(
        super::super::resources::git::text_with_bound(
            bound,
            &enrollment.lease.worktree_path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?
        .trim(),
    );
    let common_identity = Identity::directory(&common)?;
    if Identity::of(&open_directory(&common)?.metadata()?) != common_identity {
        return Err(refusal("common Git directory changed"));
    }
    let namespace = common.join(ROOT);
    let namespace_identity = ensure_directory(&namespace)?;
    let project = namespace.join(format!("project-{:x}", Sha256::digest(uuid.as_bytes())));
    let project_identity = ensure_directory(&project)?;
    let lock = private_file(&project.join("retention.lock"), true)?;
    lock.try_lock_exclusive()
        .map_err(|e| refusal(&format!("retention namespace busy: {e}")))?;
    let manifest = json!({"version":2,"project_uuid":uuid,"common_git":{"path":common,"identity":common_identity},"directory":project_identity});
    register(&project.join("namespace.json"), &manifest)?;
    let source = project.join(format!("source-{nonce}"));
    let source_identity = ensure_directory(&source)?;
    let source_manifest = json!({"version":2,"project_uuid":uuid,"nonce":nonce,"directory":source_identity,
        "private_git":{"path":private,"identity":enrollment.private_git},"worktree":enrollment.worktree,
        "lease":enrollment.lease,"config":enrollment.config});
    register(&source.join("source.json"), &source_manifest)?;
    let proof = json!({"namespace":manifest,"source":source_manifest,"custody":custody});
    Ok(Destination {
        source,
        proof,
        common,
        common_identity,
        project,
        project_identity,
        namespace,
        namespace_identity,
        manifest,
        source_identity,
        source_manifest,
        _lock: lock,
    })
}

/// Called by the existing due-project worker, never by closure retries.
/// The caller owns the automation permit through this bounded foreground action.
pub fn scheduled<S: Store>(ctx: &Ctx<'_, S>) -> Result<Option<String>, AppError> {
    let Some(config) = config(ctx.cwd())? else {
        return Ok(None);
    };
    let Some(policy) = &config.retention else {
        return Ok(None);
    };
    let uuid = ctx.store().read(|tx| {
        Ok(tx
            .project(ctx.project())?
            .ok_or_else(|| StoreError::NotFound("retention project disappeared".into()))?
            .uuid)
    })?;
    let pointer: toml::Value =
        toml::from_str(&fs::read_to_string(ctx.cwd().join(".storyhook.toml"))?)
            .map_err(|e| refusal(&e.to_string()))?;
    if pointer.get("uuid").and_then(toml::Value::as_str) != Some(&uuid) {
        return Err(refusal("retention checkout belongs to a different project"));
    }
    let bound = ctx.env().subprocess_bound(Duration::from_secs(30));
    let common = PathBuf::from(
        super::super::resources::git::text_with_bound(
            bound,
            ctx.cwd(),
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?
        .trim(),
    );
    let identity = Identity::directory(&common)?;
    let mut command = Command::new(&policy.runner[0]);
    command
        .args(&policy.runner[1..])
        .current_dir(ctx.cwd())
        .args(["prune-common", "--project-uuid", &uuid, "--common-git"])
        .arg(&common)
        .arg("--common-dev")
        .arg(identity.dev.to_string())
        .arg("--common-ino")
        .arg(identity.ino.to_string())
        .arg("--keep")
        .arg(policy.keep.to_string())
        .arg("--min-age-days")
        .arg(policy.min_age_days.to_string());
    if policy.mode == RetentionMode::Apply {
        command.arg("--apply");
    }
    let result = crate::process::run_captured_quiescent(
        command,
        ctx.env()
            .subprocess_bound(Duration::from_secs(config.timeout_seconds)),
        crate::process::TerminationPolicy::Kill,
    )
    .map_err(|e| refusal(&format!("scheduled retention unresolved: {}", e.detail())))?;
    if !result.status.success() {
        return Err(refusal(&format!(
            "scheduled retention failed: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(Some(String::from_utf8_lossy(&result.stdout).into_owned()))
}
