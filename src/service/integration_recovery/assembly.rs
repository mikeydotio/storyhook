//! Native assembly has private filesystem custody, never publication authority.
//!
//! Failed or interrupted work stays at the durably reserved path. There is no
//! Drop cleanup or JSON-to-capability path: reconciliation must prove custody
//! separately. Store permission and pathname checks are not an atomic fence
//! against a concurrently running filesystem operation; the caller must also
//! retain its central worker slot until all children have drained.
use super::{
    AssemblyClaim, BoundIntegrationProposal, IntegrationFile, IntegrationOwnerService,
    IntegrationPlan, SubmissionObservation, batch_smoothing, policy_from_pointer,
};
use crate::{
    error::AppError,
    process::{Cancellation, Captured, run_captured_query_quiescent},
    service::trial_merge::{
        BlobSource, TrialMerge, answer_oid, entry_answer, entry_spec, merge_answer,
    },
    store::Store,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

const LABEL: &str = "managed integration assembly";
const STAMP: &str = "storyhook-assembly.json";
const ANSWER_LIMIT: u64 = 8 * 1024 * 1024;

/// Observable filesystem identity; serializing it cannot acquire custody.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssemblyPathIdentity {
    /// Exact original path.
    pub path: PathBuf,
    /// Original filesystem device.
    pub device: u64,
    /// Original inode, kept alive by the opaque native capability.
    pub inode: u64,
}

/// Durable evidence to accompany a later, separately authorized publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssemblyEvidence {
    /// Evidence format, not an authority envelope.
    pub version: u8,
    /// Durable integration owner.
    pub owner: String,
    /// Exact assembly claim ordinal.
    pub epoch: u32,
    /// Reserved managed branch; no ref is written by assembly.
    pub branch: String,
    /// Original private repository directory.
    pub workspace: AssemblyPathIdentity,
    /// Digest of the create-new ownership stamp.
    pub stamp_sha256: String,
    /// Origin-bound original submission.
    pub submission: SubmissionObservation,
    /// Immutable policy, source blob identities and deterministic byte digests.
    pub plan: IntegrationPlan,
    /// Resolution commit, with exactly base then original head as its parents.
    pub commit: String,
    /// Exact resolution tree that still needs central certification.
    pub tree: String,
    /// Actual configured author identity including Git's timestamp.
    pub author: String,
    /// Actual configured committer identity including Git's timestamp.
    pub committer: String,
}

/// Only successful native assembly constructs this non-deserializable value.
/// Its evidence alone cannot authorize publication or recreate a lost claim.
///
/// ```compile_fail
/// use storyhook::service::integration_recovery::NativeAssembly;
/// let _: NativeAssembly = serde_json::from_str("{}").unwrap();
/// ```
pub struct NativeAssembly {
    evidence: AssemblyEvidence,
    custody: Custody,
    settlement: SettlementSnapshot,
}

impl NativeAssembly {
    /// Reviewable evidence; not a gate, landing or publication receipt.
    pub fn evidence(&self) -> &AssemblyEvidence {
        &self.evidence
    }

    /// Explicitly remove only the original live private assembly after all
    /// native effects have proven drained. The caller must retain exclusive
    /// central custody through this call; this handle does not prove quiescence.
    /// No historical receipt can reconstruct cleanup authority. Any uncertainty
    /// retains remaining residue; there is deliberately no Drop cleanup.
    pub fn settle(self) -> Result<(), AppError> {
        self.settle_with(|_| Ok(()))
    }

    fn settle_with(
        self,
        mut before_unlink: impl FnMut(usize) -> Result<(), AppError>,
    ) -> Result<(), AppError> {
        let mut removed = 0usize;
        let result = (|| {
            self.settlement.validate(&self.custody)?;
            let root = self.custody.directory(&self.custody.repository)?;
            let parent_path = self
                .custody
                .repository
                .parent()
                .ok_or_else(|| refuse("assembly parent missing"))?;
            let parent = self.custody.directory(parent_path)?;
            let mut paths: Vec<_> = self
                .settlement
                .nodes
                .keys()
                .filter(|path| path.as_path() != Path::new(STAMP))
                .collect();
            paths.sort_by(|a, b| {
                b.components()
                    .count()
                    .cmp(&a.components().count())
                    .then_with(|| a.cmp(b))
            });
            for path in paths {
                before_unlink(removed)?;
                parent.validate()?;
                root.validate()?;
                remove_inventoried(&root.file, path, &self.settlement)?;
                removed += 1;
            }
            // Keep the original stamp until all other inventoried entries are
            // gone and no unexpected addition is visible in the owned root.
            before_unlink(removed)?;
            if directory_names(&root.file)? != vec![std::ffi::OsString::from(STAMP)] {
                return Err(refuse("unexpected entry appeared during assembly cleanup"));
            }
            parent.validate()?;
            root.validate()?;
            remove_inventoried(&root.file, Path::new(STAMP), &self.settlement)?;
            removed += 1;
            let name = self
                .custody
                .repository
                .file_name()
                .ok_or_else(|| refuse("assembly root lacks basename"))?;
            let observed = open_entry(&parent.file, name)?;
            let metadata = observed.metadata().map_err(storage)?;
            if !metadata.is_dir()
                || metadata.dev() != root.identity.device
                || metadata.ino() != root.identity.inode
            {
                return Err(refuse("assembly root changed before final removal"));
            }
            unlink_child(&parent.file, name, true)
        })();
        result.map_err(|error| refuse(&format!(
            "cleanup incomplete at {}; removed {removed} entries; remaining residue retained: {error}",
            self.custody.repository.display()
        )))
    }

