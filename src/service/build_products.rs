//! Optional project-owned build reclamation. The store lock covers only rename;
//! no project code, recursive traversal or subprocess executes under that lock.
use super::{Ctx, project_prefix, resolve_story};
use crate::domain::{StoryCleanupLease, StoryEvent};
use crate::error::AppError;
use crate::store::{ReadOps, Store, StoreError, StoryNo};
use fs4::FileExt;
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const ENROLLMENT: &str = "storyhook-products-enrollment-v1.json";
const QUARANTINE: &str = "storyhook-detached-products-v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    /// A single private directory name, never a generic Rust default.
    pub path: String,
    pub managed_entry: String,
    /// Foreground argv. Receives only a detached journal path as its last arg.
    pub hook: Vec<String>,
    pub timeout_seconds: u64,
}
impl Config {
    fn validate(&self) -> Result<(), AppError> {
        let mut parts = Path::new(&self.path).components();
        if !matches!(parts.next(), Some(Component::Normal(_)))
            || parts.next().is_some()
            || self.path.starts_with('.')
            || self.hook.is_empty()
            || self.hook.iter().any(|s| s.is_empty())
            || self.managed_entry.is_empty()
            || !(1..=300).contains(&self.timeout_seconds)
        {
            return Err(refusal(
                "invalid [build_products] path, entry, hook or timeout",
            ));
        }
        Ok(())
    }
}
fn config(root: &Path) -> Result<Option<Config>, AppError> {
    let text = fs::read_to_string(root.join(".storyhook.toml"))?;
    let value: toml::Value = toml::from_str(&text).map_err(|e| refusal(&e.to_string()))?;
    let Some(value) = value.get("build_products") else {
        return Ok(None);
    };
    let config: Config = value
        .clone()
        .try_into()
        .map_err(|e: toml::de::Error| refusal(&e.to_string()))?;
    config.validate()?;
    Ok(config.enabled.then_some(config))
}
fn refusal(message: &str) -> AppError {
    AppError::Validation(format!("build products retained: {message}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    dev: u64,
    ino: u64,
}
impl Identity {
    fn of(meta: &fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }
    fn directory(path: &Path) -> Result<Self, AppError> {
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
            return Err(refusal("unowned or symlinked directory"));
        }
        Ok(Self::of(&meta))
    }
    fn check(self, path: &Path) -> Result<(), AppError> {
        if Self::directory(path)? != self {
            return Err(refusal("directory identity changed"));
        }
        Ok(())
    }
}
fn private_file(path: &Path, create: bool) -> Result<File, AppError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
    {
        return Err(refusal("unsafe private authority file"));
    }
    Ok(file)
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, AppError> {
    serde_json::from_reader(private_file(path, false)?).map_err(|e| refusal(&e.to_string()))
}
fn publish_at(parent: &File, value: &impl Serialize) -> Result<(), AppError> {
    use std::io::Write;
    use std::os::fd::FromRawFd;
    let temporary = CString::new(format!(".journal-{}", uuid::Uuid::new_v4())).expect("UUID");
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(&serde_json::to_vec(value).map_err(|e| refusal(&e.to_string()))?)?;
    file.sync_all()?;
    if unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            temporary.as_ptr(),
            parent.as_raw_fd(),
            c"journal.json".as_ptr(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    parent.sync_all()?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enrollment {
    version: u32,
    lease: StoryCleanupLease,
    config: Config,
    worktree: Identity,
    private_git: Identity,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u32,
    generation: i64,
    lease: StoryCleanupLease,
    directory: Identity,
    product: Identity,
    state: String,
}

pub(crate) fn generation(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
    story: StoryNo,
) -> Result<Option<(i64, StoryCleanupLease)>, StoreError> {
    if !tx.automations_enabled(project)? {
        return Ok(None);
    }
    let Some(row) = tx.story(project, story)? else {
        return Ok(None);
    };
    if row.state != "verifying" {
        return Ok(None);
    }
    super::story_reset::refuse_reserved(tx, project, story)?;
    let events = tx.events_for(project, story)?;
    let Some(index) = events.iter().rposition(|e| matches!(e.known(), Some(StoryEvent::StoryStateChanged { state, .. }) if state == "verifying")) else { return Ok(None) };
    let Some(StoryEvent::StoryCleanupLeaseRecorded { lease, .. }) =
        events.get(index + 1).and_then(|e| e.known())
    else {
        return Ok(None);
    };
    if !super::automations::permits_generation(tx, project, Some(events[index].global_seq))? {
        return Ok(None);
    }
    Ok(Some((
        events[index].global_seq.get(),
        lease.as_ref().clone(),
    )))
}

/// Only the exact generation captured by the committed submission may detach.
/// Unknown/legacy worktrees simply retain their products. Errors are diagnostics,
/// not failure of the already-committed state transition.
pub fn reclaim_handoff<S: Store>(
    ctx: &Ctx<'_, S>,
    id: &str,
    expected: (i64, StoryCleanupLease),
) -> Result<(), AppError> {
    if !ctx.hooks_enabled() {
        return Ok(());
    }
    let project = ctx.project();
    let Some(_automation) = super::automations::enter(ctx.store(), ctx.env(), project)? else {
        return Ok(());
    };
    let Some(story) = ctx.store().read(|tx| {
        if !tx.automations_enabled(project)? {
            return Ok(None);
        }
        let prefix = project_prefix(tx, project)?;
        let (story, _) = resolve_story(tx, project, &prefix, id)?;
        Ok((generation(tx, project, story)?.as_ref() == Some(&expected)).then_some(story))
    })?
    else {
        return Ok(());
    };
    let (seq, lease) = &expected;
    let worktree = &lease.worktree_path;
    let Some(config) = config(worktree)? else {
        return Ok(());
    };
    let bound = ctx.env().subprocess_bound(Duration::from_secs(30));
    let git = |args: &[&str]| {
        super::resources::git::text_with_bound(bound, worktree, args).map(|s| s.trim().to_string())
    };
    let private = PathBuf::from(git(&["rev-parse", "--absolute-git-dir"])?);
    let enrollment_path = private.join(ENROLLMENT);
    if !enrollment_path.try_exists()? {
        return Ok(());
    }
    let enrollment: Enrollment = read_json(&enrollment_path)?;
    if enrollment.version != 1 || enrollment.lease != *lease || enrollment.config != config {
        return Err(refusal(
            "dispatch enrollment or project configuration changed",
        ));
    }
    enrollment.worktree.check(worktree)?;
    enrollment.private_git.check(&private)?;
    let Some(_workspace) =
        super::workspace_lock::WorkspaceLock::try_acquire_with_bound(bound, worktree, id)?
    else {
        return Ok(());
    };
    if super::cleanup_lease::marker_at_registered(bound, worktree)?.as_ref() != Some(lease) {
        return Err(refusal("cleanup lease changed"));
    }
    // A later source commit must not turn a configured cache into tracked source.
    if !git(&["ls-files", "--", &config.path])?.is_empty() {
        return Err(refusal("configured products contain tracked source"));
    }
    let custody = private.join("storyhook-build-products-v1");
    let custody_identity = Identity::directory(&custody)?;
    let product_lock = private_file(&custody.join("products.lock"), false)?;
    if let Err(error) = product_lock.try_lock_exclusive() {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(());
        }
        return Err(error.into());
    }
    let lock_identity = Identity::of(&product_lock.metadata()?);
    // This is the same permanent inode and journal contract as managed-cargo.
    for entry in fs::read_dir(&custody)? {
        let entry = entry?;
        if entry.file_name() == "products.lock" {
            continue;
        }
        if !entry.file_name().to_string_lossy().starts_with("build-") {
            return Err(refusal("unknown custody record"));
        }
        Identity::directory(&entry.path())?;
        let row: serde_json::Value = read_json(&entry.path().join("record.json"))?;
        let token = entry
            .file_name()
            .to_string_lossy()
            .trim_start_matches("build-")
            .to_string();
        if !settled_record(&row, &token) {
            return Err(refusal("unfinished or unknown whole-build owner"));
        }
    }
    let original = worktree.join(&config.path);
    if !original.try_exists()? {
        return Ok(());
    }
    let product = Identity::directory(&original)?;
    // Git-private quarantine must be on the same filesystem: never copy/delete.
    if product.dev != enrollment.private_git.dev {
        return Err(refusal(
            "products and quarantine are on different filesystems",
        ));
    }
    let quarantine = private.join(QUARANTINE);
    match fs::create_dir(&quarantine) {
        Ok(()) => fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700))?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let quarantine_identity = Identity::directory(&quarantine)?;
    let job = quarantine.join(uuid::Uuid::new_v4().to_string());
    fs::create_dir(&job)?;
    fs::set_permissions(&job, fs::Permissions::from_mode(0o700))?;
    let journal_path = job.join("journal.json");
    let mut journal = Journal {
        version: 1,
        generation: *seq,
        lease: lease.clone(),
        directory: Identity::directory(&job)?,
        product,
        state: "prepared".into(),
    };
    let source_parent = open_directory(worktree)?;
    let destination_parent = open_directory(&job)?;
    if Identity::of(&destination_parent.metadata()?) != journal.directory {
        return Err(refusal("journal directory changed"));
    }
    publish_at(&destination_parent, &journal)?;
    // No external code here. Reset and repair transitions serialize with this
    // exact generation check and atomic rename. Long purge occurs after commit.
    ctx.store().write(|tx| {
        if generation(&*tx, project, story)?.as_ref() != Some(&expected) {
            return Err(StoreError::from(refusal(
                "Verifying generation changed before detach",
            )));
        }
        let detach = || -> Result<(), AppError> {
            enrollment.worktree.check(worktree)?;
            enrollment.private_git.check(&private)?;
            if Identity::of(&source_parent.metadata()?) != enrollment.worktree
                || Identity::of(&destination_parent.metadata()?) != journal.directory
            {
                return Err(refusal("anchored directory identity changed"));
            }
            quarantine_identity.check(&quarantine)?;
            journal.directory.check(&job)?;
            product.check(&original)?;
            custody_identity.check(&custody)?;
            if Identity::of(&private_file(&custody.join("products.lock"), false)?.metadata()?)
                != lock_identity
            {
                return Err(refusal("product lock inode changed"));
            }
            let fresh: Enrollment = read_json(&enrollment_path)?;
            let marker: StoryCleanupLease =
                read_json(&private.join(crate::domain::CLEANUP_LEASE_MARKER))?;
            if fresh.lease != *lease
                || fresh.config != config
                || fresh.worktree != enrollment.worktree
                || fresh.private_git != enrollment.private_git
                || marker != *lease
            {
                return Err(refusal("enrollment or cleanup marker changed"));
            }
            if self::config(worktree)?.as_ref() != Some(&config) {
                return Err(refusal("configuration changed before detach"));
            }
            detach_at(&source_parent, &config.path, &destination_parent, product)?;
            source_parent.sync_all()?;
            destination_parent.sync_all()?;
            Ok(())
        };
        detach().map_err(StoreError::from)
    })?;
    journal.state = "detached".into();
    publish_at(&destination_parent, &journal)?;
    drop(product_lock);
    drop(_workspace);
    journal.directory.check(&job)?;
    run_hook(ctx, worktree, &config, &journal_path)
}
fn run_hook<S: Store>(
    ctx: &Ctx<'_, S>,
    worktree: &Path,
    config: &Config,
    journal: &Path,
) -> Result<(), AppError> {
    let mut command = Command::new(&config.hook[0]);
    command
        .args(&config.hook[1..])
        .arg(journal)
        .current_dir(worktree)
        .env("STORYHOOK_DETACHED_PRODUCTS_PROTOCOL", "1");
    let result = crate::process::run_captured_quiescent(
        command,
        ctx.env()
            .subprocess_bound(Duration::from_secs(config.timeout_seconds)),
        crate::process::TerminationPolicy::Kill,
    )
    .map_err(|e| refusal(&format!("detached purge unresolved: {}", e.detail())))?;
    if !result.status.success() {
        return Err(refusal(&format!(
            "detached purge failed; retain {}: {}",
            journal.display(),
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(())
}

fn open_directory(path: &Path) -> Result<File, AppError> {
    // Walk from / with directory descriptors, refusing symlinks in every ancestor.
    if !path.is_absolute() {
        return Err(refusal("directory is not absolute"));
    }
    let mut current = File::open("/")?;
    for component in path.components() {
        match component {
            Component::RootDir => continue,
            Component::Normal(name) => {
                use std::os::unix::ffi::OsStrExt;
                let name =
                    CString::new(name.as_bytes()).map_err(|_| refusal("invalid directory name"))?;
                let fd = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                use std::os::fd::FromRawFd;
                current = unsafe { File::from_raw_fd(fd) };
            }
            _ => return Err(refusal("non-canonical directory components")),
        }
    }
    Ok(current)
}
fn child_identity(parent: &File, name: &str) -> Result<Identity, AppError> {
    let name = CString::new(name).map_err(|_| refusal("invalid product name"))?;
    let mut meta = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            meta.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let meta = unsafe { meta.assume_init() };
    if meta.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(refusal("product is not a directory"));
    }
    Ok(Identity {
        dev: meta.st_dev as u64,
        ino: meta.st_ino as u64,
    })
}
fn detach_at(
    source: &File,
    name: &str,
    destination: &File,
    expected: Identity,
) -> Result<(), AppError> {
    if child_identity(source, name)? != expected {
        return Err(refusal("product identity changed before rename"));
    }
    let source_name = CString::new(name).map_err(|_| refusal("invalid product name"))?;
    let destination_name = c"products";
    if unsafe {
        libc::renameat(
            source.as_raw_fd(),
            source_name.as_ptr(),
            destination.as_raw_fd(),
            destination_name.as_ptr(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if child_identity(destination, "products")? != expected {
        return Err(refusal(
            "detached identity changed; preserve journal for review",
        ));
    }
    Ok(())
}
fn settled_record(row: &serde_json::Value, token: &str) -> bool {
    token.len() == 32
        && token.bytes().all(|b| b.is_ascii_hexdigit())
        && row["version"] == 1
        && row["id"] == token
        && row["token"] == token
        && row["state"] == "finished"
        && row["executions"] == serde_json::json!([])
        && row["command"]
            .as_array()
            .is_some_and(|v| !v.is_empty() && v.iter().all(|x| x.as_str().is_some()))
        && row["owner"]["pid"].as_u64().is_some_and(|p| p > 1)
        && row["owner"]["start"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        && row["owner"]["boot"].as_str().is_some_and(|s| !s.is_empty())
        && row["settled_execution"].as_object().is_some_and(|e| {
            e.get("id")
                .and_then(|x| x.as_str())
                .is_some_and(|s| !s.is_empty())
                && e.get("guard").and_then(|x| x.as_str())
                    == Some(format!("lease-{token}.lock").as_str())
                && e.get("session")
                    .and_then(|x| x.as_u64())
                    .is_some_and(|p| p > 1)
        })
}
