//! Materialization has no smudge filters, symlink traversal or borrowed worktree state.

use super::*;
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};

/// Bound matches Git's full-answer limit; a larger changed file cannot exhaust the daemon.
const FILE_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(PartialEq, Eq)]
enum Input {
    Directory,
    File { mode: u32, digest: Vec<u8> },
}

/// An owned materialization of one natively established tree.
pub struct PreparedDirectory {
    directory: tempfile::TempDir,
    inputs: BTreeMap<PathBuf, Input>,
    deadline: Instant,
    cancellation: Cancellation,
}

impl PreparedDirectory {
    /// Root for the selected native command; callers must keep build outputs elsewhere.
    pub fn path(&self) -> &Path {
        self.directory.path()
    }

    /// Immutable native inventory for closed-input comparison; directories carry no bytes.
    pub(in crate::service::attribution) fn fingerprints(
        &self,
    ) -> BTreeMap<PathBuf, (u32, Vec<u8>)> {
        self.inputs
            .iter()
            .filter_map(|(path, input)| match input {
                Input::Directory => None,
                Input::File { mode, digest } => Some((path.clone(), (*mode, digest.clone()))),
            })
            .collect()
    }

    /// Read a bounded source from the retained inventory without following a replaced leaf.
    pub(in crate::service::attribution) fn source(&self, name: &str) -> Result<String, AppError> {
        self.check()?;
        let Some(Input::File { digest, .. }) = self.inputs.get(Path::new(name)) else {
            return Err(invalid("required source is not a retained file"));
        };
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.path().join(name))
            .map_err(|e| invalid(&format!("open source {name}: {e}")))?;
        // Syntax diagnosis is deliberately bounded independently of Git's larger blob allowance.
        const SOURCE_LIMIT: u64 = 64 * 1024;
        let info = file
            .metadata()
            .map_err(|e| invalid(&format!("source metadata: {e}")))?;
        if !info.is_file() || info.len() > SOURCE_LIMIT {
            return Err(invalid("source is not a bounded regular file"));
        }
        let mut bytes = Vec::new();
        file.take(SOURCE_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| invalid(&format!("read source {name}: {e}")))?;
        self.check()?;
        if bytes.len() as u64 > SOURCE_LIMIT || Sha256::digest(&bytes).as_slice() != digest {
            return Err(invalid("source changed after native materialization"));
        }
        String::from_utf8(bytes).map_err(|e| invalid(&format!("source {name} is not UTF-8: {e}")))
    }

    /// Recheck the complete path set, contents and modes after owned children have settled.
    pub fn verify_unchanged(&self) -> Result<(), AppError> {
        let mut observed = BTreeMap::new();
        self.inspect(self.path(), &mut observed)?;
        if observed != self.inputs {
            return Err(invalid("materialized tree changed during diagnosis"));
        }
        Ok(())
    }

    /// Explicitly settle owned files; failure must prevent a diagnostic cleanup claim.
    pub fn close(self) -> Result<(), AppError> {
        self.directory
            .close()
            .map_err(|e| invalid(&format!("materialization cleanup: {e}")))
    }

    fn check(&self) -> Result<(), AppError> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(invalid(
                "input recheck cancelled or active deadline reached",
            ));
        }
        Ok(())
    }

    fn inspect(
        &self,
        directory: &Path,
        found: &mut BTreeMap<PathBuf, Input>,
    ) -> Result<(), AppError> {
        self.check()?;
        if !fs::symlink_metadata(directory)
            .map_err(|e| invalid(&format!("inspect directory: {e}")))?
            .is_dir()
        {
            return Err(invalid("materialized directory changed type"));
        }
        for entry in
            fs::read_dir(directory).map_err(|e| invalid(&format!("read directory: {e}")))?
        {
            self.check()?;
            let path = entry
                .map_err(|e| invalid(&format!("read entry: {e}")))?
                .path();
            let relative = path
                .strip_prefix(self.path())
                .map_err(|e| invalid(&format!("input escaped root: {e}")))?
                .to_path_buf();
            let expected = self
                .inputs
                .get(&relative)
                .ok_or_else(|| invalid(&format!("unexpected input {relative:?}")))?;
            let metadata = fs::symlink_metadata(&path)
                .map_err(|e| invalid(&format!("inspect input {relative:?}: {e}")))?;
            let input = if metadata.is_dir() && matches!(expected, Input::Directory) {
                self.inspect(&path, found)?;
                Input::Directory
            } else if metadata.is_file() && matches!(expected, Input::File { .. }) {
                let mut file = fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&path)
                    .map_err(|e| invalid(&format!("open input {relative:?}: {e}")))?;
                let facts = file
                    .metadata()
                    .map_err(|e| invalid(&format!("input metadata: {e}")))?;
                if !facts.is_file() || facts.len() > FILE_LIMIT {
                    return Err(invalid("input type or size changed"));
                }
                let mut digest = Sha256::new();
                let mut bytes = [0u8; 16 * 1024];
                let mut count = 0u64;
                loop {
                    self.check()?;
                    let size = file
                        .read(&mut bytes)
                        .map_err(|e| invalid(&format!("read input {relative:?}: {e}")))?;
                    if size == 0 {
                        break;
                    }
                    count += size as u64;
                    if count > FILE_LIMIT {
                        return Err(invalid("input exceeds diagnostic bound"));
                    }
                    digest.update(&bytes[..size]);
                }
                Input::File {
                    mode: facts.permissions().mode() & 0o777,
                    digest: digest.finalize().to_vec(),
                }
            } else {
                return Err(invalid(&format!("input type changed: {relative:?}")));
            };
            found.insert(relative, input);
        }
        Ok(())
    }
}

pub(super) fn materialize(
    trees: &PreparedTrees,
    entries: &Entries,
) -> Result<PreparedDirectory, AppError> {
    trees.check()?;
    for entry in entries.values() {
        regular(entry)?;
    }
    let directory = tempfile::Builder::new()
        .prefix("storyhook-diagnosis-tree-")
        .tempdir()
        .map_err(|e| invalid(&format!("create materialization: {e}")))?;
    let mut inputs = BTreeMap::new();
    for (name, entry) in entries {
        trees.check()?;
        let bytes = trees.query(&["cat-file", "blob", &entry.oid])?;
        let path = directory.path().join(name);
        let relative = Path::new(name);
        for parent in relative
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            inputs.insert(parent.to_path_buf(), Input::Directory);
        }
        fs::create_dir_all(
            path.parent()
                .ok_or_else(|| invalid("materialization has no parent"))?,
        )
        .map_err(|e| invalid(&format!("create parent for {}: {e}", path.display())))?;
        let mode = if entry.mode == "100755" { 0o755 } else { 0o644 };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| invalid(&format!("create {}: {e}", path.display())))?;
        file.write_all(&bytes)
            .and_then(|()| file.set_permissions(fs::Permissions::from_mode(mode)))
            .map_err(|e| invalid(&format!("materialize {}: {e}", path.display())))?;
        inputs.insert(
            relative.to_path_buf(),
            Input::File {
                mode,
                digest: Sha256::digest(&bytes).to_vec(),
            },
        );
    }
    trees.check()?;
    let result = PreparedDirectory {
        directory,
        inputs,
        deadline: trees.deadline,
        cancellation: trees.cancellation.clone(),
    };
    result.verify_unchanged()?;
    Ok(result)
}