    /// Recheck the still-held native filesystem objects before a later phase.
    /// The caller must separately acquire that phase's durable permission.
    pub fn validate_custody(&self) -> Result<(), AppError> {
        self.custody.validate()
    }
}

/// Assemble only a live durable claim and an actual native proposal. This does
/// not resume Assembling JSON, update a ref, fetch remotely, or publish a PR.
pub fn assemble_owned<S: Store>(
    service: &IntegrationOwnerService<'_, S>,
    claim: &AssemblyClaim,
    proof: &BoundIntegrationProposal,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<NativeAssembly, AppError> {
    validate_binding(claim, proof)?;
    let permitted = || {
        if service.assembly_permitted(claim, proof)? {
            Ok(())
        } else {
            Err(refuse("durable assembly permission was revoked"))
        }
    };
    assemble(
        Inputs {
            owner: claim.id(),
            epoch: claim.epoch(),
            branch: claim.branch(),
            workspace: claim.workspace(),
            submission: claim.submission(),
            plan: claim.plan(),
        },
        &permitted,
        deadline,
        cancellation,
    )
}

fn validate_binding(
    claim: &AssemblyClaim,
    proof: &BoundIntegrationProposal,
) -> Result<(), AppError> {
    let candidate = claim.candidate();
    let original = candidate
        .pull_request
        .as_ref()
        .map_err(|_| refuse("missing original PR"))?;
    if claim.plan() != proof.plan()
        || claim.submission() != proof.submission()
        || candidate.checkout != proof.submission().checkout
        || !same_pr(&original.url, &proof.submission().pull_request)?
    {
        return Err(refuse(
            "claim and native proposal name different original submissions",
        ));
    }
    Ok(())
}

fn same_pr(left: &str, right: &str) -> Result<bool, AppError> {
    let left = crate::domain::pr_url::parse_pr_url(left)?;
    let right = crate::domain::pr_url::parse_pr_url(right)?;
    Ok(left.number == right.number
        && left.host.eq_ignore_ascii_case(&right.host)
        && left.owner.eq_ignore_ascii_case(&right.owner)
        && left.repo.eq_ignore_ascii_case(&right.repo))
}

struct Inputs<'a> {
    owner: &'a str,
    epoch: u32,
    branch: &'a str,
    workspace: &'a Path,
    submission: &'a SubmissionObservation,
    plan: &'a IntegrationPlan,
}

struct PinnedDirectory {
    identity: AssemblyPathIdentity,
    file: File,
}

impl PinnedDirectory {
    fn open(path: &Path) -> Result<Self, AppError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_NONBLOCK)
            .open(path)
            .map_err(storage)?;
        let metadata = file.metadata().map_err(storage)?;
        let pin = Self {
            identity: AssemblyPathIdentity {
                path: path.into(),
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            file,
        };
        pin.validate()?;
        Ok(pin)
    }

    fn validate(&self) -> Result<(), AppError> {
        let observed = fs::symlink_metadata(&self.identity.path).map_err(storage)?;
        let held = self.file.metadata().map_err(storage)?;
        if !observed.is_dir()
            || observed.file_type().is_symlink()
            || self.identity.path.canonicalize().map_err(storage)? != self.identity.path
            || observed.dev() != self.identity.device
            || observed.ino() != self.identity.inode
            || held.dev() != observed.dev()
            || held.ino() != observed.ino()
        {
            return Err(refuse("original assembly/source directory was replaced"));
        }
        Ok(())
    }
}

struct Custody {
    repository: PathBuf,
    directories: Vec<PinnedDirectory>,
    stamp: PathBuf,
    stamp_file: File,
    stamp_bytes: Vec<u8>,
}

