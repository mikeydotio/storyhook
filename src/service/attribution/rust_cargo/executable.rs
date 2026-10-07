//! Retain an open file so replacement cannot reuse its inode unnoticed.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};

const ARTIFACT_LIMIT: u64 = 512 * 1024 * 1024;

#[derive(PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    mode: u32,
    size: u64,
    digest: Vec<u8>,
}

/// A retained executable identity; this value grants no causal return authority.
pub struct RustExecutable {
    root: PathBuf,
    path: PathBuf,
    _file: File,
    identity: Identity,
    deadline: Instant,
}

impl RustExecutable {
    pub(super) fn retain(root: &Path, path: PathBuf, deadline: Instant) -> Result<Self, String> {
        let (file, identity) = inspect(&path, deadline)?;
        Ok(Self {
            root: root
                .canonicalize()
                .map_err(|e| format!("Cargo output root: {e}"))?,
            path,
            _file: file,
            identity,
            deadline,
        })
    }
    /// Exact executable whose file identity was retained.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Refuse replacement, changed bytes or mode before and after native use.
    pub fn verify_unchanged(&self) -> Result<(), String> {
        regular_within(&self.root, &self.path)?;
        if inspect(&self.path, self.deadline)?.1 != self.identity {
            return Err("Cargo executable changed after its build".into());
        }
        Ok(())
    }
}

fn inspect(path: &Path, deadline: Instant) -> Result<(File, Identity), String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("open Cargo executable: {e}"))?;
    let before = file
        .metadata()
        .map_err(|e| format!("Cargo executable metadata: {e}"))?;
    if !before.is_file() || before.mode() & 0o111 == 0 || before.len() > ARTIFACT_LIMIT {
        return Err("Cargo artifact is not a bounded regular executable".into());
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut size = 0;
    loop {
        if Instant::now() >= deadline {
            return Err("Cargo executable check exhausted diagnosis deadline".into());
        }
        let n = file
            .read(&mut buffer)
            .map_err(|e| format!("read Cargo executable: {e}"))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > ARTIFACT_LIMIT {
            return Err("Cargo executable grew beyond the diagnostic bound".into());
        }
        digest.update(&buffer[..n]);
    }
    let after = file
        .metadata()
        .map_err(|e| format!("recheck Cargo executable metadata: {e}"))?;
    if size != before.len()
        || before.len() != after.len()
        || before.mode() != after.mode()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err("Cargo executable changed while being inspected".into());
    }
    Ok((
        file,
        Identity {
            device: before.dev(),
            inode: before.ino(),
            mode: before.mode(),
            size,
            digest: digest.finalize().to_vec(),
        },
    ))
}