impl Custody {
    fn validate(&self) -> Result<(), AppError> {
        for directory in &self.directories {
            directory.validate()?;
        }
        // A bare repository must not acquire discovery/common-directory or
        // external-object redirects between effects, even with the same root.
        for relative in [
            ".git",
            "commondir",
            "gitdir",
            "shallow",
            "objects/info/alternates",
        ] {
            match fs::symlink_metadata(self.repository.join(relative)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(storage(error)),
                Ok(_) => return Err(refuse("private Git namespace acquired a redirect")),
            }
        }
        for relative in ["config", "HEAD", "assembly.index"] {
            match fs::symlink_metadata(self.repository.join(relative)) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(storage(error)),
                Ok(_) => {
                    return Err(refuse(
                        "private Git administration acquired a nonregular path",
                    ));
                }
            }
        }
        reject_object_symlinks(&self.repository.join("objects"))?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.stamp)
            .map_err(storage)?;
        let metadata = file.metadata().map_err(storage)?;
        let held = self.stamp_file.metadata().map_err(storage)?;
        if !metadata.is_file()
            || metadata.dev() != held.dev()
            || metadata.ino() != held.ino()
            || metadata.len() != self.stamp_bytes.len() as u64
        {
            return Err(refuse("original assembly stamp was replaced"));
        }
        let mut bytes = Vec::new();
        file.take(self.stamp_bytes.len() as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(storage)?;
        if bytes != self.stamp_bytes {
            return Err(refuse("original assembly stamp changed"));
        }
        Ok(())
    }
}

/// Git may create loose-object and pack subdirectories. None may redirect a
/// later write outside the owned object directory. Inspection refuses observed
/// symlinks; it supplements, rather than replaces, exclusive worker custody.
fn reject_object_symlinks(objects: &Path) -> Result<(), AppError> {
    match fs::symlink_metadata(objects) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage(error)),
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(refuse("private object directory acquired a redirect")),
    }
    let mut pending = vec![objects.to_owned()];
    let mut inspected = 0;
    while let Some(directory) = pending.pop() {
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(storage(error)),
            Ok(_) => return Err(refuse("private object namespace changed during inspection")),
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            // Git can remove its own temporary directories while capture polls.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(storage(error)),
        };
        for entry in entries {
            let entry = entry.map_err(storage)?;
            inspected += 1;
            if inspected > 65_536 {
                return Err(refuse(
                    "private object namespace exceeds the custody inspection bound",
                ));
            }
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(storage(error)),
            };
            if kind.is_symlink() {
                return Err(refuse("private object namespace acquired a symlink"));
            }
            if kind.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

/// Native terminal inventory. It is captured once after successful assembly,
/// never reconstructed from a receipt or refreshed to adopt later additions.
struct SettlementSnapshot {
    nodes: std::collections::BTreeMap<PathBuf, SettlementNode>,
    protected: Vec<(PathBuf, File, String)>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct SettlementNode {
    device: u64,
    inode: u64,
    mode: u32,
    directory: bool,
    length: u64,
    modified: (i64, i64),
    links: u64,
}
impl SettlementNode {
    fn of(metadata: &fs::Metadata) -> Result<Self, AppError> {
        if !metadata.is_dir() && !metadata.is_file() {
            return Err(refuse("settlement refuses nonregular private namespace"));
        }
        if metadata.is_file() && metadata.nlink() != 1 {
            return Err(refuse(
                "settlement refuses shared hard-linked private files",
            ));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            directory: metadata.is_dir(),
            // Removing children changes directory timestamps/link count. Identity
            // remains exact; file identity also includes its terminal metadata.
            length: if metadata.is_dir() { 0 } else { metadata.len() },
            modified: if metadata.is_dir() {
                (0, 0)
            } else {
                (metadata.mtime(), metadata.mtime_nsec())
            },
            links: if metadata.is_dir() {
                0
            } else {
                metadata.nlink()
            },
        })
    }
}
impl SettlementSnapshot {
    fn capture(custody: &Custody) -> Result<Self, AppError> {
        custody.validate()?;
        let root = custody.directory(&custody.repository)?;
        let mut nodes = std::collections::BTreeMap::new();
        inventory(&root.file, Path::new(""), root.identity.device, &mut nodes)?;
        let mut protected = Vec::new();
        for relative in ["config", "HEAD", "assembly.index"] {
            let path = PathBuf::from(relative);
            let file = open_entry(&root.file, path.as_os_str())?;
            let expected = nodes
                .get(&path)
                .ok_or_else(|| refuse("terminal administration file missing"))?;
            if expected.directory
                || SettlementNode::of(&file.metadata().map_err(storage)?)? != *expected
            {
                return Err(refuse(
                    "terminal administration file changed during inventory",
                ));
            }
            let hash = pinned_digest(&file)?;
            protected.push((path, file, hash));
        }
        custody.validate()?;
        Ok(Self { nodes, protected })
    }
    fn validate(&self, custody: &Custody) -> Result<(), AppError> {
        custody.validate()?;
        let root = custody.directory(&custody.repository)?;
        let mut observed = std::collections::BTreeMap::new();
        inventory(
            &root.file,
            Path::new(""),
            root.identity.device,
            &mut observed,
        )?;
        if observed != self.nodes {
            return Err(refuse(
                "private assembly namespace changed after terminal inventory",
            ));
        }
        for (path, held, expected_hash) in &self.protected {
            let file = open_entry(&root.file, path.as_os_str())?;
            let observed = SettlementNode::of(&file.metadata().map_err(storage)?)?;
            if self.nodes.get(path) != Some(&observed)
                || SettlementNode::of(&held.metadata().map_err(storage)?)? != observed
                || pinned_digest(&file)? != *expected_hash
            {
                return Err(refuse("terminal index/config/HEAD custody changed"));
            }
        }
        custody.validate()
    }
}

impl Custody {
    fn directory(&self, path: &Path) -> Result<&PinnedDirectory, AppError> {
        self.directories
            .iter()
            .find(|pin| pin.identity.path == path)
            .ok_or_else(|| refuse("missing original native directory pin"))
    }
}

fn pinned_digest(file: &File) -> Result<String, AppError> {
    use std::os::unix::fs::FileExt;
    let metadata = file.metadata().map_err(storage)?;
    if !metadata.is_file() || metadata.len() > ANSWER_LIMIT {
        return Err(refuse("terminal administration exceeds settlement bound"));
    }
    let mut bytes = vec![0; metadata.len() as usize];
    file.read_exact_at(&mut bytes, 0).map_err(storage)?;
    Ok(digest(&bytes))
}

fn forbidden_settlement_path(path: &Path) -> bool {
    [
        ".git",
        "commondir",
        "gitdir",
        "shallow",
        "config.worktree",
        "worktrees",
        "modules",
        "objects/info/alternates",
        "objects/info/http-alternates",
        "info/grafts",
        "refs/replace",
    ]
    .iter()
    .any(|forbidden| path == Path::new(forbidden) || path.starts_with(Path::new(forbidden)))
        || path
            .extension()
            .is_some_and(|extension| extension == "lock" || extension == "promisor")
}

fn inventory(
    directory: &File,
    relative: &Path,
    device: u64,
    nodes: &mut std::collections::BTreeMap<PathBuf, SettlementNode>,
) -> Result<(), AppError> {
    if relative.components().count() > 128 {
        return Err(refuse("settlement namespace exceeds depth bound"));
    }
    for name in directory_names(directory)? {
        let path = relative.join(&name);
        if forbidden_settlement_path(&path) {
            return Err(refuse(
                "settlement refuses external namespace authority or pending lock",
            ));
        }
        let file = open_entry(directory, &name)?;
        let node = SettlementNode::of(&file.metadata().map_err(storage)?)?;
        if node.device != device {
            return Err(refuse("settlement refuses a mounted private subtree"));
        }
        if nodes.len() >= 65_536 {
            return Err(refuse("settlement namespace exceeds entry bound"));
        }
        let is_directory = node.directory;
        nodes.insert(path.clone(), node);
        if is_directory {
            inventory(&file, &path, device, nodes)?;
        }
    }
    Ok(())
}

fn c_name(name: &std::ffi::OsStr) -> Result<std::ffi::CString, AppError> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(refuse("settlement requires one native child name"));
    }
    std::ffi::CString::new(bytes).map_err(storage)
}
fn open_entry(directory: &File, name: &std::ffi::OsStr) -> Result<File, AppError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_name(name)?;
    // SAFETY: a held native directory descriptor and NUL-terminated single
    // basename. NOFOLLOW prevents redirect traversal; NONBLOCK avoids FIFOs.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(storage(std::io::Error::last_os_error()));
    }
    // SAFETY: this successful openat returned a new uniquely owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn directory_names(directory: &File) -> Result<Vec<std::ffi::OsString>, AppError> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStringExt};
    // Open "." relative to the directory to obtain an independent stream
    // offset. dup alone would share readdir's offset with the retained pin.
    let dot = c".";
    // SAFETY: valid held directory fd and static NUL-terminated name.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            dot.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(storage(std::io::Error::last_os_error()));
    }
    // SAFETY: ownership of fd is transferred to DIR on success.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: fdopendir failed, so fd remains owned here.
        unsafe {
            libc::close(fd);
        }
        return Err(storage(error));
    }
    struct Stream(*mut libc::DIR);
    impl Drop for Stream {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let stream = Stream(stream);
    let mut names = Vec::new();
    loop {
        // SAFETY: errno is thread-local; readdir returns entries owned by this
        // live stream, read before the next readdir call.
        unsafe {
            *errno_location() = 0;
        }
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(0) {
                return Err(storage(error));
            }
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if names.len() >= 65_536 {
            return Err(refuse("settlement directory exceeds entry bound"));
        }
        names.push(std::ffi::OsString::from_vec(name.to_vec()));
    }
    names.sort();
    Ok(names)
}
#[cfg(target_os = "macos")]
unsafe fn errno_location() -> *mut libc::c_int {
    unsafe { libc::__error() }
}
#[cfg(not(target_os = "macos"))]
unsafe fn errno_location() -> *mut libc::c_int {
    unsafe { libc::__errno_location() }
}

fn remove_inventoried(
    root: &File,
    relative: &Path,
    snapshot: &SettlementSnapshot,
) -> Result<(), AppError> {
    let mut parent = root.try_clone().map_err(storage)?;
    let mut prefix = PathBuf::new();
    for component in relative
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .components()
    {
        let std::path::Component::Normal(name) = component else {
            return Err(refuse("nonrelative settlement path"));
        };
        prefix.push(name);
        let next = open_entry(&parent, name)?;
        if snapshot.nodes.get(&prefix)
            != Some(&SettlementNode::of(&next.metadata().map_err(storage)?)?)
        {
            return Err(refuse("settlement ancestor changed"));
        }
        parent = next;
    }
    let name = relative
        .file_name()
        .ok_or_else(|| refuse("settlement path lacks basename"))?;
    let file = open_entry(&parent, name)?;
    let expected = snapshot
        .nodes
        .get(relative)
        .ok_or_else(|| refuse("settlement node was not inventoried"))?;
    if SettlementNode::of(&file.metadata().map_err(storage)?)? != *expected {
        return Err(refuse("settlement target changed before unlink"));
    }
    unlink_child(&parent, name, expected.directory)
}
fn unlink_child(parent: &File, name: &std::ffi::OsStr, directory: bool) -> Result<(), AppError> {
    use std::os::fd::AsRawFd;
    let name = c_name(name)?;
    // SAFETY: one basename relative to a retained directory fd, never a path
    // which can follow a substituted ancestor. Caller checked exact identity.
    if unsafe {
        libc::unlinkat(
            parent.as_raw_fd(),
            name.as_ptr(),
            if directory { libc::AT_REMOVEDIR } else { 0 },
        )
    } != 0
    {
        return Err(storage(std::io::Error::last_os_error()));
    }
    Ok(())
}

struct Operation<'a> {
    permitted: &'a dyn Fn() -> Result<(), AppError>,
    deadline: Instant,
    cancellation: &'a Cancellation,
    source: PinnedDirectory,
    preparing: Vec<PinnedDirectory>,
    custody: Option<Custody>,
}

impl Operation<'_> {
    fn check(&self) -> Result<(), AppError> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(refuse("assembly deadline or cancellation reached"));
        }
        (self.permitted)()?;
        self.source.validate()?;
        for directory in &self.preparing {
            directory.validate()?;
        }
        if let Some(custody) = &self.custody {
            custody.validate()?;
        }
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(refuse(
                "assembly authority expired during permission inspection",
            ));
        }
        Ok(())
    }

    fn capture(&self, command: Command, answers: &'static [i32]) -> Result<Captured, AppError> {
        self.check()?;
        // The caller owns the total deadline; no per-child renewal or hidden
        // fixed timeout can shorten a patient fixture's explicit allowance.
        let cancelled = || self.check().is_err();
        let result =
            run_captured_query_quiescent(command, self.deadline, &cancelled, ANSWER_LIMIT, answers)
                .map_err(|error| refuse(&format!("Git child failed: {}", error.detail())))?;
        self.check()?;
        if result.stdout_truncated
            || (!result.status.success()
                && !result
                    .status
                    .code()
                    .is_some_and(|code| answers.contains(&code)))
        {
            return Err(refuse(&format!(
                "Git refused assembly: {}",
                String::from_utf8_lossy(&result.stderr)
            )));
        }
        Ok(result)
    }

    fn git(
        &self,
        root: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        answers: &'static [i32],
    ) -> Result<Captured, AppError> {
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refuse("private Git has no native custody"))?;
        if root != custody.repository {
            return Err(refuse("private Git repository differs from its owner"));
        }
        let mut command = clean_git(root);
        command.arg("--bare").args(args).envs(env.iter().copied());
        // cwd is not Git authority: .git discovery, commondir and inherited
        // object/index selectors cannot redirect these private effects.
        command
            .env("GIT_DIR", root)
            .env("GIT_COMMON_DIR", root)
            .env("GIT_OBJECT_DIRECTORY", root.join("objects"))
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", "")
            .env("GIT_INDEX_FILE", root.join("assembly.index"));
        self.capture(command, answers)
    }

    fn source_git(&self, args: &[&str]) -> Result<Captured, AppError> {
        let mut command = clean_git(&self.source.identity.path);
        command.args(args);
        self.capture(command, &[])
    }

    fn create(&self, path: &Path, bytes: &[u8]) -> Result<File, AppError> {
        self.check()?;
        let mut file = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(storage)?;
        file.write_all(bytes).map_err(storage)?;
        file.sync_all().map_err(storage)?;
        self.check()?;
        Ok(file)
    }
}

fn clean_git(root: &Path) -> Command {
    let mut command = crate::env::git_env::command(root);
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "merge.default=text",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
        ]);
    command
}

fn prepare_parent(op: &Operation<'_>, path: &Path) -> Result<(), AppError> {
    op.check()?;
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && path.canonicalize().map_err(storage)? == path =>
        {
            Ok(())
        }
        Ok(_) => Err(refuse("assembly parent is not an ordinary directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| refuse("assembly path has no parent"))?;
            prepare_parent(op, parent)?;
            op.check()?;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(storage)?;
            op.check()
        }
        Err(error) => Err(storage(error)),
    }
}

fn assemble(
    inputs: Inputs<'_>,
    permitted: &dyn Fn() -> Result<(), AppError>,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<NativeAssembly, AppError> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Err(refuse("assembly authority expired"));
    }
    permitted()?;
    let source = &inputs.submission.checkout;
    if !inputs.workspace.is_absolute()
        || source.canonicalize().map_err(storage)? != *source
        || inputs.workspace.starts_with(source)
        || inputs.plan.base != inputs.submission.base
        || inputs.plan.head != inputs.submission.head
        || inputs.plan.files.is_empty()
    {
        return Err(refuse("assembly input identity is inconsistent"));
    }
    match fs::symlink_metadata(inputs.workspace) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(storage(error)),
        Ok(_) => {
            return Err(refuse(
                "reserved assembly path already exists; never adopt or replay it",
            ));
        }
    }
    let mut op = Operation {
        permitted,
        deadline,
        cancellation,
        source: PinnedDirectory::open(source)?,
        preparing: Vec::new(),
        custody: None,
    };
    let format = text(
        &op.source_git(&["rev-parse", "--show-object-format"])?
            .stdout,
    )?;
    if !matches!(format.as_str(), "sha1" | "sha256") {
        return Err(refuse("unsupported Git object format"));
    }
    let common = text(
        &op.source_git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?
            .stdout,
    )?;
    let common = Path::new(&common).canonicalize().map_err(storage)?;
    if inputs.workspace.starts_with(&common) {
        return Err(refuse(
            "private assembly must be outside author Git administration",
        ));
    }
    // Resolve the actual configured identity in the registered source context.
    // The native Git allowlist excludes inherited GIT_AUTHOR/COMMITTER values.
    let identity = |name| -> Result<String, AppError> {
        let mut command = crate::env::git_env::command(source);
        command.args(["-c", "user.useConfigOnly=true", "var", name]);
        text(&op.capture(command, &[])?.stdout)
    };
    let author = identity("GIT_AUTHOR_IDENT")?;
    let committer = identity("GIT_COMMITTER_IDENT")?;
    let author_parts = split_identity(&author)?;
    let committer_parts = split_identity(&committer)?;
    let parent = inputs
        .workspace
        .parent()
        .ok_or_else(|| refuse("assembly path has no parent"))?;
    prepare_parent(&op, parent)?;
    if parent.canonicalize().map_err(storage)? != parent {
        return Err(refuse("assembly parent is not canonical"));
    }
    let parent_pin = PinnedDirectory::open(parent)?;
    op.preparing.push(parent_pin);
    op.check()?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(inputs.workspace)
        .map_err(storage)?;
    let root = PinnedDirectory::open(inputs.workspace)?;
    let workspace = root.identity.clone();
    op.preparing.push(root);
    let stamp_bytes = serde_json::to_vec(&serde_json::json!({
        "version":1, "owner":inputs.owner, "epoch":inputs.epoch,
        "nonce":uuid::Uuid::new_v4().simple().to_string(),
        "plan_sha256":digest(&serde_json::to_vec(inputs.plan).map_err(storage)?),
        "submission_sha256":digest(&serde_json::to_vec(inputs.submission).map_err(storage)?)
    }))
    .map_err(storage)?;
    op.check()?;
    let stamp = inputs.workspace.join(STAMP);
    let stamp_file = op.create(&stamp, &stamp_bytes)?;
    op.custody = Some(Custody {
        repository: inputs.workspace.into(),
        directories: std::mem::take(&mut op.preparing),
        stamp,
        stamp_file,
        stamp_bytes,
    });
    let root = inputs.workspace;
    op.git(
        root,
        &[
            "init",
            "--bare",
            "--quiet",
            "--template=",
            &format!("--object-format={format}"),
            ".",
        ],
        &[],
        &[],
    )?;
    let objects = PinnedDirectory::open(&root.join("objects"))?;
    op.custody
        .as_mut()
        .expect("created above")
        .directories
        .push(objects);
    let source_name = source
        .to_str()
        .ok_or_else(|| refuse("source path is not UTF-8"))?;
    // Only this absolute local registered checkout is a source. No remote URL,
    // ref mapping, remote-tracking ref or author object-store write is permitted.
    op.git(
        root,
        &[
            "fetch",
            "--no-auto-maintenance",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-recurse-submodules",
            "--no-progress",
            "--",
            source_name,
            &inputs.plan.base,
            &inputs.plan.head,
        ],
        &[],
        &[],
    )?;
    for parent in [&inputs.plan.base, &inputs.plan.head] {
        let actual = text(
            &op.git(
                root,
                &["rev-parse", "--verify", &format!("{parent}^{{commit}}")],
                &[],
                &[],
            )?
            .stdout,
        )?;
        if &actual != parent {
            return Err(refuse("imported original parent differs"));
        }
    }
    let empty = answer_oid(
        &op.git(
            root,
            &["hash-object", "-t", "tree", "-w", "--stdin"],
            &[],
            &[],
        )?
        .stdout,
        LABEL,
        "empty attribute tree",
    )?;
    let merged = op.git(
        root,
        &[
            &format!("--attr-source={empty}"),
            "-c",
            "merge.conflictStyle=diff3",
            "merge-tree",
            "--write-tree",
            "-z",
            &inputs.plan.base,
            &inputs.plan.head,
        ],
        &[],
        &[1],
    )?;
    let TrialMerge::Conflict { shape, .. } =
        merge_answer(&merged, LABEL, &inputs.plan.base, &inputs.plan.head)?
    else {
        return Err(refuse("native conflict shape changed"));
    };
    let mut blobs = Blobs { op: &op, root };
    let raw = blobs.file(&inputs.plan.base, batch_smoothing::POINTER)?;
    let policy = policy_from_pointer(raw.as_deref()).map_err(|why| refuse(&why))?;
    let batch_smoothing::Classification::UnionSmoothable(files) =
        batch_smoothing::classify(&shape, &mut blobs)
    else {
        return Err(refuse("native conflict is no longer insertion-only"));
    };
    let fresh = IntegrationPlan {
        version: 1,
        base: inputs.plan.base.clone(),
        head: inputs.plan.head.clone(),
        conflicted_tree: shape.tree,
        policy: policy.digest.clone(),
        strategy: crate::domain::conflict_smoothing::STRATEGY.into(),
        files: files
            .iter()
            .map(|file| IntegrationFile {
                path: file.path.clone(),
                base: file.base.clone(),
                ours: file.ours.clone(),
                theirs: file.theirs.clone(),
                resolved_sha256: digest(file.resolved.as_bytes()),
            })
            .collect(),
    };
    if !policy.enabled
        || !batch_smoothing::admits_all(&policy.smooth, &files)
        || fresh != *inputs.plan
    {
        return Err(refuse(
            "policy, source blob, conflict tree or resolution digest changed",
        ));
    }
    let index = root.join("assembly.index");
    let index = index
        .to_str()
        .ok_or_else(|| refuse("index path is not UTF-8"))?;
    let env = [("GIT_INDEX_FILE", index)];
    op.git(root, &["read-tree", &fresh.conflicted_tree], &env, &[])?;
    let mut approved_blobs = Vec::with_capacity(files.len());
    for (number, file) in files.iter().enumerate() {
        let name = format!("resolved-{number}.blob");
        op.create(&root.join(&name), file.resolved.as_bytes())?;
        let oid = answer_oid(
            &op.git(
                root,
                &["hash-object", "-w", "--no-filters", "--", &name],
                &[],
                &[],
            )?
            .stdout,
            LABEL,
            "resolved blob",
        )?;
        let bytes = op.git(root, &["cat-file", "blob", &oid], &[], &[])?.stdout;
        if bytes != file.resolved.as_bytes()
            || digest(&bytes) != fresh.files[number].resolved_sha256
        {
            return Err(refuse(
                "staged resolution bytes differ from the native proposal",
            ));
        }
        approved_blobs.push(oid.clone());
        op.git(
            root,
            &[
                "update-index",
                "--cacheinfo",
                &format!("100644,{oid},{}", file.path),
            ],
            &env,
            &[],
        )?;
    }
    let tree = answer_oid(
        &op.git(root, &["write-tree"], &env, &[])?.stdout,
        LABEL,
        "resolution tree",
    )?;
    let changed = op
        .git(
            root,
            &[
                "diff-tree",
                "--no-ext-diff",
                "-r",
                "--name-only",
                "-z",
                &fresh.conflicted_tree,
                &tree,
            ],
            &[],
            &[],
        )?
        .stdout;
    let mut paths: Vec<_> = changed
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| text(path))
        .collect::<Result<_, _>>()?;
    let mut expected: Vec<_> = files.iter().map(|file| file.path.clone()).collect();
    paths.sort();
    expected.sort();
    if paths != expected {
        return Err(refuse(
            "resolution changed paths outside the exact insertion proposal",
        ));
    }
    for ((file, expected_oid), evidence) in files.iter().zip(&approved_blobs).zip(&fresh.files) {
        let entry = op
            .git(
                root,
                &[
                    "--literal-pathspecs",
                    "ls-tree",
                    "-z",
                    &tree,
                    "--",
                    &file.path,
                ],
                &[],
                &[],
            )?
            .stdout;
        let expected_entry = format!("100644 blob {expected_oid}\t{}\0", file.path);
        if entry != expected_entry.as_bytes() {
            return Err(refuse(
                "final resolution tree changed an approved blob, type or mode",
            ));
        }
        let bytes = op
            .git(root, &["cat-file", "blob", expected_oid], &[], &[])?
            .stdout;
        if bytes != file.resolved.as_bytes() || digest(&bytes) != evidence.resolved_sha256 {
            return Err(refuse(
                "final resolution tree bytes differ from the native proposal",
            ));
        }
    }
    let identity_env = [
        ("GIT_AUTHOR_NAME", author_parts.0),
        ("GIT_AUTHOR_EMAIL", author_parts.1),
        ("GIT_AUTHOR_DATE", author_parts.2),
        ("GIT_COMMITTER_NAME", committer_parts.0),
        ("GIT_COMMITTER_EMAIL", committer_parts.1),
        ("GIT_COMMITTER_DATE", committer_parts.2),
    ];
    let message = format!(
        "Resolve retained integration {}\n\nStoryhook-Integration: {}\nStoryhook-Resolution: {}\n",
        inputs.owner, inputs.owner, fresh.strategy
    );
    let commit = answer_oid(
        &op.git(
            root,
            &[
                "commit-tree",
                "--no-gpg-sign",
                &tree,
                "-p",
                &fresh.base,
                "-p",
                &fresh.head,
                "-m",
                &message,
            ],
            &identity_env,
            &[],
        )?
        .stdout,
        LABEL,
        "resolution commit",
    )?;
    let observed = text(
        &op.git(root, &["show", "-s", "--format=%T%n%P", &commit], &[], &[])?
            .stdout,
    )?;
    if observed != format!("{tree}\n{} {}", fresh.base, fresh.head) {
        return Err(refuse(
            "resolution commit changed exact tree or ordered original parents",
        ));
    }
    // There are no alternates or shallow dependencies: the private repository
    // retains the complete parent closure independently of the author's checkout.
    if root.join("objects/info/alternates").exists() || root.join("shallow").exists() {
        return Err(refuse("assembly depends on external or shallow objects"));
    }
    op.git(
        root,
        &["fsck", "--connectivity-only", "--no-reflogs", &commit],
        &[],
        &[],
    )?;
    op.check()?;
    let evidence = AssemblyEvidence {
        version: 1,
        owner: inputs.owner.into(),
        epoch: inputs.epoch,
        branch: inputs.branch.into(),
        workspace,
        stamp_sha256: digest(&op.custody.as_ref().expect("created above").stamp_bytes),
        submission: inputs.submission.clone(),
        plan: fresh,
        commit,
        tree,
        author,
        committer,
    };
    // Only the successful terminal state can authorize later local deletion.
    // All assembly captures have settled; publication/gate reads may not adopt
    // additional or replaced private artifacts into this immutable inventory.
    let settlement = SettlementSnapshot::capture(op.custody.as_ref().expect("created above"))?;
    op.check()?;
    Ok(NativeAssembly {
        evidence,
        custody: op.custody.take().expect("created above"),
        settlement,
    })
}

struct Blobs<'a, 'b> {
    op: &'a Operation<'b>,
    root: &'a Path,
}
impl BlobSource for Blobs<'_, '_> {
    fn blob(&mut self, oid: &str) -> Result<Vec<u8>, AppError> {
        crate::service::trial_merge::require_pinned(oid, LABEL)?;
        Ok(self
            .op
            .git(self.root, &["cat-file", "blob", oid], &[], &[])?
            .stdout)
    }
    fn file(&mut self, treeish: &str, path: &str) -> Result<Option<Vec<u8>>, AppError> {
        let spec = entry_spec(treeish, path)?;
        let answer = self.op.git(
            self.root,
            &["rev-parse", "--verify", "--quiet", &spec],
            &[],
            &[1],
        )?;
        entry_answer(&answer, LABEL, &spec)?
            .map(|oid| self.blob(&oid))
            .transpose()
    }
}

fn split_identity(value: &str) -> Result<(&str, &str, &str), AppError> {
    let (name, rest) = value
        .rsplit_once(" <")
        .ok_or_else(|| refuse("configured Git identity is malformed"))?;
    let (email, date) = rest
        .split_once("> ")
        .ok_or_else(|| refuse("configured Git identity is malformed"))?;
    if [name, email, date]
        .iter()
        .any(|part| part.is_empty() || part.chars().any(char::is_control))
    {
        return Err(refuse("configured Git identity is incomplete"));
    }
    Ok((name, email, date))
}
fn text(bytes: &[u8]) -> Result<String, AppError> {
    String::from_utf8(bytes.to_vec())
        .map(|text| text.trim_end_matches('\n').into())
        .map_err(storage)
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn refuse(reason: &str) -> AppError {
    AppError::Validation(format!(
        "{LABEL}: {reason}; retain the reserved workspace for native reconciliation"
    ))
}
fn storage(error: impl std::fmt::Display) -> AppError {
    refuse(&error.to_string())
}

#[cfg(test)]
mod tests;
